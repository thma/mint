//! Honest measurement of limiter gain-curve smoothness.
//!
//! The limiter's look-ahead attack used to be a pure step: when a peak entered the
//! sliding-min window, the gain dropped to its final value within one sample. A step in
//! the gain signal has unbounded slew and wide spectral skirts that amplitude-modulate
//! the program material ("splatter"). The raised-cosine attack ramp spreads that drop
//! over the full look-ahead window.
//!
//! Method: a steady 997 Hz carrier plus periodic single-sample spikes that force ~7 dB
//! of gain reduction 4 times per second. The per-sample gain trajectory is recovered
//! from output/input (away from carrier zero crossings), then judged two ways:
//!
//! 1. max per-sample gain slew — a step fails this outright;
//! 2. fraction of gain-signal spectral energy above 2 kHz — a 1.2 ms raised-cosine
//!    ramp rolls off well below that, a step's skirts do not.
//!
//! Measured on this exact signal (48 kHz, balanced character, −1 dBTP ceiling):
//!   step attack (pre-ramp):  max slew ≈ 5.5e-1 /sample, HF fraction ≈ 7.9e-4
//!   raised-cosine ramp:      max slew ≈ 1.9e-2 /sample, HF fraction ≈ 5.8e-8
//! Bounds below sit between the two with wide margins in both directions.

use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::ops::limiter;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

const RATE: u32 = 48_000;
const SECONDS: usize = 8;
const CARRIER_HZ: f64 = 997.0;
const CARRIER_AMP: f64 = 0.4;
const SPIKE_VALUE: f64 = 2.0;
const SPIKE_PERIOD: usize = 12_000; // 4 Hz
const CEILING_DBTP: f64 = -1.0;

/// Below this input magnitude the output/input ratio is numerically meaningless
/// (carrier zero crossings); such samples are bridged by linear interpolation.
const GAIN_RECOVERY_MIN_INPUT: f64 = 0.05;

fn make_buffer(channels: Vec<Vec<f64>>, rate: u32) -> AudioBuffer {
    let ch_count = channels.len();
    AudioBuffer {
        channels,
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("test.wav"),
            channels: ch_count,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

fn test_signal() -> Vec<f64> {
    let n = RATE as usize * SECONDS;
    let mut sig: Vec<f64> = (0..n)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            (2.0 * std::f64::consts::PI * CARRIER_HZ * t).sin() * CARRIER_AMP
        })
        .collect();
    // Skip the first period so the initial attack ramp has room; spikes force limiting.
    for i in (SPIKE_PERIOD..n).step_by(SPIKE_PERIOD) {
        sig[i] = SPIKE_VALUE;
    }
    sig
}

/// Recover the limiter's per-sample gain trajectory as output/input, bridging
/// zero-crossing gaps (|input| too small for a stable ratio) by linear interpolation.
fn recover_gain(input: &[f64], output: &[f64]) -> Vec<f64> {
    let n = input.len();
    let mut gain = vec![f64::NAN; n];
    for i in 0..n {
        if input[i].abs() >= GAIN_RECOVERY_MIN_INPUT {
            gain[i] = output[i] / input[i];
        }
    }
    // Bridge NaN runs; extend the first/last valid value to the edges.
    let first = gain.iter().position(|g| !g.is_nan()).expect("no valid gain samples");
    let last = gain.iter().rposition(|g| !g.is_nan()).unwrap();
    for i in 0..first {
        gain[i] = gain[first];
    }
    for i in last + 1..n {
        gain[i] = gain[last];
    }
    let mut prev_valid = first;
    for i in first + 1..=last {
        if gain[i].is_nan() {
            continue;
        }
        let gap = i - prev_valid;
        for (step, slot) in (1..gap).zip(prev_valid + 1..i) {
            let t = step as f64 / gap as f64;
            gain[slot] = gain[prev_valid] * (1.0 - t) + gain[i] * t;
        }
        prev_valid = i;
    }
    gain
}

/// Max |Δgain| between neighboring recovered samples, normalized per one-sample step
/// (interpolated gap samples share the gap's average slope, so this is conservative).
fn max_slew(gain: &[f64]) -> f64 {
    gain.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f64::max)
}

/// Fraction of the gain signal's spectral energy above `hz` (DC excluded). The gain
/// trajectory of a well-behaved limiter with a 1.2 ms attack ramp is essentially
/// band-limited below ~1 kHz; step attacks leak far past that.
fn high_frequency_fraction(gain: &[f64], hz: f64) -> f64 {
    let n = gain.len();
    let mean = gain.iter().sum::<f64>() / n as f64;
    let mut buf: Vec<Complex<f64>> =
        gain.iter().map(|&g| Complex::new(g - mean, 0.0)).collect();
    FftPlanner::new().plan_fft_forward(n).process(&mut buf);

    let cutoff_bin = (hz * n as f64 / RATE as f64).round() as usize;
    let mut total = 0.0;
    let mut high = 0.0;
    for (k, c) in buf.iter().enumerate().take(n / 2).skip(1) {
        let p = c.norm_sqr();
        total += p;
        if k >= cutoff_bin {
            high += p;
        }
    }
    high / total
}

#[test]
fn attack_ramp_removes_gain_step_splatter() {
    let input = test_signal();
    let mut buffer = make_buffer(vec![input.clone()], RATE);
    limiter::apply_true_peak_limit(&mut buffer, CEILING_DBTP).expect("limiter should not fail");
    let output = &buffer.channels[0];

    // Sanity: the ceiling must hold and the spikes must actually have forced limiting.
    let ceiling_lin = 10_f64.powf(CEILING_DBTP / 20.0);
    let peak = output.iter().map(|s| s.abs()).fold(0.0_f64, f64::max);
    assert!(peak <= ceiling_lin + 1e-6, "peak {peak} exceeds ceiling {ceiling_lin}");
    let gain = recover_gain(&input, output);
    let min_gain = gain.iter().cloned().fold(1.0_f64, f64::min);
    assert!(min_gain < 0.6, "spikes did not force limiting (min gain {min_gain})");

    let slew = max_slew(&gain);
    let hf = high_frequency_fraction(&gain, 2_000.0);
    println!("max gain slew: {slew:.3e} /sample, HF(>2kHz) fraction: {hf:.3e}");

    // Step attack measured ≈ 5.5e-1; raised-cosine ramp ≈ 1.9e-2.
    assert!(slew < 0.05, "gain slew {slew:.3e} — attack still behaves like a step");
    // Step attack measured ≈ 7.9e-4; raised-cosine ramp ≈ 5.8e-8.
    assert!(hf < 1e-4, "gain HF fraction {hf:.3e} — spectral skirts too wide");
}

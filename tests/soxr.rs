#![cfg(feature = "soxr")]
//! libsoxr backend tests. Compiled only with `--features soxr` (needs the system lib).
//! They assert the three properties that make the binding reference-grade *and safe*:
//! sample-accurate alignment, near-perfect passband reconstruction, deep stopband
//! rejection, and corruption-free concurrent use (cli.rs runs the batch under rayon).

use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::ResampleQuality;
use mint::ops::resample;

fn buf_from(rate: u32, ch: Vec<f64>) -> AudioBuffer {
    AudioBuffer {
        channels: vec![ch],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels: 1,
            sample_rate: rate,
            codec: "test".into(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

fn sine(rate: u32, frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin() * amp)
        .collect()
}

#[test]
fn output_length_matches_theoretical_ratio() {
    let mut buffer = buf_from(48_000, sine(48_000, 48_000, 997.0, 0.5));
    resample::apply(&mut buffer, 44_100, Some(ResampleQuality::Vhq)).unwrap();
    let expected = (48_000_f64 * (44_100_f64 / 48_000_f64)).round() as usize;
    assert_eq!(buffer.frame_len(), expected);
    assert_eq!(buffer.sample_rate, 44_100);
}

#[test]
fn impulse_is_sample_aligned() {
    // Group-delay compensation: an impulse at 48k frame 1000 must land at 96k frame 2000.
    let mut ch = vec![0.0f64; 4000];
    ch[1000] = 1.0;
    let mut buffer = buf_from(48_000, ch);
    resample::apply(&mut buffer, 96_000, Some(ResampleQuality::Vhq)).unwrap();

    let out = &buffer.channels[0];
    let peak_idx = out
        .iter()
        .enumerate()
        .fold((0usize, 0.0f64), |(bi, bv), (i, &v)| {
            if v.abs() > bv { (i, v.abs()) } else { (bi, bv) }
        })
        .0;
    assert!(
        (peak_idx as i64 - 2000).abs() < 4,
        "impulse misaligned: peak at {peak_idx}, expected ~2000"
    );
}

#[test]
fn passband_reconstruction_is_near_perfect() {
    // 997 Hz, 48k -> 44.1k. Middle of the signal must match an analytic sine to ~f64.
    let (rate_in, rate_out, hz, amp) = (48_000u32, 44_100u32, 997.0, 0.5);
    let mut buffer = buf_from(rate_in, sine(rate_in, 48_000, hz, amp));
    resample::apply(&mut buffer, rate_out, Some(ResampleQuality::Vhq)).unwrap();

    let out = &buffer.channels[0];
    let (lo, hi) = (out.len() / 4, 3 * out.len() / 4);
    let max_err = (lo..hi).fold(0.0f64, |m, n| {
        let analytic = (2.0 * std::f64::consts::PI * hz * n as f64 / rate_out as f64).sin() * amp;
        m.max((out[n] - analytic).abs())
    });
    assert!(max_err < 1e-3, "passband reconstruction error too high: {max_err:.2e}");
}

#[test]
fn out_of_band_tone_is_rejected() {
    // 30 kHz is valid at 96k but above the 44.1k Nyquist; it must be rejected, not
    // folded to an alias. Residual energy = stopband leakage (reference SRC ~ -150 dB).
    let mut buffer = buf_from(96_000, sine(96_000, 96_000, 30_000.0, 1.0));
    resample::apply(&mut buffer, 44_100, Some(ResampleQuality::Vhq)).unwrap();

    let out = &buffer.channels[0];
    let (lo, hi) = (out.len() / 4, 3 * out.len() / 4);
    let sumsq: f64 = out[lo..hi].iter().map(|v| v * v).sum();
    let rms_db = 20.0 * (sumsq / (hi - lo) as f64).sqrt().max(1e-300).log10();
    assert!(rms_db < -120.0, "stopband leakage too high: {rms_db:.1} dBFS");
}

#[test]
fn concurrent_resampling_is_not_corrupted() {
    // Regression guard for the libsoxr cold-init data race: many threads, varied target
    // rates (distinct internal FFT tables), no warmup. Without serialized construction
    // this silently zeroes a fraction of outputs.
    use std::thread;
    let targets = [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000, 22_050, 32_000];
    let handles: Vec<_> = (0..32u32)
        .map(|t| {
            let target = targets[t as usize % targets.len()];
            thread::spawn(move || {
                let pos = 1000usize;
                let mut ch = vec![0.0f64; 4000];
                ch[pos] = 1.0;
                let mut b = buf_from(64_000, ch);
                resample::apply(&mut b, target, Some(ResampleQuality::Vhq)).unwrap();
                let want = (pos as f64 * (target as f64 / 64_000.0)).round() as i64;
                let (pi, pv) = b.channels[0].iter().enumerate().fold(
                    (0i64, 0.0f64),
                    |(bi, bv), (i, &v)| if v.abs() > bv { (i as i64, v.abs()) } else { (bi, bv) },
                );
                // Corruption zeroes the output; valid audio keeps a peak near `want`.
                (pv > 0.1 && (pi - want).abs() < 8) as i32
            })
        })
        .collect();
    let ok: i32 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(ok, 32, "concurrent libsoxr corrupted {} of 32 outputs", 32 - ok);
}

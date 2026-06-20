use anyhow::Result;

use crate::audio::buffer::AudioBuffer;

/// 4× oversampling for inter-sample peak detection.
const OVERSAMPLE: usize = 4;

/// Taps per polyphase branch of the band-limited 4× interpolator. A 12-tap-per-phase
/// windowed-sinc reconstructs the waveform *between* samples far more faithfully than
/// linear interpolation, so estimated inter-sample peaks track the true continuous peak
/// (and the BS.1770 meter) closely — even near Nyquist, exactly where linear
/// interpolation under-reads worst and lets real peaks slip past the ceiling.
const FIR_TAPS_PER_PHASE: usize = 12;

/// Gain recovery speed after a limiting event (dB per second).
/// 30 dB/s is a standard value that sounds transparent on most material.
const RELEASE_DB_PER_SECOND: f64 = 30.0;

/// Look-ahead window in milliseconds.
/// Starting gain reduction 1 ms before the peak prevents audible transient clipping.
const LOOK_AHEAD_MS: f64 = 1.0;

/// Apply an oversampled true-peak brickwall limiter in place.
///
/// Strategy:
/// 1. Band-limited 4× FIR upsample to estimate inter-sample peaks.
/// 2. Compute per-oversampled-frame gain reduction required to stay below `ceiling_dbtp`.
/// 3. Sliding-minimum look-ahead: propagate gain reduction backward so attenuation
///    starts BEFORE the peak arrives (eliminates brickwall overshoot).
/// 4. Forward pass with instant attack and controlled release.
/// 5. Downsample gain curve to original rate; apply uniformly across all channels to
///    preserve inter-channel balance.
pub fn apply_true_peak_limit(buffer: &mut AudioBuffer, ceiling_dbtp: f64) -> Result<()> {
    let n = buffer.frame_len();
    if n == 0 {
        return Ok(());
    }

    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let over_rate = buffer.sample_rate as f64 * OVERSAMPLE as f64;

    // Oversampled-sample distance for the look-ahead.
    let look_ahead = (over_rate * LOOK_AHEAD_MS / 1000.0).round() as usize;

    // Gain recovery multiplier per oversampled sample.
    // Derivation: gain goes from g to 1.0; each step multiplies by this.
    let release_per_sample = 10_f64.powf(RELEASE_DB_PER_SECOND / (20.0 * over_rate));

    let n_over = n * OVERSAMPLE;

    // Build the worst-case absolute peak across all channels at each oversampled position.
    // Using a single combined envelope preserves inter-channel phase/balance. The
    // polyphase kernels are level-preserving and built once, then reused per channel.
    let kernels = polyphase_kernels();
    let mut envelope = vec![0.0_f64; n_over];
    for channel in &buffer.channels {
        let upsampled = upsample_fir_4x(channel, &kernels);
        for (env, s) in envelope.iter_mut().zip(upsampled.iter()) {
            *env = env.max(s.abs());
        }
    }

    // Instantaneous gain reduction: 1.0 when below ceiling, < 1.0 when exceeding.
    let g_inst: Vec<f64> = envelope
        .iter()
        .map(|&p| if p > ceiling_lin { ceiling_lin / p } else { 1.0 })
        .collect();

    // Look-ahead: sliding minimum over the next `look_ahead` oversampled frames.
    // This ensures the gain is already low enough when the peak actually arrives.
    let g_la = sliding_min_forward(&g_inst, look_ahead.max(1));

    // Forward pass: instant attack (brickwall), controlled release.
    let mut g_smooth = vec![1.0_f64; n_over];
    g_smooth[0] = g_la[0];
    for i in 1..n_over {
        let prev = g_smooth[i - 1];
        let target = g_la[i];
        if target <= prev {
            // More reduction required — attack is instantaneous for a brickwall limiter.
            g_smooth[i] = target;
        } else {
            // Signal is recovering — release at the configured rate.
            g_smooth[i] = (prev * release_per_sample).min(1.0);
        }
    }

    // Apply the gain curve to original samples.
    // Each original sample i maps to oversampled position i*OVERSAMPLE.
    for channel in &mut buffer.channels {
        for (i, sample) in channel.iter_mut().enumerate() {
            *sample *= g_smooth[i * OVERSAMPLE];
        }
    }

    Ok(())
}

/// Build the `OVERSAMPLE` polyphase kernels of a windowed-sinc fractional-delay
/// interpolator. Phase `p` reconstructs the signal at offset `p/OVERSAMPLE` between
/// input samples. Generated from first principles (ideal sinc fractional delay ×
/// Blackman window), then normalized per phase to unity DC gain so levels are
/// preserved. Phase 0 reduces to an exact unit impulse, so original-sample positions
/// pass through untouched.
fn polyphase_kernels() -> [[f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE] {
    use std::f64::consts::PI;

    // Support spans `FIR_TAPS_PER_PHASE` input samples centered on the interpolation
    // point; tap `j` reads input offset `k = j - (half - 1)`.
    let half = (FIR_TAPS_PER_PHASE / 2) as isize;
    let m = (FIR_TAPS_PER_PHASE - 1) as f64;

    let mut kernels = [[0.0_f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE];
    for (p, kernel) in kernels.iter_mut().enumerate() {
        let frac = p as f64 / OVERSAMPLE as f64; // fractional delay in [0, 1)
        let mut sum = 0.0;
        for (j, coeff) in kernel.iter_mut().enumerate() {
            let k = j as isize - (half - 1);
            let x = frac - k as f64;
            // Ideal fractional-delay tap: sinc(frac - k), with the removable hole at 0.
            let sinc = if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) };
            // Blackman window over the tap support keeps side lobes (and thus passband
            // ripple / stopband leakage) low for a clean reconstruction.
            let n = j as f64;
            let w = 0.42 - 0.5 * (2.0 * PI * n / m).cos() + 0.08 * (4.0 * PI * n / m).cos();
            *coeff = sinc * w;
            sum += *coeff;
        }
        for coeff in kernel.iter_mut() {
            *coeff /= sum; // unity DC gain ⇒ no level shift introduced by detection
        }
    }
    kernels
}

/// 4× band-limited upsample via the precomputed polyphase kernels. Output position
/// `i*OVERSAMPLE + p` holds the signal reconstructed at input offset `i + p/OVERSAMPLE`
/// (so `i*OVERSAMPLE` is the original sample, phase 0 being a unit impulse). Samples
/// beyond the edges are treated as zero — negligible for peak detection.
fn upsample_fir_4x(signal: &[f64], kernels: &[[f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE]) -> Vec<f64> {
    let n = signal.len();
    let half = (FIR_TAPS_PER_PHASE / 2) as isize;
    let mut out = Vec::with_capacity(n * OVERSAMPLE);
    for i in 0..n {
        for kernel in kernels.iter() {
            let mut acc = 0.0;
            for (j, &c) in kernel.iter().enumerate() {
                let idx = i as isize + (j as isize - (half - 1));
                if idx >= 0 && (idx as usize) < n {
                    acc += signal[idx as usize] * c;
                }
            }
            out.push(acc);
        }
    }
    out
}

/// O(n) forward sliding minimum over a window of `window` samples.
///
/// `result[i] = min(values[i .. min(i + window, n)])`
///
/// Implemented by reversing the array, applying a backward sliding minimum
/// (standard monotonic deque), and reversing the output.
fn sliding_min_forward(values: &[f64], window: usize) -> Vec<f64> {
    if values.is_empty() {
        return Vec::new();
    }
    let rev: Vec<f64> = values.iter().cloned().rev().collect();
    let bwd = sliding_min_backward(&rev, window);
    bwd.into_iter().rev().collect()
}

/// O(n) backward sliding minimum using a monotonic deque.
///
/// `result[i] = min(values[max(0, i - window + 1) ..= i])`
fn sliding_min_backward(values: &[f64], window: usize) -> Vec<f64> {
    use std::collections::VecDeque;
    let n = values.len();
    let mut result = Vec::with_capacity(n);
    let mut deque: VecDeque<usize> = VecDeque::new();

    for i in 0..n {
        // Evict indices that have fallen outside the backward window.
        while deque.front().is_some_and(|&front| i - front >= window) {
            deque.pop_front();
        }
        // Remove back indices whose values are ≥ current; they can never be the minimum
        // for any future position while current is still in the window.
        while deque.back().is_some_and(|&back| values[back] >= values[i]) {
            deque.pop_back();
        }
        deque.push_back(i);
        result.push(values[*deque.front().unwrap()]);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn kernels_are_level_preserving() {
        // Each phase must sum to 1.0 (unity DC gain) or the detector would shift levels.
        for kernel in polyphase_kernels() {
            let sum: f64 = kernel.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "phase DC gain {sum} != 1.0");
        }
    }

    #[test]
    fn phase_zero_is_a_unit_impulse() {
        // Phase 0 must reproduce the original sample exactly (so sub-ceiling signals
        // pass through untouched and the gain-application indexing stays valid).
        let kernels = polyphase_kernels();
        let half = (FIR_TAPS_PER_PHASE / 2) as isize;
        for (j, &c) in kernels[0].iter().enumerate() {
            let k = j as isize - (half - 1);
            let expected = if k == 0 { 1.0 } else { 0.0 };
            assert!((c - expected).abs() < 1e-12, "phase-0 tap {k} = {c}, want {expected}");
        }
    }

    #[test]
    fn fir_recovers_inter_sample_peak_better_than_linear() {
        // Textbook worst case for inter-sample peaks: an fs/4 sine at 45° phase. Every
        // sample sits at exactly ±0.707 while the true continuous peak is 1.0, with the
        // crest landing exactly between samples. Linear interpolation can't see it; the
        // band-limited FIR should recover it.
        let n = 256;
        let sig: Vec<f64> = (0..n)
            .map(|i| (PI * i as f64 / 2.0 + PI / 4.0).sin()) // fs/4, +45°
            .collect();

        let sample_peak = sig.iter().fold(0.0_f64, |m, &s| m.max(s.abs()));
        assert!(sample_peak < 0.8, "samples should sit at ~0.707, not on the crest");

        let kernels = polyphase_kernels();
        let fir_peak = upsample_fir_4x(&sig, &kernels)
            .iter()
            .fold(0.0_f64, |m, &s| m.max(s.abs()));

        // Linear interpolation, for reference, never exceeds the (too-low) sample peak.
        let lin_peak = sig
            .windows(2)
            .flat_map(|w| [w[0], 0.5 * (w[0] + w[1])])
            .fold(0.0_f64, |m, s| m.max(s.abs()));

        // True peak is 1.0; FIR should land close, clearly beating linear (~0.707).
        assert!(fir_peak > 0.93, "FIR peak {fir_peak} too low");
        assert!(
            fir_peak > lin_peak + 0.05,
            "FIR ({fir_peak}) should clearly beat linear ({lin_peak})"
        );
    }
}

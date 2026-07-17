use anyhow::Result;

use crate::audio::buffer::AudioBuffer;
use crate::config::LimiterCharacter;

/// 4× oversampling for inter-sample peak detection.
const OVERSAMPLE: usize = 4;

/// Taps per polyphase branch of the band-limited 4× interpolator. A 12-tap-per-phase
/// windowed-sinc reconstructs the waveform *between* samples far more faithfully than
/// linear interpolation, so estimated inter-sample peaks track the true continuous peak
/// (and the BS.1770 meter) closely — even near Nyquist, exactly where linear
/// interpolation under-reads worst and lets real peaks slip past the ceiling.
const FIR_TAPS_PER_PHASE: usize = 12;

/// Soft-clip knee starts at this fraction of the linear ceiling.
const SOFT_CLIP_KNEE_RATIO: f64 = 0.97;

#[derive(Debug, Clone, Copy)]
pub struct LimiterOptions {
    /// Transient-focused (faster release) vs. balanced (smoother release).
    pub character: LimiterCharacter,
    /// Apply a gentle post-limiter saturating curve near the ceiling.
    pub soft_clip: bool,
    /// When true, one shared envelope is applied across all channels.
    pub link_channels: bool,
}

impl Default for LimiterOptions {
    fn default() -> Self {
        Self {
            character: LimiterCharacter::Balanced,
            soft_clip: false,
            link_channels: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ReleaseProfile {
    /// Slow end of the adaptive release (heavy gain reduction).
    base_db_per_second: f64,
    /// Fast end of the adaptive release (light gain reduction).
    fast_db_per_second: f64,
    /// Look-ahead in milliseconds.
    look_ahead_ms: f64,
}

fn release_profile(character: LimiterCharacter) -> ReleaseProfile {
    match character {
        // Smoother envelope movement, slightly longer look-ahead.
        LimiterCharacter::Balanced => ReleaseProfile {
            base_db_per_second: 18.0,
            fast_db_per_second: 42.0,
            look_ahead_ms: 1.2,
        },
        // Faster recovery and shorter look-ahead for a snappier feel.
        LimiterCharacter::Transient => ReleaseProfile {
            base_db_per_second: 30.0,
            fast_db_per_second: 75.0,
            look_ahead_ms: 0.6,
        },
    }
}

/// Apply an oversampled true-peak brickwall limiter in place.
///
/// Strategy:
/// 1. Band-limited 4× FIR upsample to estimate inter-sample peaks.
/// 2. Compute per-oversampled-frame gain reduction required to stay below `ceiling_dbtp`.
/// 3. Sliding-minimum look-ahead: propagate gain reduction backward so attenuation
///    starts BEFORE the peak arrives (eliminates brickwall overshoot).
/// 4. Forward pass with instant attack and controlled release.
/// 5. Raised-cosine smoothing over the look-ahead window: the attack becomes a smooth
///    ramp reaching full attenuation exactly when the peak arrives, instead of a step
///    (removes gain-slew modulation distortion; provably never lifts a sample above
///    the ceiling — see `smooth_gain_curve`).
/// 6. Downsample gain curve to original rate; apply uniformly across all channels to
///    preserve inter-channel balance.
pub fn apply_true_peak_limit(buffer: &mut AudioBuffer, ceiling_dbtp: f64) -> Result<()> {
    apply_true_peak_limit_with_options(buffer, ceiling_dbtp, LimiterOptions::default())
}

/// Apply an oversampled true-peak brickwall limiter with configurable character,
/// optional soft clipping, and channel linking mode.
pub fn apply_true_peak_limit_with_options(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    options: LimiterOptions,
) -> Result<()> {
    let n = buffer.frame_len();
    if n == 0 {
        return Ok(());
    }

    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let over_rate = buffer.sample_rate as f64 * OVERSAMPLE as f64;
    let profile = release_profile(options.character);

    // Oversampled-sample distance for the look-ahead.
    let look_ahead = (over_rate * profile.look_ahead_ms / 1000.0).round() as usize;

    let n_over = n * OVERSAMPLE;
    let kernels = polyphase_kernels();

    if options.link_channels {
        // One combined envelope preserves inter-channel imaging.
        let mut envelope = vec![0.0_f64; n_over];
        for channel in &buffer.channels {
            let upsampled = upsample_fir_4x(channel, &kernels);
            for (env, s) in envelope.iter_mut().zip(upsampled.iter()) {
                *env = env.max(s.abs());
            }
        }

        let gains = build_gain_curve(&envelope, ceiling_lin, look_ahead.max(1), over_rate, profile);
        for channel in &mut buffer.channels {
            for (i, sample) in channel.iter_mut().enumerate() {
                *sample *= gains[i * OVERSAMPLE];
            }
        }
    } else {
        // Independent envelopes can preserve more channel-specific transients at the
        // cost of possible stereo image movement under heavy limiting.
        for channel in &mut buffer.channels {
            let upsampled = upsample_fir_4x(channel, &kernels);
            let envelope: Vec<f64> = upsampled.iter().map(|s| s.abs()).collect();
            let gains =
                build_gain_curve(&envelope, ceiling_lin, look_ahead.max(1), over_rate, profile);
            for (i, sample) in channel.iter_mut().enumerate() {
                *sample *= gains[i * OVERSAMPLE];
            }
        }
    }

    if options.soft_clip {
        apply_soft_clip(buffer, ceiling_lin);
    }

    Ok(())
}

/// Build a look-ahead brickwall gain curve with adaptive, program-dependent release
/// and a raised-cosine attack ramp spanning the look-ahead window.
fn build_gain_curve(
    envelope: &[f64],
    ceiling_lin: f64,
    look_ahead: usize,
    over_rate: f64,
    profile: ReleaseProfile,
) -> Vec<f64> {
    // Instantaneous gain reduction: 1.0 when below ceiling, < 1.0 when exceeding.
    let g_inst: Vec<f64> = envelope
        .iter()
        .map(|&p| if p > ceiling_lin { ceiling_lin / p } else { 1.0 })
        .collect();

    // Look-ahead: sliding minimum over the next `look_ahead` oversampled frames.
    let g_la = sliding_min_forward(&g_inst, look_ahead);

    // Forward pass: instant attack, adaptive release.
    let mut g_smooth = vec![1.0_f64; envelope.len()];
    g_smooth[0] = g_la[0];
    for i in 1..g_smooth.len() {
        let prev = g_smooth[i - 1];
        let target = g_la[i];
        if target <= prev {
            g_smooth[i] = target;
        } else {
            // Program-dependent release: when we're deep into gain reduction, recover
            // slower; near unity gain, recover faster.
            let gr_db = (-20.0 * prev.max(1e-12).log10()).max(0.0);
            let heavy = (gr_db / 12.0).clamp(0.0, 1.0);
            let rel_db_s = profile.fast_db_per_second
                - (profile.fast_db_per_second - profile.base_db_per_second) * heavy;
            let rel_mul = 10_f64.powf(rel_db_s / (20.0 * over_rate));
            // Clamp to `target`, not 1.0: g_smooth ≤ g_la everywhere is what lets the
            // raised-cosine smoother below guarantee the ceiling (target ≤ 1.0 always).
            g_smooth[i] = (prev * rel_mul).min(target);
        }
    }

    smooth_gain_curve(&g_smooth, look_ahead)
}

/// Causally convolve the gain curve with normalized raised-cosine (Hann) taps spanning
/// the look-ahead window, so the sliding-min attack step becomes a smooth ramp that
/// reaches full attenuation exactly when the peak arrives.
///
/// No-overshoot argument: `g_la[i] = min(g_inst[i .. i+L))` implies
/// `g_la[n−k] ≤ g_inst[n]` for every `k ∈ [0, L)`, and the release pass keeps
/// `gains[i] ≤ g_la[i]` (its release branch clamps to target). Each output
/// `Σ w[k]·gains[n−k]` is therefore a convex combination of values `≤ g_inst[n]` —
/// the ramp can never lift a sample above its own ceiling requirement. Reads before
/// index 0 use `gains[0]` (= the minimum over the first window), which keeps the bound
/// valid for peaks inside the file's first look-ahead window; padding with 1.0 would not.
fn smooth_gain_curve(gains: &[f64], window: usize) -> Vec<f64> {
    if window <= 1 || gains.is_empty() {
        return gains.to_vec();
    }
    let taps = hann_smoothing_taps(window);
    let n = gains.len();
    let mut out = Vec::with_capacity(n);

    // Count of non-unity gains in the trailing window [i−window+1, i]. While zero, the
    // output is exactly 1.0 and the dot product is skipped — this keeps the limiter
    // ~O(n) for the common occasional-overshoot case and preserves bit-exact passthrough
    // of below-ceiling material (no reliance on the taps summing to exactly 1.0).
    let mut active_in_window = 0usize;
    for i in 0..n {
        if gains[i] != 1.0 {
            active_in_window += 1;
        }
        if i >= window && gains[i - window] != 1.0 {
            active_in_window -= 1;
        }
        if active_in_window == 0 {
            out.push(1.0);
            continue;
        }
        let mut acc = 0.0;
        for (k, &w) in taps.iter().enumerate() {
            let g = if i >= k { gains[i - k] } else { gains[0] };
            acc += w * g;
        }
        out.push(acc);
    }
    out
}

/// Normalized raised-cosine (Hann) taps. The `(k+1)/(len+1)` phase avoids the zero
/// endpoints of a textbook Hann window (which would waste support), and normalizing to
/// unity sum makes the smoother a convex combination — level-preserving on constant
/// input, never exceeding the window's maximum.
fn hann_smoothing_taps(len: usize) -> Vec<f64> {
    use std::f64::consts::PI;
    let mut taps: Vec<f64> = (0..len)
        .map(|k| 0.5 * (1.0 - (2.0 * PI * (k + 1) as f64 / (len + 1) as f64).cos()))
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

fn apply_soft_clip(buffer: &mut AudioBuffer, ceiling_lin: f64) {
    let knee = ceiling_lin * SOFT_CLIP_KNEE_RATIO;
    if knee <= 0.0 || knee >= ceiling_lin {
        return;
    }
    let tanh_norm = 1.0_f64.tanh();

    for channel in &mut buffer.channels {
        for sample in channel {
            let sign = sample.signum();
            let a = sample.abs();
            if a <= knee {
                continue;
            }
            let t = ((a - knee) / (ceiling_lin - knee)).clamp(0.0, 1.0);
            let shaped = knee + (ceiling_lin - knee) * (t.tanh() / tanh_norm);
            *sample = sign * shaped.min(ceiling_lin);
        }
    }
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
    fn smoothing_taps_are_normalized_and_nonnegative() {
        for len in [1_usize, 2, 3, 57, 230] {
            let taps = hann_smoothing_taps(len);
            assert_eq!(taps.len(), len);
            assert!(taps.iter().all(|&t| t > 0.0), "len {len}: zero/negative tap");
            let sum: f64 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "len {len}: tap sum {sum} != 1.0");
        }
    }

    #[test]
    fn smoothed_gain_never_exceeds_instantaneous_requirement() {
        // Encodes the no-overshoot proof of `smooth_gain_curve`: every sample's final
        // (ramped) gain must still satisfy that sample's own instantaneous requirement.
        // Includes a peak inside the first look-ahead window (left-edge padding case).
        let ceiling = 0.5;
        let look_ahead = 37;
        let profile = release_profile(LimiterCharacter::Balanced);

        // Deterministic LCG envelope: mostly sub-ceiling, occasional loud spikes, so
        // attack, release, and unity fast path all get exercised.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let envelope: Vec<f64> = (0..4096)
            .map(|i| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let r = (state >> 11) as f64 / (1_u64 << 53) as f64;
                if i == 5 || r > 0.97 { 0.5 + 2.0 * r } else { 0.4 * r }
            })
            .collect();

        let gains = build_gain_curve(&envelope, ceiling, look_ahead, 192_000.0, profile);
        for (i, (&g, &p)) in gains.iter().zip(envelope.iter()).enumerate() {
            let required = if p > ceiling { ceiling / p } else { 1.0 };
            assert!(g <= required + 1e-12, "gain {g} at {i} exceeds requirement {required}");
        }
    }

    #[test]
    fn attack_is_a_raised_cosine_ramp_not_a_step() {
        let ceiling = 1.0;
        let look_ahead = 48;
        let profile = release_profile(LimiterCharacter::Balanced);
        let peak_pos = 500;
        let peak_val = 2.0;
        let mut envelope = vec![0.1_f64; 1000];
        envelope[peak_pos] = peak_val;

        let gains = build_gain_curve(&envelope, ceiling, look_ahead, 192_000.0, profile);
        let required = ceiling / peak_val;

        // Full required attenuation exactly when the peak arrives.
        assert!(gains[peak_pos] <= required + 1e-12, "peak gain {} too high", gains[peak_pos]);

        // Attenuation starts exactly one look-ahead early — and not a sample sooner:
        // everything before is untouched unity via the fast path (bit-exact passthrough).
        let ramp_start = peak_pos - look_ahead + 1;
        for (i, &g) in gains[..ramp_start].iter().enumerate() {
            assert_eq!(g, 1.0, "gain at {i} not exactly 1.0 before the ramp");
        }
        assert!(gains[ramp_start] < 1.0, "ramp did not start at peak - look_ahead + 1");

        // The largest per-sample drop of a smoothed step is the step height times the
        // largest tap (≈ 2/L) — a fraction of the old one-sample step. The small extra
        // margin absorbs release movement overlapping the ramp.
        let step = 1.0 - required;
        let max_tap = hann_smoothing_taps(look_ahead).into_iter().fold(0.0_f64, f64::max);
        let max_slew = gains.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0_f64, f64::max);
        assert!(
            max_slew <= step * max_tap + 1e-5,
            "slew {max_slew} exceeds raised-cosine bound {}",
            step * max_tap
        );
    }

    #[test]
    fn smoothing_unity_regions_stay_exactly_one() {
        // The fast path must emit literal 1.0 wherever the trailing window holds no
        // gain reduction — this is what keeps below-ceiling material bit-exact.
        let gains = smooth_gain_curve(&vec![1.0_f64; 500], 37);
        assert!(gains.iter().all(|&g| g == 1.0), "unity input must smooth to literal 1.0");
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

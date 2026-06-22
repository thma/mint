use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::audio::buffer::AudioBuffer;

pub struct TpdfDither {
    rng: StdRng,
}

impl TpdfDither {
    pub fn new(seed: Option<u64>) -> Self {
        let rng = match seed {
            Some(seed) => StdRng::seed_from_u64(seed),
            None => StdRng::from_entropy(),
        };
        Self { rng }
    }

    pub fn sample_lsb(&mut self) -> f64 {
        // TPDF in range [-1, 1] LSB built from two independent U(-0.5, 0.5) draws.
        // Non-subtractive: added before rounding, never subtracted, so the noise
        // floor sits ~4.77 dB above an ideal undithered quantizer — the price of
        // decoupling both the mean AND variance of the error from the signal.
        let a: f64 = self.rng.gen_range(-0.5_f64..0.5_f64);
        let b: f64 = self.rng.gen_range(-0.5_f64..0.5_f64);
        a + b
    }

    pub fn sample_scaled_lsb(&mut self, amp_lsb: f64) -> f64 {
        self.sample_lsb() * amp_lsb
    }
}

#[derive(Debug, Clone, Copy)]
enum AdaptiveMode {
    Transparent,
    Balanced,
    Aggressive,
}

#[derive(Debug, Clone)]
struct ChannelState {
    hist: [f64; 5],
    prev_peak: f64,
    peak_fast: f64,
    peak_slow: f64,
    rms_sq: f64,
    lf_state: f64,
    prev_hf: f64,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            hist: [0.0; 5],
            prev_peak: 0.0,
            peak_fast: 0.0,
            peak_slow: 0.0,
            rms_sq: 0.0,
            lf_state: 0.0,
            prev_hf: 0.0,
        }
    }
}

/// MBIT+-style non-linear quantizer path.
///
/// This path keeps TPDF as the base dither source, then adapts both dither amount
/// and error-feedback shaping from a lightweight psychoacoustic/temporal model:
/// - frequency masking proxy (LF/MF/HF energies + Bark weighting)
/// - temporal masking proxy (fast/slow envelopes around transients)
/// - adaptive mode selection (Transparent/Balanced/Aggressive)
/// - stereo-correlated dither generation for stable low-level imaging.
pub fn quantize_mbit_plus(
    buffer: &mut AudioBuffer,
    max: f64,
    min_i: i32,
    max_i: i32,
    seed: Option<u64>,
) {
    if buffer.channels.is_empty() || buffer.frame_len() == 0 {
        return;
    }

    let ch_count = buffer.channels_count();
    let mut states = vec![ChannelState::default(); ch_count];

    let mut correlated = TpdfDither::new(seed);
    let mut decorrelated = (0..ch_count)
        .map(|ch| TpdfDither::new(per_channel_seed(seed, ch)))
        .collect::<Vec<_>>();

    let alpha_rms = env_alpha(0.030, buffer.sample_rate);
    let alpha_peak_fast = env_alpha(0.005, buffer.sample_rate);
    let alpha_peak_slow = env_alpha(0.080, buffer.sample_rate);
    let alpha_lf = env_alpha(0.012, buffer.sample_rate);

    for n in 0..buffer.frame_len() {
        let mix = buffer.channels.iter().map(|ch| ch[n]).sum::<f64>() / ch_count as f64;
        let abs_mix = mix.abs();

        let mut lf_energy = 0.0;
        let mut mf_energy = 0.0;
        let mut hf_energy = 0.0;

        for (ch, state) in states.iter_mut().enumerate() {
            let x = buffer.channels[ch][n];
            let abs_x = x.abs();

            state.rms_sq += alpha_rms * (x * x - state.rms_sq);
            state.peak_fast += alpha_peak_fast * (abs_x - state.peak_fast);
            state.peak_slow += alpha_peak_slow * (abs_x - state.peak_slow);

            state.lf_state += alpha_lf * (x - state.lf_state);
            let lf = state.lf_state.abs();
            let hf = (x - state.lf_state).abs();
            let mf = (abs_x - lf).max(0.0);

            lf_energy += lf;
            mf_energy += mf;
            hf_energy += hf;
        }

        let norm = 1.0 / ch_count as f64;
        lf_energy *= norm;
        mf_energy *= norm;
        hf_energy *= norm;

        let total = (lf_energy + mf_energy + hf_energy).max(1.0e-12);
        let lf_w = lf_energy / total;
        let mf_w = mf_energy / total;
        let hf_w = hf_energy / total;

        // Local loudness + dynamics proxy for adaptive mode selection.
        let rms = states
            .iter()
            .map(|s| s.rms_sq.sqrt())
            .sum::<f64>()
            * norm;
        let peak = states.iter().map(|s| s.peak_fast).sum::<f64>() * norm;
        let lufs_proxy = amp_to_db(rms);
        let dyn_range = (amp_to_db(peak) - amp_to_db(rms)).max(0.0);
        let mode = select_mode(lufs_proxy, dyn_range);

        // Temporal masking proxy: noise is less audible around transient energy.
        let prev_peak = states.iter().map(|s| s.prev_peak).sum::<f64>() * norm;
        let next_peak_est = (peak + (peak - prev_peak).max(0.0)).clamp(0.0, 1.0);
        let temporal = temporal_mask(abs_mix, prev_peak, next_peak_est);

        // Frequency masking proxies at representative center frequencies.
        let m_lf = masking_for_band(120.0, lf_energy);
        let m_mf = masking_for_band(2_000.0, mf_energy);
        let m_hf = masking_for_band(10_000.0, hf_energy);
        let masking = (lf_w * m_lf + mf_w * m_mf + hf_w * m_hf).clamp(0.05, 2.50);

        let correlation = correlation_from_scene(mode, temporal, hf_w, dyn_range);
        let base_noise = correlated.sample_lsb();

        for ch in 0..ch_count {
            let state = &mut states[ch];
            let x = buffer.channels[ch][n];

            let lsb_amp = dither_amplitude_lsb(masking, temporal, mode);
            let noise = if ch == 0 {
                base_noise * lsb_amp
            } else {
                let side = decorrelated[ch].sample_lsb();
                let side_gain = (1.0 - correlation * correlation).sqrt();
                (base_noise * correlation + side * side_gain) * lsb_amp
            };

            let fb = adaptive_feedback(state, mode, temporal, lf_w, mf_w, hf_w, masking);

            let s = x.clamp(-1.0, 1.0) * max;
            let u = s - fb;
            let w = u + noise;
            let y = (w.round() as i32).clamp(min_i, max_i);
            let e = y as f64 - u;

            state.hist.rotate_right(1);
            state.hist[0] = e;
            state.prev_peak = state.peak_fast;
            state.prev_hf = hf_w;

            buffer.channels[ch][n] = y as f64 / max;
        }
    }
}

fn per_channel_seed(seed: Option<u64>, channel: usize) -> Option<u64> {
    seed.map(|s| s ^ (channel as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn env_alpha(time_s: f64, sr: u32) -> f64 {
    let tau = (time_s * sr as f64).max(1.0);
    1.0 / tau
}

fn amp_to_db(x: f64) -> f64 {
    20.0 * x.max(1.0e-12).log10()
}

fn bark(freq_hz: f64) -> f64 {
    13.0 * (0.00076 * freq_hz).atan() + 3.5 * ((freq_hz / 7_500.0).powi(2)).atan()
}

fn hearing_threshold(freq_hz: f64, level_db: f64) -> f64 {
    let hf_penalty = (freq_hz / 8_000.0).powf(0.7);
    let lf_penalty = (200.0 / (freq_hz + 1.0)).powf(0.5);
    (1.0 + hf_penalty + lf_penalty - level_db * 0.1).clamp(0.10, 8.0)
}

fn masking_for_band(freq_hz: f64, energy: f64) -> f64 {
    let level_db = amp_to_db(energy.sqrt());
    let bark_band = bark(freq_hz);
    (hearing_threshold(freq_hz, level_db) * (1.0 / (1.0 + bark_band))).clamp(0.05, 2.5)
}

fn temporal_mask(current: f64, prev_peak: f64, next_peak_est: f64) -> f64 {
    let pre_mask = (prev_peak * 0.6).exp() - 1.0;
    let post_mask = (next_peak_est * 0.4).exp() - 1.0;
    let transient = (current * 2.0).clamp(0.0, 1.0);
    (1.0 - (pre_mask + post_mask).min(1.0)) * (0.65 + 0.35 * transient)
}

fn select_mode(lufs: f64, dynamic_range: f64) -> AdaptiveMode {
    if dynamic_range > 20.0 {
        AdaptiveMode::Transparent
    } else if lufs > -8.0 {
        AdaptiveMode::Aggressive
    } else {
        AdaptiveMode::Balanced
    }
}

fn noise_shape(freq_hz: f64, mode: AdaptiveMode) -> f64 {
    match mode {
        AdaptiveMode::Transparent => 0.30,
        AdaptiveMode::Balanced => {
            if freq_hz > 8_000.0 {
                1.80
            } else if freq_hz < 200.0 {
                0.60
            } else {
                1.0
            }
        }
        AdaptiveMode::Aggressive => {
            if freq_hz > 10_000.0 {
                2.50
            } else {
                1.20
            }
        }
    }
}

fn adaptive_feedback(
    state: &ChannelState,
    mode: AdaptiveMode,
    temporal: f64,
    lf_w: f64,
    mf_w: f64,
    hf_w: f64,
    masking: f64,
) -> f64 {
    let base = match mode {
        AdaptiveMode::Transparent => [1.00, -0.25, 0.0, 0.0, 0.0],
        AdaptiveMode::Balanced => [1.85, -1.55, 0.80, -0.20, 0.0],
        AdaptiveMode::Aggressive => [2.30, -2.40, 1.60, -0.70, 0.15],
    };

    // Keep feedback bounded; MBIT+ is adaptive but must remain numerically stable.
    let global_gain = (0.18 + 0.22 * temporal) * masking.clamp(0.2, 2.0);
    let band_blend = [
        lf_w * 0.70 + mf_w * 0.25 + hf_w * 0.05,
        lf_w * 0.45 + mf_w * 0.40 + hf_w * 0.15,
        lf_w * 0.20 + mf_w * 0.45 + hf_w * 0.35,
        lf_w * 0.10 + mf_w * 0.35 + hf_w * 0.55,
        lf_w * 0.05 + mf_w * 0.20 + hf_w * 0.75,
    ];
    let tap_freqs = [120.0, 450.0, 2_000.0, 6_000.0, 12_000.0];

    let mut fb = 0.0;
    for i in 0..state.hist.len() {
        let tap_gain = noise_shape(tap_freqs[i], mode);
        let coeff = base[i] * band_blend[i] * tap_gain * global_gain;
        fb += coeff * state.hist[i];
    }
    fb.clamp(-12.0, 12.0)
}

fn dither_amplitude_lsb(masking: f64, temporal: f64, mode: AdaptiveMode) -> f64 {
    let base = match mode {
        AdaptiveMode::Transparent => 0.70,
        AdaptiveMode::Balanced => 0.95,
        AdaptiveMode::Aggressive => 1.10,
    };
    (base * (0.70 + 0.45 * temporal) * (0.75 + 0.30 * masking)).clamp(0.40, 1.60)
}

fn correlation_from_scene(mode: AdaptiveMode, temporal: f64, hf_w: f64, dynamic_range: f64) -> f64 {
    let mode_base = match mode {
        AdaptiveMode::Transparent => 0.70,
        AdaptiveMode::Balanced => 0.82,
        AdaptiveMode::Aggressive => 0.90,
    };

    // More HF/transients and small dynamic range benefit from stronger correlation:
    // less width shimmer at very low levels.
    let dr_bias = (1.0 - (dynamic_range / 24.0).clamp(0.0, 1.0)) * 0.10;
    (mode_base + hf_w * 0.08 + (1.0 - temporal) * 0.06 + dr_bias).clamp(0.55, 0.97)
}

/// Noise-transfer curve for the shaper. Coeffs are the error-feedback FIR `h_k`;
/// the resulting noise-transfer function is `1 - H(z)`, with `H(z) = Σ h_k·z^-(k+1)`.
///
/// `Gentle` is `(1 - z^-1)^2`, derived from first principles (no memorized
/// constants). `Psychoacoustic` is a *published* curve weighted to the ear's
/// sensitivity — strictly more effective in the critical band, at the cost of more
/// total (but better-hidden) noise. Both share the same convention, so they slot in
/// behind the same error-feedback loop.
#[derive(Debug, Clone, Copy)]
pub enum ShapingCurve {
    Gentle,
    Psychoacoustic,
}

impl ShapingCurve {
    fn coeffs(self) -> &'static [f64] {
        match self {
            // (1 - z^-1)^2: gentle 2nd-order high-pass, ~+7.8 dB total noise dumped
            // near Nyquist. A null at DC but no tuned mid-band notch. Safe under
            // downstream re-encode.
            ShapingCurve::Gentle => &[2.0, -1.0],
            // Lipshitz's "minimally audible" 5-tap FIR (Lipshitz/Vanderkooy/
            // Wannamaker, "Minimally Audible Noise Shaping", JAES 39(11), 1991).
            // Coefficients copied verbatim from Audacity's reference implementation
            // (`SHAPED_BS`, lib-math/Dither.cpp), which credits the same paper.
            // Optimized for 44.1 kHz: the NTF dips to ~-27 dB right at ~4 kHz (the
            // ear's most sensitive band), reads -16.6 dB at DC, and rises to +19.4 dB
            // at Nyquist for ~+12.2 dB total noise power — the noise is moved where
            // the ear can't hear it, not removed. At other s16 rates the notch shifts
            // proportionally (still beneficial, just not perceptually tuned).
            ShapingCurve::Psychoacoustic => &[2.033, -2.165, 1.959, -1.590, 0.6149],
        }
    }

    /// Short label for the dry-run summary.
    pub fn tag(self) -> &'static str {
        match self {
            ShapingCurve::Gentle => "gentle",
            ShapingCurve::Psychoacoustic => "psychoacoustic",
        }
    }
}

/// Error-feedback noise shaper: pushes the quantization+dither noise floor out of
/// the ear's most sensitive band (~2-4 kHz) toward Nyquist, where it is far less
/// audible. The total noise *power* rises; only its perceptual weighting improves.
///
/// One instance per channel — the error history MUST NOT be shared across channels
/// or the noise-transfer function is corrupted.
pub struct NoiseShaper {
    dither: TpdfDither,
    coeffs: &'static [f64],
    /// Past total errors in LSB units, newest first: `hist[k] == e[n-1-k]`.
    hist: Vec<f64>,
}

impl NoiseShaper {
    pub fn new(curve: ShapingCurve, seed: Option<u64>) -> Self {
        let coeffs = curve.coeffs();
        Self {
            dither: TpdfDither::new(seed),
            coeffs,
            hist: vec![0.0; coeffs.len()],
        }
    }

    /// Quantize one normalized sample in [-1, 1] to an integer grid of `max` LSBs,
    /// applying TPDF dither and error-feedback noise shaping. Returns the
    /// re-quantized normalized value. All feedback math is done in LSB units.
    pub fn quantize(&mut self, sample: f64, max: f64, min_i: i32, max_i: i32) -> f64 {
        // Feedback: subtract the shaped sum of past quantization errors. With the
        // error fed back as e = y - u below, the output spectrum becomes
        // Y(z) = S(z) + (1 - H(z))*E(z), i.e. NTF = 1 - H(z).
        let fb: f64 = self.coeffs.iter().zip(&self.hist).map(|(c, h)| c * h).sum();

        let s = sample.clamp(-1.0, 1.0) * max;
        let u = s - fb; // shaped target
        let w = u + self.dither.sample_lsb(); // + the same +-1 LSB TPDF as the flat path
        let y = (w.round() as i32).clamp(min_i, max_i);

        // Feed back the TOTAL error (dither + rounding + clamp), not just the
        // rounding error — otherwise the white dither stays unshaped and dominates.
        let e = y as f64 - u;
        // Shift the history so the newest error sits at index 0 (e[n-1]).
        for i in (1..self.hist.len()).rev() {
            self.hist[i] = self.hist[i - 1];
        }
        if let Some(first) = self.hist.first_mut() {
            *first = e;
        }

        y as f64 / max
    }
}

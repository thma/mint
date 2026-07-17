//! Phase 4.2 — Per-Block Adaptive Noise Shaping
//!
//! Takes per-frame masking thresholds from Phase 4.1 (`PsychoacousticAnalysis`) and
//! designs minimal-phase FIR coefficients that respect those thresholds. Coefficients
//! are smoothly interpolated between frames using linear crossfade to eliminate zipper
//! artefacts.
//!
//! Key algorithms:
//! - **Minimal-phase design** via real cepstrum: target magnitude → log-FFT → truncate
//!   → iFFT → exponential lift → FIR.
//! - **Coefficient interpolation** (linear in coefficient space, strictly causal).
//! - **Frame-sample mapping** for smooth per-sample interpolation during quantization.
//!
//! References:
//! - Oppenheim & Schafer (1989), "Discrete-Time Signal Processing", Chapter 13 (cepstrum)
//! - Blauert & Laws (1978), "Group Delay Distortions in Electroacoustics", JAES

use std::sync::Arc;

use rustfft::{num_complex::Complex, Fft, FftPlanner};

use crate::dither::psychoacoustic::PsychoacousticAnalysis;
use crate::audio::buffer::AudioBuffer;
use crate::config::DitherCorrelation;
use crate::dither::multichannel::MultiChannelDither;

/// Maximum FIR order for adaptive filters. Balances expressiveness (ability to shape
/// across many bands) with computational cost and numerical stability. Typical range
/// is 5-15 taps for s16 quantization.
pub const MAX_FILTER_ORDER: usize = 9;

/// NTF magnitude swing cap (dB) for the **masking-adaptive** per-frame curve. Tight
/// enough that a low-order FIR tracks the spiky masking threshold without wasting
/// taps chasing its band edges (the ATH rise near Nyquist alone spans 40+ dB).
/// ±9 dB maximizes the noise-to-mask improvement on the verification harness.
const MASK_CLAMP_DB: f64 = 9.0;

/// NTF magnitude swing cap (dB) for the **fixed ATH-optimal floor**. The ATH curve
/// is smooth, so a larger swing is well-conditioned and reproduces the deep ~4 kHz
/// notch; ±16 dB matches/exceeds the published Lipshitz curve on the idle floor.
/// (Beyond ~18 dB the low-order cepstral reconstruction destabilizes.)
const FLOOR_CLAMP_DB: f64 = 16.0;

/// An adaptive noise-shaper built from psychoacoustic masking thresholds.
///
/// For each analysis frame, a minimal-phase FIR is designed such that its
/// noise-transfer function attenuates quantization noise at frequencies where
/// the ear is sensitive, and allows noise at frequencies where it is masked.
///
/// Coefficients are linearly interpolated between frame boundaries to produce
/// per-sample feedback values without audible zipper artifacts.
#[derive(Debug, Clone)]
pub struct AdaptiveShaper {
    /// Per-frame FIR coefficients. `coeffs_per_frame[f][k]` is the k-th tap of frame f.
    coeffs_per_frame: Vec<Vec<f64>>,

    /// Frame hop size (samples). Matches `psychoacoustic::HOP_SIZE`.
    hop_size: usize,

    /// Number of bins in the FFT (from the analysis phase).
    /// Reserved for Phase 4.3 (pre-masking look-ahead) and future diagnostics.
    #[allow(dead_code)]
    num_bins: usize,

    /// Sample rate for reference.
    /// Reserved for Phase 4.3 (pre-masking look-ahead) and future diagnostics.
    #[allow(dead_code)]
    sample_rate: u32,
}

impl AdaptiveShaper {
    /// Design an adaptive shaper from a complete psychoacoustic analysis.
    ///
    /// Each frame gets the perceptually better of two candidates, judged against
    /// that frame's own masking threshold:
    /// - a **masking-adaptive** curve (tracks the threshold; exploits masking when
    ///   the program is loud), and
    /// - a **fixed ATH-optimal floor** (best when the program does not mask).
    ///
    /// The per-frame selection guarantees the result is never more audible than the
    /// fixed floor on any frame — so in quiet/exposed passages the adaptive path is
    /// never worse than a fixed shaper, while loud passages still gain from masking.
    ///
    /// # Arguments
    /// - `analysis`: per-frame masking thresholds from Phase 4.1
    /// - `target_order`: desired FIR order (≤ `MAX_FILTER_ORDER`)
    pub fn from_analysis(analysis: &PsychoacousticAnalysis, target_order: usize) -> Self {
        let target_order = target_order.min(MAX_FILTER_ORDER);

        // Plan the FFTs once and reuse them across every per-frame design — the
        // analysis can have thousands of frames, so per-call planning would dominate.
        let n_fft = (analysis.num_bins - 1) * 2;
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n_fft);
        let ifft = planner.plan_fft_inverse(n_fft);

        // Fixed ATH-optimal floor, designed once. The safety net (see doc above).
        let ath = crate::dither::psychoacoustic::ath_amplitude(analysis.sample_rate, analysis.num_bins);
        let floor = design_minimal_phase_fir(&ath, target_order, FLOOR_CLAMP_DB, &fft, &ifft);

        // Precompute cos/sin of (i+1)·ω_k once: the per-frame selection evaluates
        // |NTF(ω_k)|² over hundreds of bins for thousands of frames, so the trig
        // must not live in that loop. `trig[k] = (cos[i], sin[i])` for i<order.
        let trig = ntf_eval_tables(analysis.num_bins, target_order);

        let mut inv_thr_sq = vec![0.0f64; analysis.num_bins];
        let coeffs_per_frame = analysis
            .thresholds
            .iter()
            .map(|thr| {
                for (w, &t) in inv_thr_sq.iter_mut().zip(thr.iter()) {
                    let t = t.max(1e-30);
                    *w = 1.0 / (t * t);
                }
                let adaptive = design_minimal_phase_fir(thr, target_order, MASK_CLAMP_DB, &fft, &ifft);
                // Keep whichever is less audible against THIS frame's threshold.
                if perceptual_noise(&adaptive, &inv_thr_sq, &trig)
                    <= perceptual_noise(&floor, &inv_thr_sq, &trig)
                {
                    adaptive
                } else {
                    floor.clone()
                }
            })
            .collect();

        AdaptiveShaper {
            coeffs_per_frame,
            hop_size: crate::dither::psychoacoustic::HOP_SIZE,
            num_bins: analysis.num_bins,
            sample_rate: analysis.sample_rate,
        }
    }

    /// Get the interpolated feedback coefficients for sample index `n`.
    ///
    /// Convenience wrapper that allocates; the per-sample quantizer loop should use
    /// [`coeffs_for_sample_into`](Self::coeffs_for_sample_into) with a reused buffer.
    pub fn coeffs_for_sample(&self, n: usize) -> Vec<f64> {
        let mut out = Vec::new();
        self.coeffs_for_sample_into(n, &mut out);
        out
    }

    /// Write the interpolated feedback coefficients for sample index `n` into `out`
    /// (cleared first). For a given sample, the coefficients are linearly
    /// interpolated between the two surrounding analysis frames — smooth transitions
    /// without zipper artifacts — without allocating on the hot path.
    pub fn coeffs_for_sample_into(&self, n: usize, out: &mut Vec<f64>) {
        out.clear();
        if self.coeffs_per_frame.is_empty() {
            out.resize(MAX_FILTER_ORDER, 0.0);
            return;
        }

        let frame_n = n / self.hop_size;
        let last = self.coeffs_per_frame.len() - 1;
        let coeffs_cur = &self.coeffs_per_frame[frame_n.min(last)];

        // At (or past) the last frame there is nothing to interpolate toward.
        if frame_n >= last {
            out.extend_from_slice(coeffs_cur);
            return;
        }

        // Linear interpolation between this frame and the next.
        let coeffs_next = &self.coeffs_per_frame[frame_n + 1];
        let alpha = (n % self.hop_size) as f64 / self.hop_size as f64;
        out.extend((0..coeffs_cur.len()).map(|k| {
            let c_cur = coeffs_cur[k];
            let c_next = coeffs_next.get(k).copied().unwrap_or(0.0);
            c_cur * (1.0 - alpha) + c_next * alpha
        }));
    }

    /// Number of adaptive frames (one per analysis frame).
    pub fn num_frames(&self) -> usize {
        self.coeffs_per_frame.len()
    }

    /// FIR order (number of taps).
    pub fn order(&self) -> usize {
        self.coeffs_per_frame.first().map(|c| c.len()).unwrap_or(0)
    }

    /// Get all per-frame coefficients (for testing and debugging).
    pub fn frame_coeffs(&self) -> &[Vec<f64>] {
        &self.coeffs_per_frame
    }
}

// ─── Minimal-phase filter design ──────────────────────────────────────────────

/// Design the error-feedback taps of a **monic, minimal-phase** noise-transfer
/// function whose magnitude tracks the masking threshold: `|NTF(f)| ∝ threshold(f)`.
///
/// This is the perceptually optimal shape — minimizing the masked noise power
/// `∫ |NTF|²/threshold² df` subject to the Gerzon–Craven constraint
/// `∫ ln|NTF| df = 0` yields exactly `|NTF| ∝ threshold` (Lagrange). The minimal
/// phase + monic (`ntf[0] = 1`) form is the realizable, stable error-feedback loop.
///
/// Algorithm (Oppenheim & Schafer, complex-cepstrum minimal-phase reconstruction):
/// 1. log-magnitude of the target, deviations clamped to ±`MAX_NTF_SHAPE_DB`
///    (overload safety — bounds the loop's peak gain).
/// 2. real cepstrum `c = IDFT(log|target|)`.
/// 3. causal lifter with the DC term **zeroed** ⇒ geometric mean 1 ⇒ `ntf[0] = 1`
///    (monic) and Gerzon–Craven satisfied automatically.
/// 4. `NTF = exp(DFT(ĉ))`, then `ntf = IDFT(NTF)`.
/// 5. feedback taps `coeffs[k] = −ntf[k+1]`, since the loop runs
///    `NTF(z) = 1 − Σ coeffs[k]·z^-(k+1)`.
///
/// # Arguments
/// - `target_spectrum`: desired `|NTF|` shape (one-sided, length `FRAME_SIZE/2+1`).
/// - `order`: number of feedback taps (≤ `MAX_FILTER_ORDER`).
/// - `max_shape_db`: cap on the NTF magnitude swing (see `MASK_CLAMP_DB` / `FLOOR_CLAMP_DB`).
/// - `fft`, `ifft`: pre-planned size-`n_fft` transforms (shared across frames).
fn design_minimal_phase_fir(
    target_spectrum: &[f64],
    order: usize,
    max_shape_db: f64,
    fft: &Arc<dyn Fft<f64>>,
    ifft: &Arc<dyn Fft<f64>>,
) -> Vec<f64> {
    let n_bins = target_spectrum.len();
    let n_fft = (n_bins - 1) * 2; // Reconstruct the FFT size.
    let order = order.min(MAX_FILTER_ORDER).max(1);

    // Step 1: log-magnitude, with deviations from the geometric mean clamped so the
    // resulting NTF cannot demand an unbounded peak gain. The exact monic
    // normalization is enforced in step 3 (DC of the cepstrum zeroed), so here we
    // only need the mean to center the clamp window.
    let max_ln = max_shape_db * std::f64::consts::LN_10 / 20.0;
    let mut logmag: Vec<f64> = target_spectrum.iter().map(|&m| m.max(1e-12).ln()).collect();
    // Two-sided mean of the log magnitude (DC & Nyquist count once, the rest twice).
    let mean = (logmag[0]
        + logmag[n_bins - 1]
        + 2.0 * logmag[1..n_bins - 1].iter().sum::<f64>())
        / n_fft as f64;
    for v in logmag.iter_mut() {
        *v = (*v - mean).clamp(-max_ln, max_ln);
    }

    // Step 2: build the even, two-sided log-magnitude spectrum and IDFT it to the
    // real cepstrum. (rustfft is unnormalized, so divide the inverse by n_fft.)
    let mut spec = vec![Complex::new(0.0, 0.0); n_fft];
    spec[0] = Complex::new(logmag[0], 0.0);
    spec[n_fft / 2] = Complex::new(logmag[n_bins - 1], 0.0);
    for k in 1..n_bins - 1 {
        spec[k] = Complex::new(logmag[k], 0.0);
        spec[n_fft - k] = Complex::new(logmag[k], 0.0); // mirror for an even spectrum
    }

    ifft.process(&mut spec);
    let cep: Vec<f64> = spec.iter().map(|c| c.re / n_fft as f64).collect();

    // Step 3: minimal-phase complex cepstrum — causal lifter, DC zeroed for a monic
    // NTF (ĉ[0]=0 ⇒ ntf[0]=exp(0)=1).
    let mut mp = vec![Complex::new(0.0, 0.0); n_fft];
    for n in 1..n_fft / 2 {
        mp[n] = Complex::new(2.0 * cep[n], 0.0);
    }
    mp[n_fft / 2] = Complex::new(cep[n_fft / 2], 0.0);

    // Step 4: log NTF = DFT(ĉ); NTF = exp(·); ntf impulse response = IDFT(NTF).
    fft.process(&mut mp);
    let mut ntf_spec: Vec<Complex<f64>> = mp.iter().map(|c| c.exp()).collect();
    ifft.process(&mut ntf_spec);
    let ntf: Vec<f64> = ntf_spec.iter().map(|c| c.re / n_fft as f64).collect();

    // Step 5: feedback taps coeffs[k] = −ntf[k+1] (ntf[0] ≈ 1, the monic term).
    (0..order).map(|k| -ntf[k + 1]).collect()
}

/// Per-bin `(cos((i+1)ω_k), sin((i+1)ω_k))` for i in 0..order, ω_k = π·k/(n_bins-1).
/// Built once per shaper so the per-frame NTF evaluation carries no trig.
fn ntf_eval_tables(n_bins: usize, order: usize) -> Vec<Vec<(f64, f64)>> {
    (0..n_bins)
        .map(|k| {
            let w = std::f64::consts::PI * k as f64 / (n_bins - 1) as f64;
            (0..order).map(|i| ((((i + 1) as f64) * w).cos(), (((i + 1) as f64) * w).sin())).collect()
        })
        .collect()
}

/// Perceptually-weighted noise of an NTF: `Σ_k |NTF(ω_k)|²·inv_thr_sq_k`
/// (a noise-to-mask proxy; lower = less audible), using the precomputed trig
/// tables. Drives the per-frame choice between the masking-adaptive curve and the
/// fixed ATH floor in `from_analysis`.
fn perceptual_noise(coeffs: &[f64], inv_thr_sq: &[f64], trig: &[Vec<(f64, f64)>]) -> f64 {
    inv_thr_sq
        .iter()
        .zip(trig)
        .map(|(&w, row)| {
            // |NTF(ω_k)|² for NTF = 1 − Σ coeffs[i]·z^-(i+1).
            let (mut re, mut im) = (1.0f64, 0.0f64);
            for (&c, &(cos, sin)) in coeffs.iter().zip(row) {
                re -= c * cos;
                im += c * sin;
            }
            (re * re + im * im) * w
        })
        .sum()
}

// ─── Phase 4.3: Adaptive quantization with time-varying coefficients ───────

/// Per-channel error history for adaptive quantization.
#[derive(Debug, Clone)]
struct AdaptiveChannelState {
    error_hist: Vec<f64>,
    blanking_count: usize,
}

impl AdaptiveChannelState {
    fn new(max_order: usize) -> Self {
        Self {
            error_hist: vec![0.0; max_order],
            blanking_count: 0,
        }
    }

    fn push_error(&mut self, e: f64) {
        self.error_hist.rotate_right(1);
        self.error_hist[0] = e;
    }

    /// Zero the feedback history when the output is forced to exact zero, so no
    /// stale shaped error re-enters on the next audible sample. Deliberately leaves
    /// `blanking_count` untouched — the blanking gate must stay latched through the
    /// silence (resetting it re-arms the 50 ms window and leaks dither).
    fn clear_feedback(&mut self) {
        self.error_hist.fill(0.0);
    }
}

/// Apply adaptive quantization using per-frame designed coefficients.
///
/// This function extends Phase 1 error-feedback quantization with time-varying FIR
/// coefficients from Phase 4.2's `AdaptiveShaper`. The shaper provides interpolated
/// coefficients for each sample, allowing frequency-adaptive noise shaping that
/// respects masking thresholds computed in a single analysis pass.
///
/// Simultaneous (frequency) masking only — there is no temporal pre/post-masking
/// model. The only forward-looking element is the linear crossfade toward the next
/// frame's coefficients, which suppresses zipper artifacts; it does not model
/// pre-masking.
///
/// # Arguments
/// - `buffer`: audio buffer to quantize (in-place)
/// - `max`: max amplitude scale (e.g., 32767.0 for s16)
/// - `min_i`: min quantized value (e.g., -32768 for s16)
/// - `max_i`: max quantized value (e.g., 32767 for s16)
/// - `shaper`: adaptive shaper with per-frame FIR coefficients
/// - `seed`: optional RNG seed for reproducibility
pub fn quantize_adaptive(
    buffer: &mut AudioBuffer,
    max: f64,
    min_i: i32,
    max_i: i32,
    shaper: &AdaptiveShaper,
    correlation: DitherCorrelation,
    seed: Option<u64>,
) {
    if buffer.channels.is_empty() || buffer.frame_len() == 0 {
        return;
    }

    let ch_count = buffer.channels_count();
    let max_coeffs_len = shaper.order();
    let mut states = vec![AdaptiveChannelState::new(max_coeffs_len); ch_count];
    let blanking_threshold = (buffer.sample_rate as usize) / 20; // 50 ms

    // Create multi-channel dither generator with selected correlation mode.
    let mut dither_gen = MultiChannelDither::new(ch_count, correlation, seed);
    let mut dither_samples = vec![0.0; ch_count];
    // Reused across all samples — avoids a per-sample heap allocation.
    let mut coeffs: Vec<f64> = Vec::with_capacity(max_coeffs_len);

    for n in 0..buffer.frame_len() {
        // Get adaptive coefficients for this sample (linearly interpolated between frames).
        shaper.coeffs_for_sample_into(n, &mut coeffs);

        // One draw per sample index for ALL channels, before any per-channel silence
        // gating. The draw must not live behind `ch == 0`: a silent ch0 would skip it
        // and starve every other channel of dither (they'd quantize against the
        // 0.0-initialized buffer, i.e. undithered truncation).
        dither_gen.sample_lsb_all_into(&mut dither_samples);

        for ch in 0..ch_count {
            let x = buffer.channels[ch][n];
            let state = &mut states[ch];

            // Auto-blanking: gate sustained sub-half-LSB passages to true zero.
            if x.abs() < 0.5 / 32_767.0 {
                state.blanking_count += 1;
                if state.blanking_count > blanking_threshold {
                    buffer.channels[ch][n] = 0.0;
                    state.clear_feedback();
                    continue;
                }
            } else {
                state.blanking_count = 0;
            }

            // Exact zero: no dither, no feedback. Keeps the blanking count accrued
            // above, so true zeros interspersed in a sub-LSB noise floor still gate.
            if x == 0.0 {
                buffer.channels[ch][n] = 0.0;
                state.clear_feedback();
                continue;
            }

            // Feedback term: apply time-varying FIR to error history.
            let fb = coeffs
                .iter()
                .zip(state.error_hist.iter())
                .map(|(c, e)| c * e)
                .sum::<f64>();

            // Get dither sample for this channel.
            let dither = dither_samples[ch];

            // Quantize: shaped input + dither → round → clamp.
            let s = x.clamp(-1.0, 1.0) * max;
            let u = s - fb;
            let w = u + dither;
            let y = (w.round() as i32).clamp(min_i, max_i);

            // Update error history.
            state.push_error(y as f64 - u);

            buffer.channels[ch][n] = y as f64 / max;
        }
    }
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_spectrum(n: usize) -> Vec<f64> {
        // A flat (white-noise-like) target: constant magnitude ≈ 0.5 everywhere.
        vec![0.5; n]
    }

    fn lowpass_spectrum(n: usize) -> Vec<f64> {
        // Lowpass target: 1.0 at DC, decaying toward Nyquist.
        (0..n)
            .map(|k| {
                let norm = (k as f64) / (n as f64);
                1.0 - 0.9 * norm.powi(2)
            })
            .collect()
    }

    /// Test convenience: build the FFT plans and call the real designer.
    fn design(target: &[f64], order: usize, clamp: f64) -> Vec<f64> {
        let n_fft = (target.len() - 1) * 2;
        let mut p = FftPlanner::<f64>::new();
        let fft = p.plan_fft_forward(n_fft);
        let ifft = p.plan_fft_inverse(n_fft);
        design_minimal_phase_fir(target, order, clamp, &fft, &ifft)
    }

    #[test]
    fn minimal_phase_fir_has_correct_length() {
        let spectrum = flat_spectrum(513);
        let fir = design(&spectrum, 7, MASK_CLAMP_DB);
        assert_eq!(fir.len(), 7, "FIR should have requested order");
    }

    #[test]
    fn minimal_phase_fir_respects_max_order() {
        let spectrum = flat_spectrum(513);
        for target_order in [1, 5, 15, 100] {
            let fir = design(&spectrum, target_order, MASK_CLAMP_DB);
            assert!(
                fir.len() <= target_order && fir.len() > 0,
                "FIR length {} should be at most {}",
                fir.len(),
                target_order
            );
        }
    }

    #[test]
    fn adaptive_shaper_interpolates_smoothly() {
        // Construct a dummy analysis with two frames.
        let thresholds = vec![
            flat_spectrum(513),   // Frame 0
            lowpass_spectrum(513), // Frame 1
        ];
        let analysis = PsychoacousticAnalysis {
            thresholds,
            num_bins: 513,
            sample_rate: 44_100,
        };

        let shaper = AdaptiveShaper::from_analysis(&analysis, 5);

        // Sample 0 (start of frame 0) should match frame 0's coefficients.
        let c0 = shaper.coeffs_for_sample(0);
        let cf0 = &shaper.frame_coeffs()[0];
        for (i, &cv) in c0.iter().enumerate() {
            assert!(
                (cv - cf0[i]).abs() < 1e-6,
                "sample 0 should match frame 0 coefficients"
            );
        }

        // Mid-frame sample should be a blend.
        let mid_sample = crate::dither::psychoacoustic::HOP_SIZE / 2;
        let c_mid = shaper.coeffs_for_sample(mid_sample);
        let c_start = shaper.coeffs_for_sample(0);
        let c_end = shaper.coeffs_for_sample(crate::dither::psychoacoustic::HOP_SIZE);
        for (i, &cv) in c_mid.iter().enumerate() {
            // Should be roughly between start and end.
            let min = c_start[i].min(c_end[i]);
            let max = c_start[i].max(c_end[i]);
            assert!(
                cv >= min - 1e-6 && cv <= max + 1e-6,
                "mid-frame coefficient should be interpolated"
            );
        }
    }

    #[test]
    fn adaptive_shaper_from_analysis_produces_multiple_frames() {
        let thresholds = vec![flat_spectrum(513); 3];
        let analysis = PsychoacousticAnalysis {
            thresholds,
            num_bins: 513,
            sample_rate: 44_100,
        };

        let shaper = AdaptiveShaper::from_analysis(&analysis, 7);
        assert_eq!(
            shaper.num_frames(),
            3,
            "shaper should have one entry per analysis frame"
        );
        assert_eq!(shaper.order(), 7, "FIR order should match the request");
    }

    #[test]
    fn adaptive_shaper_clamps_out_of_bounds() {
        let thresholds = vec![flat_spectrum(513); 2];
        let analysis = PsychoacousticAnalysis {
            thresholds,
            num_bins: 513,
            sample_rate: 44_100,
        };

        let shaper = AdaptiveShaper::from_analysis(&analysis, 5);

        // Requesting samples far past the end should not panic.
        let c_far = shaper.coeffs_for_sample(usize::MAX);
        assert_eq!(
            c_far.len(),
            5,
            "out-of-bounds request should return last frame's coeffs"
        );
    }

    /// The real contract: the designed NTF magnitude must track the target shape.
    /// For a lowpass target (more weight at DC than Nyquist), the reconstructed
    /// `|NTF|` must likewise be larger at DC than at Nyquist.
    #[test]
    fn minimal_phase_fir_reconstructs_target_shape() {
        let spectrum = lowpass_spectrum(513);
        let coeffs = design(&spectrum, 9, MASK_CLAMP_DB);

        // |NTF(z)| at z = ±1 (DC / Nyquist): NTF = 1 − Σ coeffs[k]·z^-(k+1).
        let ntf_mag = |z_sign: f64| {
            (1.0 - coeffs
                .iter()
                .enumerate()
                .map(|(k, c)| c * z_sign.powi(k as i32 + 1))
                .sum::<f64>())
            .abs()
        };
        let dc = ntf_mag(1.0);
        let nyq = ntf_mag(-1.0);
        assert!(
            dc > nyq,
            "lowpass target ⇒ NTF should emphasize DC over Nyquist (dc={dc:.3}, nyq={nyq:.3})"
        );

        // Monic: the leading NTF tap is 1, so the feedback design must be nonzero.
        let energy: f64 = coeffs.iter().map(|x| x.abs()).sum();
        assert!(energy > 0.0, "non-flat target must yield nonzero feedback taps");
    }

    /// A flat target needs no shaping: `|NTF| = 1`, i.e. all feedback taps ≈ 0.
    #[test]
    fn minimal_phase_fir_flat_target_is_passthrough() {
        let coeffs = design(&flat_spectrum(513), 9, MASK_CLAMP_DB);
        let max = coeffs.iter().map(|c| c.abs()).fold(0.0, f64::max);
        assert!(max < 1e-6, "flat target should produce ~zero feedback (got {max:.2e})");
    }
}

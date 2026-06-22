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

use rustfft::{num_complex::Complex, FftPlanner};

use crate::dither::psychoacoustic::PsychoacousticAnalysis;

/// Maximum FIR order for adaptive filters. Balances expressiveness (ability to shape
/// across many bands) with computational cost and numerical stability. Typical range
/// is 5-15 taps for s16 quantization.
pub const MAX_FILTER_ORDER: usize = 9;

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
    /// # Arguments
    /// - `analysis`: per-frame masking thresholds from Phase 4.1
    /// - `target_order`: desired FIR order (≤ `MAX_FILTER_ORDER`)
    ///
    /// Returns a shaper ready to interpolate coefficients during quantization.
    pub fn from_analysis(analysis: &PsychoacousticAnalysis, target_order: usize) -> Self {
        let target_order = target_order.min(MAX_FILTER_ORDER);
        let mut coeffs_per_frame = Vec::new();

        for threshold_spectrum in &analysis.thresholds {
            let coeffs = design_minimal_phase_fir(threshold_spectrum, target_order);
            coeffs_per_frame.push(coeffs);
        }

        AdaptiveShaper {
            coeffs_per_frame,
            hop_size: crate::dither::psychoacoustic::HOP_SIZE,
            num_bins: analysis.num_bins,
            sample_rate: analysis.sample_rate,
        }
    }

    /// Get the interpolated feedback coefficients for sample index `n`.
    ///
    /// Returns a vector of length = designed FIR order. For a given sample, the
    /// coefficients are linearly interpolated between the two surrounding analysis
    /// frames, ensuring smooth transitions without audible discontinuities.
    pub fn coeffs_for_sample(&self, n: usize) -> Vec<f64> {
        if self.coeffs_per_frame.is_empty() {
            return vec![0.0; MAX_FILTER_ORDER];
        }

        let frame_n = n / self.hop_size;
        let sample_in_frame = n % self.hop_size;
        let alpha = sample_in_frame as f64 / self.hop_size as f64;

        // Current frame coefficients.
        let coeffs_cur = &self.coeffs_per_frame[frame_n.min(self.coeffs_per_frame.len() - 1)];
        let len = coeffs_cur.len();

        // If at the last frame, no interpolation needed.
        if frame_n + 1 >= self.coeffs_per_frame.len() {
            return coeffs_cur.clone();
        }

        // Linear interpolation between this frame and the next.
        let coeffs_next = &self.coeffs_per_frame[frame_n + 1];
        (0..len)
            .map(|k| {
                let c_cur = coeffs_cur.get(k).copied().unwrap_or(0.0);
                let c_next = coeffs_next.get(k).copied().unwrap_or(0.0);
                c_cur * (1.0 - alpha) + c_next * alpha
            })
            .collect()
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

/// Design a minimal-phase FIR of a given order from a target amplitude spectrum.
///
/// The algorithm:
/// 1. Compute the target squared magnitude: S = |target|²
/// 2. Real cepstral analysis: log-FFT of S, truncate to get the real cepstrum
/// 3. Exponential lift + spectral factorization → minimal-phase response
/// 4. Extract FIR coefficients (impulse response)
/// 5. Normalize to prevent instability
///
/// # Arguments
/// - `target_spectrum`: target amplitude response (linear scale, one-sided FFT)
/// - `order`: desired FIR length
///
/// Returns FIR coefficients ordered as `[h_0, h_1, ..., h_{order-1}]`, normalized
/// to maximum magnitude ≤ 1.0 for numerical stability.
fn design_minimal_phase_fir(target_spectrum: &[f64], order: usize) -> Vec<f64> {
    let n_fft = (target_spectrum.len() - 1) * 2; // Reconstruct the FFT size.
    let order = order.min(n_fft / 2).max(1);

    // Step 1: Squared magnitude spectrum with regularization to prevent log blow-up.
    // Clamp to [1e-6, ∞) to avoid log of very small numbers.
    let s_mag: Vec<f64> = target_spectrum
        .iter()
        .map(|&m| (m * m).max(1e-6).min(1e6))
        .collect();

    // Step 2: Real cepstral analysis via log-FFT.
    let mut log_s = vec![Complex::new(0.0, 0.0); n_fft];
    for k in 0..s_mag.len() {
        log_s[k] = Complex::new(s_mag[k].ln(), 0.0);
    }

    // Inverse FFT to get real cepstrum.
    let mut planner = FftPlanner::<f64>::new();
    let ifft = planner.plan_fft_inverse(n_fft);
    ifft.process(&mut log_s);

    // Step 3: Truncate to minimal-phase (keep only causal part: bins 0 … order/2).
    // Double the DC and fold the Nyquist (if present) for energy preservation.
    let mut min_phase_cepstrum = vec![Complex::new(0.0, 0.0); n_fft];
    min_phase_cepstrum[0] = log_s[0] * Complex::new(2.0, 0.0); // DC doubled.
    for n in 1..=(order / 2).min(n_fft / 2 - 1) {
        min_phase_cepstrum[n] = log_s[n] * Complex::new(2.0, 0.0);
    }

    // FFT back to frequency domain.
    let fft = planner.plan_fft_forward(n_fft);
    let mut log_h_min = min_phase_cepstrum;
    fft.process(&mut log_h_min);

    // Exponentiate to get the minimum-phase magnitude response.
    let h_min: Vec<f64> = log_h_min
        .iter()
        .map(|&c| (c.re / n_fft as f64).exp().max(1e-10))
        .collect();

    // Step 4: Inverse FFT to get the impulse response (FIR coefficients).
    let mut h_coeffs = h_min
        .iter()
        .map(|&m| Complex::new(m, 0.0))
        .collect::<Vec<_>>();

    let ifft2 = planner.plan_fft_inverse(n_fft);
    ifft2.process(&mut h_coeffs);

    // Extract the first `order` real parts (causal taps).
    let mut coeffs: Vec<f64> = h_coeffs
        .iter()
        .take(order)
        .map(|c| c.re / n_fft as f64)
        .collect();

    // Step 5: Normalize to prevent runaway amplification. Scale so that the maximum
    // coefficient magnitude is at most 1.0, or slightly less to preserve dynamics.
    let max_coeff = coeffs.iter().map(|c| c.abs()).fold(0.0, f64::max);
    if max_coeff > 1.0 {
        let scale = 0.9 / (max_coeff + 1e-10);
        coeffs.iter_mut().for_each(|c| *c *= scale);
    }

    coeffs
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

    #[test]
    fn minimal_phase_fir_has_correct_length() {
        let spectrum = flat_spectrum(513);
        let fir = design_minimal_phase_fir(&spectrum, 7);
        assert_eq!(fir.len(), 7, "FIR should have requested order");
    }

    #[test]
    fn minimal_phase_fir_respects_max_order() {
        let spectrum = flat_spectrum(513);
        for target_order in [1, 5, 15, 100] {
            let fir = design_minimal_phase_fir(&spectrum, target_order);
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

    #[test]
    fn minimal_phase_fir_preserves_positivity() {
        // A valid spectrum should produce positive (or very small) FIR values on average.
        let spectrum = lowpass_spectrum(513);
        let fir = design_minimal_phase_fir(&spectrum, 9);

        // First coefficient should be the largest (typical for a lowpass shaper).
        assert!(
            fir[0] > 0.0,
            "minimal-phase FIR should have positive leading tap"
        );
        // Total energy (L1 norm) should be positive.
        let energy: f64 = fir.iter().map(|&x| x.abs()).sum();
        assert!(energy > 0.0, "FIR should have nonzero energy");
    }
}

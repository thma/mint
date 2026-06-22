//! Phase 4.1 — Psychoacoustic Analysis Pass
//!
//! Full-buffer, zero-latency analysis: Hann-windowed STFT → Bark/ERB band
//! energies → Schroeder spreading function → SFM tonality index → global
//! masking threshold per analysis frame.
//!
//! The output (`PsychoacousticAnalysis`) is consumed by Phase 4.2 to design
//! per-block, minimal-phase FIR coefficients that are interpolated across
//! frames without zipper artefacts.
//!
//! Implementation follows **MPEG-1 Psychoacoustic Model 1** (ISO/IEC 11172-3
//! Annex D) with a few practical simplifications appropriate for an offline
//! batch processor:
//! - Frame size: 1024 samples (≈23 ms @ 44.1 kHz, ≈21 ms @ 48 kHz).
//! - Overlap: 50 % (512-sample hop) — sufficient for smooth time variation.
//! - 24 Bark critical bands (DC … ~15.5 kHz).
//! - Spreading function: Schroeder/Zwicker parametric model in Bark domain.
//! - Tonality: Spectral Flatness Measure per band, clamped to [0, 1].
//! - ATH: Terhardt formula normalized to s16 full-scale (0 dBFS ≈ 96 dBSPL).
//!
//! References:
//! - ISO/IEC 11172-3:1993, Annex D ("Psychoacoustic Model 1")
//! - Terhardt (1979), "Calculating virtual pitch", Hearing Research
//! - Schroeder, Atal, Hall (1979), "Optimizing digital speech coders"

use rustfft::{num_complex::Complex, FftPlanner};
use std::f64::consts::PI;

/// STFT frame length (samples). Power of 2 for FFT efficiency.
pub const FRAME_SIZE: usize = 1024;

/// Hop between consecutive frames — 50 % overlap balances time resolution and
/// computation. Smaller hops give smoother threshold variation but cost more.
pub const HOP_SIZE: usize = 512;

/// Number of Bark critical bands in the model.
pub const NUM_BARK_BANDS: usize = 24;

/// Upper frequency limit (Hz) of each Bark critical band (MPEG-1 model).
/// Band z spans from BARK_UPPER[z-1] (or 0.0) to BARK_UPPER[z].
static BARK_UPPER: [f64; NUM_BARK_BANDS] = [
    100.0, 200.0, 300.0, 400.0, 510.0, 630.0, 770.0, 920.0,
    1_080.0, 1_270.0, 1_480.0, 1_720.0, 2_000.0, 2_320.0, 2_700.0,
    3_150.0, 3_700.0, 4_400.0, 5_300.0, 6_400.0, 7_700.0, 9_500.0,
    12_000.0, 15_500.0,
];

/// Result of analyzing a complete buffer with the psychoacoustic model.
///
/// Contains one masking threshold spectrum per analysis frame. The spectrum
/// has `FRAME_SIZE / 2 + 1` bins (DC … Nyquist); each value is a linear
/// amplitude threshold (same normalized scale as the signal, –1 … 1).
///
/// Phase 4.2 uses these curves to design per-frame minimal-phase FIR
/// coefficients: bins where the threshold is high can absorb more quantization
/// noise; bins where it is low (ATH exposed or quiet passages) must stay clean.
#[derive(Debug, Clone)]
pub struct PsychoacousticAnalysis {
    /// `thresholds[frame][bin]` — masking threshold amplitude (linear, ≥ ATH).
    pub thresholds: Vec<Vec<f64>>,
    /// Number of FFT bins = `FRAME_SIZE / 2 + 1`.
    pub num_bins: usize,
    pub sample_rate: u32,
}

impl PsychoacousticAnalysis {
    /// Analyze a mono signal (normalized –1 … 1) at the given sample rate.
    ///
    /// The whole file is in RAM, so there is no look-ahead latency: the analysis
    /// is purely causal in the sense that each frame uses only its own samples,
    /// but Phase 4.2 is free to use future frames as pre-masking look-ahead.
    pub fn analyze(signal: &[f64], sample_rate: u32) -> Self {
        let num_bins = FRAME_SIZE / 2 + 1;
        let window = hann_window(FRAME_SIZE);
        let ath = build_ath(sample_rate, num_bins);

        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(FRAME_SIZE);

        let mut thresholds = Vec::new();

        // Iterate over 50 %-overlapping frames. The last partial frame is
        // zero-padded, ensuring the whole signal is covered.
        let mut pos = 0usize;
        loop {
            // Collect one windowed frame, zero-padding past the signal end.
            let frame: Vec<Complex<f64>> = (0..FRAME_SIZE)
                .map(|i| {
                    let s = if pos + i < signal.len() {
                        signal[pos + i]
                    } else {
                        0.0
                    };
                    Complex::new(s * window[i], 0.0)
                })
                .collect();

            let mut spectrum = frame;
            fft.process(&mut spectrum);

            // One-sided power spectrum with the correct energy normalization.
            // The factor-of-2 for non-DC/Nyquist bins accounts for the folded
            // energy in the conjugate half of the two-sided spectrum.
            let power: Vec<f64> = (0..num_bins)
                .map(|k| {
                    let fold = if k == 0 || k == FRAME_SIZE / 2 {
                        1.0
                    } else {
                        2.0
                    };
                    let c = spectrum[k];
                    (c.re * c.re + c.im * c.im) * fold / FRAME_SIZE as f64
                })
                .collect();

            // Aggregate per Bark band (mean power), compute tonality, spread.
            let bark_power = power_to_bark_bands(&power, sample_rate, num_bins);
            let tonality = bark_tonality(&bark_power);
            let spread = spreading_function(&bark_power);

            // Per-band masking threshold: spread masker minus perceptual offset.
            // NMT ≈ 5.5 dB (tonal masker), TMN ≈ 6.5 dB (noise masker).
            // The mix is driven by the SFM-derived tonality index.
            let bark_threshold: Vec<f64> = (0..NUM_BARK_BANDS)
                .map(|z| {
                    let offset_db = tonality[z] * 5.5 + (1.0 - tonality[z]) * 6.5;
                    spread[z] * db_to_linear(-offset_db)
                })
                .collect();

            // Interpolate Bark thresholds to FFT bins; take max with ATH so we
            // never claim the ear is masked when it actually isn't.
            let bin_threshold = bark_to_bins(&bark_threshold, sample_rate, num_bins, &ath);
            thresholds.push(bin_threshold);

            // Advance by one hop. Stop once the frame start is past the signal.
            if pos + HOP_SIZE >= signal.len() {
                break;
            }
            pos += HOP_SIZE;
        }

        // Guard: at least one frame (handles empty / very short inputs).
        if thresholds.is_empty() {
            thresholds.push(ath);
        }

        PsychoacousticAnalysis {
            thresholds,
            num_bins,
            sample_rate,
        }
    }

    /// Masking threshold spectrum for the frame that contains sample `n`.
    ///
    /// Clamps to the last available frame for indices past the signal end,
    /// so Phase 4.2 can safely index beyond the last hop boundary.
    pub fn threshold_for_sample(&self, n: usize) -> &[f64] {
        let frame = (n / HOP_SIZE).min(self.thresholds.len() - 1);
        &self.thresholds[frame]
    }

    /// Number of analysis frames produced by `analyze`.
    pub fn num_frames(&self) -> usize {
        self.thresholds.len()
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Raised-cosine (Hann) window of length `n`.
///
/// Using the periodic form `w[i] = 0.5 * (1 - cos(2π·i/n))` — correct for STFT
/// with 50 % overlap and the OLA (overlap-add) reconstruction requirement.
fn hann_window(n: usize) -> Vec<f64> {
    (0..n)
        .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / n as f64).cos()))
        .collect()
}

/// Absolute Threshold of Hearing (ATH) per FFT bin, as a linear amplitude
/// relative to normalized full scale (0 dBFS = 96 dBSPL for s16).
///
/// Uses Terhardt's four-parameter formula (1979). Values below 20 Hz are
/// clamped to 20 Hz to avoid the singularity at DC.
fn build_ath(sample_rate: u32, num_bins: usize) -> Vec<f64> {
    let nyquist = sample_rate as f64 / 2.0;
    (0..num_bins)
        .map(|k| {
            // Frequency in kHz, clamped away from DC singularity.
            let f_hz = (k as f64 * nyquist / (num_bins - 1) as f64).max(20.0);
            let f = f_hz / 1_000.0;

            // Terhardt (1979) threshold in dB SPL.
            let ath_spl = 3.64 * f.powf(-0.8)
                - 6.5 * (-(0.6 * (f - 3.3)).powi(2)).exp()
                + 1.0e-3 * f.powi(4);

            // Normalize to 0 dBFS = 96 dBSPL (standard for 16-bit PCM at typical
            // calibration). ATH_normalized_dBFS = ATH_SPL − 96.
            db_to_linear(ath_spl - 96.0)
        })
        .collect()
}

#[inline]
fn db_to_linear(db: f64) -> f64 {
    10.0_f64.powf(db / 20.0)
}

/// Map FFT bin `k` to its Bark band index (0 … NUM_BARK_BANDS-1).
fn bin_to_bark_band(k: usize, sample_rate: u32, num_bins: usize) -> usize {
    let nyquist = sample_rate as f64 / 2.0;
    let f = k as f64 * nyquist / (num_bins - 1) as f64;
    // Linear scan is fast enough for 513 bins; replace with bisect if needed.
    for (z, &upper) in BARK_UPPER.iter().enumerate() {
        if f <= upper {
            return z;
        }
    }
    NUM_BARK_BANDS - 1
}

/// Aggregate FFT power into Bark bands (mean power per band).
fn power_to_bark_bands(power: &[f64], sample_rate: u32, num_bins: usize) -> Vec<f64> {
    let mut band_sum = vec![0.0f64; NUM_BARK_BANDS];
    let mut band_n = vec![0usize; NUM_BARK_BANDS];

    for (k, &p) in power.iter().enumerate() {
        let z = bin_to_bark_band(k, sample_rate, num_bins);
        band_sum[z] += p;
        band_n[z] += 1;
    }

    // Mean power — avoids bands with many bins dominating.
    (0..NUM_BARK_BANDS)
        .map(|z| if band_n[z] > 0 { band_sum[z] / band_n[z] as f64 } else { 0.0 })
        .collect()
}

/// Spectral Flatness Measure (SFM) per Bark band → tonality index in [0, 1].
///
/// SFM = geometric_mean / arithmetic_mean of the power in a 3-band window.
/// - SFM → 1 (flat spectrum) ⟹ noise-like ⟹ tonality → 0.
/// - SFM → 0 (peaky spectrum) ⟹ tonal ⟹ tonality → 1.
///
/// Normalization: SFM of –60 dB → tonality 1.0 (MPEG-1 Model 1 convention).
fn bark_tonality(bark_power: &[f64]) -> Vec<f64> {
    let n = bark_power.len();
    let half_win = 1usize; // 3-band window (z-1 … z+1)

    (0..n)
        .map(|z| {
            let lo = z.saturating_sub(half_win);
            let hi = (z + half_win + 1).min(n);
            let slice = &bark_power[lo..hi];

            let arith = slice.iter().sum::<f64>() / slice.len() as f64;
            if arith < 1.0e-30 {
                return 0.0; // silence → treat as noise (no masking advantage)
            }

            let log_sum: f64 = slice.iter().map(|&p| p.max(1.0e-30).ln()).sum();
            let geom = (log_sum / slice.len() as f64).exp();

            // SFM in dB. Pure tone → −∞ dB; white noise → 0 dB.
            let sfm_db = 10.0 * (geom / arith).max(1.0e-30).log10();

            // Clamp: [−60 dB, 0 dB] → [1.0, 0.0] tonality.
            (sfm_db / -60.0).clamp(0.0, 1.0)
        })
        .collect()
}

/// Bark-domain spreading function (Schroeder parametric model).
///
/// For each masker at Bark band `z_m` with mean power `P_m`, compute its
/// contribution to every other band `z` via the spreading function:
///
/// ```text
/// SF(Δz) [dB] = 15.81 + 7.5·(Δz + 0.474) − 17.5·√(1 + (Δz + 0.474)²)
/// ```
///
/// The linear sum over all maskers gives the total excitation level per band.
/// This is the core of the Schroeder/Zwicker psychoacoustic model.
fn spreading_function(bark_power: &[f64]) -> Vec<f64> {
    let n = bark_power.len();
    let mut spread = vec![0.0f64; n];

    for (z_m, &p_m) in bark_power.iter().enumerate() {
        if p_m < 1.0e-30 {
            continue; // silent maskers contribute nothing
        }
        for z in 0..n {
            let dz = z as f64 - z_m as f64;
            let dz_s = dz + 0.474;
            // Spreading function in dB, converted to linear multiplier.
            let sf_db = 15.81 + 7.5 * dz_s - 17.5 * (1.0 + dz_s * dz_s).sqrt();
            spread[z] += p_m * db_to_linear(sf_db);
        }
    }

    spread
}

/// Interpolate Bark-band masking thresholds to FFT bins and take max with ATH.
///
/// Each bin inherits the threshold of its Bark band; the ATH floor ensures
/// we never tell Phase 4.2 that a frequency bin can be masked when the ear
/// is actually sensitive there.
fn bark_to_bins(
    bark_threshold: &[f64],
    sample_rate: u32,
    num_bins: usize,
    ath: &[f64],
) -> Vec<f64> {
    (0..num_bins)
        .map(|k| {
            let z = bin_to_bark_band(k, sample_rate, num_bins);
            // ATH floor: guarantees the threshold is never below hearing acuity.
            bark_threshold[z].max(ath[k])
        })
        .collect()
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn sine(freq_hz: f64, n: usize, sample_rate: u32) -> Vec<f64> {
        (0..n)
            .map(|i| (2.0 * PI * freq_hz * i as f64 / sample_rate as f64).sin())
            .collect()
    }

    #[test]
    fn hann_window_is_normalised_and_symmetric() {
        let w = hann_window(1024);
        assert_eq!(w.len(), 1024);
        assert!((w[0]).abs() < 1e-9, "hann[0] should be 0");
        // Symmetric: w[k] = w[n-k] for k > 0
        for k in 1..512 {
            assert!(
                (w[k] - w[1024 - k]).abs() < 1e-12,
                "hann window must be symmetric"
            );
        }
        // Peak at centre
        assert!((w[512] - 1.0).abs() < 1e-9, "hann peak should be 1.0 at centre");
    }

    #[test]
    fn ath_has_expected_shape() {
        let ath = build_ath(44_100, 513);
        // ATH should be lowest around 3-4 kHz (ear most sensitive there).
        let bin_1k = (1_000.0_f64 * 512.0 / 22_050.0).round() as usize;
        let bin_4k = (4_000.0_f64 * 512.0 / 22_050.0).round() as usize;
        let bin_10k = (10_000.0_f64 * 512.0 / 22_050.0).round() as usize;
        // 1 kHz should be less sensitive than 4 kHz (ATH lower at 4k).
        assert!(
            ath[bin_4k] < ath[bin_1k],
            "ATH minimum should be near 4 kHz"
        );
        // 10 kHz should be less sensitive than 4 kHz.
        assert!(
            ath[bin_4k] < ath[bin_10k],
            "ATH should rise above 4 kHz"
        );
        // All values positive.
        assert!(ath.iter().all(|&v| v > 0.0), "ATH must be strictly positive");
    }

    #[test]
    fn analyze_sine_produces_expected_frame_count() {
        let sr = 44_100u32;
        let n = sr as usize; // 1 second
        let sig = sine(997.0, n, sr);
        let analysis = PsychoacousticAnalysis::analyze(&sig, sr);

        // Expected: ceil((n - FRAME_SIZE) / HOP_SIZE) + 1 frames ≈ 1723.
        let expected_frames = (n - FRAME_SIZE) / HOP_SIZE + 1;
        assert!(
            (analysis.num_frames() as isize - expected_frames as isize).abs() <= 2,
            "frame count off: got {}, expected ~{expected_frames}",
            analysis.num_frames()
        );
        assert_eq!(analysis.num_bins, FRAME_SIZE / 2 + 1);
    }

    #[test]
    fn threshold_for_sample_clamps_to_last_frame() {
        let sig = vec![0.1f64; 2048];
        let a = PsychoacousticAnalysis::analyze(&sig, 44_100);
        // Sample index past the end should return the last frame without panicking.
        let last = a.threshold_for_sample(usize::MAX);
        assert_eq!(last.len(), a.num_bins);
    }

    #[test]
    fn sine_raises_masking_threshold_in_its_bark_band() {
        let sr = 44_100u32;
        // Loud 1 kHz sine — should raise the masking threshold in the 920-1080 Hz band.
        let loud_sine = sine(1_000.0, sr as usize, sr);
        let silence = vec![0.0f64; sr as usize];

        let a_loud = PsychoacousticAnalysis::analyze(&loud_sine, sr);
        let a_silent = PsychoacousticAnalysis::analyze(&silence, sr);

        // Compare in the middle of the signal.
        let mid = sr as usize / 2;
        let t_loud = a_loud.threshold_for_sample(mid);
        let t_silent = a_silent.threshold_for_sample(mid);

        // Bin near 1 kHz.
        let bin_1k = (1_000.0 * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize;

        assert!(
            t_loud[bin_1k] > t_silent[bin_1k],
            "loud 1 kHz sine should raise the masking threshold at 1 kHz"
        );
    }

    #[test]
    fn spreading_raises_threshold_in_neighboring_bark_bands() {
        let sr = 44_100u32;
        // 1 kHz tone — masking should spill into adjacent Bark bands.
        let sig = sine(1_000.0, sr as usize, sr);
        let analysis = PsychoacousticAnalysis::analyze(&sig, sr);
        let mid = sr as usize / 2;
        let t = analysis.threshold_for_sample(mid);

        // Bins just above and below 1 kHz should also see an elevated threshold.
        let bin_900 = (900.0 * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize;
        let bin_1200 = (1_200.0 * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize;
        let bin_8k = (8_000.0 * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize;

        let ath = build_ath(sr, analysis.num_bins);

        // Neighbours should be well above ATH due to spreading.
        assert!(
            t[bin_900] > ath[bin_900],
            "spreading should elevate threshold near 900 Hz above ATH"
        );
        assert!(
            t[bin_1200] > ath[bin_1200],
            "spreading should elevate threshold near 1.2 kHz above ATH"
        );
        // Far away (8 kHz) should be close to ATH — little masking.
        assert!(
            t[bin_8k] < t[bin_900] * 10.0,
            "masking should be much weaker far from the masker"
        );
    }
}

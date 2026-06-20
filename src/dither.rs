use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

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

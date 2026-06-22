//! MBIT+ Phase 1: Statische Error-Feedback-Quantisierung mit Hochpass-Shaping.
//!
//! HINWEIS: Dies ist NICHT mehr der Produktionspfad. `DitherMode::MbitPlus` läuft
//! seit dem Verdrahten des adaptiven Pfads über `psychoacoustic.rs` +
//! `adaptive_mbit.rs` (siehe `ops::bitdepth`). Dieses Modul bleibt als Referenz-
//! /Vergleichsimplementierung und für die NTF-Tabellen-Tests erhalten.
//!
//! Dieses Modul implementiert ein stabiles Noise-Shaping für 16-Bit über feste,
//! eingecheckte Error-Feedback-Koeffizienten (nicht zur Laufzeit adaptiert).
//!
//! Ehrlichkeits-Hinweis: Die Koeffizienten sind *breitbandige Hochpass*-NTFs
//! (tiefster Punkt bei DC, monoton steigend zum Nyquist) — KEIN auf das
//! 2–5-kHz-Sensitivitätstal getuntes, gehörrichtiges Design. Sie sind perzeptiv
//! wirksam (gemessen, siehe `tests/dither_verification.rs`), aber das eigentlich
//! gehörgewichtete, adaptive Design lebt in `psychoacoustic.rs` + `adaptive_mbit.rs`.
//!
//! **Phase 1 Fokus**: Korrektheit und Stabilität.
//! - Konstantes TPDF-Dither (2 LSB pp, ±1 LSB).
//! - Feste Error-Feedback-Koeffizienten (stabil, minimalphasig).
//! - Auto-Blanking bei Stille (< 0.5 LSB > ~50ms).
//! - Per-Channel Noise-Unabhängigkeit, deterministische Stereo-Korrelation.

use crate::audio::buffer::AudioBuffer;
use crate::config::DitherCorrelation;
use crate::dither::multichannel::MultiChannelDither;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// MBIT+ Stärke-Stufe: bestimmt die Agressivität des Noise-Shaping.
///
/// Die dB-Angaben sind *gemessen* (siehe `tests/dither_verification.rs`, NTF-Tabelle
/// @ 44.1 kHz), nicht geschätzt. „totales Rauschen" = mittlere NTF-Leistung über
/// das Band; „perzeptiv" = ATH-gewichtete Verbesserung ggü. flachem TPDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MbitPlusStrength {
    /// Sanft (NTF 3-Tap): +4.7 dB totales Rauschen, +7.1 dB perzeptiv.
    Low,
    /// Ausgewogen (Default, NTF 5-Tap): +8.7 dB totales Rauschen, +10.4 dB perzeptiv.
    Normal,
    /// Aggressiv (NTF 7-Tap): +11.2 dB totales Rauschen, +12.1 dB perzeptiv.
    High,
}

impl MbitPlusStrength {
    pub fn from_config(value: crate::config::DitherStrength) -> Self {
        match value {
            crate::config::DitherStrength::Low => MbitPlusStrength::Low,
            crate::config::DitherStrength::Normal => MbitPlusStrength::Normal,
            crate::config::DitherStrength::High => MbitPlusStrength::High,
        }
    }

    pub fn coeffs_for_rate(self, sample_rate: u32) -> &'static [f64] {
        self.coeffs(sample_rate)
    }

    /// FIR-Koeffizienten für diese Stärke @ 44.1 kHz.
    ///
    /// WICHTIG (gemessen, nicht behauptet): Dies sind *breitbandige Hochpass*-NTFs.
    /// Ihr tiefster Punkt liegt bei DC (0 Hz), die NTF steigt monoton zum Nyquist —
    /// es gibt KEINEN getunten Notch bei 3.5/4 kHz. Das ist trotzdem perzeptiv
    /// wirksam (Rauschen landet im unempfindlichen HF-Bereich), aber es ist NICHT
    /// gehörrichtig im Sinne eines auf das 2–5-kHz-Sensitivitätstal optimierten
    /// Designs (das leistet der adaptive Phase-4-Pfad bzw. die Lipshitz-Kurve).
    fn coeffs_441(&self) -> &'static [f64] {
        match self {
            // NTF 1 − 1.330z⁻¹ + 0.410z⁻². Monotoner Hochpass: −21.9 dB @ DC,
            // −9.2 dB @ 4 kHz, +8.8 dB @ Nyquist. +4.7 dB totale Leistung.
            MbitPlusStrength::Low => &[1.330, -0.410],
            // Default-Stärke. −28 dB @ DC, −11.5 dB @ 3.5 kHz, +14.4 dB @ Nyquist,
            // +8.7 dB totale Leistung. (Das letzte 0.0-Tap ist wirkungslos.)
            MbitPlusStrength::Normal => &[1.850, -1.530, 0.760, -0.120, 0.0],
            // Aggressiv: stärkstes HF-Dumping. −20 dB @ DC, −14.8 dB @ 3.5 kHz,
            // +18.1 dB @ Nyquist, +11.2 dB totale Leistung. (Letztes 0.0-Tap unnütz.)
            MbitPlusStrength::High => &[2.050, -2.140, 1.620, -0.890, 0.310, -0.050, 0.0],
        }
    }

    /// FIR-Koeffizienten für diese Stärke @ 48 kHz.
    ///
    /// Hinweis: Die Per-Rate-Tabellen unterscheiden sich nur um ±0.02 von den
    /// 44.1-kHz-Werten — sie verschieben den Notch NICHT (er bleibt bei DC). Ein
    /// fester FIR shaped ohnehin bei einem festen Bruchteil der Nyquist-Frequenz,
    /// d.h. die Hochpass-Ecke wandert bei höheren Raten automatisch in Hz mit.
    /// Die separaten Tabellen sind daher weitgehend kosmetisch.
    fn coeffs_48(&self) -> &'static [f64] {
        match self {
            MbitPlusStrength::Low => &[1.300, -0.395],
            MbitPlusStrength::Normal => &[1.820, -1.500, 0.740, -0.115, 0.0],
            MbitPlusStrength::High => &[2.010, -2.100, 1.590, -0.870, 0.300, -0.048, 0.0],
        }
    }

    /// FIR-Koeffizienten für diese Stärke @ 88.2 kHz.
    fn coeffs_882(&self) -> &'static [f64] {
        match self {
            MbitPlusStrength::Low => &[1.310, -0.412],
            MbitPlusStrength::Normal => &[1.860, -1.560, 0.775, -0.125, 0.0],
            MbitPlusStrength::High => &[2.070, -2.180, 1.650, -0.910, 0.320, -0.052, 0.0],
        }
    }

    /// FIR-Koeffizienten für diese Stärke @ 96 kHz.
    fn coeffs_96(&self) -> &'static [f64] {
        match self {
            MbitPlusStrength::Low => &[1.290, -0.387],
            MbitPlusStrength::Normal => &[1.830, -1.530, 0.760, -0.120, 0.0],
            MbitPlusStrength::High => &[2.030, -2.150, 1.620, -0.895, 0.315, -0.050, 0.0],
        }
    }

    /// Select coefficients for the given sample rate.
    fn coeffs(&self, sample_rate: u32) -> &'static [f64] {
        match sample_rate {
            44_100 => self.coeffs_441(),
            48_000 => self.coeffs_48(),
            88_200 => self.coeffs_882(),
            96_000 => self.coeffs_96(),
            // Fallback: use 48k for other common rates.
            _ if (sample_rate as f64 - 44_100.0).abs() < 5_000.0 => self.coeffs_441(),
            _ => self.coeffs_48(),
        }
    }
}

/// Per-Channel-Zustand für Error-Feedback Quantisierung.
#[derive(Debug, Clone)]
struct ChannelState {
    /// Error history, newest first (for FIR feedback).
    error_hist: Vec<f64>,
    /// Auto-Blanking: Anzahl Samples unter dem Schwellwert (0.5 LSB).
    blanking_count: usize,
    /// Auto-Blanking-Schwelle in Samples (~50 ms).
    blanking_threshold: usize,
}

impl ChannelState {
    fn new(coeffs_len: usize, sample_rate: u32) -> Self {
        let blanking_threshold = (sample_rate as usize) / 20; // 50 ms.
        Self {
            error_hist: vec![0.0; coeffs_len],
            blanking_count: 0,
            blanking_threshold,
        }
    }

    /// Shift error history and push new error.
    fn push_error(&mut self, e: f64) {
        self.error_hist.rotate_right(1);
        self.error_hist[0] = e;
    }

    /// Reset error history and blanking counter.
    fn reset(&mut self) {
        self.error_hist.fill(0.0);
        self.blanking_count = 0;
    }

    /// Check if blanking should be applied (silence).
    fn should_blank(&mut self, sample: f64) -> bool {
        if sample.abs() < 0.5 / 32_767.0 {
            self.blanking_count += 1;
            self.blanking_count > self.blanking_threshold
        } else {
            self.blanking_count = 0;
            false
        }
    }
}

/// MBIT+ Phase 1 quantizer: stable, gehörgewichtet, auto-blanking.
pub fn quantize(
    buffer: &mut AudioBuffer,
    max: f64,
    min_i: i32,
    max_i: i32,
    strength: MbitPlusStrength,
    correlation: DitherCorrelation,
    seed: Option<u64>,
) {
    if buffer.channels.is_empty() || buffer.frame_len() == 0 {
        return;
    }

    let coeffs = strength.coeffs(buffer.sample_rate);
    let ch_count = buffer.channels_count();

    // Initialize per-channel state.
    let mut states = vec![ChannelState::new(coeffs.len(), buffer.sample_rate); ch_count];
    
    // Create multi-channel dither generator with selected correlation mode.
    let mut dither_gen = MultiChannelDither::new(ch_count, correlation, seed);
    let mut dither_samples = vec![0.0; ch_count];

    for n in 0..buffer.frame_len() {
        for ch in 0..ch_count {
            let state = &mut states[ch];
            let x = buffer.channels[ch][n];

            // Auto-blanking: when silent long enough, shut off dither and feedback.
            if state.should_blank(x) {
                buffer.channels[ch][n] = 0.0;
                state.reset();
                continue;
            }

            // True digital silence stays silent. We still keep the blanking counter
            // updated above so a near-silent tail can trigger the same reset path,
            // but once the input is exactly zero there is no reason to add dither.
            if x == 0.0 {
                buffer.channels[ch][n] = 0.0;
                state.reset();
                continue;
            }

            // Feedback term: apply FIR to error history.
            let fb = coeffs.iter().zip(&state.error_hist).map(|(c, e)| c * e).sum::<f64>();

            // Generate dither samples for all channels at this sample index.
            if ch == 0 {
                dither_samples = dither_gen.sample_lsb_all();
            }

            // Get dither sample for this channel.
            let dither = dither_samples[ch];

            // Quantization: shaped input → dither → round → clamp.
            let s = x.clamp(-1.0, 1.0) * max;
            let u = s - fb;
            let w = u + dither;
            let y = (w.round() as i32).clamp(min_i, max_i);

            // Feed back total error.
            let e = y as f64 - u;
            state.push_error(e);

            buffer.channels[ch][n] = y as f64 / max;
        }
    }
}

/// Per-Channel seed derivation for decorrelated TPDF.
/// Deterministic distinct sub-seed per channel, so each channel gets independent TPDF.
/// (Replaced by MultiChannelDither in Phase 4.4, but kept for reference.)
#[allow(dead_code)]
fn per_channel_seed(seed: Option<u64>, channel: usize) -> Option<u64> {
    seed.map(|s| s ^ (channel as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// Portable TPDF dither generator.
/// (Replaced by MultiChannelDither in Phase 4.4, but kept for reference.)
#[allow(dead_code)]
struct TpdfDither {
    rng: StdRng,
}

#[allow(dead_code)]
impl TpdfDither {
    fn new(seed: Option<u64>) -> Self {
        let rng = match seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        };
        Self { rng }
    }

    fn sample_lsb(&mut self) -> f64 {
        let a = self.rng.gen_range(-0.5_f64..0.5_f64);
        let b = self.rng.gen_range(-0.5_f64..0.5_f64);
        a + b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanking_threshold_reasonable() {
        let s = ChannelState::new(5, 44_100);
        assert_eq!(s.blanking_threshold, 2_205); // 50 ms @ 44.1 kHz.
    }

    #[test]
    fn coeffs_are_defined_for_common_rates() {
        let lo = MbitPlusStrength::Low;
        assert!(!lo.coeffs(44_100).is_empty());
        assert!(!lo.coeffs(48_000).is_empty());
        assert!(!lo.coeffs(88_200).is_empty());
        assert!(!lo.coeffs(96_000).is_empty());
    }
}

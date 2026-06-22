//! Phase 4.4 — Stereo and multi-channel dither correlation modes
//!
//! Provides configurable TPDF dither generation for stereo and multi-channel audio,
//! with support for three correlation strategies:
//!
//! - **Decorrelated (default):** Each channel gets independent TPDF noise.
//!   - Pros: Maximum SNR, no stereo imaging artifacts
//!   - Cons: Slight width in the dither noise (not mono)
//!
//! - **Correlated:** All channels share identical dither.
//!   - Pros: Mono dither noise (no stereo width)
//!   - Cons: Reduced SNR per channel, potential imaging artifacts
//!
//! - **Mid-Side:** Dither applied to mid (L+R) and side (L-R) channels.
//!   - Pros: Balanced SNR and stereo coherence
//!   - Cons: Moderate complexity, requires 2-channel input
//!
//! For >2 channels, all modes gracefully extend:
//! - Decorrelated: Each channel independent
//! - Correlated: All channels share one dither stream
//! - Mid-Side: Falls back to decorrelated (true M-S only valid for stereo)

pub use crate::config::DitherCorrelation;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// Multi-channel TPDF dither generator with configurable correlation mode.
///
/// Creates per-channel dither samples according to the selected correlation strategy.
#[derive(Debug)]
pub struct MultiChannelDither {
    /// Shared RNG for correlated mode.
    shared_rng: StdRng,
    /// Per-channel RNGs for decorrelated mode.
    per_ch_rngs: Vec<StdRng>,
    correlation: DitherCorrelation,
    num_channels: usize,
}

impl MultiChannelDither {
    /// Create a new multi-channel dither generator.
    ///
    /// # Arguments
    /// - `num_channels`: number of audio channels (1 = mono, 2 = stereo, >2 = multichannel)
    /// - `correlation`: dither correlation mode (default: Decorrelated)
    /// - `seed`: optional RNG seed for reproducibility
    pub fn new(num_channels: usize, correlation: DitherCorrelation, seed: Option<u64>) -> Self {
        let base_seed = seed.unwrap_or_else(|| 0x_DEAD_BEEF_CAFE_F00D_u64);
        let shared_rng = StdRng::seed_from_u64(base_seed);

        // Per-channel RNGs use a deterministic derivation of the base seed.
        let per_ch_rngs = (0..num_channels)
            .map(|ch| {
                let per_ch_seed = base_seed ^ (ch as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
                StdRng::seed_from_u64(per_ch_seed)
            })
            .collect();

        Self {
            shared_rng,
            per_ch_rngs,
            correlation,
            num_channels,
        }
    }

    /// Generate TPDF dither samples for all channels.
    ///
    /// Returns a vector of `num_channels` dither values, each in range [-1, 1] LSB.
    /// Allocating convenience wrapper; the per-sample quantizer loop should call
    /// [`sample_lsb_all_into`](Self::sample_lsb_all_into) with a reused buffer.
    pub fn sample_lsb_all(&mut self) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.num_channels);
        self.sample_lsb_all_into(&mut out);
        out
    }

    /// Fill `out` (cleared first) with one dither value per channel — same RNG draw
    /// order as [`sample_lsb_all`], without allocating on the hot path.
    pub fn sample_lsb_all_into(&mut self, out: &mut Vec<f64>) {
        out.clear();
        match self.correlation {
            DitherCorrelation::Decorrelated => self.fill_decorrelated(out),
            DitherCorrelation::Correlated => {
                let a = self.shared_rng.gen_range(-0.5_f64..0.5_f64);
                let b = self.shared_rng.gen_range(-0.5_f64..0.5_f64);
                out.resize(self.num_channels, a + b);
            }
            DitherCorrelation::MidSide => {
                // True M-S only defined for stereo; otherwise decorrelated fallback.
                if self.num_channels != 2 {
                    self.fill_decorrelated(out);
                    return;
                }
                let m = {
                    let a = self.per_ch_rngs[0].gen_range(-0.5_f64..0.5_f64);
                    let b = self.per_ch_rngs[0].gen_range(-0.5_f64..0.5_f64);
                    a + b
                };
                let s = {
                    let a = self.per_ch_rngs[1].gen_range(-0.5_f64..0.5_f64);
                    let b = self.per_ch_rngs[1].gen_range(-0.5_f64..0.5_f64);
                    a + b
                };
                out.push((m + s) / 2.0);
                out.push((m - s) / 2.0);
            }
        }
    }

    /// Decorrelated mode: each channel gets independent TPDF (writes into `out`).
    fn fill_decorrelated(&mut self, out: &mut Vec<f64>) {
        for rng in self.per_ch_rngs.iter_mut() {
            let a = rng.gen_range(-0.5_f64..0.5_f64);
            let b = rng.gen_range(-0.5_f64..0.5_f64);
            out.push(a + b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decorrelated_dither_differs_per_channel() {
        let mut dither_gen = MultiChannelDither::new(2, DitherCorrelation::Decorrelated, Some(42));
        let samples = dither_gen.sample_lsb_all();
        
        assert_eq!(samples.len(), 2, "should return 2 samples");
        // Channels should differ (very low probability of match by chance).
        let diff = (samples[0] - samples[1]).abs();
        assert!(diff > 0.01, "decorrelated dither should produce different values per channel");
    }

    #[test]
    fn correlated_dither_same_all_channels() {
        let mut dither_gen = MultiChannelDither::new(2, DitherCorrelation::Correlated, Some(42));
        let samples = dither_gen.sample_lsb_all();
        
        assert_eq!(samples.len(), 2, "should return 2 samples");
        assert_eq!(samples[0], samples[1], "correlated dither should be identical across channels");
    }

    #[test]
    fn mid_side_dither_differs_per_channel() {
        let mut dither_gen = MultiChannelDither::new(2, DitherCorrelation::MidSide, Some(42));
        let samples = dither_gen.sample_lsb_all();
        
        assert_eq!(samples.len(), 2, "should return 2 samples");
        let diff = (samples[0] - samples[1]).abs();
        assert!(diff > 0.01, "mid-side dither should produce different L/R values");
    }

    #[test]
    fn multichannel_decorrelated_all_independent() {
        let mut dither_gen = MultiChannelDither::new(6, DitherCorrelation::Decorrelated, Some(100));
        let samples = dither_gen.sample_lsb_all();
        
        assert_eq!(samples.len(), 6, "should return 6 samples");
        // Check that not all channels are identical (highly unlikely if truly independent).
        let all_same = samples.iter().all(|&s| (s - samples[0]).abs() < 1e-10);
        assert!(!all_same, "independent channels should not all have identical dither");
    }

    #[test]
    fn multichannel_correlated_all_identical() {
        let mut dither_gen = MultiChannelDither::new(6, DitherCorrelation::Correlated, Some(100));
        let samples = dither_gen.sample_lsb_all();
        
        assert_eq!(samples.len(), 6, "should return 6 samples");
        // All should be identical.
        for i in 1..samples.len() {
            assert_eq!(samples[i], samples[0], "correlated dither must be identical across all channels");
        }
    }

    #[test]
    fn multichannel_mid_side_falls_back_to_decorrelated() {
        let mut dither_gen = MultiChannelDither::new(6, DitherCorrelation::MidSide, Some(100));
        let samples = dither_gen.sample_lsb_all();
        
        assert_eq!(samples.len(), 6, "should return 6 samples");
        // For >2 channels, should use decorrelated fallback.
        let all_same = samples.iter().all(|&s| (s - samples[0]).abs() < 1e-10);
        assert!(!all_same, "mid-side for >2 channels should fall back to decorrelated");
    }

    #[test]
    fn samples_in_lsb_range() {
        let mut dither_gen = MultiChannelDither::new(4, DitherCorrelation::Decorrelated, Some(42));
        for _ in 0..100 {
            let samples = dither_gen.sample_lsb_all();
            for sample in samples {
                assert!(
                    sample >= -1.0 && sample <= 1.0,
                    "dither sample {sample} outside [-1, 1] range"
                );
            }
        }
    }

    #[test]
    fn reproducible_with_seed() {
        let mut dither_gen1 = MultiChannelDither::new(2, DitherCorrelation::Decorrelated, Some(999));
        let mut dither_gen2 = MultiChannelDither::new(2, DitherCorrelation::Decorrelated, Some(999));
        
        for _ in 0..10 {
            let s1 = dither_gen1.sample_lsb_all();
            let s2 = dither_gen2.sample_lsb_all();
            assert_eq!(s1, s2, "same seed should produce identical sequences");
        }
    }

    #[test]
    fn different_seeds_produce_different_sequences() {
        let mut dither_gen1 = MultiChannelDither::new(2, DitherCorrelation::Decorrelated, Some(111));
        let mut dither_gen2 = MultiChannelDither::new(2, DitherCorrelation::Decorrelated, Some(222));
        
        let s1 = dither_gen1.sample_lsb_all();
        let s2 = dither_gen2.sample_lsb_all();
        assert_ne!(s1, s2, "different seeds should produce different sequences");
    }
}

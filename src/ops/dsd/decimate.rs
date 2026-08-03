//! DSD -> PCM decimation. **A measurement instrument, not part of any output path.**
//!
//! A modulator without a decimator is an assertion: you cannot tell a 130 dB loop from a
//! 70 dB one by looking at a bitstream. This is the mirror of `interpolate.rs` and it is
//! what `tests/dsd_verification.rs` measures through — the same stance
//! `tests/dither_verification.rs` takes for the PCM shapers.
//!
//! It is `pub` rather than `#[cfg(test)]` because integration tests link the library
//! compiled *without* `cfg(test)`, so a test-gated module would be invisible to them.
//! Nothing in `pipeline.rs` calls it. (If DSD *input* is ever wanted — explicitly out of
//! scope in `docs/dsd-design.md` section 5 — this is most of it.)
//!
//! # Why it has to be this good
//!
//! Decimating a 1-bit stream is not "average some samples". Above the audio band a DSD64
//! stream is nearly all noise, rising to enormous levels near 1.4 MHz — and decimation by
//! two folds everything above the new Nyquist straight back down, with content near the
//! old sample rate landing *inside the audio band*. A sloppy anti-alias filter therefore
//! does not blur the measurement, it invents a noise floor and makes a good modulator look
//! mediocre. Each stage here is a Kaiser-windowed half-band with ~180 dB of stopband, so
//! the folded residue lands around -240 dBFS: roughly 120 dB below anything the modulator
//! itself produces in band, i.e. the instrument does not show up in its own readings.
//!
//! # Structure
//!
//! A cascade of identical /2 half-band stages. Each keeps `0 .. 0.20*fs_in` flat and
//! rejects from `0.30*fs_in` up, so the band that survives the whole cascade unharmed is
//! `0 .. 0.40*fs_out` — 35.3 kHz when decimating to 88.2 kHz, comfortably past 20 kHz.
//! Half-band symmetry means every second tap is exactly zero and the rest are mirrored, so
//! a 121-tap filter costs 33 multiplies per output sample.
//!
//! Stages are linear phase and compensated, so output sample `m` lines up with input
//! sample `m * factor` to the sample. The edges are zero-extended: the first and last
//! `~taps/2 * factor` samples of a result are filter transient and should be discarded by
//! the caller (the harness windows them away).

use anyhow::{Result, bail};
use std::f64::consts::PI;

use crate::ops::dsd::modulator::level;

/// Target stopband attenuation of one stage, in dB. Sets both the Kaiser shape parameter
/// and the tap count. 180 dB is not gold-plating: see the module docs on folding.
const STOPBAND_DB: f64 = 180.0;
/// Transition band of one stage, as a fraction of its input sample rate. The stage passes
/// up to `0.25 - HALF_TRANSITION` and stops from `0.25 + HALF_TRANSITION`.
const HALF_TRANSITION: f64 = 0.05;

/// One /2 half-band decimation stage.
#[derive(Debug, Clone)]
struct HalfBand {
    /// Full symmetric impulse response; every second tap either side of the centre is
    /// exactly zero, which the inner loop skips rather than multiplies.
    taps: Vec<f64>,
    centre: usize,
}

impl HalfBand {
    fn design() -> Self {
        // Kaiser's two standard estimates: shape from the attenuation, length from the
        // attenuation and the transition width.
        let beta = 0.1102 * (STOPBAND_DB - 8.7);
        let d_omega = 2.0 * PI * (2.0 * HALF_TRANSITION);
        let mut n = ((STOPBAND_DB - 8.0) / (2.285 * d_omega)).ceil() as usize;
        n = n.max(15) | 1; // odd, so there is a true centre tap
        let centre = n / 2;

        let i0_beta = bessel_i0(beta);
        let mut taps = vec![0.0; n];
        for (i, tap) in taps.iter_mut().enumerate() {
            let m = i as f64 - centre as f64;
            // 0.5 * sinc(m/2): the ideal half-band. sinc vanishes at every even non-zero
            // m, which is where the free half of the multiplies goes.
            let ideal = if m == 0.0 {
                0.5
            } else {
                (0.5 * PI * m).sin() / (PI * m)
            };
            let r = m / centre as f64;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
            *tap = ideal * w;
        }
        // Unity DC gain, so a cascade neither scales nor biases the signal.
        let sum: f64 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);

        Self { taps, centre }
    }

    /// Filter and drop every second sample. Output `m` is aligned with input `2*m`.
    fn process(&self, x: &[f64]) -> Vec<f64> {
        let d = self.centre;
        let mut out = Vec::with_capacity(x.len() / 2);
        for m in 0..x.len() / 2 {
            let c = 2 * m;
            let mut acc = self.taps[d] * x[c];
            // Odd offsets only; the even ones are the structural zeros.
            let mut k = 1;
            while k <= d {
                let h = self.taps[d - k];
                let lo = if c >= k { x[c - k] } else { 0.0 };
                let hi = x.get(c + k).copied().unwrap_or(0.0);
                acc += h * (lo + hi);
                k += 2;
            }
            out.push(acc);
        }
        out
    }
}

/// Power-of-two decimation from a DSD rate down to a PCM rate.
#[derive(Debug, Clone)]
pub struct Decimator {
    stage: HalfBand,
    stages: u32,
}

impl Decimator {
    /// `factor` must be a power of two — the same constraint the interpolation side has,
    /// and for the same reason (the DSD rates are all `2^k * 44.1 kHz`).
    pub fn new(factor: u32) -> Result<Self> {
        if factor == 0 || !factor.is_power_of_two() {
            bail!("DSD decimation needs a power-of-two factor, got {factor}");
        }
        Ok(Self {
            stage: HalfBand::design(),
            stages: factor.trailing_zeros(),
        })
    }

    pub fn for_rates(dsd_hz: u32, pcm_hz: u32) -> Result<Self> {
        if pcm_hz == 0 || !dsd_hz.is_multiple_of(pcm_hz) {
            bail!("cannot decimate {dsd_hz} Hz to {pcm_hz} Hz");
        }
        Self::new(dsd_hz / pcm_hz)
    }

    /// Decimate a bitstream. `true` maps to +1, matching the modulator's polarity.
    pub fn process_bits(&self, bits: &[bool]) -> Vec<f64> {
        let samples: Vec<f64> = bits.iter().copied().map(level).collect();
        self.process(&samples)
    }

    /// Decimate an already-levelled signal (+-1 for a real bitstream, anything for a
    /// transparency check on the filter itself).
    pub fn process(&self, input: &[f64]) -> Vec<f64> {
        let mut buf = input.to_vec();
        for _ in 0..self.stages {
            buf = self.stage.process(&buf);
        }
        buf
    }

    pub fn factor(&self) -> u32 {
        1 << self.stages
    }

    /// Taps in one stage — the harness uses it to size the transient it discards.
    pub fn taps_per_stage(&self) -> usize {
        self.stage.taps.len()
    }

    /// `|H(e^jw)|` of a single stage, for the test that proves the instrument is clean.
    pub fn stage_response(&self, normalized_freq: f64) -> f64 {
        let w = 2.0 * PI * normalized_freq;
        let d = self.stage.centre as f64;
        let mut re = 0.0;
        let mut im = 0.0;
        for (i, &h) in self.stage.taps.iter().enumerate() {
            let ang = w * (i as f64 - d);
            re += h * ang.cos();
            im -= h * ang.sin();
        }
        (re * re + im * im).sqrt()
    }
}

/// Modified Bessel function of the first kind, order 0 — the Kaiser window's kernel.
/// Straight power series: all terms positive, so no cancellation, and it converges in a
/// few dozen terms for the arguments a window needs.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..128 {
        let f = half / k as f64;
        term *= f * f;
        sum += term;
        if term < 1e-18 * sum {
            break;
        }
    }
    sum
}

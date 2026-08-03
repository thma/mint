//! 1-bit sigma-delta modulator: the stage that actually makes a DSD stream.
//!
//! This is the only place in mint where the f64 buffer is mapped onto a one-bit grid, and
//! it is the DSD counterpart of the "quantize exactly once" invariant that `ops/bitdepth`
//! carries for PCM. The modulator must see the *un-quantized* buffer; running
//! `bitdepth::apply` in front of it would be a real bug, not a rounding detail.
//!
//! # Level convention
//!
//! DSD level is not PCM level. The Scarlet Book reference "0 dB" is **50 % modulation
//! index**, and the format tops out at 71.43 % (the 20/28 rule, +3.1 dB). So PCM 0 dBFS
//! enters the loop scaled by [`INPUT_SCALE`] = 0.5 — not an attenuation, because a DAC
//! reads 50 % modulation as *its* 0 dBFS. Playback level is unchanged; what changes is
//! that the modulator gets the headroom the format assumes it has.
//!
//! # Structure
//!
//! Order-7 CIFB (cascade of integrators, feedback) with three resonators and one plain
//! integrator, i.e. NTF zeros = one at DC plus three conjugate pairs spread over the audio
//! band. State form rather than a plain error-feedback FIR for one reason: only here can
//! the integrator states be bounded individually, and that bound is the whole stability
//! insurance for a 1-bit loop (see [`Modulator::step`]).
//!
//! The per-sample update is a single in-place sequential pass:
//!
//! ```text
//! y     = x[6] + dither                       quantizer input
//! v     = sign(y)                             the output bit, +-1
//! d     = u - v                               input and feedback enter together
//! x[0] += a[0]*d - g[0]*x[1]                  x[1] still the OLD value
//! x[1] += a[1]*d + x[0]                       x[0] already NEW -> non-delaying
//! x[2] += a[2]*d + x[1] - g[1]*x[3]
//! ...
//! x[6] += a[6]*d + x[5]
//! ```
//!
//! The mixed delaying/non-delaying pair is what puts each resonator's poles *on* the unit
//! circle (`z^2 + (g-2)z + 1`, product of roots = 1) — with two delaying integrators they
//! would sit at radius `sqrt(1+g)` and the notch would not be a notch. Driving every stage
//! from the same `d = u - v` makes `STF = 1 - NTF`: flat to within the in-band NTF ripple
//! (i.e. exactly as flat as the noise suppression is deep) and gently low-pass above the
//! band, which is free and welcome ahead of a DAC.
//!
//! # Where the coefficients come from
//!
//! `tools/design_ntf.py`, run once offline (precedent: `dither/static_fir.rs`). It designs
//! a Chebyshev-II high-pass NTF — Schreier's `synthesizeChebyshevNTF` construction — with
//! the stopband on the audio band, bisecting the stopband attenuation until
//! `sup|NTF| == H_inf`, then solves the linear system that maps the CIFB gains onto that
//! transfer function. One NTF per DSD rate, because the notch has to sit in 0–20 kHz at
//! every rate; the resonator gains `g` shrink with rate while `a` barely moves.
//!
//! Nothing at build time depends on that script: [`tests/dsd_verification.rs`] re-derives
//! `|NTF(e^jw)|` from the table and re-measures every claim below.
//!
//! # H_inf = 1.30, order 7 — and why not the 5/1.5 in the design note
//!
//! `docs/dsd-design.md` sketched order 5 with `H_inf = 1.5`. Measured (`--sweep`), that
//! combination is stable only up to ~57 % modulation: it *diverges* well below the 71.43 %
//! the format permits, and 0 dBFS PCM — 50 % — would sit barely 1 dB inside the cliff.
//! Order 7 at a lower `H_inf` dominates it on both axes at once:
//!
//! | order | H_inf | max stable amplitude | in-band SNR |
//! |---|---|---|---|
//! | 5 | 1.50 | 0.57 | 119.6 dB |
//! | 5 | 1.35 | 0.75 | 110.2 dB |
//! | **7** | **1.30** | **0.75** | **121.1 dB** |
//! | 7 | 1.45 | 0.58 | 137.7 dB |
//!
//! so the table is order 7 at `H_inf = 1.30` — same stability as the best order-5 option
//! and 11 dB more suppression. Lee's criterion (`H_inf < 2`) has plenty of room left.
//!
//! # What "stable" means here, precisely
//!
//! Maximum stable amplitude depends on the *shape* of the input, not just its peak — a
//! sine is the easy case — and on [`QUANTIZER_DITHER`], which buys idle-tone suppression
//! partly out of stability margin. At DSD64, with the shipped dither:
//!
//! | input | max stable modulation | margin over 0 dBFS |
//! |---|---|---|
//! | sine | 0.691 | +2.8 dB |
//! | sustained DC | 0.661 | +2.4 dB |
//! | hard square wave | 0.637 | +2.1 dB |
//!
//! **The operating range is 0 to 0.5, not 0 to 0.7143.** PCM is bounded at ±1 and the
//! ceiling stage caps the true peak at or below 0 dBFS, so `INPUT_SCALE` puts a hard
//! bound of 50 % modulation on anything the pipeline can hand the loop. Every probe above
//! clears that by at least 2.1 dB, and the battery behind [`Ntf::state_limit`] confirms
//! that nothing at 50 % touches a clamp.
//!
//! The format's 71.43 % is a statement about what a DSD stream can *represent*, not a
//! level mint can reach: it would take PCM at +3.1 dBFS. Fed one anyway, the loop
//! overloads rather than diverging — the state clamps bound it, it recovers to full SNR
//! once the input is sane, and [`ModulatorStats::overload_samples`] says it happened.

use crate::dither::TpdfDither;
use crate::ops::dsd::DsdRate;

/// Loop order. Fixed for every rate; only the NTF zeros move.
pub const ORDER: usize = 7;
/// Resonator pairs. The odd stage out is the plain integrator that puts a zero at DC.
pub const RESONATORS: usize = ORDER / 2;

/// PCM 0 dBFS -> 50 % modulation index, the Scarlet Book reference level.
pub const INPUT_SCALE: f64 = 0.5;
/// The 20/28 rule: the largest modulation index the format allows (+3.1 dB over reference).
pub const MAX_MODULATION: f64 = 20.0 / 28.0;

/// TPDF dither at the comparator, in units of the quantizer input.
///
/// Against idle tones — the classic DSD complaint. A steady DC offset drives a 1-bit loop
/// into a limit cycle whose fundamental sits at `fs * modulation`, which for any small
/// offset lands squarely in the audio band (a 1e-3 DC offset puts one at 1410 Hz).
///
/// **This is much larger than the 1e-3…1e-2 `docs/dsd-design.md` assumed, because that
/// range was measured to do nothing at all.** The reason is worth writing down: dither
/// added at the comparator is shaped by the loop exactly like the quantization error, so
/// the very notch that buys the in-band SNR also removes the dither's in-band randomizing
/// power. To perturb the decision it has to be a real fraction of the quantizer input,
/// which runs at `sigma ~ 0.3`. Measured at DSD64, peak-to-median over the in-band
/// spectrum with a 1e-3 DC offset:
///
/// | dither | idle tone | in-band cost @ 0 dBFS |
/// |---|---|---|
/// | 0 (or 2e-3) | 34.5 dB | reference |
/// | 0.15 | 22.7 dB | +0.6 dB |
/// | **0.20** | **16.1 dB** | **+1.2 dB** |
/// | 0.30 | 15.0 dB | +3.1 dB |
///
/// 0.20 puts the worst probe within ~2 dB of the ~14 dB floor a tone-free spectrum has
/// anyway, for about 1 dB of SNR. Larger buys nothing and costs stability margin.
///
/// Changing this invalidates [`Ntf::state_limit`]: rerun `tools/design_ntf.py --dither`.
const QUANTIZER_DITHER: f64 = 0.2;

/// Consecutive clamped samples after which the loop is considered stuck rather than
/// merely clipped, and the states are dumped. A stable loop never clamps at all, and a
/// briefly overdriven one recovers within a few samples; only genuine divergence holds
/// the states pinned this long.
const RESET_AFTER_CLAMPED: u32 = 64;

/// Noise-transfer function for one DSD rate, in the realized CIFB coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Ntf {
    /// Feedback/feedforward gains, one per integrator.
    pub a: [f64; ORDER],
    /// Resonator gains; `g = 2 - 2*cos(theta)` for a zero pair at `exp(+-j*theta)`.
    pub g: [f64; RESONATORS],
    /// Per-state clamp: the worst state peak over a battery of hostile probes at 0 dBFS
    /// (DC, steps, square waves, tones from 20 Hz up), times 32.
    ///
    /// The margin is deliberately lopsided because the failure modes are. Too tight and
    /// the clamp distorts *legal* audio — the peaks span five orders of magnitude between
    /// the first and last integrator and move with the signal. Too loose only delays
    /// detection: once the loop goes, it grows by about `H_inf` per sample, so even 32x is
    /// crossed within a dozen.
    pub state_limit: [f64; ORDER],
}

/// What the modulator saw, for `ProcessingReport` and the verification harness.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ModulatorStats {
    /// Largest modulation index reached, as a fraction (0.5 == the SACD 0 dB reference).
    pub max_modulation: f64,
    /// Samples on which at least one integrator state hit its clamp.
    pub overload_samples: u64,
    /// Times the loop stayed clamped long enough to be declared diverged and reset.
    pub resets: u64,
    pub samples: u64,
}

impl ModulatorStats {
    /// Peak modulation in percent — the number the report shows and warns on.
    pub fn max_modulation_percent(&self) -> f64 {
        self.max_modulation * 100.0
    }

    /// Did the run exceed what the format allows? (Only reachable from PCM hotter than
    /// 0 dBFS true-peak, which the ceiling stage is there to prevent.)
    pub fn exceeds_format_maximum(&self) -> bool {
        self.max_modulation > MAX_MODULATION
    }
}

/// One instance per channel. The integrator states *are* the modulator's memory, so
/// sharing an instance across channels would cross-modulate them — the same rule the PCM
/// [`crate::dither::NoiseShaper`] follows for its error history.
pub struct Modulator {
    ntf: &'static Ntf,
    x: [f64; ORDER],
    dither: TpdfDither,
    dither_scale: f64,
    stats: ModulatorStats,
    clamped_run: u32,
}

impl std::fmt::Debug for Modulator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Modulator")
            .field("x", &self.x)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl Modulator {
    pub fn new(rate: DsdRate, seed: Option<u64>) -> Self {
        Self::with_quantizer_dither(rate, seed, QUANTIZER_DITHER)
    }

    /// Same, with the comparator dither amplitude overridden. Exists so the idle-tone gate
    /// in `tests/dsd_verification.rs` can measure the dithered loop *against* the
    /// undithered one — a claim that the dither suppresses idle tones is worth only as
    /// much as the comparison behind it. Production always uses [`Modulator::new`].
    pub fn with_quantizer_dither(rate: DsdRate, seed: Option<u64>, dither_scale: f64) -> Self {
        Self {
            ntf: ntf_for(rate),
            x: [0.0; ORDER],
            dither: TpdfDither::new(seed),
            dither_scale,
            stats: ModulatorStats::default(),
            clamped_run: 0,
        }
    }

    /// Per-channel instance with an independent but reproducible dither stream. Same seed
    /// derivation as [`crate::dither::multichannel::MultiChannelDither`], so a given
    /// `seed` reproduces a given file bit for bit regardless of rayon scheduling.
    pub fn for_channel(rate: DsdRate, seed: Option<u64>, channel: usize) -> Self {
        let base = seed.unwrap_or(0x_DEAD_BEEF_CAFE_F00D_u64);
        let derived = base ^ (channel as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        Self::new(rate, Some(derived))
    }

    /// Modulate one block of interpolated PCM, appending one bit per sample.
    ///
    /// `true` is the high level (+1); that is the polarity DSF stores directly, so the
    /// packer never has to invert. Blocks are independent only in their *input* — the loop
    /// state carries across them, which is exactly why the DSD stage keeps one modulator
    /// alive per channel for the whole file.
    pub fn modulate_into(&mut self, pcm: &[f64], out: &mut Vec<bool>) {
        out.reserve(pcm.len());
        for &sample in pcm {
            let u = sample * INPUT_SCALE;
            let m = u.abs();
            if m > self.stats.max_modulation {
                self.stats.max_modulation = m;
            }
            out.push(self.step(u));
        }
        self.stats.samples += pcm.len() as u64;
    }

    /// Convenience for tests and one-shot use.
    pub fn modulate(&mut self, pcm: &[f64]) -> Vec<bool> {
        let mut out = Vec::with_capacity(pcm.len());
        self.modulate_into(pcm, &mut out);
        out
    }

    pub fn stats(&self) -> ModulatorStats {
        self.stats
    }

    /// Current integrator states — only interesting to the stability tests.
    pub fn states(&self) -> [f64; ORDER] {
        self.x
    }

    /// One sample through the loop. `u` is already scaled to modulation units.
    fn step(&mut self, u: f64) -> bool {
        // The comparator is the whole quantizer: its "error" is whatever it takes to turn
        // y into +-1, and the loop shapes that error — including the dither we add here.
        let y = self.x[ORDER - 1] + self.dither_scale * self.dither.sample_lsb();
        let bit = y >= 0.0;
        let v = if bit { 1.0 } else { -1.0 };
        let d = u - v;

        let mut clamped = false;
        // `prev` carries each stage's *new* value into the next one; the resonator term
        // reads its partner's *old* value, since that stage has not been updated yet.
        let mut prev = 0.0;
        for s in 0..ORDER {
            let mut inc = self.ntf.a[s] * d + prev;
            if s % 2 == 0 && s + 1 < ORDER {
                inc -= self.ntf.g[s / 2] * self.x[s + 1];
            }
            let next = self.x[s] + inc;

            // Clamp inside the loop, not after it, so a saturating stage cannot hand a
            // runaway value to the one behind it within the same sample.
            let limit = self.ntf.state_limit[s];
            self.x[s] = if next.is_finite() {
                if next > limit {
                    clamped = true;
                    limit
                } else if next < -limit {
                    clamped = true;
                    -limit
                } else {
                    next
                }
            } else {
                // Only reachable if the input was non-finite; a NaN state would poison the
                // rest of the file, so treat it as the hardest possible overload.
                clamped = true;
                0.0
            };
            prev = self.x[s];
        }

        if clamped {
            self.stats.overload_samples += 1;
            self.clamped_run += 1;
            if self.clamped_run >= RESET_AFTER_CLAMPED {
                self.x = [0.0; ORDER];
                self.clamped_run = 0;
                self.stats.resets += 1;
            }
        } else {
            self.clamped_run = 0;
        }

        bit
    }
}

/// Map a bit back to its analog level. The decimator and every measurement start here.
#[inline]
pub fn level(bit: bool) -> f64 {
    if bit { 1.0 } else { -1.0 }
}

pub fn ntf_for(rate: DsdRate) -> &'static Ntf {
    match rate {
        DsdRate::Dsd64 => &NTF_TABLE[0],
        DsdRate::Dsd128 => &NTF_TABLE[1],
        DsdRate::Dsd256 => &NTF_TABLE[2],
        DsdRate::Dsd512 => &NTF_TABLE[3],
    }
}

// ── generated by tools/design_ntf.py — see the module docs before editing ────────────
// order 7, H_inf 1.30, design band edge 20000 Hz, clamp margin 32x
static NTF_TABLE: [Ntf; 4] = [
    // Dsd64: OSR 70.56, cheby2 stopband 96.8653 dB, sup|NTF| 1.3000, max|pole| 0.9744
    Ntf {
        a: [
            2.2724216034552567e-07,
            9.26066190830913e-06,
            0.00015006899133047555,
            0.0019098013872938155,
            0.015624733852852589,
            0.0981493993293024,
            0.40828402366863814,
        ],
        g: [
            0.00037327793941899223,
            0.001211771763051095,
            0.0018839376283741327,
        ],
        state_limit: [1.88616e-05, 0.00078569, 0.01337, 0.16515, 1.31094, 7.7433, 31.957],
    },
    // Dsd128: OSR 141.12, cheby2 stopband 137.4572 dB, sup|NTF| 1.3000, max|pole| 0.9744
    Ntf {
        a: [
            2.2845762566911657e-07,
            8.468153384972664e-06,
            0.0001515394267155463,
            0.0018659225161741266,
            0.01587822933080096,
            0.09807122247087836,
            0.40828402366863814,
        ],
        g: [
            9.330289104436673e-05,
            0.00030293670654191196,
            0.000471034098112888,
        ],
        state_limit: [1.88917e-05, 0.000715542, 0.0131491, 0.15971, 1.28724, 7.84264, 32.6354],
    },
    // Dsd256: OSR 282.24, cheby2 stopband 179.1942 dB, sup|NTF| 1.3000, max|pole| 0.9744
    Ntf {
        a: [
            2.2833624889962966e-07,
            8.274950701882852e-06,
            0.00015187851422072602,
            0.0018549886244656445,
            0.015941596676757642,
            0.09805167629329443,
            0.408284023668639,
        ],
        g: [
            2.332468581123237e-05,
            7.573378678871201e-05,
            0.00011776163028764941,
        ],
        state_limit: [1.84597e-05, 0.000691708, 0.0128579, 0.153276, 1.28703, 7.74282, 32.0899],
    },
    // Dsd512: OSR 564.48, cheby2 stopband 221.2353 dB, sup|NTF| 1.3000, max|pole| 0.9744
    Ntf {
        a: [
            2.2827995339406473e-07,
            8.226956093973554e-06,
            0.00015196150358659088,
            0.0018522573918993179,
            0.01595743810532783,
            0.09804678962620964,
            0.40828402366863936,
        ],
        g: [
            5.831106645981521e-06,
            1.8933422328393235e-05,
            2.944060168297291e-05,
        ],
        state_limit: [1.83988e-05, 0.000683847, 0.0130455, 0.158787, 1.3036, 7.38484, 32.2472],
    },
];

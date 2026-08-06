//! DSD (1-bit / Direct Stream Digital) output stage.
//!
//! Unlike every other output path in mint, this one **streams**. The canonical
//! `AudioBuffer` holds the whole file as planar f64, which is the right call at PCM rates
//! but impossible at DSD rates: five minutes of DSD64 stereo would be ~13.5 GB as f64
//! (~54 GB at DSD256). So the DSD stage never materializes a full-rate buffer — it walks
//! the PCM buffer in blocks, and each block is interpolated, modulated, packed and
//! written before the next one is touched.
//!
//! Design and the derivation of the intermediate PCM rate: `docs/dsd-design.md`.

use std::path::Path;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::audio::io_write_dsd::{BLOCK_SAMPLES, DsfSummary, DsfWriter};
use crate::config::ResampleQuality;
use crate::ops::dsd::interpolate::SoxrStream;
use crate::ops::dsd::modulator::{Modulator, ModulatorStats};

pub mod decimate;
pub mod interpolate;
pub mod modulator;
pub mod pack;

/// The four DSD rates mint can write. All are multiples of 44 100 Hz — the reason the
/// intermediate PCM rate is pulled into the 44.1 kHz family before interpolation, so the
/// ratio is an exact power of two (`docs/dsd-design.md` section 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DsdRate {
    Dsd64,
    Dsd128,
    Dsd256,
    Dsd512,
}

impl DsdRate {
    /// Sample rate of the 1-bit stream, in Hz.
    pub const fn hz(self) -> u32 {
        44_100 * self.multiple()
    }

    /// The "64" in DSD64: how many times 44 100 Hz the stream runs at.
    pub const fn multiple(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 64,
            DsdRate::Dsd128 => 128,
            DsdRate::Dsd256 => 256,
            DsdRate::Dsd512 => 512,
        }
    }

    /// Config / dry-run label.
    pub const fn tag(self) -> &'static str {
        match self {
            DsdRate::Dsd64 => "dsd64",
            DsdRate::Dsd128 => "dsd128",
            DsdRate::Dsd256 => "dsd256",
            DsdRate::Dsd512 => "dsd512",
        }
    }

    pub const ALL: [DsdRate; 4] = [
        DsdRate::Dsd64,
        DsdRate::Dsd128,
        DsdRate::Dsd256,
        DsdRate::Dsd512,
    ];
}

/// The 44.1 kHz family: `44 100 * 2^k`. Stops at 705 600 Hz because that is the highest
/// rate any cap below can select (DSD512 / 32).
const FAMILY_44K1: [u32; 5] = [44_100, 88_200, 176_400, 352_800, 705_600];

/// PCM rate the modulator is fed from, derived from the source rate. The default; an
/// explicit `rate` on the target overrides it (and is validated, not derived).
///
/// ```text
/// intermediate = min( nearest 44.1-family rate, dsd_rate / 32 )
/// ```
///
/// Two independent conditions, there for different reasons (`docs/dsd-design.md` § 2.1):
///
/// * **The family** makes the interpolation ratio an exact power of two. 2 822 400 / 176 400
///   is 16, / 88 200 is 32, / 44 100 is 64; out of the 48 kHz family it would be 14.7, which
///   means a fractional SRC immediately in front of the most level-sensitive stage in the
///   chain. For every common source rate the nearest 44.1 rate is also the next one *down*
///   (192 -> 176.4, 96 -> 88.2, 48 -> 44.1), because the 44.1 family sits just under the 48
///   one; the only case that loses real bandwidth is 48 -> 44.1, the same trade mint already
///   makes for every 44.1 kHz deliverable.
///
/// * **The cap** is a low-pass we get for free. Above roughly 30 kHz a DSD64 stream is
///   shaped noise, not signal, and players band-limit around 50 kHz anyway — so program
///   content up there buys nothing, eats stability margin (the loop's stability depends on
///   the *instantaneous* level at the quantizer node, not the audible one) and invites
///   intermodulation back into the audio band. Capping the input rate hands that job to the
///   interpolation filter, which is running regardless. `/32` puts the input Nyquist at
///   `dsd_rate / 64` — 44.1 kHz at DSD64.
pub fn intermediate_pcm_rate(source_rate: u32, rate: DsdRate) -> u32 {
    let nearest = FAMILY_44K1
        .into_iter()
        .min_by_key(|candidate| candidate.abs_diff(source_rate))
        .expect("the family table is never empty");
    nearest.min(rate.hz() / 32)
}

/// Whether `pcm_rate` can feed `rate` — i.e. whether the interpolation ratio is a power of
/// two. The escape hatch for an explicit `rate` validates exactly this and nothing else:
/// keeping ultrasonic content in a low-rate DSD file is a legitimate archival choice, a
/// fractional ratio is not.
pub fn interpolation_factor(pcm_rate: u32, rate: DsdRate) -> Option<u32> {
    let hz = rate.hz();
    if pcm_rate < 2 || !hz.is_multiple_of(pcm_rate) {
        return None;
    }
    let factor = hz / pcm_rate;
    (factor >= 2 && factor.is_power_of_two()).then_some(factor)
}

/// What one encoded file turned out to be — the numbers `ProcessingReport` quotes.
#[derive(Debug, Clone, Copy)]
pub struct DsdEncoding {
    pub rate: DsdRate,
    /// PCM rate the modulator was actually fed from.
    pub pcm_rate: u32,
    /// `dsd_rate / pcm_rate`, always a power of two.
    pub interpolation: u32,
    /// Merged across channels: worst modulation index, summed overloads.
    pub stats: ModulatorStats,
    pub file: DsfSummary,
}

/// PCM in, `.dsf` on disk. The whole DSD output path.
///
/// `pcm` is the finished, *un-quantized* delivery buffer at `pcm_rate` — the modulator is
/// itself the quantizer, so running `ops::bitdepth` in front of this would be a real bug.
/// `pipeline::process` guarantees that structurally by giving DSD its own arm.
///
/// Nothing here is ever resident at DSD rate: one libsoxr instance, one modulator per
/// channel and one writer live for the whole file, and each pass interpolates, modulates,
/// packs and writes roughly one container block before touching the next input frames.
pub fn encode_dsf(
    path: &Path,
    pcm: &[Vec<f64>],
    pcm_rate: u32,
    rate: DsdRate,
    quality: Option<ResampleQuality>,
    seed: Option<u64>,
) -> Result<DsdEncoding> {
    let channels = pcm.len();
    if channels == 0 {
        bail!("DSD output needs at least one channel");
    }
    let frames = pcm[0].len();
    if pcm.iter().any(|c| c.len() != frames) {
        bail!("DSD output got a ragged buffer (channels differ in length)");
    }
    let interpolation = interpolation_factor(pcm_rate, rate).ok_or_else(|| {
        anyhow::anyhow!(
            "{pcm_rate} Hz cannot feed {} ({} Hz): the ratio must be a power of two",
            rate.tag(),
            rate.hz()
        )
    })?;

    let mut src = SoxrStream::new_power_of_two(pcm_rate, rate.hz(), channels, quality)?;
    let mut mods: Vec<Modulator> = (0..channels)
        .map(|c| Modulator::for_channel(rate, seed, c))
        .collect();
    let mut writer = DsfWriter::create(path, channels, rate)?;

    // Clock the loop in DSD samples rather than PCM frames: one pass is one container
    // block per channel, so the writer's accumulator stays near empty whatever the ratio.
    // At x32 that is 1024 input frames, at x512 it is 64.
    let step = (BLOCK_SAMPLES as usize / interpolation as usize).max(1);
    let mut chunk: Vec<Vec<f64>> = vec![Vec::with_capacity(step); channels];
    let mut bits: Vec<Vec<bool>> = vec![Vec::new(); channels];

    let mut pos = 0;
    while pos < frames {
        let n = step.min(frames - pos);
        for (dst, src_channel) in chunk.iter_mut().zip(pcm) {
            dst.clear();
            dst.extend_from_slice(&src_channel[pos..pos + n]);
        }
        let up = src.process(&chunk)?;
        modulate_into(&mut mods, up, &mut bits);
        writer.write(&bits)?;
        pos += n;
    }

    let tail = src.finish()?;
    modulate_into(&mut mods, tail, &mut bits);
    writer.write(&bits)?;

    // Fill the final container block with *modulated* silence instead of a pad pattern:
    // the loop state runs straight through it, so there is no splice discontinuity at all
    // (see `audio::io_write_dsd` on why a spliced 0x00 would be full-scale DC).
    let short = writer.samples_to_block_boundary() as usize;
    if short > 0 {
        let silence = vec![0.0; short];
        for (modulator, out) in mods.iter_mut().zip(bits.iter_mut()) {
            out.clear();
            modulator.modulate_into(&silence, out);
        }
        writer.write(&bits)?;
    }
    let file = writer.finish()?;

    Ok(DsdEncoding {
        rate,
        pcm_rate,
        interpolation,
        stats: merge_stats(&mods),
        file,
    })
}

/// Run one interpolated block through every channel's modulator, reusing `out`.
fn modulate_into(mods: &mut [Modulator], up: &[Vec<f64>], out: &mut [Vec<bool>]) {
    for ((modulator, pcm), bits) in mods.iter_mut().zip(up).zip(out.iter_mut()) {
        bits.clear();
        modulator.modulate_into(pcm, bits);
    }
}

/// Collapse the per-channel stats into one file-level view: worst case for the modulation
/// index, totals for the fault counters. `samples` is per channel, so it is not summed.
fn merge_stats(mods: &[Modulator]) -> ModulatorStats {
    mods.iter()
        .map(Modulator::stats)
        .fold(ModulatorStats::default(), |mut acc, s| {
            acc.max_modulation = acc.max_modulation.max(s.max_modulation);
            acc.overload_samples += s.overload_samples;
            acc.resets += s.resets;
            acc.samples = acc.samples.max(s.samples);
            acc
        })
}

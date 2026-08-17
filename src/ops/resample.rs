use anyhow::{Result, anyhow};
use crate::ops::soxr::{HI_PREC_CLOCK, ROLLOFF_SMALL, Recipe, Soxr};

use crate::audio::buffer::AudioBuffer;
use crate::config::ResampleQuality;
use crate::ops::dsd::interpolate::SoxrStream;
use crate::progress::{JobControl, Stage};

/// Input frames per pass of the conversion loop. Large enough that the per-call FFI
/// crossing disappears into the filtering, small enough that the interleaved scratch
/// either side of it (a few hundred kB) never shows up next to the audio itself.
const BLOCK_FRAMES: usize = 16_384;

/// Resample `buffer` to `target_rate` in place.
///
/// SRC is libsoxr throughout (reference-grade, multi-stage, FFI to the system C lib).
/// Contract: planar f64 in, planar f64 out, output length == round(in_frames * ratio).
pub fn apply(buffer: &mut AudioBuffer, target_rate: u32, quality: Option<ResampleQuality>) -> Result<()> {
    apply_with(buffer, target_rate, quality, JobControl::default())
}

/// [`apply`], reporting progress and honouring cancellation between blocks.
pub fn apply_with(
    buffer: &mut AudioBuffer,
    target_rate: u32,
    quality: Option<ResampleQuality>,
    ctl: JobControl<'_>,
) -> Result<()> {
    if target_rate == buffer.sample_rate {
        return Ok(());
    }

    let channels = buffer.channels_count();
    let in_frames = buffer.frame_len();
    if channels == 0 || in_frames == 0 {
        buffer.sample_rate = target_rate;
        return Ok(());
    }

    apply_soxr(buffer, target_rate, quality, channels, in_frames, ctl)
}

/// libsoxr as a *pure* resampler: f64 in, f64 out. Because the I/O datatype is
/// floating point, libsoxr's internal TPDF dither never engages (it only applies on
/// INT16 output) — quantization + our shaped dither stay the single, final step in
/// `pipeline.rs`, preserving the "canonical f64 buffer, quantize exactly once" invariant.
/// libsoxr also derives the multi-stage decomposition (half-band cascade + polyphase/DFT)
/// from the rate pair on its own, so large ratios like 192k -> 44.1k are staged for us.
///
/// Driven blockwise through [`SoxrStream`] — the same stream the DSD stage feeds its
/// modulator from, which is where the priming discard and the length contract are
/// implemented. It was written because a DSD-rate buffer is impossible; here it earns its
/// keep for a duller reason. The whole-file version interleaved the input, allocated an
/// output at least as large, and then deinterleaved into a third buffer, so a rate
/// conversion cost three extra copies of the audio (1.6 GB on a five-minute 96 kHz stereo
/// master) purely as FFI scratch. Blockwise, that is one block.
///
/// Output is bit-identical to the one-shot path — libsoxr is deterministic and its state
/// does not depend on how the input is chopped up, which the tests below pin across every
/// rate pair mint actually converts between.
fn apply_soxr(
    buffer: &mut AudioBuffer,
    target_rate: u32,
    quality: Option<ResampleQuality>,
    channels: usize,
    in_frames: usize,
    ctl: JobControl<'_>,
) -> Result<()> {
    let ratio = target_rate as f64 / buffer.sample_rate as f64;
    let expected = (in_frames as f64 * ratio).round() as usize;

    let mut stream = SoxrStream::new(buffer.sample_rate, target_rate, channels, quality)?;
    let mut out: Vec<Vec<f64>> = (0..channels)
        .map(|_| Vec::with_capacity(expected))
        .collect();
    let mut chunk: Vec<Vec<f64>> = vec![Vec::with_capacity(BLOCK_FRAMES); channels];

    let mut progress = ctl.stage(Stage::Resample, in_frames as u64);
    let mut pos = 0;
    while pos < in_frames {
        let n = BLOCK_FRAMES.min(in_frames - pos);
        for (dst, source) in chunk.iter_mut().zip(&buffer.channels) {
            dst.clear();
            dst.extend_from_slice(&source[pos..pos + n]);
        }
        for (channel, ready) in out.iter_mut().zip(stream.process(&chunk)?) {
            channel.extend_from_slice(ready);
        }
        pos += n;
        progress.tick(pos as u64)?;
    }
    progress.finish();
    for (channel, tail) in out.iter_mut().zip(stream.finish()?) {
        channel.extend_from_slice(tail);
    }

    // The contract the rest of the pipeline (and `estimated_output_bytes`) relies on.
    // `SoxrStream::finish` enforces it; failing loudly beats shipping a short file.
    if out[0].len() != expected {
        return Err(anyhow!(
            "resampler produced {} frames, expected {expected} ({} -> {target_rate} Hz)",
            out[0].len(),
            buffer.sample_rate
        ));
    }

    buffer.channels = out;
    buffer.sample_rate = target_rate;
    Ok(())
}

/// Construct a libsoxr instance with mint's fixed I/O contract: f64 in, f64 out. The
/// floating-point I/O datatype is deliberate — it keeps libsoxr's internal TPDF dither
/// from ever engaging (it only applies on INT16 output), so quantization stays the single
/// final step in `pipeline.rs`.
///
/// **Every** Soxr instance in the codebase must be built here. libsoxr lazily builds
/// shared FFT/coefficient tables on first use of a given configuration, and doing that
/// from several threads at once races and silently corrupts output; `cli.rs` runs the
/// batch under rayon. The lock is process-wide precisely so the one-shot path and the DSD
/// streaming path (`ops::dsd::interpolate`) serialize against *each other* — two separate
/// locks would not. Construction is cheap next to processing, which stays parallel.
pub(crate) fn create_soxr(
    in_rate: f64,
    out_rate: f64,
    channels: usize,
    quality: Option<ResampleQuality>,
) -> Result<Soxr> {
    use std::sync::Mutex;
    static INIT_LOCK: Mutex<()> = Mutex::new(());

    let (recipe, flags) = soxr_quality(quality);

    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Soxr::create(in_rate, out_rate, channels, recipe, flags)
}

/// Map our quality tiers onto libsoxr recipes. `ROLLOFF_SMALL` keeps passband ripple
/// <= 0.01 dB at every tier (flattest response); the top tier adds `HI_PREC_CLOCK` to
/// maximize ratio accuracy for irrational conversions (e.g. the 44.1 <-> 48 kHz family).
pub(crate) fn soxr_quality(quality: Option<ResampleQuality>) -> (Recipe, std::os::raw::c_ulong) {
    match quality.unwrap_or(ResampleQuality::Hq) {
        ResampleQuality::Lq => (Recipe::Low, ROLLOFF_SMALL),
        ResampleQuality::Mq => (Recipe::Medium, ROLLOFF_SMALL),
        ResampleQuality::Hq => (Recipe::High, ROLLOFF_SMALL),
        ResampleQuality::Vhq => (Recipe::VeryHigh, ROLLOFF_SMALL | HI_PREC_CLOCK),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Which libsoxr engine each quality tier actually selects.
    ///
    /// `docs/gui-proposal.md` §10 recommends exactly this and §11 Q6 turns on it: `vhq` is
    /// *already* not bit-reproducible across platforms (`cr64` on macOS arm64, `cr64s` on
    /// Debian amd64), and vendoring LiveKit's `soxr-sys` would silently downgrade every tier
    /// to the single-precision `cr32` — a change worth −127 dBFS of deviation that no other
    /// test in this repo would notice. It is one FFI call, and it only became possible when
    /// mint started binding the C API itself.
    ///
    /// Measured here (96 -> 44.1 kHz, macOS arm64, system libsoxr 0.1.3):
    ///
    /// | tier | engine |
    /// |---|---|
    /// | `lq`  | `cr32` |
    /// | `mq`  | `cr32` |
    /// | `hq`  | `cr32` |
    /// | `vhq` | `cr64` |
    ///
    /// **Only `vhq` runs in double precision**, which is worth knowing on its own: it is the
    /// tier `README.md`'s reproducibility claim is about, and the other three are single
    /// precision *by design* (`hq` is `SOXR_20_BITQ` — 20 bits does not need an f64 engine).
    /// So the assertion is about the precision class of `vhq` and not the exact name, which
    /// legitimately varies with platform and CPU: `cr64` here, `cr64s` on an AVX x86-64 box.
    /// What must never happen silently is `vhq` dropping to `cr32`. Run with `--nocapture`
    /// for the table.
    #[test]
    fn every_quality_tier_selects_the_engine_it_is_supposed_to() {
        use crate::config::quality_tag;
        for quality in ResampleQuality::ALL {
            let soxr = create_soxr(96_000.0, 44_100.0, 2, Some(quality)).expect("create");
            let engine = soxr.engine();
            println!("  {:<4} -> {engine}", quality_tag(quality));
            assert!(!engine.is_empty(), "libsoxr named no engine for {quality:?}");
            if matches!(quality, ResampleQuality::Vhq) {
                assert!(
                    engine.starts_with("cr64"),
                    "vhq fell back to the {engine} engine. Single precision at the top tier \
                     is a ~127 dBFS change nothing else here would notice — this is what a \
                     vendored soxr built without WITH_CR64 does."
                );
            }
        }
    }

    use crate::audio::buffer::{OutputSampleFormat, SourceInfo};
    use std::path::PathBuf;

    /// The whole-file conversion the blockwise one replaced: interleave everything, run it
    /// through libsoxr in one call, drain the flush tail, then deinterleave the window
    /// `[delay, delay + expected)`. Kept as the definition of "right" for
    /// `blockwise_matches_the_whole_file_path`; it is not shippable because those three
    /// buffers are the whole file each.
    fn whole_file(
        buffer: &mut AudioBuffer,
        target_rate: u32,
        quality: Option<ResampleQuality>,
    ) -> Result<()> {
        let channels = buffer.channels_count();
        let in_frames = buffer.frame_len();
        let ratio = target_rate as f64 / buffer.sample_rate as f64;
        let expected = (in_frames as f64 * ratio).round() as usize;

        let soxr = create_soxr(
            buffer.sample_rate as f64,
            target_rate as f64,
            channels,
            quality,
        )?;

        let mut input = vec![0f64; in_frames * channels];
        for frame in 0..in_frames {
            for (c, ch) in buffer.channels.iter().enumerate() {
                input[frame * channels + c] = ch[frame];
            }
        }

        // Steady-state latency, queried before processing: the first `delay` output
        // frames are filter priming rather than aligned audio.
        let delay = soxr.delay().round() as usize;
        let margin = 16_384;
        let mut output = vec![0f64; (expected + delay + margin) * channels];

        let (_idone, odone) = soxr
            .process(Some(&input), &mut output)
            .map_err(|e| anyhow!("libsoxr process failed: {e}"))?;
        let mut produced = odone;

        loop {
            if produced * channels >= output.len() {
                output.resize(output.len() + margin * channels, 0.0);
            }
            let (_i, odone) = soxr
                .process(None, &mut output[produced * channels..])
                .map_err(|e| anyhow!("libsoxr drain failed: {e}"))?;
            produced += odone;
            if odone == 0 {
                break;
            }
        }

        let mut out_channels = vec![vec![0f64; expected]; channels];
        for frame in 0..expected {
            let src = frame + delay;
            for c in 0..channels {
                out_channels[c][frame] = if src < produced {
                    output[src * channels + c]
                } else {
                    0.0
                };
            }
        }

        buffer.channels = out_channels;
        buffer.sample_rate = target_rate;
        Ok(())
    }

    fn tone(rate: u32, frames: usize, hz: f64, amp: f64) -> Vec<f64> {
        (0..frames)
            .map(|n| (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin() * amp)
            .collect()
    }

    fn buffer_from(rate: u32, channels: Vec<Vec<f64>>) -> AudioBuffer {
        let count = channels.len();
        AudioBuffer {
            channels,
            sample_rate: rate,
            src_info: SourceInfo {
                path: PathBuf::from("t.wav"),
                channels: count,
                sample_rate: rate,
                codec: "test".into(),
            },
            out_format: OutputSampleFormat::F32,
        }
    }

    /// Every rate pair mint converts between, at both quality tiers that differ in clock
    /// precision. `HI_PREC_CLOCK` (vhq) is the case where a drifting fractional position
    /// would show up first, so it is deliberately included rather than assumed.
    #[test]
    fn blockwise_matches_the_whole_file_path() {
        let cases = [
            (96_000u32, 44_100u32),
            (96_000, 48_000),
            (44_100, 48_000),
            (48_000, 44_100),
            (192_000, 44_100),
            (192_000, 96_000),
            (44_100, 96_000),
            (88_200, 44_100),
        ];
        for quality in [ResampleQuality::Hq, ResampleQuality::Vhq] {
            for (from, to) in cases {
                // A length that is not a multiple of the block size, so the tail runs
                // short on the last pass.
                let source = vec![
                    tone(from, 30_001, 997.0, 0.5),
                    tone(from, 30_001, 3_137.0, 0.3),
                ];

                let mut want = buffer_from(from, source.clone());
                whole_file(&mut want, to, Some(quality)).expect("reference resample");

                let mut got = buffer_from(from, source);
                apply(&mut got, to, Some(quality)).expect("blockwise resample");

                assert_eq!(got.sample_rate, to);
                for (c, (a, b)) in got.channels.iter().zip(&want.channels).enumerate() {
                    assert_eq!(a.len(), b.len(), "{from}->{to} {quality:?} ch{c} length");
                    for (i, (&x, &y)) in a.iter().zip(b).enumerate() {
                        assert_eq!(
                            x.to_bits(),
                            y.to_bits(),
                            "{from}->{to} {quality:?} ch{c}[{i}]: {x} != {y}"
                        );
                    }
                }
            }
        }
    }
}

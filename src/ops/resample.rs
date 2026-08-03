use anyhow::{Result, anyhow};
use libsoxr::{Datatype, IOSpec, QualityFlags, QualityRecipe, QualitySpec, Soxr};

use crate::audio::buffer::AudioBuffer;
use crate::config::ResampleQuality;

/// Resample `buffer` to `target_rate` in place.
///
/// SRC is libsoxr throughout (reference-grade, multi-stage, FFI to the system C lib).
/// Contract: planar f64 in, planar f64 out, output length == round(in_frames * ratio).
pub fn apply(buffer: &mut AudioBuffer, target_rate: u32, quality: Option<ResampleQuality>) -> Result<()> {
    if target_rate == buffer.sample_rate {
        return Ok(());
    }

    let channels = buffer.channels_count();
    let in_frames = buffer.frame_len();
    if channels == 0 || in_frames == 0 {
        buffer.sample_rate = target_rate;
        return Ok(());
    }

    apply_soxr(buffer, target_rate, quality, channels, in_frames)
}

/// libsoxr as a *pure* resampler: f64 in, f64 out. Because the I/O datatype is
/// floating point, libsoxr's internal TPDF dither never engages (it only applies on
/// INT16 output) — quantization + our shaped dither stay the single, final step in
/// `pipeline.rs`, preserving the "canonical f64 buffer, quantize exactly once" invariant.
/// libsoxr also derives the multi-stage decomposition (half-band cascade + polyphase/DFT)
/// from the rate pair on its own, so large ratios like 192k -> 44.1k are staged for us.
fn apply_soxr(
    buffer: &mut AudioBuffer,
    target_rate: u32,
    quality: Option<ResampleQuality>,
    channels: usize,
    in_frames: usize,
) -> Result<()> {
    let ratio = target_rate as f64 / buffer.sample_rate as f64;
    let expected = (in_frames as f64 * ratio).round() as usize;

    let soxr = create_soxr(
        buffer.sample_rate as f64,
        target_rate as f64,
        channels,
        quality,
    )?;

    // libsoxr's split-channel buffer path in the wrapper is fragile; interleave instead.
    let mut input = vec![0f64; in_frames * channels];
    for frame in 0..in_frames {
        for (c, ch) in buffer.channels.iter().enumerate() {
            input[frame * channels + c] = ch[frame];
        }
    }

    // libsoxr introduces a linear-phase group delay; its first `delay()` output frames
    // are the filter priming, not aligned audio. We drain everything, then drop those
    // leading frames so the result is sample-aligned with the source. Query delay before
    // processing — it is the steady-state latency.
    let delay = soxr.delay().round() as usize;

    // Hold `expected + delay` aligned frames plus headroom for the flush tail.
    let margin = 16_384;
    let mut output = vec![0f64; (expected + delay + margin) * channels];

    let (_idone, odone) = soxr
        .process(Some(&input), &mut output)
        .map_err(|e| anyhow!("libsoxr process failed: {e}"))?;
    let mut produced = odone;

    // Drain the internal buffer (NULL input) until it stops producing the flush tail.
    loop {
        if produced * channels >= output.len() {
            output.resize(output.len() + margin * channels, 0.0);
        }
        let (_i, odone) = soxr
            .process::<f64, f64>(None, &mut output[produced * channels..])
            .map_err(|e| anyhow!("libsoxr drain failed: {e}"))?;
        produced += odone;
        if odone == 0 {
            break;
        }
    }

    // Deinterleave the aligned window [delay, delay + expected) into planar channels,
    // padding with silence if libsoxr produced fewer frames than the theoretical length.
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
    let io = IOSpec::new(Datatype::Float64I, Datatype::Float64I);
    let q = QualitySpec::new(&recipe, flags);

    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Soxr::create(in_rate, out_rate, channels as u32, Some(&io), Some(&q), None)
        .map_err(|e| anyhow!("failed to create libsoxr resampler: {e}"))
}

/// Map our quality tiers onto libsoxr recipes. `ROLLOFF_SMALL` keeps passband ripple
/// <= 0.01 dB at every tier (flattest response); the top tier adds `HI_PREC_CLOCK` to
/// maximize ratio accuracy for irrational conversions (e.g. the 44.1 <-> 48 kHz family).
fn soxr_quality(quality: Option<ResampleQuality>) -> (QualityRecipe, QualityFlags) {
    match quality.unwrap_or(ResampleQuality::Hq) {
        ResampleQuality::Lq => (QualityRecipe::Low, QualityFlags::ROLLOFF_SMALL),
        ResampleQuality::Mq => (QualityRecipe::Medium, QualityFlags::ROLLOFF_SMALL),
        ResampleQuality::Hq => (QualityRecipe::High, QualityFlags::ROLLOFF_SMALL),
        ResampleQuality::Vhq => (
            QualityRecipe::VeryHigh,
            QualityFlags::ROLLOFF_SMALL | QualityFlags::HI_PREC_CLOCK,
        ),
    }
}

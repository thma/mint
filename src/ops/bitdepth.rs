use anyhow::{Result, bail};

use crate::audio::buffer::{AudioBuffer, OutputSampleFormat};
use crate::config::DitherMode;
use crate::dither::{NoiseShaper, ShapingCurve, TpdfDither};

#[derive(Debug, Clone, Copy)]
pub struct BitDepthApplyResult {
    pub reduced: bool,
    pub dithered: bool,
    /// True when error-feedback noise shaping ran (a strict subset of `dithered`).
    pub shaped: bool,
}

pub fn apply(
    buffer: &mut AudioBuffer,
    current: &mut OutputSampleFormat,
    target: OutputSampleFormat,
    dither_mode: Option<DitherMode>,
    seed: Option<u64>,
) -> Result<BitDepthApplyResult> {
    let mode = dither_mode.unwrap_or(DitherMode::Tpdf);
    let reduced = is_reduction(*current, target);

    if reduced && matches!(mode, DitherMode::None) {
        bail!("dither='none' is not allowed when reducing bit depth");
    }

    // `reduced` already implies an integer target (f32 is the top rank, so nothing
    // reduces *to* it), so no separate is-integer check is needed. Every mode but
    // `None` dithers, and `None` was already rejected above on a reduction.
    let dithered = reduced && !matches!(mode, DitherMode::None);
    // Shaping only helps at 16-bit; at s24 the noise floor is already inaudible, so
    // the shaped modes gracefully degrade to flat TPDF here (and to nothing for f32).
    // The gate (`DitherMode::shapes`) is shared with the dry-run summary so they can't
    // drift; `shaping_curve` then selects which curve to run.
    let shaped = dithered && mode.shapes(target);
    let curve = shaped.then(|| mode.shaping_curve()).flatten();

    if reduced {
        quantize_in_place(buffer, target, dithered, curve, seed);
    }

    *current = target;
    buffer.out_format = target;

    Ok(BitDepthApplyResult { reduced, dithered, shaped })
}

fn is_reduction(current: OutputSampleFormat, target: OutputSampleFormat) -> bool {
    bit_depth_rank(target) < bit_depth_rank(current)
}

fn bit_depth_rank(format: OutputSampleFormat) -> u8 {
    match format {
        OutputSampleFormat::F32 => 3,
        OutputSampleFormat::S24 => 2,
        OutputSampleFormat::S16 => 1,
    }
}

fn quantize_in_place(
    buffer: &mut AudioBuffer,
    target: OutputSampleFormat,
    use_dither: bool,
    curve: Option<ShapingCurve>,
    seed: Option<u64>,
) {
    // Only ever called on a reduction, so the target is always an integer format
    // (grid is Some). Resolve the grid once — it's loop-invariant.
    let Some((max, min_i, max_i)) = target.int_grid() else {
        return;
    };

    if let Some(curve) = curve {
        // One shaper per channel: the error-feedback history must stay per-channel,
        // unlike the flat path which can share a single dither stream.
        for (ch, channel) in buffer.channels.iter_mut().enumerate() {
            let mut shaper = NoiseShaper::new(curve, per_channel_seed(seed, ch));
            for sample in channel {
                *sample = shaper.quantize(*sample, max, min_i, max_i);
            }
        }
        return;
    }

    let mut dither = use_dither.then(|| TpdfDither::new(seed));
    for channel in &mut buffer.channels {
        for sample in channel {
            *sample = quantize_to_grid(*sample, max, min_i, max_i, dither.as_mut());
        }
    }
}

/// Deterministic distinct sub-seed per channel, so each channel's shaper gets its
/// own error history and dither stream. StdRng (ChaCha) diffuses adjacent seeds
/// well, so the multiplicative mix is just to keep channels decorrelated.
fn per_channel_seed(seed: Option<u64>, channel: usize) -> Option<u64> {
    seed.map(|s| s ^ (channel as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

fn quantize_to_grid(
    sample: f64,
    max: f64,
    min_i: i32,
    max_i: i32,
    dither: Option<&mut TpdfDither>,
) -> f64 {
    let dither_term = dither.map_or(0.0, |rng| rng.sample_lsb() / max);
    let scaled = ((sample.clamp(-1.0, 1.0) + dither_term) * max).round() as i32;
    let clamped = scaled.clamp(min_i, max_i);
    clamped as f64 / max
}

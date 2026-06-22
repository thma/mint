use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::audio::buffer::OutputSampleFormat;
use crate::audio::io_read;
use crate::audio::io_write;
use crate::config::{DitherMode, OutputCodec, ResolvedTarget, format_tag};
use crate::ops::loudness::LoudnessAnalysis;
use crate::ops::{bitdepth, limiter, loudness, resample};

#[derive(Debug, Clone)]
pub struct ProcessingReport {
    /// Name of the target that produced this output.
    pub target: String,
    pub input: String,
    pub output: String,
    pub in_rate: u32,
    pub out_rate: u32,
    /// Full loudness/dynamics metering of the untouched source.
    pub source: LoudnessAnalysis,
    /// Full loudness/dynamics metering of the delivered signal (measured before
    /// quantization). The authoritative QC numbers for what was shipped.
    pub delivered: LoudnessAnalysis,
    /// Peak gain reduction applied by the loudness limiter (None when no loudness
    /// step ran or no limiting was needed). Surfaces dynamics lost to hit the target.
    pub limiter_gain_reduction_db: Option<f64>,
    /// True when TPDF dither was applied during quantization.
    pub dithered: bool,
    /// True when error-feedback noise shaping ran (implies `dithered`).
    pub shaped: bool,
    /// Name of the shaping family when shaping ran ("gentle" |
    /// "psychoacoustic" | "mbit+"); `None` otherwise. Mirrors the dry-run label.
    pub shaping_curve: Option<&'static str>,
    /// Output container/codec: "wav" | "flac" | "mp3".
    pub codec: String,
    /// True when a Broadcast WAV `bext` chunk was written.
    pub bwf: bool,
    /// Sample-format tag for PCM codecs (s16/s24/f32), or "<N> kbps" for MP3.
    pub format: String,
    pub warnings: Vec<String>,
}

/// A task that failed to process. Kept structured (not a pre-formatted string) so both
/// the console summary and the `--json` report can render it.
#[derive(Debug, Clone)]
pub struct TaskFailure {
    pub target: String,
    pub input: String,
    pub error: String,
}

/// Process a single input file for a single resolved target.
///
/// The DSP chain is *derived* from the target and the source format rather than
/// listed in config — only the steps that are actually needed run, in the one
/// canonical order:
///   loudness (if `lufs`) -> resample (if rate differs) -> ceiling enforcement
///   -> quantize once -> write.
pub fn process(input: &Path, target: &ResolvedTarget, seed: Option<u64>) -> Result<ProcessingReport> {
    let mut buffer = io_read::read_audio(input)?;
    let source_rate = buffer.sample_rate;
    let ceiling = target.ceiling_dbtp;
    let limiter_options = limiter::LimiterOptions {
        character: target.limiter_character,
        soft_clip: target.limiter_soft_clip,
        link_channels: target.limiter_link_channels,
    };
    let mut warnings = Vec::new();
    let mut limiter_gain_reduction = None;

    // Full source metering, measured once on the untouched input so the report shows
    // loudness/dynamics uniformly whether or not a loudness step runs.
    let source = loudness::analyze(&buffer)?;

    // 1. Loudness normalization — only when the target specifies a LUFS goal.
    //    The loudness step applies a constant gain and (for on_clip=limit) runs
    //    the limiter, so it also enforces the ceiling on its own output.
    let loudness_ran = if let Some(lufs) = target.lufs {
        let result =
            loudness::apply(&mut buffer, lufs, Some(ceiling), Some(target.on_clip), limiter_options)?;
        warnings.extend(result.warnings);

        // Heavy-limiting check: hitting the LUFS target required squashing peaks.
        // Both delivered loudness and true peak look clean afterwards, so this
        // gain-reduction number is the only place the lost dynamics become visible.
        if result.limiter_gain_reduction_db > 0.0 {
            limiter_gain_reduction = Some(result.limiter_gain_reduction_db);
            if result.limiter_gain_reduction_db > target.warn_limiting_db {
                warnings.push(format!(
                    "heavy limiting: {:.1} dB of peak gain reduction to reach {lufs:.1} LUFS \
                     under the {ceiling:.1} dBTP ceiling (source true peak {:.1} dBTP); \
                     consider a lower target or more source headroom",
                    result.limiter_gain_reduction_db, source.true_peak_dbtp
                ));
            }
        }

        if (result.out_lufs - lufs).abs() > 0.1 {
            warnings.push(format!(
                "loudness target not met within +/-0.1 LU (target {lufs:.2}, actual {:.2})",
                result.out_lufs
            ));
        }
        true
    } else {
        false
    };

    // 2. Resample — only when the delivery rate differs from the source rate.
    let out_rate = target.rate.unwrap_or(source_rate);
    if out_rate != source_rate {
        resample::apply(&mut buffer, out_rate, Some(target.quality))?;
        // SRC can raise inter-sample peaks above the previously enforced ceiling.
        warnings.extend(loudness::recheck_and_limit_if_needed(
            &mut buffer,
            ceiling,
            limiter_options,
        )?);
    }

    // 3. Ceiling enforcement for the pure-transcode path (no loudness step ran).
    //    When a loudness step ran, its policy + the post-resample re-check already
    //    hold the ceiling, so this is skipped to avoid duplicate handling.
    if !loudness_ran {
        warnings.extend(loudness::enforce_ceiling(
            &mut buffer,
            ceiling,
            target.on_clip,
            limiter_options,
        )?);
    }

    // Delivered metering for the report, measured before any quantization/encode.
    let delivered = loudness::analyze(&buffer)?;

    // Resolve + guard the output path.
    let output_path = target.render_output_path(input, out_rate)?;
    if !target.overwrite && output_path == input {
        bail!(
            "refusing to overwrite input file without --force or overwrite=true: {}",
            input.display()
        );
    }
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory: {}", parent.display()))?;
    }

    // 4/5. Quantize (once, if the codec is PCM-based) and encode/write. MP3 is lossy:
    //      libmp3lame quantizes internally, so the f64 buffer is fed straight in and the
    //      bit-depth/dither stage is skipped.
    let mut dithered = false;
    let mut shaped = false;
    let mut shaping_curve = None;
    let format_tag_str;

    match target.codec {
        OutputCodec::Mp3 => {
            io_write::write_mp3(&output_path, &buffer, target.mp3_bitrate)?;
            format_tag_str = format!("{} kbps", target.mp3_bitrate);
        }
        codec => {
            // WAV and FLAC quantize the full-precision buffer exactly once, with dither.
            let mut current = OutputSampleFormat::F32;
            let in_format = current;
            let bd = bitdepth::apply(
                &mut buffer,
                &mut current,
                target.format,
                Some(target.dither),
                Some(target.dither_strength),
                Some(target.dither_correlation),
                seed,
            )?;
            dithered = bd.dithered;
            shaped = bd.shaped;
            shaping_curve = bd.shaping_tag;

            if bd.reduced
                && matches!(target.dither, DitherMode::None)
                && matches!(target.format, OutputSampleFormat::S16)
            {
                warnings.push(format!(
                    "undithered bit-depth reduction {} -> s16 requested (dither=none); this can introduce correlated quantization distortion on low-level material",
                    format_tag(in_format)
                ));
            }

            format_tag_str = format_tag(target.format).to_string();

            match codec {
                OutputCodec::Flac => {
                    io_write::write_flac(
                        &output_path,
                        &buffer,
                        target.format.bits_per_sample() as usize,
                    )?;
                }
                // WAV (with an optional EBU R128 bext chunk built from the delivered metering).
                _ => {
                    let bext = target.bwf.then(|| io_write::BextMetadata {
                        description: "EBU R128 master (mint)".to_string(),
                        integrated_lufs: delivered.integrated_lufs,
                        loudness_range_lu: delivered.loudness_range_lu,
                        max_true_peak_dbtp: delivered.true_peak_dbtp,
                        max_momentary_lufs: delivered.max_momentary_lufs,
                        max_short_term_lufs: delivered.max_short_term_lufs,
                        sample_rate: buffer.sample_rate,
                        channels: buffer.channels_count(),
                        bits: target.format.bits_per_sample(),
                    });
                    io_write::write_wav(&output_path, &buffer, bext.as_ref())?;
                }
            }
        }
    }

    Ok(ProcessingReport {
        target: target.name.clone(),
        input: input.display().to_string(),
        output: output_path.display().to_string(),
        in_rate: source_rate,
        out_rate,
        source,
        delivered,
        limiter_gain_reduction_db: limiter_gain_reduction,
        dithered,
        shaped,
        shaping_curve,
        codec: target.codec.ext().to_string(),
        bwf: target.bwf && target.codec == OutputCodec::Wav,
        format: format_tag_str,
        warnings,
    })
}

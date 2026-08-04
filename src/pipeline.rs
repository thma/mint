use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::audio::buffer::OutputSampleFormat;
use crate::audio::io_read;
use crate::audio::io_write;
use crate::config::{DitherMode, OnClipPolicy, OutputCodec, ResolvedTarget, format_tag};
use crate::ops::dsd::modulator::MAX_MODULATION;
use crate::ops::loudness::LoudnessAnalysis;
use crate::ops::{bitdepth, dsd, limiter, loudness, resample};

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
    /// Mean gain reduction (dB) over the limited portion (None when no limiting ran).
    /// Together with `limiter_active_percent` it distinguishes one shaved needle from
    /// wall-to-wall limiting — peak GR alone can't.
    pub limiter_avg_gain_reduction_db: Option<f64>,
    /// Percent (0–100) of the signal's samples the limiter attenuated (None when no
    /// limiting ran).
    pub limiter_active_percent: Option<f64>,
    /// True when TPDF dither was applied during quantization.
    pub dithered: bool,
    /// True when error-feedback noise shaping ran (implies `dithered`).
    pub shaped: bool,
    /// Name of the shaping family when shaping ran ("gentle" |
    /// "psychoacoustic" | "adaptive"); `None` otherwise. Mirrors the dry-run label.
    pub shaping_curve: Option<&'static str>,
    /// Output container/codec: "wav" | "flac" | "mp3" | "dsf".
    pub codec: String,
    /// True when a Broadcast WAV `bext` chunk was written.
    pub bwf: bool,
    /// Sample-format tag for PCM codecs (s16/s24/f32), "<N> kbps" for MP3, "1-bit" for DSD.
    pub format: String,
    /// DSD-specific figures; `None` for every PCM and MP3 target.
    pub dsd: Option<DsdReport>,
    pub warnings: Vec<String>,
}

/// What the DSD stage did, for the console summary and `--json`.
///
/// Two of these are QC numbers rather than trivia. `max_modulation_percent` is the level in
/// the units the format actually uses (50 % is the SACD 0 dB reference, 71.43 % its
/// maximum), and `overload_samples` counts the samples on which the sigma-delta loop hit a
/// state clamp — nonzero means the loop was driven past what a 1-bit quantizer can track.
#[derive(Debug, Clone)]
pub struct DsdReport {
    /// "dsd64" | "dsd128" | "dsd256" | "dsd512".
    pub rate: &'static str,
    pub rate_hz: u32,
    /// PCM rate the modulator was fed from (derived unless the target set `rate`).
    pub intermediate_pcm_rate: u32,
    /// `rate_hz / intermediate_pcm_rate`; always a power of two.
    pub interpolation_factor: u32,
    pub modulator_order: usize,
    /// Peak modulation index in percent.
    pub max_modulation_percent: f64,
    pub overload_samples: u64,
    /// Times the loop stayed clamped long enough to be declared diverged and reset.
    pub resets: u64,
    /// `sampleCount` as written — includes the silence the last block was topped up with.
    pub samples_per_channel: u64,
    pub file_bytes: u64,
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
    let mut limiter_avg_gain_reduction = None;
    let mut limiter_active_percent = None;

    // Full source metering, measured once on the untouched input so the report shows
    // loudness/dynamics uniformly whether or not a loudness step runs.
    let source = loudness::analyze(&buffer)?;

    // 1. Loudness normalization — only when the target specifies a LUFS goal.
    //    The loudness step applies a constant gain and (for on_clip=limit) runs
    //    the limiter, so it also enforces the ceiling on its own output.
    let loudness_ran = if let Some(lufs) = target.lufs {
        let result = loudness::apply(
            &mut buffer,
            lufs,
            Some(ceiling),
            Some(target.on_clip),
            limiter_options,
            target.max_limiting_db,
        )?;
        warnings.extend(result.warnings);

        // Heavy-limiting check: hitting the LUFS target required squashing peaks.
        // Both delivered loudness and true peak look clean afterwards, so this
        // gain-reduction number is the only place the lost dynamics become visible.
        // Maximize is exempt — limiting is the point there, and its own max_limiting_db
        // cap (warned about in loudness::apply) is the guard instead.
        if result.limiter_gain_reduction_db > 0.0 {
            limiter_gain_reduction = Some(result.limiter_gain_reduction_db);
            limiter_avg_gain_reduction = Some(result.limiter_avg_gain_reduction_db);
            limiter_active_percent = Some(result.limiter_active_ratio * 100.0);
            if result.limiter_gain_reduction_db > target.warn_limiting_db
                && !matches!(target.on_clip, OnClipPolicy::Maximize)
            {
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

    // 2. Resample — only when the PCM stage's rate differs from the source rate. For a DSD
    //    target that rate is the *intermediate* one the modulator will be interpolated up
    //    from, derived from the source unless the target set `rate` explicitly.
    let pcm_rate = target.pcm_stage_rate(source_rate);
    let out_rate = target.delivery_rate(source_rate);
    if pcm_rate != source_rate {
        resample::apply(&mut buffer, pcm_rate, Some(target.quality))?;
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
        // Makeup-gain convergence needs a LUFS goal; without one, maximize is just limit.
        if matches!(target.on_clip, OnClipPolicy::Maximize) {
            warnings.push(
                "on_clip=maximize has no effect without lufs; behaving as limit".to_string(),
            );
        }
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

    // 4/5. Quantize (once, if the codec is PCM-based) and encode/write. Two codecs skip the
    //      bit-depth/dither stage because they carry their own quantizer: MP3 (libmp3lame
    //      quantizes internally) and DSD (the sigma-delta loop *is* the quantizer, and must
    //      see the un-quantized buffer — `bitdepth::apply` in front of it would be a real
    //      bug, which is why it lives in the WAV/FLAC arm only).
    //
    //      Matched exhaustively, deliberately: with a catch-all arm a new `OutputCodec`
    //      variant compiles fine and quietly gets quantized and written as a WAV.
    let mut dithered = false;
    let mut shaped = false;
    let mut shaping_curve = None;
    let mut dsd_report = None;
    let format_tag_str;

    match target.codec {
        OutputCodec::Mp3 => {
            io_write::write_mp3(&output_path, &buffer, target.mp3_bitrate)?;
            format_tag_str = format!("{} kbps", target.mp3_bitrate);
        }
        OutputCodec::Dsf => {
            let dsd_rate = target
                .dsd
                .context("DSD codec resolved without a dsd rate (config resolution bug)")?;
            let encoding = dsd::encode_dsf(
                &output_path,
                &buffer.channels,
                pcm_rate,
                dsd_rate,
                Some(target.quality),
                seed,
            )?;
            let stats = encoding.stats;

            // Only reachable from PCM hotter than 0 dBFS true peak, which the ceiling stage
            // exists to prevent — so if it fires, the ceiling stage was bypassed or the
            // ceiling itself is set above 0.
            if stats.exceeds_format_maximum() {
                warnings.push(format!(
                    "peak modulation {:.1} % exceeds the format maximum of {:.2} % \
                     (the 20/28 rule); lower ceiling_dbtp",
                    stats.max_modulation_percent(),
                    MAX_MODULATION * 100.0
                ));
            }
            if stats.overload_samples > 0 {
                warnings.push(format!(
                    "sigma-delta overload on {} of {} samples ({} loop reset(s)); the \
                     modulator clipped rather than diverged, but the tail of the level \
                     distribution is past what a 1-bit loop can track",
                    stats.overload_samples, stats.samples, stats.resets
                ));
            }

            format_tag_str = "1-bit".to_string();
            dsd_report = Some(DsdReport {
                rate: dsd_rate.tag(),
                rate_hz: dsd_rate.hz(),
                intermediate_pcm_rate: encoding.pcm_rate,
                interpolation_factor: encoding.interpolation,
                modulator_order: dsd::modulator::ORDER,
                max_modulation_percent: stats.max_modulation_percent(),
                overload_samples: stats.overload_samples,
                resets: stats.resets,
                samples_per_channel: encoding.file.samples_per_channel,
                file_bytes: encoding.file.file_bytes,
            });
        }
        OutputCodec::Wav | OutputCodec::Flac => {
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

            if target.codec == OutputCodec::Flac {
                io_write::write_flac(
                    &output_path,
                    &buffer,
                    target.format.bits_per_sample() as usize,
                )?;
            } else {
                // WAV, with an optional EBU R128 bext chunk built from the delivered metering.
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

    Ok(ProcessingReport {
        target: target.name.clone(),
        input: input.display().to_string(),
        output: output_path.display().to_string(),
        in_rate: source_rate,
        out_rate,
        source,
        delivered,
        limiter_gain_reduction_db: limiter_gain_reduction,
        limiter_avg_gain_reduction_db: limiter_avg_gain_reduction,
        limiter_active_percent,
        dithered,
        shaped,
        shaping_curve,
        codec: target.codec.ext().to_string(),
        bwf: target.bwf && target.codec == OutputCodec::Wav,
        format: format_tag_str,
        dsd: dsd_report,
        warnings,
    })
}

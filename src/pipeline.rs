use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::audio::buffer::OutputSampleFormat;
use crate::audio::io_read;
use crate::audio::io_write;
use crate::config::{DitherMode, OnClipPolicy, OutputCodec, ResolvedTarget, format_tag};
use crate::ops::dsd::modulator::MAX_MODULATION;
use crate::ops::loudness::{LUFS_TOLERANCE, LoudnessAnalysis};
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

/// Refuse to write over something the user did not agree to lose.
///
/// The old check compared `output_path == input` as raw `PathBuf`s, which asked the wrong
/// question twice: it missed `./master.wav` against `master.wav` (or a symlink, or a
/// different-case spelling on a case-insensitive volume), and it said nothing at all about a
/// *pre-existing output* — so `overwrite = false`, the documented default, protected only
/// the input and silently clobbered the previous render.
///
/// Both are one question now, asked in the right order and only when it can be answered:
/// nothing needs comparing unless something is already at the output path, and if something
/// is, [`same_file`] can compare the two as the files they actually name rather than as the
/// strings they were spelled with.
fn guard_output_path(input: &Path, output_path: &Path, overwrite: bool) -> Result<()> {
    // `symlink_metadata` rather than `exists()`: a dangling symlink at the output path is
    // something the rename will replace, so it counts as occupied.
    if std::fs::symlink_metadata(output_path).is_err() {
        return Ok(());
    }
    if overwrite {
        return Ok(());
    }
    if same_file(output_path, input) {
        bail!(
            "refusing to overwrite the input file without --force or overwrite=true: {}",
            input.display()
        );
    }
    bail!(
        "output file already exists: {} (pass --force, or set overwrite = true on the target, \
         to replace it)",
        output_path.display()
    )
}

/// Whether two existing paths name the same file on disk.
///
/// Both are known to exist by the time this is asked, which is what makes it answerable:
/// the identity of a file that is not there yet has no meaning.
///
/// On Unix this is `(device, inode)`, which is the only comparison that catches a *hard
/// link* — two directory entries for one file, indistinguishable to `canonicalize` because
/// both are already canonical. Elsewhere `canonicalize` is the best available answer: it
/// resolves `.`, `..` and symlinks and applies the volume's own case rules, and misses only
/// the hard-link case.
#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Process a single input file for a single resolved target.
///
/// The DSP chain is *derived* from the target and the source format rather than
/// listed in config — only the steps that are actually needed run, in the one
/// canonical order:
///   loudness (if `lufs`) -> resample (if rate differs) -> re-verify ceiling and loudness
///   (only after a resample) -> ceiling enforcement (only if no loudness step ran)
///   -> quantize once -> write.
///
/// Ahead of all of it: everything that can make the job impossible is settled from the input
/// header, so a doomed target costs a header read rather than the whole chain.
pub fn process(input: &Path, target: &ResolvedTarget, seed: Option<u64>) -> Result<ProcessingReport> {
    // Everything that can be known to be impossible is decided from the *header*, before the
    // file is even decoded. All of it used to surface from inside a writer instead — a
    // 192 kHz FLAC target or a 7.1 DSD one failed after the whole chain had run, and
    // `overwrite = false` discovered the pre-existing output only once it had been replaced.
    // A doomed job now costs one header read, which matters in a batch: a hundred of them
    // used to cost a hundred decodes to say the same thing a hundred times.
    let probe = io_read::probe_source(input)?;
    let source_rate = probe.sample_rate;
    let out_rate = target.delivery_rate(source_rate);
    if let Some(reason) = target.delivery_blocker(source_rate, probe.channels) {
        bail!("target '{}': {reason}", target.name);
    }
    let output_path = target.render_output_path(input, out_rate)?;
    // Before `create_dir_all`, so a refused job leaves no empty directories behind. The
    // guard does not need the parent to exist: a path whose parent is missing has nothing
    // sitting at it, which is the only case it cares about.
    guard_output_path(input, &output_path, target.overwrite)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory: {}", parent.display()))?;
    }

    let mut buffer = io_read::read_audio(input)?;
    // The two read the same track parameters out of the same container, so this cannot
    // differ — but the output path was already rendered from the header's answer, and a
    // silent disagreement would put the wrong `{rate}` in the file name.
    debug_assert_eq!(buffer.sample_rate, source_rate, "probe and decode disagree on the rate");
    // Asked again with the decoded channel count: `probe.channels` is `None` for a container
    // that does not declare one, and the check above cannot fire on what it cannot see.
    if let Some(reason) = target.delivery_blocker(source_rate, Some(buffer.channels_count())) {
        bail!("target '{}': {reason}", target.name);
    }

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
    // Loudness as the normalization stage left it. Only the post-resample report reads it,
    // and only to separate the loudness the *resampler* removed from the loudness the
    // limiter had already eaten before it — two different stories about one shortfall.
    let mut pre_src_lufs = None;

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
        pre_src_lufs = Some(result.out_lufs);

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

        // No "target not met" check here: at this point the resampler has not run yet.
        // It is made once, against the delivered measurement, further down.
        true
    } else {
        false
    };

    // 2. Resample — only when the PCM stage's rate differs from the source rate. For a DSD
    //    target that rate is the *intermediate* one the modulator will be interpolated up
    //    from, derived from the source unless the target set `rate` explicitly.
    let pcm_rate = target.pcm_stage_rate(source_rate);
    if pcm_rate != source_rate {
        resample::apply(&mut buffer, pcm_rate, Some(target.quality))?;
        // SRC can raise inter-sample peaks above the previously enforced ceiling.
        let recheck = loudness::recheck_and_limit_if_needed(&mut buffer, ceiling, limiter_options)?;
        warnings.extend(recheck.warnings);

        // ...and it moves the integrated loudness too, which the gain computed at the source
        // rate could not have known about. Corrected here rather than merely reported,
        // because what a deliverable is judged on is the loudness of the file that ships.
        // Every `on_clip` policy, for the same reason the ceiling re-check above ignores
        // `on_clip`: the correction cannot exceed the headroom, so no policy's promise about
        // peaks is broken by it.
        if let Some(lufs) = target.lufs {
            warnings.extend(loudness::correct_loudness_after_resample(
                &mut buffer,
                lufs,
                ceiling,
                recheck.true_peak_dbtp,
                pre_src_lufs.unwrap_or(f64::NAN),
            )?);
        }
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

    // The loudness verdict is read off `delivered`, not off the loudness step's own output.
    // Those are the same number only when nothing followed it; a resample changes the
    // integrated loudness (band-limiting a 96 kHz master to 44.1 kHz drops the ultrasonic
    // energy BS.1770's K-weighting was counting), and the post-SRC ceiling pass can change
    // it again. Checking the pre-SRC figure hid a measured -2.54 LU delivery error behind a
    // clean report.
    // `is_finite` keeps the digitally-silent case out of this branch explicitly rather than
    // by accident: a silent source normalizes with an infinite gain, so the buffer is NaN
    // and every comparison against it is already false. Being explicit means the reporting
    // of that case (report.md defect 11) can be changed without tripping over this line.
    if let Some(lufs) = target.lufs
        && delivered.integrated_lufs.is_finite()
        && (delivered.integrated_lufs - lufs).abs() > LUFS_TOLERANCE
    {
        warnings.push(format!(
            "loudness target not met within +/-{LUFS_TOLERANCE:.1} LU \
             (target {lufs:.2}, delivered {:.2})",
            delivered.integrated_lufs
        ));
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

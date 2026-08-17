use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::audio::buffer::OutputSampleFormat;
use crate::audio::io_read;
use crate::audio::io_write;
use crate::config::{
    DitherMode, OnClipPolicy, OutputCodec, ResolvedTarget, dither_strength_tag, dither_tag,
    format_tag, quality_tag,
};
use crate::ops::dsd::modulator::MAX_MODULATION;
use crate::ops::loudness::{LUFS_TOLERANCE, LoudnessAnalysis};
use crate::ops::{bitdepth, dsd, limiter, loudness, resample};
use crate::progress::{JobControl, Stage};

#[derive(Debug, Clone)]
pub struct ProcessingReport {
    /// Name of the target that produced this output.
    pub target: String,
    pub input: String,
    pub output: String,
    pub in_rate: u32,
    pub out_rate: u32,
    /// Channel count of the delivered file.
    pub channels: usize,
    /// Frames per channel as delivered, at `out_rate`. For DSD this is `sampleCount`
    /// (1-bit samples, padding included); for MP3 it is what was handed to the encoder,
    /// which adds its own padding on top.
    pub frames: u64,
    /// Size of the file that was written, read back from the filesystem rather than
    /// predicted — this is the number `estimated_output_bytes` was guessing at.
    pub file_bytes: u64,
    /// Wall-clock time for this one (input × target) job.
    pub elapsed_ms: u64,
    /// Full loudness/dynamics metering of the untouched source.
    pub source: LoudnessAnalysis,
    /// Full loudness/dynamics metering of the delivered signal (measured before
    /// quantization). The authoritative QC numbers for what was shipped.
    pub delivered: LoudnessAnalysis,
    /// The target's `lufs`, as declared; `None` when it asked for no loudness at all.
    /// Carried rather than left in the config because a report is read long after the
    /// render — often from a persisted `--json` document, with no config in reach — and
    /// without the figure it was judged against, `delivered.integrated_lufs` is a number
    /// with no verdict attached. See [`Self::lufs_verdict`].
    pub declared_lufs: Option<f64>,
    /// The target's `ceiling_dbtp`, as declared. Same reason as `declared_lufs`: it is what
    /// makes `delivered.true_peak_dbtp` a pass or a fail rather than a measurement.
    pub declared_ceiling_dbtp: f64,
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
    /// Static gain the normalization step applied (dB); `None` when the target set no
    /// `lufs` and no loudness step ran. Computed on every normalized render and, until
    /// now, thrown away — it is the single number that says how far the master moved.
    pub loudness_gain_db: Option<f64>,
    /// Further static gain applied by the post-resample correction (dB); `None` when that
    /// pass did not run at all (no resample, or no `lufs` for it to correct towards), and
    /// `0.0` when it ran and found nothing outside its tolerance.
    /// Separate from `loudness_gain_db` because they are answers to different questions:
    /// one is the normalization, the other is what band-limiting cost it.
    pub post_resample_gain_db: Option<f64>,
    /// Resampler quality tier actually used ("lq".."vhq"); `None` when no SRC ran at all.
    /// A DSD target always resamples — the interpolation to the 1-bit rate is one.
    pub src_quality: Option<&'static str>,
    /// Dither mode as configured ("tpdf" | "none" | "shaped" | …); `None` for the codecs
    /// with no bit-depth stage (MP3, DSD), where the setting is silently ignored.
    /// `dithered`/`shaping_curve` say what *happened*; this says what was *asked for*,
    /// and the pair is what distinguishes flat TPDF from adaptive-degraded-to-TPDF.
    pub dither_mode: Option<&'static str>,
    /// Dither strength as configured; `None` for the same codecs as `dither_mode`.
    pub dither_strength: Option<&'static str>,
    /// RNG seed pinned for this render, or `None` when it was left to entropy — in which
    /// case the run is not reproducible. Feeds both the dither streams and the DSD
    /// modulator's comparator dither, which is why it sits here rather than under dither.
    pub seed: Option<u64>,
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

impl ProcessingReport {
    /// Did the delivery hit the loudness it was asked for? `None` when the target declared
    /// no `lufs`, which is not the same answer as `false` — nothing was promised, so there
    /// is nothing to pass or fail.
    ///
    /// The judgement is [`loudness::meets_target`]'s, the same one the render's own
    /// "loudness target not met" warning is made with. A reader that re-implemented the
    /// ±0.1 LU rule could disagree with the warning sitting in `warnings` on the same row.
    pub fn lufs_verdict(&self) -> Option<bool> {
        self.declared_lufs
            .map(|declared| loudness::meets_target(self.delivered.integrated_lufs, declared))
    }

    /// Did the delivered true peak stay under the declared ceiling? Unconditional: every
    /// target has a `ceiling_dbtp`, so this is always a verdict.
    ///
    /// Strict, unlike the loudness one — the ceiling is a limit, not a target, and a
    /// tolerance around it would be permission to exceed it. Worth knowing that the render
    /// itself is *not* strict here: `loudness`'s `TP_TOLERANCE_DB` lets a limited delivery
    /// sit up to 0.05 dB over before it re-limits (the slack between the limiter's
    /// oversampled detector and the BS.1770 meter), so a peak parked exactly on the ceiling
    /// can read as a fail here with no warning beside it.
    pub fn ceiling_verdict(&self) -> bool {
        self.delivered.true_peak_dbtp <= self.declared_ceiling_dbtp
    }
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

/// Why a render would destroy something the user did not agree to lose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// The output path names the input master itself.
    Input,
    /// Something else is already sitting at the output path.
    Existing,
}

impl Conflict {
    /// The prose the CLI bails with. One producer, so a front end that renders this in a
    /// dialog and the CLI that prints it cannot describe the same situation differently —
    /// and so the two fixes the message names stay the only two on offer.
    pub fn message(self, input: &Path, output: &Path) -> String {
        match self {
            Conflict::Input => format!(
                "refusing to overwrite the input file without --force or overwrite=true: {}",
                input.display()
            ),
            Conflict::Existing => format!(
                "output file already exists: {} (pass --force, or set overwrite = true on the \
                 target, to replace it)",
                output.display()
            ),
        }
    }
}

/// Would writing `output_path` destroy something? A *query* — it answers and returns,
/// touching nothing. That is what lets a form ask it per keystroke, and it is why the
/// overwrite policy can stay here instead of migrating into a writer where it would be
/// asked too late and by code that does not know what the target consented to.
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
pub fn output_conflict(input: &Path, output_path: &Path, overwrite: bool) -> Option<Conflict> {
    // `symlink_metadata` rather than `exists()`: a dangling symlink at the output path is
    // something the rename will replace, so it counts as occupied.
    if std::fs::symlink_metadata(output_path).is_err() {
        return None;
    }
    if overwrite {
        return None;
    }
    if same_file(output_path, input) {
        return Some(Conflict::Input);
    }
    Some(Conflict::Existing)
}

/// Enforcement half of [`output_conflict`]: the same question, as a hard stop.
fn guard_output_path(input: &Path, output_path: &Path, overwrite: bool) -> Result<()> {
    match output_conflict(input, output_path, overwrite) {
        None => Ok(()),
        Some(conflict) => bail!("{}", conflict.message(input, output_path)),
    }
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
    process_with(input, target, seed, JobControl::default())
}

/// [`process`], with cancellation and progress.
///
/// `ctl` is two borrowed pointers and nothing else — see `crate::progress`. With
/// `JobControl::default()` every hook below is two `Option` checks per block, which is why
/// `process` can stay the signature the CLI and every test already call.
///
/// Cancellation semantics, stated because a mastering engineer will assume them: a
/// cancelled job propagates `Cancelled` through the same `?` path as a failure, unwinding
/// past `audio::atomic_write::Staged`'s `Drop`. **The previous render survives and no
/// partial file is left behind.**
pub fn process_with(
    input: &Path,
    target: &ResolvedTarget,
    seed: Option<u64>,
    ctl: JobControl<'_>,
) -> Result<ProcessingReport> {
    let started = std::time::Instant::now();
    // Everything that can be known to be impossible is decided from the *header*, before the
    // file is even decoded. All of it used to surface from inside a writer instead — a
    // 192 kHz FLAC target or a 7.1 DSD one failed after the whole chain had run, and
    // `overwrite = false` discovered the pre-existing output only once it had been replaced.
    // A doomed job now costs one header read, which matters in a batch: a hundred of them
    // used to cost a hundred decodes to say the same thing a hundred times.
    ctl.report(Stage::Probe, 0.0);
    ctl.check()?;
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

    let mut buffer = io_read::read_audio_with(input, ctl)?;
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
    let mut loudness_gain_db = None;
    let mut post_resample_gain_db = None;
    // Loudness as the normalization stage left it. Only the post-resample report reads it,
    // and only to separate the loudness the *resampler* removed from the loudness the
    // limiter had already eaten before it — two different stories about one shortfall.
    let mut pre_src_lufs = None;

    // Full source metering, measured once on the untouched input so the report shows
    // loudness/dynamics uniformly whether or not a loudness step runs.
    let source = loudness::analyze_with(&buffer, Stage::AnalyzeSource, ctl)?;

    // 1. Loudness normalization — only when the target specifies a LUFS goal.
    //    The loudness step applies a constant gain and (for on_clip=limit) runs
    //    the limiter, so it also enforces the ceiling on its own output.
    let loudness_ran = if let Some(lufs) = target.lufs {
        let result = loudness::apply_with(
            &mut buffer,
            lufs,
            Some(ceiling),
            Some(target.on_clip),
            limiter_options,
            target.max_limiting_db,
            ctl,
        )?;
        warnings.extend(result.warnings);
        pre_src_lufs = Some(result.out_lufs);
        loudness_gain_db = Some(result.gain_db);

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
        resample::apply_with(&mut buffer, pcm_rate, Some(target.quality), ctl)?;
        // SRC can raise inter-sample peaks above the previously enforced ceiling.
        ctl.report(Stage::PostSrcVerify, 0.0);
        let recheck =
            loudness::recheck_and_limit_if_needed_with(&mut buffer, ceiling, limiter_options, ctl)?;
        warnings.extend(recheck.warnings);

        // ...and it moves the integrated loudness too, which the gain computed at the source
        // rate could not have known about. Corrected here rather than merely reported,
        // because what a deliverable is judged on is the loudness of the file that ships.
        // Every `on_clip` policy, for the same reason the ceiling re-check above ignores
        // `on_clip`: the correction cannot exceed the headroom, so no policy's promise about
        // peaks is broken by it.
        if let Some(lufs) = target.lufs {
            let correction = loudness::correct_loudness_after_resample(
                &mut buffer,
                lufs,
                ceiling,
                recheck.true_peak_dbtp,
                pre_src_lufs.unwrap_or(f64::NAN),
            )?;
            warnings.extend(correction.warnings);
            post_resample_gain_db = Some(correction.applied_db);
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
        warnings.extend(loudness::enforce_ceiling_with(
            &mut buffer,
            ceiling,
            target.on_clip,
            limiter_options,
            ctl,
        )?);
    }

    // Delivered metering for the report, measured before any quantization/encode.
    let delivered = loudness::analyze_with(&buffer, Stage::AnalyzeDelivered, ctl)?;

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
    // The comparison itself lives in `loudness::meets_target` and only there: this warning
    // and `ProcessingReport::lufs_verdict` are the same judgement made at two different
    // times, and a second copy of `±LUFS_TOLERANCE` is how they would come to differ.
    if let Some(lufs) = target.lufs
        && delivered.integrated_lufs.is_finite()
        && !loudness::meets_target(delivered.integrated_lufs, lufs)
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
    // What was *asked for*, set only by the arm that actually has a bit-depth stage. On
    // MP3 and DSD these settings are silently ignored (see CLAUDE.md), and a report that
    // named a dither mode there would be describing something that never ran.
    let mut dither_mode = None;
    let mut dither_strength = None;
    let format_tag_str;

    match target.codec {
        OutputCodec::Mp3 => {
            // One blocking call into libmp3lame with nothing to count; reported as started
            // rather than given a bar that would be invented.
            ctl.report(Stage::Encode, 0.0);
            io_write::write_mp3(&output_path, &buffer, target.mp3_bitrate)?;
            format_tag_str = format!("{} kbps", target.mp3_bitrate);
        }
        OutputCodec::Dsf => {
            let dsd_rate = target
                .dsd
                .context("DSD codec resolved without a dsd rate (config resolution bug)")?;
            let encoding = dsd::encode_dsf_with(
                &output_path,
                &buffer.channels,
                pcm_rate,
                dsd_rate,
                Some(target.quality),
                seed,
                ctl,
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
            let bd = bitdepth::apply_with(
                &mut buffer,
                &mut current,
                target.format,
                Some(target.dither),
                Some(target.dither_strength),
                Some(target.dither_correlation),
                seed,
                ctl,
            )?;
            dithered = bd.dithered;
            shaped = bd.shaped;
            shaping_curve = bd.shaping_tag;
            dither_mode = Some(dither_tag(target.dither));
            dither_strength = Some(dither_strength_tag(target.dither_strength));

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

    // Read back rather than predicted: this is the ground truth `estimated_output_bytes`
    // is judged against, and a stat of a file we just closed is not worth guarding.
    let file_bytes = fs::metadata(&output_path)
        .with_context(|| format!("failed to stat the file just written: {}", output_path.display()))?
        .len();
    debug_assert!(
        dsd_report.as_ref().is_none_or(|d: &DsdReport| d.file_bytes == file_bytes),
        "the DSF writer's own byte count disagrees with the file on disk"
    );

    Ok(ProcessingReport {
        target: target.name.clone(),
        input: input.display().to_string(),
        output: output_path.display().to_string(),
        in_rate: source_rate,
        out_rate,
        channels: buffer.channels_count(),
        // The DSD branch delivers 1-bit samples, so its own count is the delivered one;
        // the PCM buffer it was interpolated from is an intermediate, not the deliverable.
        frames: dsd_report
            .as_ref()
            .map_or(buffer.frame_len() as u64, |d| d.samples_per_channel),
        file_bytes,
        elapsed_ms: started.elapsed().as_millis() as u64,
        source,
        delivered,
        declared_lufs: target.lufs,
        declared_ceiling_dbtp: ceiling,
        limiter_gain_reduction_db: limiter_gain_reduction,
        limiter_avg_gain_reduction_db: limiter_avg_gain_reduction,
        limiter_active_percent,
        loudness_gain_db,
        post_resample_gain_db,
        // A DSD target always resamples even when the PCM stage rate matches the source:
        // the interpolation up to the 1-bit rate is an SRC, on the same quality tier.
        src_quality: (pcm_rate != source_rate || target.codec.is_dsd())
            .then(|| quality_tag(target.quality)),
        dither_mode,
        dither_strength,
        seed,
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

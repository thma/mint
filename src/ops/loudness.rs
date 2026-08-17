use anyhow::{Context, Result};
use ebur128::{Channel, EbuR128, Mode};

use crate::audio::buffer::AudioBuffer;
use crate::config::OnClipPolicy;
use crate::diagnostics::WarningKind;
use crate::ops::limiter;
use crate::progress::{JobControl, Stage};

/// True-peak slack (dB) we tolerate above the ceiling before re-limiting. Absorbs the
/// tiny disagreement between the limiter's own oversampled detector and the BS.1770
/// meter; anything larger triggers another verification pass.
const TP_TOLERANCE_DB: f64 = 0.05;

/// Safety cap on verification passes. With the band-limited detector the first pass
/// already lands within hundredths of a dB, so 1–2 passes is typical; this is just a
/// stop so a pathological signal can't loop forever.
const MAX_LIMIT_PASSES: usize = 6;

/// Convergence tolerance for maximize's makeup-gain loop — half the pipeline's ±0.1 LU
/// "target not met" threshold, so a converged run never trips that warning.
const MAKEUP_TOLERANCE_LU: f64 = 0.05;

/// Cap on maximize's makeup-gain passes. Convergence is monotone from below and
/// typically lands within tolerance in 2–3 passes; this bounds pathological programs.
const MAX_MAKEUP_PASSES: usize = 5;

#[derive(Debug, Clone)]
pub struct LoudnessApplyResult {
    pub in_lufs: f64,
    pub out_lufs: f64,
    pub gain_db: f64,
    pub true_peak_in_dbtp: f64,
    pub true_peak_out_dbtp: f64,
    /// Peak gain reduction the brickwall limiter had to apply to hold the ceiling
    /// (0.0 when no limiting ran). This is the headline "how hard was the source
    /// squashed" number — the only signal that a loudness target was reached at
    /// the cost of dynamics, since `out_lufs`/`true_peak_out` both look clean.
    pub limiter_gain_reduction_db: f64,
    /// Mean gain reduction (dB) across the samples the limiter actually attenuated
    /// (0.0 when no limiting ran). Peak GR alone can't distinguish one shaved needle
    /// from wall-to-wall limiting; this and `limiter_active_ratio` can.
    pub limiter_avg_gain_reduction_db: f64,
    /// Fraction (0..=1) of assessable samples the limiter attenuated.
    pub limiter_active_ratio: f64,
    pub warnings: Vec<WarningKind>,
}

pub fn apply(
    buffer: &mut AudioBuffer,
    target_lufs: f64,
    true_peak_ceiling_dbtp: Option<f64>,
    on_clip: Option<OnClipPolicy>,
    limiter_options: limiter::LimiterOptions,
    max_limiting_db: f64,
) -> Result<LoudnessApplyResult> {
    apply_with(
        buffer,
        target_lufs,
        true_peak_ceiling_dbtp,
        on_clip,
        limiter_options,
        max_limiting_db,
        JobControl::default(),
    )
}

/// [`apply`], cancellable and reporting progress.
#[allow(clippy::too_many_arguments)]
pub fn apply_with(
    buffer: &mut AudioBuffer,
    target_lufs: f64,
    true_peak_ceiling_dbtp: Option<f64>,
    on_clip: Option<OnClipPolicy>,
    limiter_options: limiter::LimiterOptions,
    max_limiting_db: f64,
    ctl: JobControl<'_>,
) -> Result<LoudnessApplyResult> {
    ctl.report(Stage::Loudness, 0.0);
    let in_lufs = measure_lufs_with(buffer, Stage::Loudness, ctl)?;
    let tp_in = measure_true_peak_dbtp_with(buffer, Stage::Loudness, ctl)?;

    let gain_db = target_lufs - in_lufs;
    let mut warnings = Vec::new();

    let ceiling = true_peak_ceiling_dbtp.unwrap_or(-1.0);
    let policy = on_clip.unwrap_or(OnClipPolicy::Limit);

    // Maximize is normalization *with* makeup-gain convergence — its own control flow.
    if matches!(policy, OnClipPolicy::Maximize) {
        return apply_maximize(
            buffer, target_lufs, ceiling, limiter_options, max_limiting_db, in_lufs, tp_in, ctl,
        );
    }

    let predicted_tp = tp_in + gain_db;
    let needs_ceiling_enforcement = predicted_tp > ceiling;

    // Determine the effective gain that will be applied before the limiter (if any) runs.
    let effective_gain_db = match policy {
        OnClipPolicy::ReduceGain if needs_ceiling_enforcement => {
            // Cap gain so the predicted true peak does not exceed the ceiling.
            // Preserves dynamics entirely; target LUFS may not be met.
            warnings.push(WarningKind::ReduceGainCapped {
                predicted_dbtp: predicted_tp,
                ceiling_dbtp: ceiling,
            });
            gain_db.min(ceiling - tp_in)
        }
        OnClipPolicy::Warn if needs_ceiling_enforcement => {
            warnings.push(WarningKind::PredictedPeakOverCeiling {
                predicted_dbtp: predicted_tp,
                ceiling_dbtp: ceiling,
            });
            gain_db
        }
        // Limit policy: apply the full gain and let the limiter enforce the ceiling below.
        _ => gain_db,
    };

    let gain_lin = 10_f64.powf(effective_gain_db / 20.0);
    for channel in &mut buffer.channels {
        for sample in channel {
            *sample *= gain_lin;
        }
    }

    // For the Limit policy, run the oversampled true-peak limiter after gain, then verify
    // the BS.1770 meter agrees the ceiling is held.
    let mut limiter_gain_reduction_db = 0.0;
    let mut limiter_avg_gain_reduction_db = 0.0;
    let mut limiter_active_ratio = 0.0;
    if matches!(policy, OnClipPolicy::Limit) && needs_ceiling_enforcement {
        // The limiter tallies its own per-sample gain reduction as it applies the curve.
        // Diffing against a full copy of the buffer would be a second whole-file
        // allocation for a statistic the limiter already has in hand.
        let mut tally =
            limiter::GainReductionTally::new(buffer.channels_count(), buffer.frame_len());
        let (final_tp, passes) =
            limit_and_verify(buffer, ceiling, limiter_options, Some(&mut tally), ctl)?;
        // The loudest peak sat at `predicted_tp`; the limiter pulls it down to the
        // ceiling, so this is the maximum gain reduction applied anywhere in the file.
        limiter_gain_reduction_db = predicted_tp - ceiling;
        (limiter_avg_gain_reduction_db, limiter_active_ratio) = tally.result();
        if final_tp - ceiling > TP_TOLERANCE_DB {
            warnings.push(WarningKind::ResidualTruePeak {
                measured_dbtp: final_tp,
                passes,
                ceiling_dbtp: Some(ceiling),
            });
        }
    }

    let out_lufs = measure_lufs_with(buffer, Stage::Loudness, ctl)?;
    let tp_out = measure_true_peak_dbtp_with(buffer, Stage::Loudness, ctl)?;

    Ok(LoudnessApplyResult {
        in_lufs,
        out_lufs,
        gain_db: effective_gain_db,
        true_peak_in_dbtp: tp_in,
        true_peak_out_dbtp: tp_out,
        limiter_gain_reduction_db,
        limiter_avg_gain_reduction_db,
        limiter_active_ratio,
        warnings,
    })
}

/// `Maximize`: loudness normalization with makeup-gain convergence. A single gain
/// application undershoots hot targets because the limiter eats integrated loudness;
/// this loop re-adds the measured shortfall until the delivered LUFS lands on target
/// (± `MAKEUP_TOLERANCE_LU`), the peak gain reduction reaches `max_limiting_db`, or
/// `MAX_MAKEUP_PASSES` runs out (the pipeline's "target not met" warning then reports it).
///
/// Every pass restarts from the pristine source (× the accumulated gain): re-limiting an
/// already-limited signal would compound envelope modulation, while restarting keeps
/// exactly one limiter generation in the delivered audio. Convergence is monotone from
/// below — limiting only ever eats loudness and `out(gain + Δ) ≤ out(gain) + Δ`, so the
/// target is approached but never overshot.
fn apply_maximize(
    buffer: &mut AudioBuffer,
    target_lufs: f64,
    ceiling: f64,
    limiter_options: limiter::LimiterOptions,
    max_limiting_db: f64,
    in_lufs: f64,
    tp_in: f64,
    ctl: JobControl<'_>,
) -> Result<LoudnessApplyResult> {
    let pristine = buffer.channels.clone();
    let mut warnings = Vec::new();

    // The gain at which peak gain reduction hits exactly max_limiting_db; makeup gain
    // never exceeds it. Clamping unconditionally is safe: gains below the ceiling-breach
    // point produce no gain reduction at all and sit below this bound anyway.
    let cap_gain_db = ceiling + max_limiting_db - tp_in;
    let desired_gain_db = target_lufs - in_lufs;
    let mut gain_db = desired_gain_db.min(cap_gain_db);
    let mut hit_cap = desired_gain_db > cap_gain_db;

    let mut out_lufs = in_lufs;
    let mut limited = false;
    let mut residual_tp_warning: Option<WarningKind> = None;
    // Reset per makeup pass: only the last pass's limiting survives into the delivered
    // audio, so only it may be reported.
    let mut tally = limiter::GainReductionTally::new(buffer.channels_count(), buffer.frame_len());

    for pass in 1..=MAX_MAKEUP_PASSES {
        let gain_lin = 10_f64.powf(gain_db / 20.0);
        for (channel, source) in buffer.channels.iter_mut().zip(&pristine) {
            for (sample, &s) in channel.iter_mut().zip(source) {
                *sample = s * gain_lin;
            }
        }

        limited = tp_in + gain_db > ceiling;
        if limited {
            tally.reset();
            let (final_tp, passes) =
                limit_and_verify(buffer, ceiling, limiter_options, Some(&mut tally), ctl)?;
            // Only the last pass's audio survives, so earlier residuals are irrelevant.
            residual_tp_warning = (final_tp - ceiling > TP_TOLERANCE_DB).then_some(
                WarningKind::ResidualTruePeak {
                    measured_dbtp: final_tp,
                    passes,
                    ceiling_dbtp: Some(ceiling),
                },
            );
        }

        out_lufs = measure_lufs_with(buffer, Stage::Loudness, ctl)?;
        let shortfall = target_lufs - out_lufs;
        // The last pass must not raise the gain again: gain_db has to describe the
        // audio actually delivered.
        if shortfall <= MAKEUP_TOLERANCE_LU || hit_cap || pass == MAX_MAKEUP_PASSES {
            break;
        }
        let desired = gain_db + shortfall;
        hit_cap = desired > cap_gain_db;
        gain_db = desired.min(cap_gain_db);
    }

    if hit_cap && target_lufs - out_lufs > MAKEUP_TOLERANCE_LU {
        warnings.push(WarningKind::MaximizationCapped {
            max_limiting_db,
            target_lufs,
            delivered_lufs: out_lufs,
        });
    }
    warnings.extend(residual_tp_warning);

    let (limiter_avg_gain_reduction_db, limiter_active_ratio) =
        if limited { tally.result() } else { (0.0, 0.0) };

    let tp_out = measure_true_peak_dbtp_with(buffer, Stage::Loudness, ctl)?;
    Ok(LoudnessApplyResult {
        in_lufs,
        out_lufs,
        gain_db,
        true_peak_in_dbtp: tp_in,
        true_peak_out_dbtp: tp_out,
        limiter_gain_reduction_db: if limited {
            ((tp_in + gain_db) - ceiling).max(0.0)
        } else {
            0.0
        },
        limiter_avg_gain_reduction_db,
        limiter_active_ratio,
        warnings,
    })
}

/// Build a BS.1770 meter for `buffer` with the channel map the *delivery* implies.
///
/// The default ebur128 map is right for everything except mono, where it treats a
/// 1-channel file as a single `Left` speaker. That is not how a mono deliverable is heard:
/// it is played through both, and BS.1770-4 gives L and R the same +0 dB weight, so the
/// same programme is 3.01 LU louder as dual mono than as one channel (measured on mint's
/// own path: -12.03 vs -15.04 LUFS). Metering it as one channel therefore normalizes every
/// mono master 3.01 LU too loud — a -14 LUFS mono track plays back at about -11.
/// `Channel::DualMono` is libebur128's answer: it doubles that channel's energy.
///
/// The single construction site for every meter in the crate, so no measurement path can
/// pick up a different map than the one the delivered gain was computed from.
fn meter_for(buffer: &AudioBuffer, mode: Mode) -> Result<EbuR128> {
    let channels = buffer.channels_count() as u32;
    let mut meter = EbuR128::new(channels, buffer.sample_rate, mode)
        .context("failed to initialize ebur128 meter")?;
    if channels == 1 {
        meter
            .set_channel(0, Channel::DualMono)
            .context("failed to set the dual-mono channel map")?;
    }
    Ok(meter)
}

/// Poll interval for the momentary/short-term meters, and the hop every meter feed uses.
/// EBU Tech 3341 refreshes the momentary meter at least every 100 ms, so we hop at that rate
/// and track the maxima.
const METER_HOP_MS: f64 = 100.0;

/// Frames per hop at `rate`: `floor(rate / 10)`, unchanged from the expression `analyze_with`
/// has always used — deliberately, because changing it would move that function's
/// momentary/short-term poll instants and with them two figures in every report.
///
/// It has a second job now, and the second job is *nearly* free. ebur128's own
/// `samples_in_100ms` is `(rate + 5) / 10`, and the two agree for every rate whose value is a
/// multiple of ten — which is every rate in the 44.1 and 48 kHz families and every rate
/// `pcm_stage_rate` can derive. Where they agree, a hop boundary is also a `needed_frames`
/// boundary and the chunked feed drives the internal K-filter with exactly the call
/// segmentation the whole-buffer feed did, so bit-identity is *structural*.
///
/// They disagree for any rate whose last decimal digit is **five or more** — `floor` rounds
/// down where `samples_in_100ms` rounds up. Of the rates anyone actually ships, that is
/// **11 025 Hz** (1102 against 1103) and only 11 025 Hz; an arbitrary WAV header can of course
/// carry 8 008 or 44 107. There, bit-identity rests on the weaker but still measured claim: libebur128 accumulates into persistent state, so any split of the
/// input yields the same gating sequence, and the one theoretical divergence — the per-call
/// denormal flush that is live on aarch64 macOS — does not fire on real programme material.
/// The test asserts both: the structural equality where it holds, and bit-identity at 11 025
/// where it does not.
fn meter_hop(rate: u32) -> usize {
    ((rate as f64 * METER_HOP_MS / 1000.0) as usize).max(1)
}

/// Feed the whole buffer to `meter` in [`meter_hop`] slabs, reporting `stage` and honouring
/// cancellation between them.
///
/// The single feed path for all three meters. Before it existed, `analyze_with` hopped (so it
/// could poll) while `measure_lufs` and `measure_true_peak_dbtp` handed ebur128 the entire
/// planar buffer in one call — which on a five-minute 96 kHz master is 673 ms of integrated
/// metering and 1.15 s of true-peak metering that no cancel could interrupt, repeated up to
/// nine times in a `maximize` render.
///
/// `which` names the meter in the error context, because "the loudness meter refused these
/// frames" and "the true-peak meter did" are different diagnoses and both call sites used to
/// say which. `after_hop` sees the meter once per slab with the frame index just fed: that is
/// `analyze_with`'s momentary/short-term poll, and the two single-value meters pass a no-op.
/// Nothing is allocated per hop — `slabs` is one `Vec` of channel-count fat pointers, reused,
/// and `&channel[pos..end]` on a planar buffer is free. "The buffer is the budget" is
/// untouched.
fn feed_hopped(
    meter: &mut EbuR128,
    buffer: &AudioBuffer,
    which: &str,
    stage: Stage,
    ctl: JobControl<'_>,
    mut after_hop: impl FnMut(&EbuR128, usize),
) -> Result<()> {
    let frames = buffer.frame_len();
    let hop = meter_hop(buffer.sample_rate);
    let mut progress = ctl.stage(stage, frames as u64);
    let mut slabs: Vec<&[f64]> = Vec::with_capacity(buffer.channels_count());
    let mut pos = 0;
    while pos < frames {
        progress.tick(pos as u64)?;
        let end = (pos + hop).min(frames);
        slabs.clear();
        slabs.extend(buffer.channels.iter().map(|c| &c[pos..end]));
        meter
            .add_frames_planar_f64(&slabs)
            .with_context(|| format!("failed to feed frames into ebur128 {which}"))?;
        after_hop(meter, end);
        pos = end;
    }
    progress.finish();
    Ok(())
}

/// Integrated loudness (BS.1770), cancellable and reporting progress under `stage`.
///
/// There is no `ctl`-less twin, unlike every other `*_with` in the crate: this one is private
/// and all six of its callers are inside a job that has a `JobControl` in hand, so a wrapper
/// would only be a way to lose cancellation by accident.
///
/// `stage` is a parameter rather than a constant because the meter runs inside three
/// different stages — the normalization, `maximize`'s convergence loop, and the post-SRC
/// correction — and a bar that named the wrong one would be worse than none.
fn measure_lufs_with(buffer: &AudioBuffer, stage: Stage, ctl: JobControl<'_>) -> Result<f64> {
    let mut meter = meter_for(buffer, Mode::I)?;
    feed_hopped(&mut meter, buffer, "loudness meter", stage, ctl, |_, _| {})?;
    meter
        .loudness_global()
        .context("failed to query integrated loudness")
}

pub fn measure_true_peak_dbtp(buffer: &AudioBuffer) -> Result<f64> {
    measure_true_peak_dbtp_with(buffer, Stage::Ceiling, JobControl::default())
}

/// [`measure_true_peak_dbtp`], cancellable and reporting progress under `stage`.
pub fn measure_true_peak_dbtp_with(
    buffer: &AudioBuffer,
    stage: Stage,
    ctl: JobControl<'_>,
) -> Result<f64> {
    let channels = buffer.channels_count() as u32;
    // Goes through `meter_for` for uniformity only: true peak is a per-channel sample
    // maximum and the channel map never enters it. Nothing here changes for mono.
    let mut meter = meter_for(buffer, Mode::TRUE_PEAK)?;
    feed_hopped(&mut meter, buffer, "true-peak meter", stage, ctl, |_, _| {})?;
    max_true_peak_dbtp(&meter, channels)
}

/// Max true peak (dBTP) across channels of a meter that already has audio fed in and was
/// created with `Mode::TRUE_PEAK`. Shared by the single-value and full-analysis paths.
fn max_true_peak_dbtp(meter: &EbuR128, channels: u32) -> Result<f64> {
    let mut max_peak = f64::NEG_INFINITY;
    for ch in 0..channels {
        let peak = meter
            .true_peak(ch)
            .with_context(|| format!("failed to query true peak for channel {ch}"))?;
        let peak_dbtp = if peak <= 0.0 { f64::NEG_INFINITY } else { 20.0 * peak.log10() };
        if peak_dbtp > max_peak {
            max_peak = peak_dbtp;
        }
    }
    Ok(max_peak)
}

/// A full loudness/dynamics snapshot of a buffer — the meters a mastering engineer reads
/// for QC: integrated loudness, loudness range, the loudest momentary (400 ms) and
/// short-term (3 s) windows, and true peak. Derived crest figures (PLR/PSR) are below.
/// Fields can be `-inf`/`0` for silence or clips shorter than a meter's integration time.
#[derive(Debug, Clone)]
pub struct LoudnessAnalysis {
    pub integrated_lufs: f64,
    pub loudness_range_lu: f64,
    pub max_momentary_lufs: f64,
    pub max_short_term_lufs: f64,
    pub true_peak_dbtp: f64,
}

impl LoudnessAnalysis {
    /// Peak to Loudness Ratio (dB): true peak − integrated loudness. The crest factor of
    /// the whole programme; higher means more dynamic range preserved.
    pub fn plr(&self) -> f64 {
        self.true_peak_dbtp - self.integrated_lufs
    }

    /// Peak to Short-term Ratio (dB): true peak − loudest short-term window. The crest at
    /// the loudest passage; a low value flags heavy limiting / a "hot" master.
    pub fn psr(&self) -> f64 {
        self.true_peak_dbtp - self.max_short_term_lufs
    }
}

/// One-pass loudness + dynamics analysis (integrated, LRA, max momentary/short-term,
/// true peak). Audio is fed in 100 ms hops so the sliding momentary/short-term meters can
/// be polled for their maxima; integrated, LRA and true peak are read once at the end.
pub fn analyze(buffer: &AudioBuffer) -> Result<LoudnessAnalysis> {
    analyze_with(buffer, Stage::AnalyzeSource, JobControl::default())
}

/// [`analyze`], reporting progress and honouring cancellation. `stage` distinguishes the
/// two calls the pipeline makes — the same measurement, once on the source and once on
/// what will ship, and a progress bar that could not tell them apart would run twice.
pub fn analyze_with(
    buffer: &AudioBuffer,
    stage: Stage,
    ctl: JobControl<'_>,
) -> Result<LoudnessAnalysis> {
    let channels = buffer.channels_count() as u32;
    let rate = buffer.sample_rate;
    // LRA ⊇ S ⊇ M, and TRUE_PEAK adds the peak meter — one meter covers every figure.
    let mut meter = meter_for(buffer, Mode::I | Mode::LRA | Mode::TRUE_PEAK)?;

    // A window's value is only meaningful once that much audio has been fed.
    let momentary_ready = (rate as f64 * 0.4) as usize;
    let short_term_ready = (rate as f64 * 3.0) as usize;

    let mut max_momentary = f64::NEG_INFINITY;
    let mut max_short_term = f64::NEG_INFINITY;

    // The polling half of the hop; the hop itself is `feed_hopped`'s, shared with the two
    // single-value meters so there is one answer to "how is a meter fed".
    feed_hopped(&mut meter, buffer, "meter", stage, ctl, |meter, end| {
        if end >= momentary_ready
            && let Ok(m) = meter.loudness_momentary()
            && m.is_finite()
        {
            max_momentary = max_momentary.max(m);
        }
        if end >= short_term_ready
            && let Ok(s) = meter.loudness_shortterm()
            && s.is_finite()
        {
            max_short_term = max_short_term.max(s);
        }
    })?;

    Ok(LoudnessAnalysis {
        integrated_lufs: meter.loudness_global().context("integrated loudness")?,
        loudness_range_lu: meter.loudness_range().context("loudness range")?,
        max_momentary_lufs: max_momentary,
        max_short_term_lufs: max_short_term,
        true_peak_dbtp: max_true_peak_dbtp(&meter, channels)?,
    })
}

/// Run the brickwall limiter and then *verify* the result against the BS.1770 true-peak
/// meter (ebur128), re-limiting with a progressively lower internal ceiling until the
/// measured true peak is at/under `ceiling_dbtp`.
///
/// The limiter's own oversampled detector is good but is not the canonical meter that
/// downstream tools and streaming platforms use; this loop closes that gap so the
/// delivered file genuinely respects the configured ceiling. Convergence is guaranteed:
/// the internal ceiling only ever decreases when the meter reports an overshoot, which
/// strictly lowers the next pass's peak.
///
/// Returns the final measured true peak (dBTP) and the number of passes taken.
///
/// `tally`, when supplied, accumulates across the passes: gain reductions in dB add where
/// gains multiply, so what comes out is what a single before/after diff of the whole
/// buffer would have measured.
fn limit_and_verify(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    limiter_options: limiter::LimiterOptions,
    mut tally: Option<&mut limiter::GainReductionTally>,
    ctl: JobControl<'_>,
) -> Result<(f64, usize)> {
    let mut internal_ceiling = ceiling_dbtp;
    let mut tp = f64::INFINITY;

    for pass in 1..=MAX_LIMIT_PASSES {
        // The control reaches *through* this loop rather than only around it: up to six
        // whole-file limiter passes happen here, and a cancel that waited them out would
        // feel broken.
        match tally.as_deref_mut() {
            Some(t) => limiter::apply_true_peak_limit_tallied_with(
                buffer,
                internal_ceiling,
                limiter_options,
                t,
                ctl,
            )?,
            None => {
                limiter::apply_true_peak_limit_with(buffer, internal_ceiling, limiter_options, ctl)?
            }
        }
        // `Ceiling` rather than the stage this loop was entered from: the limiter above it
        // hardcodes `Ceiling` too, so the verification meter and the pass it verifies stay
        // one bar rather than alternating between two labels every pass.
        tp = measure_true_peak_dbtp_with(buffer, Stage::Ceiling, ctl)?;

        let overshoot = tp - ceiling_dbtp;
        if overshoot <= TP_TOLERANCE_DB {
            return Ok((tp, pass));
        }
        // Pull the internal target down by the residual the meter still sees, so the
        // next pass attenuates the offending inter-sample peak the rest of the way.
        internal_ceiling -= overshoot;
    }

    Ok((tp, MAX_LIMIT_PASSES))
}

/// Enforce a true-peak ceiling on a buffer that did *not* go through a loudness
/// normalization step (a pure transcode). Honors the `on_clip` policy:
/// - `limit`: oversampled brickwall limiter down to the ceiling.
/// - `reduce_gain`: a single static attenuation so the peak just meets the ceiling
///   (dynamics untouched).
/// - `warn`: report only, leave samples as-is.
///
/// No-op (returns no warnings) when the signal is already at or below the ceiling.
pub fn enforce_ceiling(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    on_clip: OnClipPolicy,
    limiter_options: limiter::LimiterOptions,
) -> Result<Vec<WarningKind>> {
    enforce_ceiling_with(buffer, ceiling_dbtp, on_clip, limiter_options, JobControl::default())
}

/// [`enforce_ceiling`], cancellable.
pub fn enforce_ceiling_with(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    on_clip: OnClipPolicy,
    limiter_options: limiter::LimiterOptions,
    ctl: JobControl<'_>,
) -> Result<Vec<WarningKind>> {
    let tp = measure_true_peak_dbtp_with(buffer, Stage::Ceiling, ctl)?;
    if tp <= ceiling_dbtp {
        return Ok(Vec::new());
    }

    match on_clip {
        // Maximize without a loudness step has nothing to maximize toward — it degrades
        // to plain ceiling enforcement (the pipeline warns about the no-op intent).
        OnClipPolicy::Limit | OnClipPolicy::Maximize => {
            let (final_tp, passes) =
                limit_and_verify(buffer, ceiling_dbtp, limiter_options, None, ctl)?;
            let mut warnings = vec![WarningKind::CeilingEnforcedByLimiter {
                measured_dbtp: tp,
                ceiling_dbtp,
                after_src: false,
            }];
            if final_tp - ceiling_dbtp > TP_TOLERANCE_DB {
                // `ceiling_dbtp: None`: the line above has just named it, and repeating it
                // here would reword a shipped warning.
                warnings.push(WarningKind::ResidualTruePeak {
                    measured_dbtp: final_tp,
                    passes,
                    ceiling_dbtp: None,
                });
            }
            Ok(warnings)
        }
        OnClipPolicy::ReduceGain => {
            let gain_db = ceiling_dbtp - tp; // negative: attenuation
            let gain_lin = 10_f64.powf(gain_db / 20.0);
            for channel in &mut buffer.channels {
                for sample in channel {
                    *sample *= gain_lin;
                }
            }
            Ok(vec![WarningKind::CeilingEnforcedByGain {
                measured_dbtp: tp,
                ceiling_dbtp,
                gain_db,
            }])
        }
        OnClipPolicy::Warn => {
            Ok(vec![WarningKind::CeilingNotEnforced { measured_dbtp: tp, ceiling_dbtp }])
        }
    }
}

/// Outcome of the post-SRC ceiling re-check.
///
/// `true_peak_dbtp` is the peak of the buffer as it now stands. It is handed back rather
/// than left to be re-measured because [`correct_loudness_after_resample`] runs straight
/// afterwards and needs exactly this number; a second true-peak pass over a half-gigabyte
/// buffer to recover a value that was just computed is real time for nothing.
#[derive(Debug, Clone)]
pub struct RecheckResult {
    pub warnings: Vec<WarningKind>,
    pub true_peak_dbtp: f64,
}

/// Re-measure the true peak after an operation that can introduce new inter-sample peaks
/// (most importantly sample-rate conversion) and apply the limiter if the ceiling is exceeded.
///
/// SRC filters can produce inter-sample peaks up to ~3 dBTP above the peak of the input;
/// this function ensures the ceiling remains enforced without requiring a manual second
/// loudness step in the pipeline config.
///
/// Deliberately does NOT re-run maximize's makeup-gain convergence: it trims only the
/// new inter-sample overshoots (hundredths of a dB). The *loudness* side of the same
/// question — the SRC changing the integrated figure itself — is
/// [`correct_loudness_after_resample`]'s job.
///
/// Warnings are non-empty only when the limiter was actually invoked.
pub fn recheck_and_limit_if_needed(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    limiter_options: limiter::LimiterOptions,
) -> Result<RecheckResult> {
    recheck_and_limit_if_needed_with(buffer, ceiling_dbtp, limiter_options, JobControl::default())
}

/// [`recheck_and_limit_if_needed`], cancellable.
pub fn recheck_and_limit_if_needed_with(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    limiter_options: limiter::LimiterOptions,
    ctl: JobControl<'_>,
) -> Result<RecheckResult> {
    let tp = measure_true_peak_dbtp_with(buffer, Stage::PostSrcVerify, ctl)?;
    if tp <= ceiling_dbtp {
        return Ok(RecheckResult { warnings: Vec::new(), true_peak_dbtp: tp });
    }
    let (final_tp, passes) = limit_and_verify(buffer, ceiling_dbtp, limiter_options, None, ctl)?;
    let mut warnings = vec![WarningKind::CeilingEnforcedByLimiter {
        measured_dbtp: tp,
        ceiling_dbtp,
        after_src: true,
    }];
    if final_tp - ceiling_dbtp > TP_TOLERANCE_DB {
        warnings.push(WarningKind::ResidualTruePeak {
            measured_dbtp: final_tp,
            passes,
            ceiling_dbtp: None,
        });
    }
    Ok(RecheckResult { warnings, true_peak_dbtp: final_tp })
}

/// How far the delivered integrated loudness may sit from its target before the report says
/// so. What every reference tool promises (±0.1–0.2 LU). `pipeline` makes that check against
/// the *delivered* measurement; this module only borrows the number, so that "corrected in
/// silence" and "reported" cannot end up meaning different sizes of miss.
pub const LUFS_TOLERANCE: f64 = 0.1;

/// Whether a delivered integrated loudness counts as having hit `target_lufs`
/// (±[`LUFS_TOLERANCE`]).
///
/// One producer, so the warning the pipeline emits and the verdict a report renders cannot
/// disagree about where the line is. A persisted report is judged long after the render, by
/// a reader that must not own a second copy of the rule.
pub fn meets_target(delivered_lufs: f64, target_lufs: f64) -> bool {
    (delivered_lufs - target_lufs).abs() <= LUFS_TOLERANCE
}

/// Smallest post-SRC loudness drift worth correcting: half the reporting threshold, for the
/// same reason `MAKEUP_TOLERANCE_LU` is half it — a correction that lands inside this can
/// never leave the delivery outside the figure that gets reported. Ordinary band-limited
/// programme drifts about 0.03 LU across a 96 → 44.1 kHz conversion, so this is a no-op on
/// nearly every real job.
const SRC_DRIFT_TOLERANCE_LU: f64 = LUFS_TOLERANCE / 2.0;

/// Re-verify the integrated loudness after a sample-rate conversion and pull it back onto
/// target with a static gain.
///
/// The normalization gain was computed from the loudness of the *source* rate. A conversion
/// changes that figure: BS.1770's K-weighting keeps rising above 20 kHz, so band-limiting a
/// 96 kHz master to 44.1 kHz removes energy the meter had been counting. On ordinary
/// programme the drift is ~0.03 LU; on an ultrasonic-heavy source it was measured at
/// **2.54 LU**, delivered silently because the target check ran on the pre-SRC number.
///
/// The correction is a single static gain, and an upward one is capped at the true-peak
/// headroom (`ceiling − true_peak`, which is why the caller passes the peak it just
/// measured). That cap is the whole design: within it the delivered dynamics are untouched
/// and no limiter runs, so the correction cannot compound a second limiter generation into
/// the audio or eat back the loudness it just added. What the headroom cannot buy is
/// reported instead — the pipeline's delivered-loudness check then quotes the real residual.
///
/// Integrated loudness moves dB-for-dB with a static gain: BS.1770's relative gate shifts
/// with the signal and selects the same blocks. Only the fixed −70 LUFS absolute gate can
/// make it inexact, and only for material sitting on that threshold — hence one pass, not a
/// convergence loop, with the delivered measurement as the check.
///
/// Applies to every `on_clip` policy, `warn` included. `warn`'s contract is about *clipping*
/// — report the overshoot, do not correct it — and it says nothing about loudness; the
/// headroom cap means the correction cannot push a peak up anyway. Excluding it would also
/// have been inconsistent with the ceiling re-check on the line above, which already ignores
/// `on_clip` after a resample.
///
/// `pre_src_lufs` is the loudness the buffer had before the conversion, used only to say in
/// the warning how much of the shortfall the resampler is actually responsible for — the
/// rest is loudness the limiter ate on the way here, which is a different story about the
/// same file.
/// What the post-resample correction did. The gain is returned rather than only warned
/// about because it is the second half of the static gain a render applied — reporting
/// the normalization gain alone would attribute the whole move to the wrong stage.
#[derive(Debug, Clone, Default)]
pub struct SrcCorrection {
    /// Static gain applied here (dB), 0.0 when the correction was a no-op.
    pub applied_db: f64,
    pub warnings: Vec<WarningKind>,
}

pub fn correct_loudness_after_resample(
    buffer: &mut AudioBuffer,
    target_lufs: f64,
    ceiling_dbtp: f64,
    true_peak_dbtp: f64,
    pre_src_lufs: f64,
) -> Result<SrcCorrection> {
    correct_loudness_after_resample_with(
        buffer,
        target_lufs,
        ceiling_dbtp,
        true_peak_dbtp,
        pre_src_lufs,
        JobControl::default(),
    )
}

/// [`correct_loudness_after_resample`], cancellable. The last function in this module that
/// still ran a whole-file meter no cancel could reach.
#[allow(clippy::too_many_arguments)]
pub fn correct_loudness_after_resample_with(
    buffer: &mut AudioBuffer,
    target_lufs: f64,
    ceiling_dbtp: f64,
    true_peak_dbtp: f64,
    pre_src_lufs: f64,
    ctl: JobControl<'_>,
) -> Result<SrcCorrection> {
    let measured = measure_lufs_with(buffer, Stage::PostSrcVerify, ctl)?;
    // Digital silence meters as -inf: there is no gain that makes it -14 LUFS, and the
    // delivered-loudness check reports the miss.
    if !measured.is_finite() {
        return Ok(SrcCorrection::default());
    }

    let shortfall = target_lufs - measured;
    if shortfall.abs() <= SRC_DRIFT_TOLERANCE_LU {
        return Ok(SrcCorrection::default());
    }

    // Attenuation is always available; gain is only what sits under the ceiling.
    let headroom = (ceiling_dbtp - true_peak_dbtp).max(0.0);
    let applied_db = if shortfall > 0.0 { shortfall.min(headroom) } else { shortfall };

    if applied_db.abs() > f64::EPSILON {
        let gain_lin = 10_f64.powf(applied_db / 20.0);
        for channel in &mut buffer.channels {
            for sample in channel {
                *sample *= gain_lin;
            }
        }
    }

    // Small corrections happen in silence — the number is noise. One large enough to have
    // tripped the delivery check is worth saying out loud even though it succeeded: it means
    // the source carries content the delivery rate cannot, and the same master at a different
    // rate will not measure the same.
    let mut warnings = Vec::new();
    if shortfall.abs() > LUFS_TOLERANCE {
        warnings.push(WarningKind::SrcLoudnessCorrected {
            measured_lufs: measured,
            target_lufs,
            applied_db,
            // Both clauses of the sentence are decided here, where the numbers that decide
            // them live, and carried as data. `message()` renders them; it does not re-derive
            // "was this capped?" from a comparison it would have to keep in step with this one.
            src_drift_lu: pre_src_lufs.is_finite().then_some(measured - pre_src_lufs),
            capped_by_ceiling: (applied_db < shortfall - 1e-9).then_some(ceiling_dbtp),
        });
    }
    Ok(SrcCorrection { applied_db, warnings })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::buffer::{OutputSampleFormat, SourceInfo};
    use std::path::PathBuf;

    /// The whole-buffer feed the hopped one replaced: hand ebur128 the entire planar buffer
    /// in one `add_frames_planar_f64`. Kept as the definition of "right" for
    /// `hopped_metering_is_bit_identical_to_a_single_feed`.
    ///
    /// It is not shippable — not because of memory (it allocates only channel-count fat
    /// pointers) but because it is uninterruptible: on the 461 MB reference master it is
    /// 673 ms of integrated metering and 1.15 s of true-peak metering during which a cancel
    /// cannot be seen, repeated up to nine times in a `maximize` render.
    fn whole_file(meter: &mut EbuR128, buffer: &AudioBuffer) {
        let planar: Vec<&[f64]> = buffer.channels.iter().map(Vec::as_slice).collect();
        meter.add_frames_planar_f64(&planar).expect("whole-buffer feed");
    }

    /// The mode has to be passed in rather than fixed: ebur128's integrated figure is not
    /// identical across mode sets — `Mode::I` alone and `Mode::I | LRA | TRUE_PEAK` differ in
    /// the last few ULPs on the same audio, because the extra meters change the arithmetic
    /// the shared gating blocks go through. That is a property of the library and predates
    /// this change; the point here is to hold *feeding* constant, so each reference is built
    /// with the same mode as the path it is checking.
    fn whole_file_lufs(buffer: &AudioBuffer, mode: Mode) -> f64 {
        let mut meter = meter_for(buffer, mode).expect("meter");
        whole_file(&mut meter, buffer);
        meter.loudness_global().expect("integrated")
    }

    fn whole_file_true_peak(buffer: &AudioBuffer, mode: Mode) -> f64 {
        let mut meter = meter_for(buffer, mode).expect("meter");
        whole_file(&mut meter, buffer);
        max_true_peak_dbtp(&meter, buffer.channels_count() as u32).expect("true peak")
    }

    /// Deterministic programme-like material: a tone per channel plus a reproducible noise
    /// floor, so the K-filter, the gating and the 4× true-peak interpolator all have
    /// something non-trivial to chew on and the result is the same on every run.
    fn signal(rate: u32, channels: usize, frames: usize) -> AudioBuffer {
        let mut state = 0x5DEE_CE66_D125_ABCDu64;
        let mut rng = move || {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((state >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
        };
        let channels_data = (0..channels)
            .map(|c| {
                let f = 440.0 * (c as f64 + 1.0);
                (0..frames)
                    .map(|n| {
                        let t = n as f64 / rate as f64;
                        0.4 * (std::f64::consts::TAU * f * t).sin() + 0.02 * rng()
                    })
                    .collect()
            })
            .collect();
        AudioBuffer {
            channels: channels_data,
            sample_rate: rate,
            src_info: SourceInfo {
                path: PathBuf::from("synthetic"),
                channels,
                sample_rate: rate,
                codec: "test".to_string(),
            },
            out_format: OutputSampleFormat::F32,
        }
    }

    /// `to_bits()`, not a tolerance — CLAUDE.md's rule for every streaming stage, and the
    /// only assertion that would catch the one way this change could go wrong: a hop that is
    /// not a multiple of ebur128's `samples_in_100ms` moves where its internal per-call
    /// denormal flush fires, which is a real (if inaudible) difference in the returned f64.
    ///
    /// Lengths are deliberately ragged as well as aligned: an exact multiple of the hop, one
    /// frame either side of it, a buffer shorter than a single hop, and a buffer shorter than
    /// the 400 ms momentary window.
    #[test]
    fn hopped_metering_is_bit_identical_to_a_single_feed() {
        // 11 025 is in the list on purpose: it is the one reachable rate where the hop is
        // *not* ebur128's own 100 ms block, so it is the case the structural argument does
        // not cover and only a measurement can settle.
        for rate in [11_025u32, 44_100, 48_000, 96_000, 192_000] {
            let hop = meter_hop(rate);
            for channels in [1usize, 2] {
                for frames in [hop * 37, hop * 37 + 1, hop * 37 - 1, hop - 1, hop / 3, hop * 2] {
                    let buffer = signal(rate, channels, frames);
                    let case = format!("{rate} Hz, {channels} ch, {frames} frames");

                    let hopped = measure_lufs_with(&buffer, Stage::Loudness, JobControl::default())
                        .expect("hopped lufs");
                    assert_eq!(
                        hopped.to_bits(),
                        whole_file_lufs(&buffer, Mode::I).to_bits(),
                        "integrated loudness differs when hopped ({case})"
                    );

                    let hopped = measure_true_peak_dbtp(&buffer).expect("hopped true peak");
                    assert_eq!(
                        hopped.to_bits(),
                        whole_file_true_peak(&buffer, Mode::TRUE_PEAK).to_bits(),
                        "true peak differs when hopped ({case})"
                    );
                }
            }
        }
    }

    /// `analyze_with` hopped before this change too, and now shares the feeder. The same
    /// property, asserted through the entry point the *report* actually reads — with the
    /// reference built on its own combined mode set, because the figures it returns are the
    /// combined meter's and comparing them to a `Mode::I`-only meter measures the mode, not
    /// the feed. (That mismatch is worth 4 ULPs and it is how this test first failed.)
    #[test]
    fn the_full_analysis_agrees_with_a_single_feed_on_integrated_and_peak() {
        let mode = Mode::I | Mode::LRA | Mode::TRUE_PEAK;
        for rate in [44_100u32, 96_000] {
            for frames in [rate as usize * 4, rate as usize * 4 + 1] {
                let buffer = signal(rate, 2, frames);
                let analysis =
                    analyze_with(&buffer, Stage::AnalyzeSource, JobControl::default()).expect("analyze");
                assert_eq!(
                    analysis.integrated_lufs.to_bits(),
                    whole_file_lufs(&buffer, mode).to_bits(),
                    "{rate} Hz / {frames} frames"
                );
                assert_eq!(
                    analysis.true_peak_dbtp.to_bits(),
                    whole_file_true_peak(&buffer, mode).to_bits(),
                    "{rate} Hz / {frames} frames"
                );
            }
        }
    }

    /// Where the hop equals ebur128's own `samples_in_100ms` — `(rate + 5) / 10` in its
    /// source — the bit-identity above is structural rather than lucky. That is every rate
    /// divisible by ten, which is every rate in the two families mint derives within.
    ///
    /// And where it does not: 11 025 Hz, asserted as the exception rather than left as a
    /// silent hole in the claim. The bit-identity test covers that rate by measurement.
    #[test]
    fn the_hop_is_ebur128s_own_block_at_every_rate_divisible_by_ten() {
        for rate in [8_000u32, 22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 176_400, 192_000] {
            assert_eq!(rate % 10, 0, "{rate} Hz is not in the regime this test describes");
            assert_eq!(meter_hop(rate), ((rate + 5) / 10) as usize, "{rate} Hz");
        }
        // The exception is not 11 025 specifically: it is every rate whose last decimal
        // digit is five or more, because `floor` rounds down where `samples_in_100ms` rounds
        // up. 11 025 is simply the only one anyone ships. Both halves asserted, so neither
        // the rule nor the example in `meter_hop`'s doc can go stale unnoticed.
        // (Below 10 Hz the `.max(1)` floor makes them agree again, which is a curiosity
        // rather than a case — nothing decodes at 5 Hz.)
        for rate in [11_025u32, 8_008, 44_107, 95] {
            assert_ne!(meter_hop(rate), ((rate + 5) / 10) as usize, "{rate} Hz");
        }
        for rate in [11_020u32, 44_104, 100] {
            assert_eq!(meter_hop(rate), ((rate + 5) / 10) as usize, "{rate} Hz");
        }
    }
}

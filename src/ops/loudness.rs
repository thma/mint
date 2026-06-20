use anyhow::{Context, Result};
use ebur128::{EbuR128, Mode};

use crate::audio::buffer::AudioBuffer;
use crate::config::OnClipPolicy;
use crate::ops::limiter;

/// True-peak slack (dB) we tolerate above the ceiling before re-limiting. Absorbs the
/// tiny disagreement between the limiter's own oversampled detector and the BS.1770
/// meter; anything larger triggers another verification pass.
const TP_TOLERANCE_DB: f64 = 0.05;

/// Safety cap on verification passes. With the band-limited detector the first pass
/// already lands within hundredths of a dB, so 1–2 passes is typical; this is just a
/// stop so a pathological signal can't loop forever.
const MAX_LIMIT_PASSES: usize = 6;

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
    pub warnings: Vec<String>,
}

pub fn apply(
    buffer: &mut AudioBuffer,
    target_lufs: f64,
    true_peak_ceiling_dbtp: Option<f64>,
    on_clip: Option<OnClipPolicy>,
) -> Result<LoudnessApplyResult> {
    let in_lufs = measure_lufs(buffer)?;
    let tp_in = measure_true_peak_dbtp(buffer)?;

    let gain_db = target_lufs - in_lufs;
    let mut warnings = Vec::new();

    let ceiling = true_peak_ceiling_dbtp.unwrap_or(-1.0);
    let policy = on_clip.unwrap_or(OnClipPolicy::Limit);
    let predicted_tp = tp_in + gain_db;
    let needs_ceiling_enforcement = predicted_tp > ceiling;

    // Determine the effective gain that will be applied before the limiter (if any) runs.
    let effective_gain_db = match policy {
        OnClipPolicy::ReduceGain if needs_ceiling_enforcement => {
            // Cap gain so the predicted true peak does not exceed the ceiling.
            // Preserves dynamics entirely; target LUFS may not be met.
            warnings.push(format!(
                "clip policy reduce_gain applied; target LUFS may not be reached \
                 (predicted {:.2} dBTP > ceiling {:.2} dBTP)",
                predicted_tp, ceiling
            ));
            gain_db.min(ceiling - tp_in)
        }
        OnClipPolicy::Warn if needs_ceiling_enforcement => {
            warnings.push(format!(
                "predicted true peak {:.2} dBTP exceeds ceiling {:.2} dBTP",
                predicted_tp, ceiling
            ));
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
    if matches!(policy, OnClipPolicy::Limit) && needs_ceiling_enforcement {
        let (final_tp, passes) = limit_and_verify(buffer, ceiling)?;
        // The loudest peak sat at `predicted_tp`; the limiter pulls it down to the
        // ceiling, so this is the maximum gain reduction applied anywhere in the file.
        limiter_gain_reduction_db = predicted_tp - ceiling;
        if final_tp - ceiling > TP_TOLERANCE_DB {
            warnings.push(format!(
                "true peak still {final_tp:.2} dBTP after {passes} limiter passes \
                 (ceiling {ceiling:.2} dBTP)"
            ));
        }
    }

    let out_lufs = measure_lufs(buffer)?;
    let tp_out = measure_true_peak_dbtp(buffer)?;

    Ok(LoudnessApplyResult {
        in_lufs,
        out_lufs,
        gain_db: effective_gain_db,
        true_peak_in_dbtp: tp_in,
        true_peak_out_dbtp: tp_out,
        limiter_gain_reduction_db,
        warnings,
    })
}

fn measure_lufs(buffer: &AudioBuffer) -> Result<f64> {
    let channels = buffer.channels_count() as u32;
    let mut meter = EbuR128::new(channels, buffer.sample_rate, Mode::I)
        .context("failed to initialize ebur128 loudness meter")?;

    let planar: Vec<&[f64]> = buffer.channels.iter().map(Vec::as_slice).collect();
    meter
        .add_frames_planar_f64(&planar)
        .context("failed to feed frames into ebur128 loudness meter")?;
    meter
        .loudness_global()
        .context("failed to query integrated loudness")
}

pub fn measure_true_peak_dbtp(buffer: &AudioBuffer) -> Result<f64> {
    let channels = buffer.channels_count() as u32;
    let mut meter = EbuR128::new(channels, buffer.sample_rate, Mode::TRUE_PEAK)
        .context("failed to initialize ebur128 true-peak meter")?;

    let planar: Vec<&[f64]> = buffer.channels.iter().map(Vec::as_slice).collect();
    meter
        .add_frames_planar_f64(&planar)
        .context("failed to feed frames into ebur128 true-peak meter")?;

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

/// Poll interval for the momentary/short-term meters. EBU Tech 3341 refreshes the
/// momentary meter at least every 100 ms, so we hop at that rate and track the maxima.
const METER_HOP_MS: f64 = 100.0;

/// One-pass loudness + dynamics analysis (integrated, LRA, max momentary/short-term,
/// true peak). Audio is fed in 100 ms hops so the sliding momentary/short-term meters can
/// be polled for their maxima; integrated, LRA and true peak are read once at the end.
pub fn analyze(buffer: &AudioBuffer) -> Result<LoudnessAnalysis> {
    let channels = buffer.channels_count() as u32;
    let rate = buffer.sample_rate;
    // LRA ⊇ S ⊇ M, and TRUE_PEAK adds the peak meter — one meter covers every figure.
    let mut meter = EbuR128::new(channels, rate, Mode::I | Mode::LRA | Mode::TRUE_PEAK)
        .context("failed to initialize ebur128 meter")?;

    let frames = buffer.frame_len();
    let hop = ((rate as f64 * METER_HOP_MS / 1000.0) as usize).max(1);
    // A window's value is only meaningful once that much audio has been fed.
    let momentary_ready = (rate as f64 * 0.4) as usize;
    let short_term_ready = (rate as f64 * 3.0) as usize;

    let mut max_momentary = f64::NEG_INFINITY;
    let mut max_short_term = f64::NEG_INFINITY;

    let mut pos = 0;
    while pos < frames {
        let end = (pos + hop).min(frames);
        let slabs: Vec<&[f64]> = buffer.channels.iter().map(|c| &c[pos..end]).collect();
        meter
            .add_frames_planar_f64(&slabs)
            .context("failed to feed frames into ebur128 meter")?;

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
        pos = end;
    }

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
fn limit_and_verify(buffer: &mut AudioBuffer, ceiling_dbtp: f64) -> Result<(f64, usize)> {
    let mut internal_ceiling = ceiling_dbtp;
    let mut tp = f64::INFINITY;

    for pass in 1..=MAX_LIMIT_PASSES {
        limiter::apply_true_peak_limit(buffer, internal_ceiling)?;
        tp = measure_true_peak_dbtp(buffer)?;

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
) -> Result<Vec<String>> {
    let tp = measure_true_peak_dbtp(buffer)?;
    if tp <= ceiling_dbtp {
        return Ok(Vec::new());
    }

    match on_clip {
        OnClipPolicy::Limit => {
            let (final_tp, passes) = limit_and_verify(buffer, ceiling_dbtp)?;
            let mut warnings = vec![format!(
                "true peak {tp:.2} dBTP exceeded ceiling {ceiling_dbtp:.2} dBTP; limiter applied"
            )];
            if final_tp - ceiling_dbtp > TP_TOLERANCE_DB {
                warnings.push(format!(
                    "true peak still {final_tp:.2} dBTP after {passes} limiter passes"
                ));
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
            Ok(vec![format!(
                "true peak {tp:.2} dBTP exceeded ceiling {ceiling_dbtp:.2} dBTP; \
                 reduced gain by {gain_db:.2} dB"
            )])
        }
        OnClipPolicy::Warn => Ok(vec![format!(
            "true peak {tp:.2} dBTP exceeds ceiling {ceiling_dbtp:.2} dBTP; \
             not corrected (on_clip=warn)"
        )]),
    }
}

/// Re-measure the true peak after an operation that can introduce new inter-sample peaks
/// (most importantly sample-rate conversion) and apply the limiter if the ceiling is exceeded.
///
/// SRC filters can produce inter-sample peaks up to ~3 dBTP above the peak of the input;
/// this function ensures the ceiling remains enforced without requiring a manual second
/// loudness step in the pipeline config.
///
/// Returns warnings if the limiter was actually invoked.
pub fn recheck_and_limit_if_needed(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
) -> Result<Vec<String>> {
    let tp = measure_true_peak_dbtp(buffer)?;
    if tp <= ceiling_dbtp {
        return Ok(Vec::new());
    }
    let (final_tp, passes) = limit_and_verify(buffer, ceiling_dbtp)?;
    let mut warnings = vec![format!(
        "true peak {tp:.2} dBTP exceeded ceiling {ceiling_dbtp:.2} dBTP after SRC; limiter re-applied"
    )];
    if final_tp - ceiling_dbtp > TP_TOLERANCE_DB {
        warnings.push(format!(
            "true peak still {final_tp:.2} dBTP after {passes} limiter passes"
        ));
    }
    Ok(warnings)
}

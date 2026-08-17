//! What a render found worth saying, as data.
//!
//! A warning used to be a `String` pushed into `ProcessingReport.warnings` at one of eighteen
//! sites across `pipeline` and `ops::loudness`. That is right for a console and wrong for
//! everything else: a reader cannot group, count, filter or colour by kind, and cannot ask
//! *is this the same warning as that one* without comparing sentences. `docs/gui-proposal.md`
//! §6 Tier 3 asks for "structured `WarningKind`/`FailureKind` alongside the prose", and
//! *alongside* is the operative word — the enum does not replace the sentence, it **owns**
//! it. [`WarningKind::message`] is the single producer of every one of those eighteen
//! strings, moved here byte for byte.
//!
//! Two rules, both learned from the surfaces this imitates:
//!
//! - **The kind is the warning.** There is no `Warning { kind, message }` wrapper, for the
//!   same reason [`crate::pipeline::Conflict`] is not one: a struct pairing a kind with a
//!   message invites the two to disagree, and the second field is a function of the first.
//! - **Every variant carries the figures its sentence quotes, never the sentence.** The one
//!   thing that must not happen here is a producer building prose and handing it in — that
//!   is the `Vec<String>` this module exists to delete, wearing an enum.
//!
//! [`FailureKind`] is the other half and works the other way round. mint has ~50 `bail!`
//! sites and rewriting them into an error enum would be a large change that buys a front end
//! nothing it cannot get more cheaply: [`FailureKind::classify`] walks a live
//! `anyhow::Error`'s chain with `downcast_ref` and answers *what kind of failure was that*
//! without ever looking at the message. `progress::Cancelled` was already doing exactly this
//! (its doc says "so a caller can tell 'the user cancelled' from 'the render failed' without
//! matching on message text"); this generalizes it to the failures a front end actually
//! branches on.

use std::fmt;
use std::path::PathBuf;

use crate::audio::buffer::OutputSampleFormat;
use crate::config::{DeliveryBlocker, format_tag};
use crate::ops::dsd::modulator::MAX_MODULATION;
use crate::ops::loudness::LUFS_TOLERANCE;
use crate::pipeline::{Conflict, ConflictError, DeliveryBlockedError};
use crate::progress::Cancelled;

/// What a warning is *about*, for a reader grouping or ordering a pile of them. Coarser than
/// the kind on purpose — this is the axis a report groups on, not a second classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WarningSubject {
    /// The delivered loudness, or what reaching it cost.
    Loudness,
    /// The true-peak ceiling.
    Peak,
    /// The bit-depth reduction.
    Dither,
    /// The sigma-delta modulator.
    Dsd,
}

impl WarningSubject {
    pub fn label(self) -> &'static str {
        match self {
            Self::Loudness => "loudness",
            Self::Peak => "peak",
            Self::Dither => "dither",
            Self::Dsd => "dsd",
        }
    }

    pub const ALL: [WarningSubject; 4] =
        [Self::Loudness, Self::Peak, Self::Dither, Self::Dsd];
}

/// One thing a render decided was worth saying.
///
/// Exhaustive and not `#[non_exhaustive]`: a new warning must be a compile error in every
/// renderer that switches on this, the same bargain the `match target.codec` arms make.
#[derive(Debug, Clone, PartialEq)]
pub enum WarningKind {
    // ── loudness normalization (ops::loudness::apply_with) ────────────────────────────
    /// `on_clip = reduce_gain`: the gain was capped to protect the ceiling, so the LUFS
    /// target is knowingly not reached.
    ReduceGainCapped { predicted_dbtp: f64, ceiling_dbtp: f64 },
    /// `on_clip = warn`: the peak will exceed the ceiling and nothing was done about it.
    PredictedPeakOverCeiling { predicted_dbtp: f64, ceiling_dbtp: f64 },
    /// `maximize` ran out of `max_limiting_db` before it reached the target.
    MaximizationCapped { max_limiting_db: f64, target_lufs: f64, delivered_lufs: f64 },

    // ── ceiling enforcement (ops::loudness) ───────────────────────────────────────────
    /// The verification loop gave up with the meter still over the ceiling.
    ///
    /// `ceiling_dbtp` is `Some` when the sentence names it and `None` when the sentence it
    /// is attached to has already named it one line above. Two spellings of one fact, kept
    /// apart because collapsing them would reword four shipped warnings; unify them in a
    /// change that is *about* the wording, not in the one that introduced this type.
    ResidualTruePeak { measured_dbtp: f64, passes: usize, ceiling_dbtp: Option<f64> },
    /// A pure transcode was over the ceiling and the limiter brought it down. `after_src`
    /// distinguishes the post-resample re-application from the first one.
    CeilingEnforcedByLimiter { measured_dbtp: f64, ceiling_dbtp: f64, after_src: bool },
    /// `on_clip = reduce_gain` on a pure transcode: one static attenuation, dynamics intact.
    CeilingEnforcedByGain { measured_dbtp: f64, ceiling_dbtp: f64, gain_db: f64 },
    /// `on_clip = warn` on a pure transcode: reported, not corrected.
    CeilingNotEnforced { measured_dbtp: f64, ceiling_dbtp: f64 },

    // ── after the resample (ops::loudness::correct_loudness_after_resample) ───────────
    /// Band-limiting moved the integrated loudness and a static gain pulled it back.
    /// `src_drift_lu` is `None` when the pre-SRC figure was unmeasurable, and
    /// `capped_by_ceiling` is `Some(ceiling)` when the headroom ran out before the target.
    SrcLoudnessCorrected {
        measured_lufs: f64,
        target_lufs: f64,
        applied_db: f64,
        src_drift_lu: Option<f64>,
        capped_by_ceiling: Option<f64>,
    },

    // ── pipeline decisions ────────────────────────────────────────────────────────────
    /// The loudness target was reached, and this is what it cost in dynamics.
    HeavyLimiting {
        gain_reduction_db: f64,
        target_lufs: f64,
        ceiling_dbtp: f64,
        source_true_peak_dbtp: f64,
    },
    /// `on_clip = maximize` with no `lufs` to maximize towards.
    MaximizeWithoutLufs,
    /// The delivered loudness missed its target by more than [`LUFS_TOLERANCE`].
    LoudnessTargetMissed { target_lufs: f64, delivered_lufs: f64 },
    /// Peak modulation past the 20/28 rule — only reachable from PCM hotter than 0 dBFS.
    DsdOverModulation { max_modulation_percent: f64 },
    /// The sigma-delta loop hit its state clamp.
    DsdSigmaDeltaOverload { overload_samples: u64, samples: u64, resets: u64 },
    /// `dither = "none"` into s16.
    UnditheredReduction { from: OutputSampleFormat },
}

impl WarningKind {
    /// Stable snake_case discriminant: the JSON key, the grouping key, and the only thing a
    /// front end may switch on textually. Never shown to a user — that is [`Self::message`].
    pub fn tag(&self) -> &'static str {
        match self {
            Self::ReduceGainCapped { .. } => "reduce_gain_capped",
            Self::PredictedPeakOverCeiling { .. } => "predicted_peak_over_ceiling",
            Self::MaximizationCapped { .. } => "maximization_capped",
            Self::ResidualTruePeak { .. } => "residual_true_peak",
            Self::CeilingEnforcedByLimiter { .. } => "ceiling_enforced_by_limiter",
            Self::CeilingEnforcedByGain { .. } => "ceiling_enforced_by_gain",
            Self::CeilingNotEnforced { .. } => "ceiling_not_enforced",
            Self::SrcLoudnessCorrected { .. } => "src_loudness_corrected",
            Self::HeavyLimiting { .. } => "heavy_limiting",
            Self::MaximizeWithoutLufs => "maximize_without_lufs",
            Self::LoudnessTargetMissed { .. } => "loudness_target_missed",
            Self::DsdOverModulation { .. } => "dsd_over_modulation",
            Self::DsdSigmaDeltaOverload { .. } => "dsd_sigma_delta_overload",
            Self::UnditheredReduction { .. } => "undithered_reduction",
        }
    }

    pub fn subject(&self) -> WarningSubject {
        match self {
            Self::ReduceGainCapped { .. }
            | Self::MaximizationCapped { .. }
            | Self::SrcLoudnessCorrected { .. }
            | Self::HeavyLimiting { .. }
            | Self::MaximizeWithoutLufs
            | Self::LoudnessTargetMissed { .. } => WarningSubject::Loudness,
            Self::PredictedPeakOverCeiling { .. }
            | Self::ResidualTruePeak { .. }
            | Self::CeilingEnforcedByLimiter { .. }
            | Self::CeilingEnforcedByGain { .. }
            | Self::CeilingNotEnforced { .. } => WarningSubject::Peak,
            Self::UnditheredReduction { .. } => WarningSubject::Dither,
            Self::DsdOverModulation { .. } | Self::DsdSigmaDeltaOverload { .. } => {
                WarningSubject::Dsd
            }
        }
    }

    /// **The single producer of every warning mint prints.** Each arm is the `format!` that
    /// used to sit at the site that raises it, moved rather than copied.
    ///
    /// `tests/warnings.rs` pins the bytes; `README.md` quotes none of them, but `--json`
    /// carries them and the GUI's Report view prints them whole.
    pub fn message(&self) -> String {
        match self {
            Self::ReduceGainCapped { predicted_dbtp, ceiling_dbtp } => format!(
                "clip policy reduce_gain applied; target LUFS may not be reached \
                 (predicted {predicted_dbtp:.2} dBTP > ceiling {ceiling_dbtp:.2} dBTP)"
            ),
            Self::PredictedPeakOverCeiling { predicted_dbtp, ceiling_dbtp } => format!(
                "predicted true peak {predicted_dbtp:.2} dBTP exceeds ceiling \
                 {ceiling_dbtp:.2} dBTP"
            ),
            Self::MaximizationCapped { max_limiting_db, target_lufs, delivered_lufs } => format!(
                "maximization capped: reached max_limiting_db {max_limiting_db:.1} dB before \
                 {target_lufs:.1} LUFS (delivered {delivered_lufs:.2} LUFS); raise \
                 max_limiting_db or lower the target"
            ),
            Self::ResidualTruePeak { measured_dbtp, passes, ceiling_dbtp } => match ceiling_dbtp {
                Some(ceiling) => format!(
                    "true peak still {measured_dbtp:.2} dBTP after {passes} limiter passes \
                     (ceiling {ceiling:.2} dBTP)"
                ),
                None => format!(
                    "true peak still {measured_dbtp:.2} dBTP after {passes} limiter passes"
                ),
            },
            Self::CeilingEnforcedByLimiter { measured_dbtp, ceiling_dbtp, after_src } => {
                if *after_src {
                    format!(
                        "true peak {measured_dbtp:.2} dBTP exceeded ceiling \
                         {ceiling_dbtp:.2} dBTP after SRC; limiter re-applied"
                    )
                } else {
                    format!(
                        "true peak {measured_dbtp:.2} dBTP exceeded ceiling \
                         {ceiling_dbtp:.2} dBTP; limiter applied"
                    )
                }
            }
            Self::CeilingEnforcedByGain { measured_dbtp, ceiling_dbtp, gain_db } => format!(
                "true peak {measured_dbtp:.2} dBTP exceeded ceiling {ceiling_dbtp:.2} dBTP; \
                 reduced gain by {gain_db:.2} dB"
            ),
            Self::CeilingNotEnforced { measured_dbtp, ceiling_dbtp } => format!(
                "true peak {measured_dbtp:.2} dBTP exceeds ceiling {ceiling_dbtp:.2} dBTP; \
                 not corrected (on_clip=warn)"
            ),
            Self::SrcLoudnessCorrected {
                measured_lufs,
                target_lufs,
                applied_db,
                src_drift_lu,
                capped_by_ceiling,
            } => {
                let drift = match src_drift_lu {
                    Some(lu) => format!("{lu:+.2} LU of that across the resample"),
                    None => "source loudness unmeasurable".to_string(),
                };
                let capped = match capped_by_ceiling {
                    Some(ceiling) => format!(", capped by the {ceiling:.2} dBTP ceiling"),
                    None => String::new(),
                };
                format!(
                    "delivered loudness was {measured_lufs:.2} LUFS against a \
                     {target_lufs:.2} target ({drift}); corrected by {applied_db:+.2} dB{capped}"
                )
            }
            Self::HeavyLimiting {
                gain_reduction_db,
                target_lufs,
                ceiling_dbtp,
                source_true_peak_dbtp,
            } => format!(
                "heavy limiting: {gain_reduction_db:.1} dB of peak gain reduction to reach \
                 {target_lufs:.1} LUFS under the {ceiling_dbtp:.1} dBTP ceiling (source true \
                 peak {source_true_peak_dbtp:.1} dBTP); consider a lower target or more \
                 source headroom"
            ),
            Self::MaximizeWithoutLufs => {
                "on_clip=maximize has no effect without lufs; behaving as limit".to_string()
            }
            // Reads `LUFS_TOLERANCE` rather than spelling 0.1, so this sentence and
            // `loudness::meets_target`'s rule cannot come to disagree about the size of a miss.
            Self::LoudnessTargetMissed { target_lufs, delivered_lufs } => format!(
                "loudness target not met within +/-{LUFS_TOLERANCE:.1} LU \
                 (target {target_lufs:.2}, delivered {delivered_lufs:.2})"
            ),
            Self::DsdOverModulation { max_modulation_percent } => format!(
                "peak modulation {max_modulation_percent:.1} % exceeds the format maximum of \
                 {:.2} % (the 20/28 rule); lower ceiling_dbtp",
                MAX_MODULATION * 100.0
            ),
            Self::DsdSigmaDeltaOverload { overload_samples, samples, resets } => format!(
                "sigma-delta overload on {overload_samples} of {samples} samples \
                 ({resets} loop reset(s)); the modulator clipped rather than diverged, but \
                 the tail of the level distribution is past what a 1-bit loop can track"
            ),
            Self::UnditheredReduction { from } => format!(
                "undithered bit-depth reduction {} -> s16 requested (dither=none); this can \
                 introduce correlated quantization distortion on low-level material",
                format_tag(*from)
            ),
        }
    }
}

impl fmt::Display for WarningKind {
    /// So a caller that only wants the sentence never has to know the enum exists — which is
    /// what let all eighteen sites change without their consumers changing.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

/// What kind of failure ended a job, recovered from a live `anyhow::Error`.
///
/// Deliberately a *classifier* rather than an error type. mint's failures are `anyhow` all
/// the way down and their prose is already single-producer where it matters
/// ([`Conflict::message`], [`DeliveryBlocker`]); what a front end lacks is not better text
/// but the ability to branch — offer `--force` here, select a target there, say "cancelled"
/// rather than "failed". Each variant below corresponds to a branch some caller actually
/// takes; `Other` is the honest answer for the rest, and adding a variant means a consumer
/// asked for one.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureKind {
    /// The user stopped it. Not a failure at all, and the one case that must never be
    /// coloured like one.
    Cancelled,
    /// A file would be destroyed. Carries which of the two, because the remedies differ:
    /// `Existing` is what `--force` is for and `Input` is what it is not.
    OutputConflict(Conflict),
    /// An input the target cannot deliver — a rule that needs the source, so it cannot be
    /// caught at config load.
    DeliveryBlocked(DeliveryBlocker),
    /// Two jobs in one batch resolved onto one output path, or one job's output is another
    /// job's input. Refused before anything runs, and refused even under `--force`.
    BatchCollision { over_input: bool },
    /// Anything else: a config that does not resolve, an unreadable source, a writer that
    /// failed. Deliberately one bucket rather than five — mint has ~50 `bail!` sites and a
    /// variant only earns its place when a consumer branches on it. `Other` is the honest
    /// answer, and "render mint's message" is the right handling for all of it.
    Other,
}

impl FailureKind {
    /// Classify a live error by walking its chain with `downcast_ref`. **Never inspects a
    /// message string** — that is the whole point, and the reason the enabling changes
    /// attach types to errors rather than parse them back out.
    pub fn classify(error: &anyhow::Error) -> Self {
        for cause in error.chain() {
            if cause.downcast_ref::<Cancelled>().is_some() {
                return Self::Cancelled;
            }
            if let Some(refusal) = cause.downcast_ref::<ConflictError>() {
                return Self::OutputConflict(refusal.conflict);
            }
            if let Some(refusal) = cause.downcast_ref::<DeliveryBlockedError>() {
                return Self::DeliveryBlocked(refusal.blocker.clone());
            }
            if let Some(collision) = cause.downcast_ref::<BatchCollision>() {
                return Self::BatchCollision { over_input: collision.over_input() };
            }
        }
        Self::Other
    }

    pub fn tag(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::OutputConflict(Conflict::Input) => "output_is_the_input",
            Self::OutputConflict(Conflict::Existing) => "output_exists",
            Self::DeliveryBlocked(_) => "delivery_blocked",
            Self::BatchCollision { over_input: false } => "batch_output_collision",
            Self::BatchCollision { over_input: true } => "batch_writes_over_an_input",
            Self::Other => "other",
        }
    }

    /// Whether `--force` (or `overwrite = true`) makes this job possible.
    ///
    /// The one fact a front end offering a one-click fix must not guess. Only a
    /// pre-existing *output* is force-able: the input-master case and both batch-collision
    /// cases are refused under `--force` as well, because two jobs racing onto one path and
    /// a job rendering over another job's source are not consent problems.
    pub fn force_can_fix(&self) -> bool {
        matches!(self, Self::OutputConflict(Conflict::Existing))
    }
}

/// The two whole-batch refusals `config::check_planned_outputs` can raise, as an error.
///
/// Same bargain as [`crate::pipeline::ConflictError`]: `Display` is the sentence `bail!` used
/// to build, so `{}` and `{:#}` render exactly what they rendered before, and the error is
/// one link long so nothing is appended to it. Wrapping the old message in a `.context()`
/// with a marker underneath was the obvious move and the wrong one — it turns
/// `{:#}` into "…the job that reads it may or may not have got there first: batch plan
/// refused", which is a reworded diagnostic.
#[derive(Debug, Clone, thiserror::Error)]
pub enum BatchCollision {
    /// Two inputs render to one output path.
    #[error(
        "inputs '{}' and '{}' both render to '{}' for target '{target}' — they share a stem \
         and no naming placeholder carries the input's directory. Render the two sources in \
         separate runs (with different `dir`s), or rename one of them",
        first.display(), second.display(), output.display()
    )]
    SharedOutput { first: PathBuf, second: PathBuf, output: PathBuf, target: String },
    /// One job's output is another job's input.
    #[error(
        "target '{target}' would write '{}' over a different input of this same run; the job \
         that reads it may or may not have got there first",
        output.display()
    )]
    WritesOverAnInput { target: String, output: PathBuf },
}

impl BatchCollision {
    /// True when the collision is one job's output landing on another job's *input* — the
    /// half `--force` does not reach, because which job reads first decides the result.
    pub fn over_input(&self) -> bool {
        matches!(self, Self::WritesOverAnInput { .. })
    }
}


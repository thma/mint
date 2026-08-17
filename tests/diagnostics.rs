//! The structured diagnostics: `WarningKind` and `FailureKind`.
//!
//! Two properties, and the first one is why this file exists.
//!
//! 1. **The prose did not move.** Every sentence in `WarningKind::message` was lifted out of
//!    a `format!` at one of eighteen sites in `pipeline` and `ops::loudness`, and the bytes
//!    are asserted here as literals. A reworded warning is a change to what a render *says*
//!    about a master — visible in `--json`, in the console and in the GUI's Report view — and
//!    it should never happen as a side effect of moving code.
//! 2. **The classification is read, never guessed.** `FailureKind::classify` walks a live
//!    error's chain with `downcast_ref` and must never consult a message. Every case below
//!    is produced by making mint actually fail, not by hand-building an error, because the
//!    thing under test is precisely whether the type survives the trip.
//!
//! The exhaustive `match` in `every_variant_renders_its_own_sentence` is the tripwire: a new
//! `WarningKind` variant stops this file compiling, which is the only reliable way to make
//! someone write its sentence down here on purpose.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::config::{self, Config, DeliveryBlocker, PlannedOutput, ResolvedTarget};
use mint::diagnostics::{FailureKind, WarningKind, WarningSubject};
use mint::pipeline::{self, Conflict};
use mint::progress::{JobControl, Stage};

const RATE: u32 = 48_000;

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("diagnostics-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_master(path: &Path) -> PathBuf {
    write_master_at(path, RATE)
}

fn write_master_at(path: &Path, rate: u32) -> PathBuf {
    let frames = rate as usize / 4;
    let tone = |hz: f64| -> Vec<f64> {
        (0..frames)
            .map(|n| 0.4 * (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin())
            .collect()
    };
    let buffer = AudioBuffer {
        channels: vec![tone(997.0), tone(1245.0)],
        sample_rate: rate,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: 2,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    io_write::write_wav(path, &buffer, None).expect("write master");
    path.to_path_buf()
}

fn target_in(dir: &Path, name: &str, body: &str) -> ResolvedTarget {
    let dir = dir.display().to_string().replace('\\', "\\\\");
    Config::from_toml_str(&format!("[target.{name}]\ndir = \"{dir}\"\n{body}"))
        .unwrap_or_else(|e| panic!("config: {e:#}"))
        .resolve_target(name)
        .expect("resolve")
}

// ═══════════════════════════ WarningKind ═══════════════════════════

/// Every variant, its exact sentence, its tag and its subject.
///
/// The `match` is exhaustive on purpose and the arms construct rather than destructure, so a
/// new variant is a compile error here before it is a missing line in a report.
#[test]
fn every_variant_renders_its_own_sentence() {
    // (variant, expected message). The messages are the pre-refactor `format!` output,
    // character for character.
    let cases: Vec<(WarningKind, &str)> = vec![
        (
            WarningKind::ReduceGainCapped { predicted_dbtp: -0.5, ceiling_dbtp: -1.0 },
            "clip policy reduce_gain applied; target LUFS may not be reached \
             (predicted -0.50 dBTP > ceiling -1.00 dBTP)",
        ),
        (
            WarningKind::PredictedPeakOverCeiling { predicted_dbtp: -0.5, ceiling_dbtp: -1.0 },
            "predicted true peak -0.50 dBTP exceeds ceiling -1.00 dBTP",
        ),
        (
            WarningKind::MaximizationCapped {
                max_limiting_db: 6.5,
                target_lufs: -9.0,
                delivered_lufs: -10.25,
            },
            "maximization capped: reached max_limiting_db 6.5 dB before -9.0 LUFS \
             (delivered -10.25 LUFS); raise max_limiting_db or lower the target",
        ),
        (
            WarningKind::ResidualTruePeak {
                measured_dbtp: -0.8,
                passes: 3,
                ceiling_dbtp: Some(-1.0),
            },
            "true peak still -0.80 dBTP after 3 limiter passes (ceiling -1.00 dBTP)",
        ),
        (
            WarningKind::ResidualTruePeak { measured_dbtp: -0.8, passes: 3, ceiling_dbtp: None },
            "true peak still -0.80 dBTP after 3 limiter passes",
        ),
        (
            WarningKind::CeilingEnforcedByLimiter {
                measured_dbtp: -0.2,
                ceiling_dbtp: -1.0,
                after_src: false,
            },
            "true peak -0.20 dBTP exceeded ceiling -1.00 dBTP; limiter applied",
        ),
        (
            WarningKind::CeilingEnforcedByLimiter {
                measured_dbtp: -0.2,
                ceiling_dbtp: -1.0,
                after_src: true,
            },
            "true peak -0.20 dBTP exceeded ceiling -1.00 dBTP after SRC; limiter re-applied",
        ),
        (
            WarningKind::CeilingEnforcedByGain {
                measured_dbtp: -0.2,
                ceiling_dbtp: -1.0,
                gain_db: -0.8,
            },
            "true peak -0.20 dBTP exceeded ceiling -1.00 dBTP; reduced gain by -0.80 dB",
        ),
        (
            WarningKind::CeilingNotEnforced { measured_dbtp: -0.2, ceiling_dbtp: -1.0 },
            "true peak -0.20 dBTP exceeds ceiling -1.00 dBTP; not corrected (on_clip=warn)",
        ),
        (
            WarningKind::SrcLoudnessCorrected {
                measured_lufs: -16.54,
                target_lufs: -16.0,
                applied_db: 0.3,
                src_drift_lu: Some(-0.54),
                capped_by_ceiling: Some(-1.0),
            },
            "delivered loudness was -16.54 LUFS against a -16.00 target \
             (-0.54 LU of that across the resample); corrected by +0.30 dB, \
             capped by the -1.00 dBTP ceiling",
        ),
        (
            WarningKind::SrcLoudnessCorrected {
                measured_lufs: -16.54,
                target_lufs: -16.0,
                applied_db: 0.54,
                src_drift_lu: None,
                capped_by_ceiling: None,
            },
            "delivered loudness was -16.54 LUFS against a -16.00 target \
             (source loudness unmeasurable); corrected by +0.54 dB",
        ),
        (
            WarningKind::HeavyLimiting {
                gain_reduction_db: 4.2,
                target_lufs: -14.0,
                ceiling_dbtp: -1.0,
                source_true_peak_dbtp: -0.2,
            },
            "heavy limiting: 4.2 dB of peak gain reduction to reach -14.0 LUFS under the \
             -1.0 dBTP ceiling (source true peak -0.2 dBTP); consider a lower target or \
             more source headroom",
        ),
        (
            WarningKind::MaximizeWithoutLufs,
            "on_clip=maximize has no effect without lufs; behaving as limit",
        ),
        (
            WarningKind::LoudnessTargetMissed { target_lufs: -14.0, delivered_lufs: -14.35 },
            "loudness target not met within +/-0.1 LU (target -14.00, delivered -14.35)",
        ),
        (
            WarningKind::DsdOverModulation { max_modulation_percent: 75.0 },
            "peak modulation 75.0 % exceeds the format maximum of 71.43 % (the 20/28 rule); \
             lower ceiling_dbtp",
        ),
        (
            WarningKind::DsdSigmaDeltaOverload {
                overload_samples: 702,
                samples: 131_072,
                resets: 3,
            },
            "sigma-delta overload on 702 of 131072 samples (3 loop reset(s)); the modulator \
             clipped rather than diverged, but the tail of the level distribution is past \
             what a 1-bit loop can track",
        ),
        (
            WarningKind::UnditheredReduction { from: OutputSampleFormat::F32 },
            "undithered bit-depth reduction f32 -> s16 requested (dither=none); this can \
             introduce correlated quantization distortion on low-level material",
        ),
    ];

    for (warning, expected) in &cases {
        assert_eq!(&warning.message(), expected, "the sentence for {}", warning.tag());
        // `Display` is what every existing consumer prints through; it must be the message
        // and nothing decorated.
        assert_eq!(&warning.to_string(), expected);
    }

    // The tripwire. Exhaustive, so a new variant fails to compile here; every arm then has
    // to be findable in `cases` above, so it also has to have its sentence written down.
    let covered: Vec<&str> = cases.iter().map(|(w, _)| w.tag()).collect();
    let sample = WarningKind::MaximizeWithoutLufs;
    let all_tags: Vec<&str> = match sample {
        WarningKind::ReduceGainCapped { .. }
        | WarningKind::PredictedPeakOverCeiling { .. }
        | WarningKind::MaximizationCapped { .. }
        | WarningKind::ResidualTruePeak { .. }
        | WarningKind::CeilingEnforcedByLimiter { .. }
        | WarningKind::CeilingEnforcedByGain { .. }
        | WarningKind::CeilingNotEnforced { .. }
        | WarningKind::SrcLoudnessCorrected { .. }
        | WarningKind::HeavyLimiting { .. }
        | WarningKind::MaximizeWithoutLufs
        | WarningKind::LoudnessTargetMissed { .. }
        | WarningKind::DsdOverModulation { .. }
        | WarningKind::DsdSigmaDeltaOverload { .. }
        | WarningKind::UnditheredReduction { .. } => vec![
            "reduce_gain_capped",
            "predicted_peak_over_ceiling",
            "maximization_capped",
            "residual_true_peak",
            "ceiling_enforced_by_limiter",
            "ceiling_enforced_by_gain",
            "ceiling_not_enforced",
            "src_loudness_corrected",
            "heavy_limiting",
            "maximize_without_lufs",
            "loudness_target_missed",
            "dsd_over_modulation",
            "dsd_sigma_delta_overload",
            "undithered_reduction",
        ],
    };
    for tag in &all_tags {
        assert!(covered.contains(tag), "{tag} has no sentence pinned in this file");
    }
    // ...and no tag is spelled twice, since the tag is the JSON key and the grouping key.
    let mut sorted = all_tags.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all_tags.len(), "two variants share a tag");
}

#[test]
fn every_subject_is_reachable_and_labelled() {
    // A subject nothing maps to is a legend entry for a group that can never appear.
    let representatives = [
        (WarningSubject::Loudness, WarningKind::MaximizeWithoutLufs),
        (
            WarningSubject::Peak,
            WarningKind::CeilingNotEnforced { measured_dbtp: -0.2, ceiling_dbtp: -1.0 },
        ),
        (
            WarningSubject::Dither,
            WarningKind::UnditheredReduction { from: OutputSampleFormat::F32 },
        ),
        (WarningSubject::Dsd, WarningKind::DsdOverModulation { max_modulation_percent: 75.0 }),
    ];
    assert_eq!(WarningSubject::ALL.len(), representatives.len());
    for (subject, warning) in &representatives {
        assert_eq!(&warning.subject(), subject, "{}", warning.tag());
        assert!(WarningSubject::ALL.contains(subject));
        assert!(!subject.label().is_empty());
    }
}

/// The two spellings of the residual-peak warning are one variant with an `Option`, and the
/// difference between them is exactly one clause. Pinned because the tempting cleanup —
/// making the ceiling non-optional — silently rewords two shipped warnings.
#[test]
fn the_residual_peak_warning_keeps_both_of_its_spellings() {
    let with = WarningKind::ResidualTruePeak {
        measured_dbtp: -0.8,
        passes: 2,
        ceiling_dbtp: Some(-1.0),
    };
    let without =
        WarningKind::ResidualTruePeak { measured_dbtp: -0.8, passes: 2, ceiling_dbtp: None };
    assert_eq!(with.tag(), without.tag(), "one kind, two spellings");
    assert!(with.message().starts_with(&without.message()));
    assert_eq!(with.message().strip_prefix(&without.message()), Some(" (ceiling -1.00 dBTP)"));
}

// ═══════════════════════════ FailureKind ═══════════════════════════

/// A cancel is not a failure, and the classifier has to say so from the chain — the whole
/// reason `progress::Cancelled` is a type.
#[test]
fn a_cancelled_render_classifies_as_cancelled() {
    let dir = tmp("cancel");
    let input = write_master(&dir.join("master.wav"));
    let target = target_in(&dir, "t", "format = \"s16\"\nlufs = -14.0\n");

    let cancel = AtomicBool::new(false);
    let sink = |_: Stage, fraction: f64| {
        if fraction > 0.1 {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let err = pipeline::process_with(
        &input,
        &target,
        Some(1),
        JobControl { cancel: Some(&cancel), progress: Some(&sink) },
    )
    .expect_err("the sink cancels the job");

    assert_eq!(FailureKind::classify(&err), FailureKind::Cancelled);
    assert_eq!(FailureKind::classify(&err).tag(), "cancelled");
    assert!(!FailureKind::classify(&err).force_can_fix(), "a cancel is not a --force problem");
}

/// A pre-existing output and an output that *is* the input are two different verdicts with
/// two different remedies, and the GUI branches on exactly this.
#[test]
fn the_two_overwrite_conflicts_classify_apart_and_only_one_is_forceable() {
    let dir = tmp("conflict");
    let input = write_master(&dir.join("master.wav"));
    let target = target_in(&dir, "t", "format = \"s24\"\n");

    // First render succeeds and leaves a file at the output path.
    pipeline::process(&input, &target, Some(1)).expect("first render");
    let err = pipeline::process(&input, &target, Some(1)).expect_err("the second must refuse");
    let kind = FailureKind::classify(&err);
    assert_eq!(kind, FailureKind::OutputConflict(Conflict::Existing));
    assert_eq!(kind.tag(), "output_exists");
    assert!(kind.force_can_fix(), "replacing a previous render is what --force is for");
    // The message is still mint's, unchanged by having become a typed error.
    assert_eq!(format!("{err:#}"), format!("{err}"), "the refusal is one link, not a chain");
    assert!(format!("{err:#}").contains("output file already exists"), "{err:#}");

    // An output path that names the input itself: a different verdict, and not force-able.
    let onto_input = target_in(&dir, "u", "format = \"s24\"\nnaming = \"{stem}.{ext}\"\n");
    let onto_input = ResolvedTarget { dir: dir.to_path_buf(), ..onto_input };
    let err = pipeline::process(&input, &onto_input, Some(1)).expect_err("must refuse");
    let kind = FailureKind::classify(&err);
    assert_eq!(kind, FailureKind::OutputConflict(Conflict::Input));
    assert_eq!(kind.tag(), "output_is_the_input");
    assert!(!kind.force_can_fix(), "mint refuses this under --force too");
    assert!(format!("{err:#}").contains("refusing to overwrite the input file"), "{err:#}");
}

/// The rule that needs the input survives into the chain as the rule, not as its sentence.
#[test]
fn a_delivery_blocker_classifies_as_itself() {
    let dir = tmp("blocked");
    // The blocker has to come from the *input*, which is the whole reason it exists: an
    // explicit `rate = 176400` on a FLAC target is refused when the config loads, so this
    // path is only reachable from a target that follows a source mint's encoder cannot take.
    let input = write_master_at(&dir.join("master.wav"), 192_000);
    let target = target_in(&dir, "t", "format = \"s24\"\ncodec = \"flac\"\n");

    let err = pipeline::process(&input, &target, Some(1)).expect_err("must refuse");
    assert_eq!(
        FailureKind::classify(&err),
        FailureKind::DeliveryBlocked(DeliveryBlocker::FlacRate { rate: 192_000 })
    );
    assert_eq!(FailureKind::classify(&err).tag(), "delivery_blocked");
    // And the sentence the CLI prints is unchanged: one link, mint's own prefix and reason.
    assert_eq!(format!("{err:#}"), format!("{err}"));
    assert_eq!(
        format!("{err:#}"),
        format!("target 't': {}", DeliveryBlocker::FlacRate { rate: 192_000 })
    );
    // The dry-run's predicate and the run's refusal are the same answer, per `delivery_blocker`'s
    // own doc — asserted on the type now, not on two strings that happen to match.
    assert_eq!(
        target.delivery_blocker(192_000, Some(2)),
        Some(DeliveryBlocker::FlacRate { rate: 192_000 })
    );
}

/// The two whole-batch refusals, which no single job can see. Neither is force-able, and the
/// GUI has to tell them from a per-job conflict to decide whether to offer a fix at all.
#[test]
fn both_batch_collisions_classify_and_neither_is_forceable() {
    let dir = tmp("batch");
    let a = write_master(&dir.join("a.wav"));
    let b = write_master(&dir.join("b.wav"));
    let out = dir.join("out");

    // Two different inputs, one output path: the targets' naming drops the stem.
    let collide = PlannedOutput { input: &a, target: "t", output: out.join("fixed.wav") };
    let collide_b = PlannedOutput { input: &b, target: "t", output: out.join("fixed.wav") };
    let err = config::check_planned_outputs(&[collide, collide_b], &[a.clone(), b.clone()])
        .expect_err("two inputs on one path must be refused");
    let kind = FailureKind::classify(&err);
    assert_eq!(kind, FailureKind::BatchCollision { over_input: false });
    assert_eq!(kind.tag(), "batch_output_collision");
    assert!(!kind.force_can_fix());
    assert_eq!(format!("{err:#}"), format!("{err}"), "no marker text is appended");
    assert!(format!("{err:#}").contains("both render to"), "{err:#}");

    // One job's output lands on another job's input.
    let over = PlannedOutput { input: &a, target: "t", output: b.clone() };
    let err = config::check_planned_outputs(&[over], &[a.clone(), b.clone()])
        .expect_err("writing over another input must be refused");
    let kind = FailureKind::classify(&err);
    assert_eq!(kind, FailureKind::BatchCollision { over_input: true });
    assert_eq!(kind.tag(), "batch_writes_over_an_input");
    assert!(!kind.force_can_fix(), "mint refuses this even under --force");
    assert_eq!(format!("{err:#}"), format!("{err}"));
    assert!(format!("{err:#}").contains("over a different input"), "{err:#}");
}

/// Everything mint has no branch for is honestly `Other` — not a guess, and not a variant
/// invented because a string looked like one.
#[test]
fn an_unclassified_failure_is_other() {
    let dir = tmp("other");
    let target = target_in(&dir, "t", "format = \"s24\"\n");
    let err = pipeline::process(&dir.join("does-not-exist.wav"), &target, Some(1))
        .expect_err("a missing input must fail");
    assert_eq!(FailureKind::classify(&err), FailureKind::Other);
    assert_eq!(FailureKind::classify(&err).tag(), "other");
    assert!(!FailureKind::classify(&err).force_can_fix());
}

/// `classify` reads the chain, not the outermost error — so a refusal that later picks up a
/// `context` still classifies. This is the property a bare `downcast_ref` on the top-level
/// error would not have, and the reason the GUI stopped doing that.
#[test]
fn classification_survives_being_wrapped_in_context() {
    use anyhow::Context as _;

    let dir = tmp("wrapped");
    let input = write_master(&dir.join("master.wav"));
    let target = target_in(&dir, "t", "format = \"s24\"\n");
    pipeline::process(&input, &target, Some(1)).expect("first render");

    let err = pipeline::process(&input, &target, Some(1))
        .context("while rendering the album")
        .expect_err("the second must refuse");
    assert_eq!(
        FailureKind::classify(&err),
        FailureKind::OutputConflict(Conflict::Existing),
        "a context line must not hide the verdict"
    );
}

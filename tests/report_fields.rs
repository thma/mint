//! What a render *says it did*, checked against what it actually did.
//!
//! Everything here was computed inside `pipeline::process` and thrown away before
//! (report.md §9 item 6): the applied gain, the dither settings, the seed, the real file
//! size. A figure that is merely reported is easy to get subtly wrong and impossible to
//! notice, so each one is pinned against an independent measurement rather than against
//! itself — the size against `fs::metadata` *and* against the dry-run's own prediction,
//! the gain against the source and target loudness, the DSD frame count against the
//! container's `sampleCount`.
//!
//! The last test is the T1-3 pair: `pipeline::output_conflict` is a query and
//! `guard_output_path` is the enforcement of the same rule, and the only thing keeping
//! them from drifting is that one is implemented in terms of the other. This checks the
//! observable consequence — the query's verdict predicts whether the render is refused.

use std::path::{Path, PathBuf};

use mint::diagnostics::WarningKind;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::config::{Config, ResolvedTarget, SizeEstimate};
use mint::json_report;
use mint::ops::loudness;
use mint::pipeline::{self, Conflict};

const RATE: u32 = 48_000;
/// 0.1 s. Enough for anything that does not read a loudness figure, and short enough that
/// a debug-build DSD encode stays free.
const SHORT: usize = RATE as usize / 10;
/// 1.5 s. BS.1770's integrated measurement gates on 400 ms blocks, so a shorter clip
/// meters as `-inf` and every gain assertion turns into `inf == inf`.
const METERED: usize = RATE as usize * 3 / 2;

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("report-fields-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn tone(frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / RATE as f64).sin())
        .collect()
}

/// A stereo master on disk, quiet enough that no target here needs the limiter.
fn write_master(path: &Path, frames: usize) -> PathBuf {
    let buffer = AudioBuffer {
        channels: vec![tone(frames, 997.0, 0.2), tone(frames, 1245.0, 0.2)],
        sample_rate: RATE,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: 2,
            sample_rate: RATE,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    io_write::write_wav(path, &buffer, None).expect("write master");
    path.to_path_buf()
}

/// Resolve one target out of a config whose `dir` is the scratch directory. The path is
/// spliced into TOML, so a Windows separator has to survive being a TOML basic string.
fn target_in(dir: &Path, name: &str, body: &str) -> ResolvedTarget {
    let dir = dir.display().to_string().replace('\\', "\\\\");
    let toml = format!("[target.{name}]\ndir = \"{dir}\"\n{body}");
    Config::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("config did not resolve: {e:#}"))
        .resolve_target(name)
        .expect("resolve")
}

#[test]
fn the_reported_size_is_the_file_on_disk_and_the_size_the_dry_run_promised() {
    let dir = tmp("size");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s24\"\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    let on_disk = std::fs::metadata(&report.output).expect("stat").len();
    assert_eq!(report.file_bytes, on_disk, "the report must not predict, it must measure");

    // WAV claims `Exact`, so the dry-run and the render have to agree to the byte. This is
    // the first time the two numbers can be compared without a human reading both.
    let estimate = target
        .estimated_output_bytes(RATE, SHORT as u64, 2)
        .expect("an estimate for a WAV target");
    assert!(
        matches!(estimate, SizeEstimate::Exact(_)),
        "WAV claims an exact size: {estimate:?}"
    );
    assert_eq!(estimate.bytes(), report.file_bytes, "the dry-run promised a different size");

    assert_eq!(report.channels, 2);
    assert_eq!(report.frames, SHORT as u64, "no resample, so frames are unchanged");
}

#[test]
fn a_normalized_render_reports_the_gain_it_applied() {
    let dir = tmp("gain");
    let input = write_master(&dir.join("master.wav"), METERED);
    let target = target_in(&dir, "t", "format = \"s24\"\nlufs = -20.0\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    let gain = report.loudness_gain_db.expect("a target with lufs must report its gain");
    let expected = -20.0 - report.source.integrated_lufs;
    assert!(
        (gain - expected).abs() < 0.01,
        "reported {gain:.4} dB, but the source measured {:.4} LUFS against a -20.0 target \
         (expected {expected:.4} dB)",
        report.source.integrated_lufs
    );
    // Quiet source, generous target: nothing for the limiter to do, so the gain alone has
    // to land the delivered loudness. If it does not, the reported number is not the
    // number that was applied.
    assert!(
        (report.delivered.integrated_lufs - -20.0).abs() < 0.1,
        "delivered {:.3} LUFS",
        report.delivered.integrated_lufs
    );
    assert_eq!(
        report.post_resample_gain_db, None,
        "no resample ran, so the post-SRC correction never got the chance to"
    );
    assert_eq!(report.src_quality, None, "no SRC ran");
}

#[test]
fn a_pure_transcode_reports_no_gain_at_all() {
    let dir = tmp("nogain");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s16\"\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    assert_eq!(report.loudness_gain_db, None, "no lufs means no normalization step ran");
    assert_eq!(report.post_resample_gain_db, None);
}

#[test]
fn a_resampled_render_reports_the_quality_tier_and_the_post_src_correction() {
    let dir = tmp("src");
    let input = write_master(&dir.join("master.wav"), METERED);
    let target =
        target_in(&dir, "t", "format = \"s24\"\nrate = 44100\nlufs = -20.0\nquality = \"hq\"\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    assert_eq!(report.src_quality, Some("hq"), "the tier that actually ran, not the default");
    assert_eq!(report.out_rate, 44_100);
    // The correction pass ran; whether it had anything to do is the program's business,
    // but "ran and did nothing" (0.0) must be distinguishable from "never ran" (None).
    assert!(report.post_resample_gain_db.is_some(), "the correction pass ran, so it must report");
}

#[test]
fn the_dither_settings_reported_are_the_ones_that_were_asked_for() {
    let dir = tmp("dither");
    let input = write_master(&dir.join("master.wav"), SHORT);

    // s16: adaptive engages, so what was asked for and what happened agree.
    let engaged = target_in(&dir, "a", "format = \"s16\"\ndither = \"adaptive\"\n");
    let report = pipeline::process(&input, &engaged, Some(3)).expect("process");
    assert_eq!(report.dither_mode, Some("adaptive"));
    assert_eq!(report.dither_strength, Some("normal"));
    assert!(report.dithered);
    assert_eq!(report.shaping_curve, Some("adaptive"));

    // s24: the same request degrades to flat TPDF. Before this pair of fields existed, the
    // report was indistinguishable from a target that had simply asked for tpdf — which is
    // the exact case T1-2 was written for.
    let degraded = target_in(&dir, "b", "format = \"s24\"\ndither = \"adaptive\"\n");
    let report = pipeline::process(&input, &degraded, Some(3)).expect("process");
    assert_eq!(report.dither_mode, Some("adaptive"), "the request is reported as made");
    assert_eq!(report.shaping_curve, None, "...and the shaping is reported as skipped");
    assert!(report.dithered, "flat TPDF still ran");
}

#[test]
fn the_seed_is_reported_exactly_when_the_render_is_reproducible() {
    let dir = tmp("seed");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s16\"\n");

    let pinned = pipeline::process(&input, &target, Some(0xC0FFEE)).expect("process");
    assert_eq!(pinned.seed, Some(0xC0FFEE));

    let unpinned = target_in(&dir, "u", "format = \"s16\"\n");
    let report = pipeline::process(&input, &unpinned, None).expect("process");
    assert_eq!(report.seed, None, "an unseeded run must not claim to be reproducible");
}

#[test]
fn a_dsd_render_reports_the_delivered_1_bit_length_and_no_dither_settings() {
    let dir = tmp("dsd");
    let input = write_master(&dir.join("master.wav"), SHORT);
    // `dither` is set deliberately: on a DSD target it is silently ignored, and the report
    // must say so by omission rather than by echoing a setting that never ran.
    let target = target_in(&dir, "t", "codec = \"dsf\"\ndsd = \"dsd64\"\ndither = \"shaped\"\n");

    let report = pipeline::process(&input, &target, Some(5)).expect("process");
    let dsd = report.dsd.as_ref().expect("a dsf target reports its DSD figures");

    assert_eq!(report.dither_mode, None, "DSD has no bit-depth stage to configure");
    assert_eq!(report.dither_strength, None);
    assert!(!report.dithered);
    // The seed still matters here — it drives the modulator's comparator dither.
    assert_eq!(report.seed, Some(5));
    // A DSD target always resamples, even from a rate that needs no PCM conversion: the
    // interpolation up to the 1-bit rate is an SRC on the same tier.
    assert_eq!(report.src_quality, Some("vhq"));

    assert_eq!(
        report.frames, dsd.samples_per_channel,
        "the delivered length is the 1-bit one, not the intermediate PCM buffer's"
    );
    let on_disk = std::fs::metadata(&report.output).expect("stat").len();
    assert_eq!(report.file_bytes, on_disk);
    assert_eq!(dsd.file_bytes, on_disk, "the writer's own accounting must match the file");
}

/// The declared figures, and the verdict they make possible.
///
/// A report is read after the fact — from the console, from `--json`, from a persisted run
/// — and by then the config that stated `lufs` and `ceiling_dbtp` is out of reach. Without
/// them travelling with the report, `delivered.integrated_lufs` is a number nobody can grade
/// and every front end has to keep its own copy of the ±0.1 LU rule. So the verdict is
/// pinned here *against the warning the same render emitted*: those two are one judgement
/// asked at two moments, and the only thing keeping them together is that both go through
/// `loudness::meets_target`.
#[test]
fn a_report_carries_what_it_was_judged_against_and_agrees_with_its_own_warning() {
    let dir = tmp("verdict");
    let input = write_master(&dir.join("master.wav"), METERED);

    // Quiet source, generous target: reachable, so the verdict is a pass and the render is
    // silent about loudness.
    let met = target_in(&dir, "met", "format = \"s24\"\nlufs = -20.0\n");
    let report = pipeline::process(&input, &met, Some(1)).expect("process");
    assert_eq!(report.declared_lufs, Some(-20.0), "the target's own figure, verbatim");
    assert_eq!(report.declared_ceiling_dbtp, -1.0, "the built-in default ceiling");
    assert_eq!(report.lufs_verdict(), Some(true));
    assert!(report.ceiling_verdict(), "delivered {:.2} dBTP", report.delivered.true_peak_dbtp);
    assert!(
        !report.warnings.iter().any(|w| matches!(w, WarningKind::LoudnessTargetMissed { .. })),
        "a passing verdict must not sit next to the warning that says otherwise: {:?}",
        report.warnings
    );

    // A target louder than its own ceiling allows: the limiter holds the ceiling and the
    // loudness lands short. Verdict and warning must fail together.
    let missed = target_in(&dir, "missed", "format = \"s24\"\nlufs = -3.0\nceiling_dbtp = -9.0\n");
    let report = pipeline::process(&input, &missed, Some(1)).expect("process");
    assert_eq!(report.declared_lufs, Some(-3.0));
    assert_eq!(report.declared_ceiling_dbtp, -9.0, "the target's ceiling, not the default");
    assert_eq!(
        report.lufs_verdict(),
        Some(false),
        "delivered {:.2} LUFS against a -3.0 target",
        report.delivered.integrated_lufs
    );
    assert!(
        report.warnings.iter().any(|w| matches!(w, WarningKind::LoudnessTargetMissed { .. })),
        "the verdict and the warning are the same judgement: {:?}",
        report.warnings
    );
    // No ceiling assertion on this one: the limiter parks the peak *on* the ceiling and is
    // allowed 0.05 dB of slack there (`TP_TOLERANCE_DB`, the disagreement between its own
    // oversampled detector and the BS.1770 meter), while `ceiling_verdict` is a strict
    // `<=`. Landing at -8.99 against a -9.00 ceiling is a pass to the pipeline and a fail
    // to the verdict, so a test that pinned either answer would be pinning the tolerance.

    // A ceiling genuinely blown: `on_clip = "warn"` reports the overshoot instead of
    // correcting it, which is the one delivery mint ships knowingly over the ceiling.
    let over = target_in(&dir, "over", "format = \"s24\"\nceiling_dbtp = -20.0\non_clip = \"warn\"\n");
    let report = pipeline::process(&input, &over, Some(1)).expect("process");
    assert_eq!(report.declared_ceiling_dbtp, -20.0);
    assert!(
        !report.ceiling_verdict(),
        "delivered {:.2} dBTP is above the declared -20.0 ceiling",
        report.delivered.true_peak_dbtp
    );

    // No loudness asked for: `None`, which is not `Some(false)`. Nothing was promised, so
    // there is nothing to fail — a Report view that showed a red cross here would be
    // inventing a requirement.
    let silent = target_in(&dir, "silent", "format = \"s16\"\n");
    let report = pipeline::process(&input, &silent, Some(1)).expect("process");
    assert_eq!(report.declared_lufs, None);
    assert_eq!(report.lufs_verdict(), None);
    assert_eq!(report.declared_ceiling_dbtp, -1.0);
}

/// One line, drawn once: the warning and the verdict are the same judgement.
///
/// The render decides "loudness target not met" while the buffer is still in RAM; a Report
/// view decides whether to print a cross months later, off a persisted document. Both go
/// through `loudness::meets_target`, and nothing but that shared call keeps a render that
/// shipped clean from being graded as a failure by the screen that displays it. So this
/// asserts the *equivalence* over a spread of targets — reachable, unreachable, and none at
/// all — rather than a hand-written expectation per case, which would be a third opinion
/// about where the line is.
#[test]
fn the_loudness_warning_fires_exactly_when_the_verdict_fails() {
    let dir = tmp("verdict-agree");
    let input = write_master(&dir.join("master.wav"), METERED);
    let mut warnings_fired = Vec::new();

    for (name, body) in [
        // Comfortably reachable: quiet source, quiet target, nothing in the way.
        ("reachable", "format = \"s24\"\nlufs = -20.0\n"),
        // Reachable downwards, which is a different code path (a negative gain).
        ("quiet", "format = \"s24\"\nlufs = -30.0\n"),
        // Unreachable: the ceiling holds the delivery well below what was asked for.
        ("blocked", "format = \"s24\"\nlufs = -3.0\nceiling_dbtp = -9.0\n"),
        // Nothing promised, so nothing can be missed.
        ("silent", "format = \"s24\"\n"),
    ] {
        let target = target_in(&dir, name, body);
        let report = pipeline::process(&input, &target, Some(1)).expect("process");
        let warned = report.warnings.iter().any(|w| matches!(w, WarningKind::LoudnessTargetMissed { .. }));

        let expected = match report.declared_lufs {
            Some(declared) => !loudness::meets_target(report.delivered.integrated_lufs, declared),
            None => false,
        };
        assert_eq!(
            warned, expected,
            "target '{name}': delivered {:.3} LUFS against {:?}, warnings {:?}",
            report.delivered.integrated_lufs, report.declared_lufs, report.warnings
        );
        // ...and the column a front end renders is the same answer, with `None` reading as
        // "no requirement" rather than as a quiet pass.
        assert_eq!(
            matches!(report.lufs_verdict(), Some(false)),
            warned,
            "target '{name}': verdict {:?} beside warnings {:?}",
            report.lufs_verdict(),
            report.warnings
        );
        warnings_fired.push(warned);
    }

    // An equivalence over four silent renders would hold trivially. These four are chosen so
    // both sides of the line are actually crossed; if the spread ever stops crossing it, the
    // test above has quietly stopped testing anything.
    assert!(warnings_fired.contains(&true), "no target missed its loudness — the miss side is untested");
    assert!(warnings_fired.contains(&false), "every target warned — the pass side is untested");
}

/// The declared figures survive the trip through `--json`, at the schema version this build
/// promises.
///
/// The version is asserted as a **literal** rather than read from the constant: comparing the
/// constant against itself would agree with any future value of it, and this line is where a
/// removal, a rename or a change of meaning has to be thought about on purpose. Adding a key
/// must *not* move it — every reader has to tolerate unknown keys, and a bump strands the run
/// history the GUI persists in this exact schema.
///
/// It has moved once, to **2**, and this is the record of why: Tier 3 turned `warnings` from
/// an array of strings into an array of `{kind, subject, message}` objects and added
/// `failures[].kind`. The prose did not change and is still in `message` — the *element type*
/// did, which is the "meaning changes" case the constant's own doc names.
#[test]
fn the_json_task_carries_the_declared_figures_at_an_unchanged_schema_version() {
    let dir = tmp("json-declared");
    let input = write_master(&dir.join("master.wav"), METERED);

    let judged = target_in(&dir, "judged", "format = \"s24\"\nlufs = -20.0\nceiling_dbtp = -2.0\n");
    let ungraded = target_in(&dir, "ungraded", "format = \"s16\"\n");
    let reports = vec![
        pipeline::process(&input, &judged, Some(1)).expect("process"),
        pipeline::process(&input, &ungraded, Some(1)).expect("process"),
    ];

    let text = json_report::render(Some("mint.toml"), &reports, &[]).expect("render --json");
    let doc: serde_json::Value = serde_json::from_str(&text).expect("--json must be JSON");

    assert_eq!(
        doc["schema_version"].as_u64(),
        Some(2),
        "schema 2 is `warnings` as objects; anything further that removes, renames or \
         re-means a key bumps it again"
    );

    // Tasks are emitted in the order they were rendered, so these indices are the two
    // targets above.
    assert_eq!(doc["tasks"][0]["declared_lufs"].as_f64(), Some(-20.0));
    assert_eq!(doc["tasks"][0]["declared_ceiling_dbtp"].as_f64(), Some(-2.0));
    assert!(
        doc["tasks"][1]["declared_lufs"].is_null(),
        "a target that asked for no loudness must serialize as null, not as a number a \
         reader could grade the delivery against"
    );
    assert_eq!(
        doc["tasks"][1]["declared_ceiling_dbtp"].as_f64(),
        Some(-1.0),
        "the built-in ceiling is still a declared figure — every target has one"
    );
}

#[test]
fn the_conflict_query_predicts_what_the_render_will_refuse() {
    let dir = tmp("conflict");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s24\"\n");
    let output = target.render_output_path(&input, RATE).expect("output path");

    // Nothing there yet: the query says clear, and the render agrees.
    assert_eq!(pipeline::output_conflict(&input, &output, false), None);
    pipeline::process(&input, &target, Some(1)).expect("the first render must succeed");

    // Now something is: the query says so, and the same render is refused.
    assert_eq!(
        pipeline::output_conflict(&input, &output, false),
        Some(Conflict::Existing),
        "an existing output is a conflict once overwrite is off"
    );
    let err = pipeline::process(&input, &target, Some(1)).expect_err("the second must be refused");
    assert!(
        format!("{err:#}").contains("output file already exists"),
        "the guard must bail with the query's own message: {err:#}"
    );
    // ...and the message the GUI would show is the message the CLI prints.
    assert!(
        Conflict::Existing
            .message(&input, &output)
            .contains("output file already exists")
    );

    // Consent removes the conflict without moving the rule anywhere else.
    assert_eq!(pipeline::output_conflict(&input, &output, true), None);
    let overwriting = target_in(&dir, "t", "format = \"s24\"\noverwrite = true\n");
    pipeline::process(&input, &overwriting, Some(1)).expect("overwrite = true must be honoured");

    // The input case is a different conflict with a different message, and it is decided by
    // file identity rather than by spelling — hence the `./`-style detour to the same file.
    let spelled_differently = dir.join(".").join("master.wav");
    assert_eq!(
        pipeline::output_conflict(&input, &spelled_differently, false),
        Some(Conflict::Input),
        "the output resolving to the input master is its own case"
    );
    assert!(
        Conflict::Input
            .message(&input, &spelled_differently)
            .contains("refusing to overwrite the input file")
    );
}

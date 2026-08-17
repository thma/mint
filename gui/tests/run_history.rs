//! The two files the GUI owns beside the config: the run history and the project sidecar.
//!
//! The load-bearing property of a history file is that it is **not a GUI format**. It is
//! `json_report::render`'s output byte for byte, so `jq`, a QC script and the next version
//! of mint all read a persisted run exactly as they read `mint --json`. The first assertion
//! below is therefore a byte comparison and not a "contains the right keys" check: the
//! moment this crate reformats, re-orders or adds a key, the file stops being the document
//! it claims to be, and only a byte comparison notices.
//!
//! The second is the reason `SCHEMA_VERSION` exists at all. It is bumped when a key is
//! removed, renamed, or changes meaning — so counting `tasks` in a document from another
//! version is counting something that may not be what it is called. A foreign file is listed
//! and *said to be foreign*; it is never summarized anyway, and never silently skipped
//! either, which would look like it was never written.
//!
//! For the sidecar the failure to catch is subtler: an absent key is how it spells `None`,
//! so a saved-then-cleared `seed` that left its old value behind would silently restore a
//! *different* seed on the next launch, and a "reproducible" render would produce different
//! bits. Hence the clear-and-reload round trip at the end.

use std::path::PathBuf;

use mint::diagnostics::FailureKind;
use mint::ops::loudness::LoudnessAnalysis;
use mint::pipeline::{Conflict, ProcessingReport, TaskFailure};
use mint_gui::history::{self, Project};
use mint_gui::state::RunOptions;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-history-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn config(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("mint.toml");
    std::fs::write(&path, "[target.cd]\npreset = \"cd\"\n").expect("write config");
    path
}

fn one_report() -> ProcessingReport {
    let analysis = |integrated: f64| LoudnessAnalysis {
        integrated_lufs: integrated,
        loudness_range_lu: 7.1,
        max_momentary_lufs: integrated + 2.0,
        max_short_term_lufs: integrated + 1.0,
        true_peak_dbtp: -1.0,
    };
    ProcessingReport {
        target: "cd".into(),
        input: "/masters/take.wav".into(),
        output: "/out/take-cd.wav".into(),
        in_rate: 96_000,
        out_rate: 44_100,
        channels: 2,
        frames: 44_100,
        file_bytes: 176_400,
        elapsed_ms: 1234,
        source: analysis(-18.3),
        delivered: analysis(-14.0),
        declared_lufs: Some(-14.0),
        declared_ceiling_dbtp: -1.0,
        limiter_gain_reduction_db: Some(2.3),
        limiter_avg_gain_reduction_db: Some(0.8),
        limiter_active_percent: Some(4.1),
        loudness_gain_db: Some(3.4),
        post_resample_gain_db: Some(0.0),
        src_quality: Some("vhq"),
        dither_mode: Some("adaptive"),
        dither_strength: Some("low"),
        seed: Some(7),
        dithered: true,
        shaped: false,
        shaping_curve: None,
        codec: "wav".into(),
        bwf: false,
        format: "s24".into(),
        dsd: None,
        warnings: Vec::new(),
    }
}

#[test]
fn a_persisted_run_is_the_json_document_byte_for_byte() {
    let dir = scratch("persist");
    let config = config(&dir);
    let reports = vec![one_report()];
    let failures = vec![TaskFailure {
        target: "hires".into(),
        input: "/masters/take.wav".into(),
        error: "output file already exists".into(),
        kind: FailureKind::OutputConflict(Conflict::Existing),
    }];

    let path = history::persist(&config, Some("demo"), &reports, &failures).expect("persist");
    let written = std::fs::read_to_string(&path).expect("read it back");
    let expected = mint::json_report::render(Some("demo"), &reports, &failures).expect("render");
    assert_eq!(written, expected, "the history file is the --json document, unmodified");

    let document: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    assert_eq!(
        document["schema_version"].as_u64(),
        Some(u64::from(mint::json_report::SCHEMA_VERSION)),
        "and it says which schema it is"
    );

    // Two batches can finish inside one civil second; neither may overwrite the other.
    let second = history::persist(&config, Some("demo"), &reports, &failures).expect("persist");
    assert_ne!(path, second, "a run is never lost to a same-second collision");
}

#[test]
fn a_file_from_another_schema_is_listed_but_not_summarized() {
    let dir = scratch("schema");
    let config = config(&dir);
    history::persist(&config, Some("demo"), &[one_report()], &[]).expect("persist");

    let runs = history::runs_dir(&config);
    let alien = runs.join("1999-01-01T00-00-00.json");
    std::fs::write(&alien, r#"{"schema_version": 999, "tasks": [1,2,3], "failures": []}"#)
        .expect("write the foreign run");
    let junk = runs.join("1999-01-01T00-00-01.json");
    std::fs::write(&junk, "not json at all").expect("write the unreadable run");

    let listed = history::list(&config);
    assert_eq!(listed.len(), 3, "nothing is skipped: {:?}", listed.len());

    let alien = listed.iter().find(|file| file.path == alien).expect("the foreign run is listed");
    assert_eq!(alien.schema_version, Some(999));
    assert!(alien.summary.is_none(), "counting its tasks would be counting something else");

    let junk = listed.iter().find(|file| file.path == junk).expect("the unreadable run is listed");
    assert_eq!(junk.schema_version, None);
    assert!(junk.summary.is_none());

    let ours = listed.iter().find(|file| file.summary.is_some()).expect("ours is summarized");
    let summary = ours.summary.expect("checked above");
    assert_eq!((summary.tasks, summary.failures), (1, 0));
}

#[test]
fn the_sidecar_round_trips_and_an_unpinned_option_is_an_absent_key() {
    let dir = scratch("sidecar");
    let config = config(&dir);

    let project = Project {
        inputs: vec![PathBuf::from("/masters/a.wav"), PathBuf::from("/masters/b b.wav")],
        included: ["cd".to_string()].into_iter().collect(),
        run_options: RunOptions { seed: Some(42), jobs: None, force: true, fail_fast: false },
    };
    history::save_project(&config, &project).expect("save");
    let back = history::load_project(&config).expect("load");
    assert_eq!(back.inputs, project.inputs);
    assert_eq!(back.included, project.included);
    assert_eq!(back.run_options, project.run_options);

    let text = std::fs::read_to_string(history::sidecar_dir(&config).join("project.toml"))
        .expect("read the sidecar");
    assert!(text.contains("# Written by mint-gui"), "it says what it is: {text}");
    assert!(!text.contains("jobs"), "an unpinned option is an absent key, not a zero: {text}");

    // Clearing the seed must remove it. A stale value here restores a *different* seed, and
    // "reproducible" would quietly mean something else.
    history::save_project(
        &config,
        &Project { inputs: Vec::new(), included: Default::default(), run_options: RunOptions::default() },
    )
    .expect("save again");
    let back = history::load_project(&config).expect("load");
    assert_eq!(back.run_options, RunOptions::default());
    assert!(back.inputs.is_empty());
}

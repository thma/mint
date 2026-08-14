//! Cancellation and progress, checked against the two things that actually matter: that a
//! cancelled job destroys nothing, and that adding a progress sink does not change a single
//! output bit.
//!
//! The second is the one worth being paranoid about. `JobControl` threads a parameter
//! through six DSP loops, and the failure mode of getting that wrong is not a crash — it is
//! a render that is subtly different when the GUI runs it than when the CLI does. The
//! module-level bit-equality suites in `ops::limiter` and `ops::resample` cover the gain
//! curve and the resampler; this covers the whole chain, end to end, on a real file.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::config::{Config, ResolvedTarget};
use mint::pipeline;
use mint::progress::{Cancelled, JobControl, Stage};

const RATE: u32 = 48_000;
const FRAMES: usize = RATE as usize; // 1 s — long enough for several blocks in every stage

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("job-control-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_master(path: &Path) -> PathBuf {
    let tone = |hz: f64, amp: f64| -> Vec<f64> {
        (0..FRAMES)
            .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / RATE as f64).sin())
            .collect()
    };
    let buffer = AudioBuffer {
        channels: vec![tone(997.0, 0.4), tone(1245.0, 0.4)],
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

fn target_in(dir: &Path, name: &str, body: &str) -> ResolvedTarget {
    let dir = dir.display().to_string().replace('\\', "\\\\");
    Config::from_toml_str(&format!("[target.{name}]\ndir = \"{dir}\"\n{body}"))
        .unwrap_or_else(|e| panic!("config: {e:#}"))
        .resolve_target(name)
        .expect("resolve")
}

/// Anything the writers stage through `atomic_write` and might leave behind.
fn leftover_temp_files(dir: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.to_string_lossy().contains("mint-tmp") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, &mut found);
    found
}

#[test]
fn a_progress_sink_does_not_change_one_output_bit() {
    let dir = tmp("identical");
    let input = write_master(&dir.join("master.wav"));

    // A target that exercises every hooked stage: decode, analyze, loudness + limiter,
    // resample, post-SRC verify, quantize, encode.
    let body = "format = \"s16\"\nrate = 44100\nlufs = -14.0\ndither = \"adaptive\"\n";

    let plain = target_in(&dir, "plain", body);
    let watched = target_in(&dir, "watched", body);

    pipeline::process(&input, &plain, Some(99)).expect("plain render");

    let seen: Mutex<Vec<(Stage, f64)>> = Mutex::new(Vec::new());
    let sink = |stage: Stage, fraction: f64| seen.lock().unwrap().push((stage, fraction));
    let never = AtomicBool::new(false);
    let ctl = JobControl { cancel: Some(&never), progress: Some(&sink) };
    let report = pipeline::process_with(&input, &watched, Some(99), ctl).expect("watched render");

    let a = std::fs::read(dir.join("plain/master.wav")).expect("plain output");
    let b = std::fs::read(dir.join("watched/master.wav")).expect("watched output");
    assert_eq!(a, b, "the observed render differs from the unobserved one");

    // ...and the sink actually ran, or the assertion above proves nothing.
    let seen = seen.into_inner().unwrap();
    assert!(seen.len() > 10, "expected a stream of progress events, got {}", seen.len());
    assert_eq!(report.out_rate, 44_100);
}

#[test]
fn progress_is_monotonic_within_a_stage_and_ends_at_one() {
    let dir = tmp("monotonic");
    let input = write_master(&dir.join("master.wav"));
    let target = target_in(&dir, "t", "format = \"s16\"\nrate = 44100\nlufs = -14.0\n");

    let seen: Mutex<Vec<(Stage, f64)>> = Mutex::new(Vec::new());
    let sink = |stage: Stage, fraction: f64| seen.lock().unwrap().push((stage, fraction));
    let never = AtomicBool::new(false);
    pipeline::process_with(
        &input,
        &target,
        Some(1),
        JobControl { cancel: Some(&never), progress: Some(&sink) },
    )
    .expect("render");

    let seen = seen.into_inner().unwrap();
    for stage in [Stage::Decode, Stage::Resample, Stage::AnalyzeSource, Stage::AnalyzeDelivered] {
        let fractions: Vec<f64> =
            seen.iter().filter(|(s, _)| *s == stage).map(|(_, f)| *f).collect();
        assert!(!fractions.is_empty(), "{stage:?} never reported");
        assert!(
            fractions.windows(2).all(|w| w[1] >= w[0]),
            "{stage:?} went backwards: {fractions:?}"
        );
        assert_eq!(fractions.last(), Some(&1.0), "{stage:?} did not finish at 1.0");
        assert!(
            fractions.iter().all(|f| (0.0..=1.0).contains(f)),
            "{stage:?} left 0..=1: {fractions:?}"
        );
    }
}

#[test]
fn a_cancelled_job_leaves_the_previous_render_intact_and_no_partial_file() {
    let dir = tmp("cancel");
    let input = write_master(&dir.join("master.wav"));
    let target = target_in(
        &dir,
        "t",
        "format = \"s16\"\nrate = 44100\nlufs = -14.0\noverwrite = true\n",
    );

    // A good render first, so there is something to destroy.
    let first = pipeline::process(&input, &target, Some(1)).expect("first render");
    let before = std::fs::read(&first.output).expect("first output");
    assert!(!before.is_empty());

    // Now cancel from inside the run: the flag flips the moment a stage reports past a
    // tenth, which proves the hook fires *during* a stage rather than only between them.
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
    .expect_err("a cancelled job must not report success");

    // Distinguishable from a failure without matching on message text — the GUI has to tell
    // "the user stopped it" from "the render broke".
    assert!(
        err.downcast_ref::<Cancelled>().is_some(),
        "cancellation must surface as Cancelled, got: {err:#}"
    );

    let after = std::fs::read(&first.output).expect("the previous render must still be there");
    assert_eq!(before, after, "a cancelled job overwrote the previous render");
    let leftovers = leftover_temp_files(&dir);
    assert!(leftovers.is_empty(), "staging files survived the unwind: {leftovers:?}");
}

#[test]
fn cancelling_a_dsd_encode_stops_it_and_writes_nothing() {
    let dir = tmp("cancel-dsd");
    let input = write_master(&dir.join("master.wav"));
    let target = target_in(&dir, "t", "codec = \"dsf\"\ndsd = \"dsd64\"\n");

    // The stage the whole facility is for: the one that runs for minutes. Cancel once the
    // encode itself is under way, not merely once the job has started.
    let cancel = AtomicBool::new(false);
    let sink = |stage: Stage, fraction: f64| {
        if stage == Stage::Encode && fraction > 0.05 {
            cancel.store(true, Ordering::Relaxed);
        }
    };
    let err = pipeline::process_with(
        &input,
        &target,
        Some(1),
        JobControl { cancel: Some(&cancel), progress: Some(&sink) },
    )
    .expect_err("the encode must stop");
    assert!(err.downcast_ref::<Cancelled>().is_some(), "{err:#}");

    assert!(
        !dir.join("t/master.dsf").exists(),
        "a cancelled DSD encode must not leave a .dsf behind"
    );
    assert!(leftover_temp_files(&dir).is_empty(), "staging file survived");
}

#[test]
fn a_flag_that_is_never_set_costs_nothing_and_changes_nothing() {
    // The path the CLI takes. `process` is `process_with(JobControl::default())`, so this
    // pins that the default really is inert rather than, say, reporting to a sink that
    // happens to be absent but still short-circuiting a loop.
    let dir = tmp("default");
    let input = write_master(&dir.join("master.wav"));
    let via_default = target_in(&dir, "a", "format = \"s24\"\nrate = 44100\nlufs = -16.0\n");
    let via_explicit = target_in(&dir, "b", "format = \"s24\"\nrate = 44100\nlufs = -16.0\n");

    let one = pipeline::process(&input, &via_default, Some(7)).expect("default");
    let two = pipeline::process_with(&input, &via_explicit, Some(7), JobControl::default())
        .expect("explicit default");

    assert_eq!(one.file_bytes, two.file_bytes);
    assert_eq!(
        std::fs::read(&one.output).unwrap(),
        std::fs::read(&two.output).unwrap(),
        "process and process_with(default) produced different audio"
    );
}

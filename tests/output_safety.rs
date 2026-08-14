//! What mint is allowed to destroy, and what it must not.
//!
//! Three separate promises, none of which had a test before:
//!
//! 1. **The input file is never written over** unless the user says so — and "the input
//!    file" is a question about *identity*, not spelling. A leading `./`, an unresolved
//!    `..`, a symlink or a hard link all name the same bytes, and the old raw `PathBuf`
//!    comparison saw four different paths. Globs make the `./` spelling the common case.
//! 2. **An existing output is not silently replaced.** `overwrite` defaults to `false`, and
//!    until now that guarded only the input — the previous render was always clobbered.
//! 3. **A write that fails never destroys what was already there.** Every writer stages to a
//!    sibling temp file and renames, so the deliverable from the last good run survives a
//!    disk-full, an error, or a Ctrl-C in the middle of a long encode.
//!
//! Rules 1 and 2 are enforced in `pipeline::process` (before any DSP runs, so a doomed job
//! costs nothing) and rule 3 in `audio::atomic_write`, driven directly here because the
//! failure it defends against cannot be provoked through the pipeline.
//!
//! The batch-level rules — two inputs with the same stem racing onto one output, and one
//! job's output landing on another job's input — live in `config::check_planned_outputs`,
//! because `pipeline::process` sees one job at a time and structurally cannot see either.

use std::path::{Path, PathBuf};

use mint::audio::atomic_write::Staged;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::audio::io_write_dsd::DsfWriter;
use mint::config::{Config, PlannedOutput, ResolvedTarget, check_planned_outputs};
use mint::ops::dsd::DsdRate;
use mint::pipeline;

/// Scratch dir carrying the pid and the test name, so the parallel test harness cannot make
/// two of these tread on each other — the same convention `tests/dsd_pipeline.rs` uses.
fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("output-safety-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn tone(rate: u32, frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin())
        .collect()
}

/// A short stereo master on disk. `amp` is the handle for telling two renders apart.
fn write_master(path: &Path, amp: f64) -> PathBuf {
    let rate = 48_000;
    let frames = rate as usize / 10;
    let buffer = AudioBuffer {
        channels: vec![
            tone(rate, frames, 997.0, amp),
            tone(rate, frames, 1245.0, amp),
        ],
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

/// Resolve a single target out of a TOML fragment, through `Config::from_path` so the
/// production validation path runs.
fn target(dir: &Path, body: &str) -> ResolvedTarget {
    let toml = format!("[target.t]\nformat = \"s24\"\n{body}\n");
    let path = dir.join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    Config::from_path(&path)
        .expect("config should resolve")
        .resolve_target("t")
        .expect("target should resolve")
}

fn err(input: &Path, target: &ResolvedTarget) -> String {
    match pipeline::process(input, target, Some(1)) {
        Ok(r) => panic!("expected a refusal, got a render to {}", r.output),
        Err(e) => format!("{e:#}"),
    }
}

fn siblings(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read scratch dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect()
}

// ═══════════════════ 1. the input file, identified rather than spelled ═══════════════════

/// `PathBuf` equality compares components and normalizes almost nothing: it folds `a//b` and
/// `a/./b`, but a leading `./` and *any* `..` survive as components. So a path that walks out
/// of a subdirectory and back names the same file and compares unequal, and the old guard
/// rendered the master onto itself.
#[test]
fn an_input_spelled_through_a_parent_directory_is_still_the_input() {
    let dir = tmp("dotdot");
    std::fs::create_dir_all(dir.join("sub")).expect("subdir");
    let input = write_master(&dir.join("song.wav"), 0.5);
    let before = std::fs::read(&input).expect("read master");

    let t = target(
        &dir,
        &format!("dir = \"{}\"\nnaming = \"{{stem}}.{{ext}}\"\n", dir.display()),
    );
    let spelled = dir.join("sub").join("..").join("song.wav");
    // Premise: this is exactly the comparison the old guard made.
    assert_ne!(
        spelled,
        t.render_output_path(&spelled, 48_000).expect("render"),
        "premise: the two spellings must compare unequal as paths"
    );

    let message = err(&spelled, &t);
    assert!(
        message.contains("refusing to overwrite the input file"),
        "expected the input-file refusal, got: {message}"
    );
    assert_eq!(before, std::fs::read(&input).expect("read master"), "master was modified");
}

/// `canonicalize` would catch a symlink; only `(device, inode)` catches a hard link, which
/// is why the Unix path does not use `canonicalize`.
#[cfg(unix)]
#[test]
fn an_input_reached_through_a_hard_link_is_still_the_input() {
    let dir = tmp("hardlink");
    let input = write_master(&dir.join("song.wav"), 0.5);
    let before = std::fs::read(&input).expect("read master");

    let link = dir.join("song_link.wav");
    std::fs::hard_link(&input, &link).expect("hard link");
    // Premise: the two paths are already canonical and differ, so nothing short of an inode
    // comparison can tell that they are one file.
    assert_ne!(
        input.canonicalize().unwrap(),
        link.canonicalize().unwrap(),
        "premise: canonicalize cannot see through a hard link"
    );

    let t = target(
        &dir,
        &format!("dir = \"{}\"\nnaming = \"{{stem}}_link.{{ext}}\"\n", dir.display()),
    );
    let message = err(&input, &t);
    assert!(
        message.contains("refusing to overwrite the input file"),
        "expected the input-file refusal, got: {message}"
    );
    assert_eq!(before, std::fs::read(&input).expect("read master"), "master was modified");
}

#[cfg(unix)]
#[test]
fn an_input_reached_through_a_symlink_is_still_the_input() {
    let dir = tmp("symlink");
    let input = write_master(&dir.join("song.wav"), 0.5);
    let before = std::fs::read(&input).expect("read master");

    let link = dir.join("song_link.wav");
    std::os::unix::fs::symlink(&input, &link).expect("symlink");

    let t = target(
        &dir,
        &format!("dir = \"{}\"\nnaming = \"{{stem}}_link.{{ext}}\"\n", dir.display()),
    );
    let message = err(&input, &t);
    assert!(
        message.contains("refusing to overwrite the input file"),
        "expected the input-file refusal, got: {message}"
    );
    assert_eq!(before, std::fs::read(&input).expect("read master"), "master was modified");
}

#[test]
fn force_is_what_lets_a_render_land_on_its_own_input() {
    let dir = tmp("force-input");
    let input = write_master(&dir.join("song.wav"), 0.5);
    let before = std::fs::read(&input).expect("read master");

    // `--force` is `overwrite = true` on every target (cli::resolve_selected_targets).
    let t = target(
        &dir,
        &format!(
            "dir = \"{}\"\nnaming = \"{{stem}}.{{ext}}\"\noverwrite = true\n",
            dir.display()
        ),
    );
    pipeline::process(&input, &t, Some(1)).expect("overwrite = true must permit it");
    assert_ne!(
        before,
        std::fs::read(&input).expect("read master"),
        "the render should have replaced the input"
    );
}

// ═════════════════════════ 2. an existing output is not collateral ═══════════════════════

#[test]
fn an_existing_output_is_not_replaced_without_overwrite() {
    let dir = tmp("existing-output");
    let input = write_master(&dir.join("song.wav"), 0.5);
    let t = target(&dir, &format!("dir = \"{}\"\n", dir.display()));

    let first = pipeline::process(&input, &t, Some(1)).expect("first render");
    let rendered = PathBuf::from(&first.output);
    let bytes = std::fs::read(&rendered).expect("read first render");

    let message = err(&input, &t);
    assert!(
        message.contains("output file already exists"),
        "expected the existing-output refusal, got: {message}"
    );
    assert!(
        message.contains(&rendered.display().to_string()),
        "the refusal must name the file it is protecting: {message}"
    );
    assert_eq!(
        bytes,
        std::fs::read(&rendered).expect("read again"),
        "the first render must survive the refused second one"
    );
}

/// The other half of the same rule: the fix must not have made an ordinary re-render
/// impossible, only deliberate.
#[test]
fn overwrite_true_replaces_an_existing_output() {
    let dir = tmp("overwrite-output");
    let input = write_master(&dir.join("song.wav"), 0.25);
    let t = target(
        &dir,
        &format!("dir = \"{}\"\noverwrite = true\n", dir.display()),
    );

    let first = pipeline::process(&input, &t, Some(1)).expect("first render");
    let rendered = PathBuf::from(&first.output);
    let bytes = std::fs::read(&rendered).expect("read first render");

    // A louder master, so "was it replaced" is answerable from the bytes alone.
    write_master(&dir.join("song.wav"), 0.5);
    pipeline::process(&input, &t, Some(1)).expect("second render");
    assert_ne!(
        bytes,
        std::fs::read(&rendered).expect("read second render"),
        "overwrite = true must actually replace"
    );
}

// ═══════════════════ 3. a failed write leaves the last good file alone ═══════════════════

#[test]
fn a_staged_write_dropped_before_commit_leaves_the_previous_file_intact() {
    let dir = tmp("staged-drop");
    let path = dir.join("deliverable.bin");
    std::fs::write(&path, b"the previous good render").expect("seed");

    {
        let staged = Staged::new(&path).expect("stage");
        std::fs::write(staged.path(), b"half a file").expect("partial write");
        assert!(staged.path().exists(), "the staging file should exist while held");
        // Dropped without commit — the shape of every `?` that unwinds out of a writer.
    }

    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"the previous good render",
        "an uncommitted staging write must not touch the destination"
    );
    assert_eq!(
        siblings(&dir),
        vec!["deliverable.bin".to_string()],
        "the staging file must be cleaned up on drop"
    );
}

/// The writer with the longest destruction window: `DsfWriter` used to `File::create` the
/// deliverable and then stream for the whole encode, patching the header at the very end.
#[test]
fn a_dsf_writer_dropped_before_finish_leaves_the_previous_file_intact() {
    let dir = tmp("dsf-drop");
    let path = dir.join("track.dsf");
    std::fs::write(&path, b"the previous good dsf").expect("seed");

    {
        let mut writer = DsfWriter::create(&path, 2, DsdRate::Dsd64).expect("create");
        writer.write(&[vec![true; 64], vec![false; 64]]).expect("one block");
    }

    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"the previous good dsf",
        "an abandoned DSD encode must not have touched the deliverable"
    );
    assert_eq!(
        siblings(&dir),
        vec!["track.dsf".to_string()],
        "the staging file must be cleaned up on drop"
    );
}

#[test]
fn a_finished_dsf_writer_replaces_the_file_and_leaves_no_staging_behind() {
    let dir = tmp("dsf-commit");
    let path = dir.join("track.dsf");
    std::fs::write(&path, b"the previous good dsf").expect("seed");

    let mut writer = DsfWriter::create(&path, 2, DsdRate::Dsd64).expect("create");
    writer.write(&[vec![true; 64], vec![false; 64]]).expect("one block");
    writer.finish().expect("finish");

    let bytes = std::fs::read(&path).expect("read");
    assert_eq!(&bytes[0..4], b"DSD ", "the committed file must be the DSF");
    assert_eq!(
        siblings(&dir),
        vec!["track.dsf".to_string()],
        "no staging file may survive a successful commit"
    );
}

/// The pid alone is not a unique staging name: `cli.rs` runs jobs under rayon, so several
/// can be staging siblings of one directory at the same instant, and two inputs can share a
/// final file name.
#[test]
fn concurrent_stagings_of_one_path_never_share_a_temp_name() {
    let dir = tmp("staging-names");
    let path = dir.join("same.wav");

    let names: Vec<PathBuf> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..16)
            .map(|_| s.spawn(|| Staged::new(&path).expect("stage").path().to_path_buf()))
            .collect();
        handles.into_iter().map(|h| h.join().expect("join")).collect()
    });

    let unique: std::collections::HashSet<_> = names.iter().collect();
    assert_eq!(unique.len(), names.len(), "staging names collided: {names:?}");
}

// ══════════════════════ 4. batch-level collisions, before any audio ══════════════════════

#[test]
fn two_inputs_with_the_same_stem_are_rejected_before_anything_is_written() {
    let dir = tmp("same-stem");
    let a = dir.join("a");
    let b = dir.join("b");
    std::fs::create_dir_all(&a).expect("a");
    std::fs::create_dir_all(&b).expect("b");
    let in_a = write_master(&a.join("song.wav"), 0.5);
    let in_b = write_master(&b.join("song.wav"), 0.5);

    let out = dir.join("out").join("song.wav");
    let planned = vec![
        PlannedOutput { input: &in_a, target: "t", output: out.clone() },
        PlannedOutput { input: &in_b, target: "t", output: out.clone() },
    ];

    let message = match check_planned_outputs(&planned, &[in_a.clone(), in_b.clone()]) {
        Ok(()) => panic!("two inputs rendering to one path must be rejected"),
        Err(e) => format!("{e:#}"),
    };
    // Naming both sources is the point: the user has to know *which two* collided, and no
    // naming placeholder carries a directory, so the resolution is theirs to pick.
    assert!(message.contains(&in_a.display().to_string()), "{message}");
    assert!(message.contains(&in_b.display().to_string()), "{message}");
    assert!(message.contains(&out.display().to_string()), "{message}");
    assert!(!dir.join("out").exists(), "nothing may be written before the batch is accepted");
}

/// The case a per-job guard cannot see by construction: when job A checks its destination,
/// job B may not have read its source yet.
#[test]
fn an_output_that_lands_on_another_input_of_the_same_batch_is_rejected() {
    let dir = tmp("output-over-input");
    let song = write_master(&dir.join("song.wav"), 0.5);
    let master = write_master(&dir.join("song_master.wav"), 0.5);

    let planned = vec![PlannedOutput {
        input: &song,
        target: "t",
        output: master.clone(),
    }];

    let message = match check_planned_outputs(&planned, &[song.clone(), master.clone()]) {
        Ok(()) => panic!("writing over another input must be rejected"),
        Err(e) => format!("{e:#}"),
    };
    assert!(message.contains("input of this same run"), "{message}");
    assert!(message.contains(&master.display().to_string()), "{message}");
}

/// The batch rule must not swallow the *documented* in-place render. A job whose output is
/// its own input is `overwrite`'s question, not the batch's — the batch only cares about a
/// job stepping on a file some *other* job is going to read.
#[test]
fn a_job_writing_over_its_own_input_is_left_to_the_overwrite_rule() {
    let dir = tmp("in-place");
    let song = write_master(&dir.join("song.wav"), 0.5);
    let planned = vec![PlannedOutput {
        input: &song,
        target: "t",
        output: song.clone(),
    }];
    check_planned_outputs(&planned, std::slice::from_ref(&song))
        .expect("an in-place render must reach the per-job overwrite guard");
}

/// The staging name has to fit the same `NAME_MAX` as the real one, so the final name it
/// carries for recognizability is truncated rather than appended to.
#[test]
fn a_staging_name_stays_within_the_filesystem_name_limit() {
    let dir = tmp("long-name");
    let long = format!("{}.wav", "x".repeat(250));
    let staged = Staged::new(&dir.join(&long)).expect("stage");
    let name = staged.path().file_name().expect("name").len();
    assert!(
        name <= 255,
        "staging name is {name} bytes, past NAME_MAX: {:?}",
        staged.path()
    );
    // And it must still be usable, not merely short.
    std::fs::write(staged.path(), b"x").expect("a staging file at the limit must be writable");
    staged.commit().expect("commit");
    assert!(dir.join(&long).exists());
}

/// The two sides of that rule come from different places — the output from `dir` in the
/// config, the input from argv or a glob — so they routinely disagree about spelling while
/// naming one file. Raw path equality let `dir = "./m3"` (or an absolute `dir`) walk past a
/// clash that `dir = "m3"` caught, and `--force` makes the per-job guard return before it
/// could catch it either.
#[test]
fn the_batch_rule_sees_through_a_spelling_difference_between_dir_and_argv() {
    let dir = tmp("spelling");
    std::fs::create_dir_all(dir.join("m3")).expect("m3");
    let song = write_master(&dir.join("m3/song.wav"), 0.5);
    let master = write_master(&dir.join("m3/song_master.wav"), 0.5);
    let inputs = vec![song.clone(), master.clone()];

    // Every spelling of the one path the config could plausibly produce.
    for spelling in [
        dir.join("m3/song_master.wav"),
        dir.join("./m3/song_master.wav"),
        dir.join("m3/../m3/song_master.wav"),
    ] {
        let planned = vec![PlannedOutput {
            input: &song,
            target: "t",
            output: spelling.clone(),
        }];
        assert!(
            check_planned_outputs(&planned, &inputs).is_err(),
            "'{}' names an input of the run and must be rejected",
            spelling.display()
        );
    }
}

#[test]
fn distinct_outputs_are_accepted() {
    let dir = tmp("no-collision");
    let a = write_master(&dir.join("a.wav"), 0.5);
    let b = write_master(&dir.join("b.wav"), 0.5);
    let inputs = vec![a.clone(), b.clone()];
    let planned = vec![
        PlannedOutput { input: &a, target: "t", output: dir.join("out/a.wav") },
        PlannedOutput { input: &b, target: "t", output: dir.join("out/b.wav") },
    ];
    check_planned_outputs(&planned, &inputs).expect("distinct paths must pass");
}

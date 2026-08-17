//! The batch runner, driven end to end through the app.
//!
//! `render_headless.rs` covers one job and the progress it reports; this covers the *fan-out*
//! — the part that differs from the spike and mirrors `cli.rs::run`. Two inputs × two targets
//! is four jobs and four distinct files, which is the assertion that a cross product went
//! out to a private pool and came back complete: a coordinator that lost a job to a dropped
//! sender, handed two workers the same index, or joined before the last one finished would
//! all produce three files and a green test everywhere else. It carries on one step past the
//! last job, because the end of a batch is not the end of the shell's work: the run is
//! persisted, the view hands over to Report, and four rendered tasks turn into four rows.
//! That handover is the one path by which a *real* report reaches a screen — everything else
//! about the Report view is asserted in `report_verdict.rs`, on synthesized figures.
//!
//! The second test is `fail_fast`, which takes the sequential branch for the same reason
//! `cli.rs` does — "strict ordering" and "stop at the first failure" are one requirement.
//! `cli.rs` simply `break`s; here the jobs that will never run still need a terminal state,
//! or they sit in the list spinning for the rest of the session, so the assertion is on both
//! halves: exactly one failure recorded, and nothing after it ran.
//!
//! The last two are the **pre-flight**, and they are the reason this file is not only about
//! fan-out. `cli.rs::run` asks `config::check_planned_outputs` about the whole batch before
//! any audio is touched, because two jobs fighting over one path is invisible from inside
//! `pipeline::process` — and the second of its two rules is refused *even under* `--force`,
//! since whichever job reads first decides what gets rendered. A GUI that lists that in a
//! panel and then starts the batch anyway is worse than one that never checked: it reports
//! success on a batch mint refuses, and in the `{stem}_master` case it eats a master. Both
//! tests assert on the filesystem as well as on the refusal, because "no report" and "no
//! bytes moved" are different claims.
//!
//! Cancellation is deliberately not tested here. Catching a batch mid-flight needs a job
//! slow enough to reach and makes the test a race; the guarantee that matters — a cancelled
//! job leaves the previous render intact and no partial file — is `pipeline`'s and is pinned
//! by the library's own `tests/job_control.rs`.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::config::{self, PlannedOutput, ResolvedTarget};
use mint_gui::app::{MintApp, Mode};
use mint_gui::problems::{Fix, Severity};
use mint_gui::state::AppState;

const RATE: u32 = 48_000;
/// A quarter of a second: long enough to be a real render through the whole chain, short
/// enough that four of them in a debug build are still a test and not a coffee break.
const FRAMES: usize = RATE as usize / 4;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-batch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_master(path: &Path) {
    let tone = |hz: f64| -> Vec<f64> {
        (0..FRAMES)
            .map(|n| 0.3 * (2.0 * std::f64::consts::PI * hz * n as f64 / RATE as f64).sin())
            .collect()
    };
    let buffer = AudioBuffer {
        channels: vec![tone(997.0), tone(1245.0)],
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
}

fn two_targets(dir: &Path) -> PathBuf {
    let path = dir.join("mint.toml");
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    std::fs::write(
        &path,
        format!(
            "[defaults]\ndir = \"{escaped}\"\n\n\
             [target.cd]\nformat = \"s16\"\nrate = 44100\n\n\
             [target.web]\nformat = \"s24\"\n"
        ),
    )
    .expect("write config");
    path
}

/// One target, with `[defaults] dir` at `dir` so a planned output path is a real one.
fn one_target(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("mint.toml");
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    std::fs::write(&path, format!("[defaults]\ndir = \"{escaped}\"\n{body}")).expect("write config");
    path
}

/// The batch plan for one target over `inputs`, in the library's own shape — what
/// `cli.rs::plan_outputs` builds, built here with the library's own calls so the expected
/// refusal below is mint's answer to mint's question and not to this file's idea of one.
fn plan<'a>(target: &'a ResolvedTarget, inputs: &'a [PathBuf]) -> Vec<PlannedOutput<'a>> {
    inputs
        .iter()
        .map(|input| PlannedOutput {
            input,
            target: &target.name,
            output: target
                .render_output_path(input, target.delivery_rate(RATE))
                .expect("the naming renders"),
        })
        .collect()
}

/// Start the batch and pump frames until the worker set is done. `step` rather than `run`:
/// `run` loops until the UI stops asking to be repainted, and a job in flight asks on every
/// event.
fn render(harness: &mut Harness<'static, MintApp>) {
    harness.state_mut().select_mode(Mode::Run);
    harness.run();
    let ctx = harness.ctx.clone();
    harness.state_mut().start_render(ctx);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while harness.state().run_view().busy() && std::time::Instant::now() < deadline {
        harness.step();
    }
    assert!(!harness.state().run_view().busy(), "the batch never finished");
}

#[test]
fn two_inputs_times_two_targets_is_four_jobs_and_four_files() {
    let dir = scratch("cross");
    let one = dir.join("one.wav");
    let two = dir.join("two.wav");
    write_master(&one);
    write_master(&two);
    let config = two_targets(&dir);

    let mut state = AppState::load(&config);
    state.add_inputs([one, two]);
    let mut harness =
        Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), MintApp::new(state));
    render(&mut harness);

    let run = harness.state().run_view();
    assert!(run.failures().is_empty(), "unexpected failures: {:#?}", run.failures());
    assert_eq!(run.reports().len(), 4, "every (input × target) pair reports back");

    let mut outputs: Vec<&str> = run.reports().iter().map(|r| r.output.as_str()).collect();
    outputs.sort_unstable();
    outputs.dedup();
    assert_eq!(outputs.len(), 4, "four distinct paths: {outputs:#?}");
    for output in &outputs {
        assert!(Path::new(output).exists(), "missing output {output}");
    }

    // The shell's end-of-batch work: the run is on disk in the --json schema, and it is the
    // one just rendered.
    let listed = mint_gui::history::list(&config);
    assert_eq!(listed.len(), 1, "the finished batch persisted exactly one run file");
    let summary = listed[0].summary.expect("this build's own schema");
    assert_eq!((summary.tasks, summary.failures), (4, 0));

    // ...and the other half of it: the batch hands over to the Report view rather than
    // leaving four finished rows in the Run list for someone to notice. Four verdicts on
    // screen, one per task — `✓` is the report's own glyph and nothing else drawn in this
    // mode uses it, so the count is the row count. This is the only place a *rendered*
    // report reaches the screen; `report_verdict.rs` covers what the glyph means with
    // figures no real render can be asked to produce.
    assert_eq!(harness.state().mode(), Mode::Report, "the finished batch shows its report");
    harness.run();
    assert!(
        harness.query_all_by_label_contains("✓").count() >= 4,
        "every task's verdict must reach the table"
    );

    // And the third half of it: a finished batch changes the answer to the questions the
    // Problems panel asks about *outputs*, and no source moved, so nothing about the source
    // cache would have noticed. Four files that were not there are there now, and a second
    // Render would fail on all four — a panel still saying "mint has no objection" while the
    // matrix beside it paints `⟳` on every cell is the disagreement being ruled out.
    let state = harness.state().state();
    let warnings: Vec<&str> =
        state.problems.iter().map(|problem| problem.message.as_str()).collect();
    assert_eq!(warnings.len(), 4, "one per written output: {warnings:#?}");
    let expected = {
        let first = &harness.state().run_view().reports()[0];
        let (input, output) = (Path::new(&first.input), Path::new(&first.output));
        mint::pipeline::output_conflict(input, output, false)
            .expect("the file the batch just wrote is in the way of the next one")
            .message(input, output)
    };
    assert!(warnings.contains(&expected.as_str()), "mint's own wording: {expected}");
}

#[test]
fn a_batch_whose_jobs_would_fight_over_one_path_never_starts() {
    // Two masters with the same stem in different directories. No naming placeholder carries
    // an input's parent directory, so both jobs render to one file and whichever finishes
    // last wins — the batch-level question `pipeline::process` structurally cannot see.
    let dir = scratch("collide");
    let one = dir.join("a/song.wav");
    let two = dir.join("b/song.wav");
    std::fs::create_dir_all(dir.join("a")).expect("scratch dir");
    std::fs::create_dir_all(dir.join("b")).expect("scratch dir");
    write_master(&one);
    write_master(&two);
    let config = one_target(&dir, "[target.cd]\nformat = \"s16\"\nrate = 44100\n");

    let mut state = AppState::load(&config);
    state.add_inputs([one.clone(), two.clone()]);

    // What mint says about this batch, asked of mint — the same call `cli.rs::run` makes on
    // line 106, before it touches audio.
    let inputs = vec![one, two];
    let target =
        state.config().expect("the fixture resolves").resolve_target("cd").expect("resolves");
    let expected = {
        let err = config::check_planned_outputs(&plan(&target, &inputs), &inputs)
            .expect_err("mint must refuse this batch");
        format!("{err:#}")
    };
    let output = target.render_output_path(&inputs[0], target.delivery_rate(RATE)).expect("path");

    let mut harness =
        Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), MintApp::new(state));
    render(&mut harness);

    let run = harness.state().run_view();
    assert!(run.reports().is_empty(), "no job may run: {:#?}", run.reports());
    assert!(run.failures().is_empty(), "and none may be reported as failing either");
    assert_eq!(run.refusal(), Some(expected.as_str()), "mint's own refusal, unedited");
    assert!(!output.exists(), "nothing was written: {}", output.display());

    // The button is in the project bar and the refusal is written in the Run view, so the
    // click has to land the user where the reason is.
    assert_eq!(harness.state().mode(), Mode::Run);
    harness.run();
    assert!(
        harness.query_all_by_label_contains(&expected).next().is_some(),
        "the refusal must reach the screen: {expected}"
    );
}

#[test]
fn force_does_not_let_one_job_write_over_another_jobs_master() {
    // `naming = "{stem}_master.{ext}"` over a batch that already contains `song_master.wav`.
    // The library rejects this *even under* `--force`, because with the jobs running
    // concurrently the one reading that master may or may not have got there first — and the
    // GUI used to run it and shrink the master from under it.
    let dir = scratch("eat-master");
    let song = dir.join("song.wav");
    let master = dir.join("song_master.wav");
    write_master(&song);
    write_master(&master);
    let config = one_target(
        &dir,
        "[target.cd]\nformat = \"s16\"\nrate = 44100\nnaming = \"{stem}_master.{ext}\"\n",
    );

    let mut state = AppState::load(&config);
    state.add_inputs([song.clone(), master.clone()]);

    // With force *off* — the state the panel is normally read in — this cell earns two rows:
    // step 8's refusal, and step 9's "there is already a file there" warning. The warning
    // must not carry the one-click [Enable force] button, because `--force` does not lift
    // this refusal: the button would sit directly under mint's own "would write over a
    // different input of this same run" offering to go ahead and do it.
    let rows = |state: &AppState| {
        state.problems.iter().map(|p| (p.severity, p.message.clone())).collect::<Vec<_>>()
    };
    assert!(
        state.problems.iter().any(|p| p.severity == Severity::Warning),
        "the occupied output path is still reported: {:#?}",
        rows(&state)
    );
    assert!(
        !state.problems.iter().any(|p| p.fix == Some(Fix::EnableForce)),
        "no [Enable force] button on a cell mint refuses under force anyway: {:#?}",
        rows(&state)
    );

    state.run_options.force = true;
    state.recollect_problems();

    let inputs = vec![song, master.clone()];
    let target =
        state.config().expect("the fixture resolves").resolve_target("cd").expect("resolves");
    let expected = {
        let err = config::check_planned_outputs(&plan(&target, &inputs), &inputs)
            .expect_err("mint must refuse this batch even under --force");
        format!("{err:#}")
    };
    let before = std::fs::metadata(&master).expect("the master is there").len();

    let mut harness =
        Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), MintApp::new(state));
    render(&mut harness);

    let run = harness.state().run_view();
    assert!(run.reports().is_empty(), "no job may run: {:#?}", run.reports());
    assert_eq!(run.refusal(), Some(expected.as_str()), "mint's own refusal, unedited");
    assert_eq!(
        std::fs::metadata(&master).expect("the master is still there").len(),
        before,
        "the other input of the run must not have been rendered over"
    );
}

#[test]
fn fail_fast_stops_the_batch_and_the_rest_never_run() {
    let dir = scratch("failfast");
    let bad = dir.join("bad.wav");
    let good = dir.join("good.wav");
    // A header mint cannot read, so the first job fails inside `probe_source` — before any
    // audio is touched, which is the failure mode `fail_fast` is for.
    std::fs::write(&bad, b"not a wav at all").expect("write the fake master");
    write_master(&good);
    let config = two_targets(&dir);

    let mut state = AppState::load(&config);
    state.add_inputs([bad, good]);
    state.run_options.fail_fast = true;
    let mut harness =
        Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), MintApp::new(state));
    render(&mut harness);

    let run = harness.state().run_view();
    assert_eq!(run.failures().len(), 1, "the batch stopped at the first failure");
    assert!(
        run.failures()[0].error.contains("failed to probe audio format"),
        "mint's own message: {}",
        run.failures()[0].error
    );
    assert!(run.reports().is_empty(), "nothing after it ran: {:#?}", run.reports());
}

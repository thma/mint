//! The journey, in one frame at a time: a master lands, the matrix fills, a cell is
//! selected, and the chain preview describes it.
//!
//! Every other test in this directory drives one module. This one exists because the shell
//! can be wired wrong while every module is right — a pane never called, a mode with no
//! content, a selection that one screen writes and another does not read. Those are exactly
//! the failures that look fine in a code review and blank in a window.
//!
//! The chain assertion is deliberately not a string this file wrote. It asks the library for
//! `describe()`'s own steps and then looks for them on screen, so the test cannot drift into
//! pinning the GUI's idea of the chain: `describe()` is a separate code path from
//! `pipeline::process` (CLAUDE.md says so, and says the two drift silently), and the only
//! honest thing this screen can do is show what it was told.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint_gui::app::{MintApp, Mode};
use mint_gui::state::AppState;

const RATE: u32 = 48_000;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-shell-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_master(path: &Path) {
    let frames = RATE as usize / 4;
    let tone: Vec<f64> = (0..frames)
        .map(|n| 0.3 * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / RATE as f64).sin())
        .collect();
    let buffer = AudioBuffer {
        channels: vec![tone.clone(), tone],
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

#[test]
fn a_master_reaches_every_screen_that_should_see_it() {
    let dir = scratch("journey");
    let master = dir.join("take-01.wav");
    write_master(&master);

    let config = dir.join("mint.toml");
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    std::fs::write(
        &config,
        format!(
            "[defaults]\ndir = \"{escaped}\"\n\n\
             [target.cd]\npreset = \"cd\"\nlufs = -12.0\n\n\
             [target.hires]\npreset = \"hires\"\n"
        ),
    )
    .expect("write config");

    let mut state = AppState::load(&config);
    // The same door the file picker and a drop both go through.
    assert_eq!(state.add_inputs([master.clone()]), 0, "a .wav is not refused");
    assert_eq!(state.sources.len(), 1);
    state.selected = Some("cd".to_string());
    state.selected_source = Some(0);

    // What the chain preview must end up showing, asked of the library rather than written
    // out here.
    let steps = state
        .config()
        .expect("the fixture resolves")
        .resolve_target("cd")
        .expect("cd resolves")
        .describe(RATE);
    assert!(!steps.is_empty(), "describe() has something to say about a cd target");

    let mut app = MintApp::new(state);
    app.select_mode(Mode::Plan);
    let mut harness = Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), app);
    harness.run();

    // The file's name, on every screen that names it — the Sources pane, the matrix's
    // sticky column and the chain preview's header. `query_all`, because three screens
    // agreeing is the point rather than an ambiguity.
    assert!(
        harness.query_all_by_label_contains("take-01.wav").count() >= 3,
        "the source must reach the pane, the matrix and the chain preview"
    );
    assert!(
        harness.query_all_by_label_contains("48000 Hz · 2 ch").next().is_some(),
        "the probe reaches the pane"
    );

    // The project bar: the arithmetic on the left of the `=` produces the number on its
    // right, which is what the runner will attempt.
    assert!(
        harness.query_all_by_label_contains("1 sources × 2 targets = 2 jobs").next().is_some(),
        "the batch is counted"
    );

    // The Targets pane and the matrix's column headers, both naming every target.
    for target in ["cd", "hires"] {
        assert!(
            harness.query_all_by_label(target).next().is_some(),
            "the {target} target must be on screen"
        );
    }

    // The chain preview, under the matrix: `describe()`'s own steps, verbatim.
    for step in &steps {
        assert!(
            harness.query_all_by_label_contains(step.as_str()).next().is_some(),
            "the chain preview must render describe()'s step verbatim: {step}"
        );
    }

    // The drawer defaults to Problems, and this fixture has none to show.
    harness.get_by_label("Problems");
    assert!(
        harness.query_all_by_label_contains("mint has no objection").next().is_some(),
        "an empty list says it is empty rather than looking broken"
    );

    // Run and Report both have something to say before anything has been rendered — an
    // empty screen is indistinguishable from a broken one.
    harness.state_mut().select_mode(Mode::Run);
    harness.run();
    assert!(harness.query_all_by_label_contains("no jobs yet").next().is_some());

    harness.state_mut().select_mode(Mode::Report);
    harness.run();
    assert!(harness.query_all_by_label_contains("no run yet").next().is_some());
    assert!(
        harness.query_all_by_label_contains("no runs yet").next().is_some(),
        "the history panel is beside it, and says where the files will go"
    );

    // And the CLI line reproduces the whole thing from a terminal (§5.3).
    let line = harness.state().state().cli_command();
    assert!(line.contains("--config"), "{line}");
    assert!(line.ends_with("take-01.wav"), "the real input path, not the placeholder: {line}");
    assert!(!line.contains("--target"), "every target is included, so the CLI needs no flags");
}

#[test]
fn unticking_the_last_target_cannot_silently_mean_all_of_them() {
    // `included` is empty == all, which is the CLI's own default. The consequence is that
    // "no targets" is not a state the run options can represent, so the last untick has to
    // be refused rather than applied — otherwise the click that looks like it turns the last
    // target off turns every one of them on.
    let dir = scratch("include");
    let config = dir.join("mint.toml");
    std::fs::write(&config, "[target.cd]\npreset = \"cd\"\n\n[target.hires]\npreset = \"hires\"\n")
        .expect("write config");

    let mut state = AppState::load(&config);
    assert_eq!(state.included_names(), vec!["cd", "hires"], "empty means all");

    state.set_included("hires", false);
    assert_eq!(state.included_names(), vec!["cd"]);

    state.set_included("cd", false);
    assert_eq!(state.included_names(), vec!["cd"], "the last untick is refused, not inverted");

    state.set_included("hires", true);
    assert_eq!(state.included_names(), vec!["cd", "hires"]);
    assert!(state.included.is_empty(), "back to every target is back to the empty set");
}

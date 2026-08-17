//! The Problems panel: *every* fault at once, in mint's own words.
//!
//! This is the test the panel exists for. Every check underneath it stops at its first
//! error — `Config::validate` at the first target that will not resolve, `check_collisions`
//! at the first colliding pair, `check_planned_outputs` at the first clash — which is right
//! for a CLI about to exit and useless to someone editing a config, who otherwise fixes one
//! thing, re-runs, and learns about the next one. So the assertion that matters is a
//! **count**: three independent faults must produce three rows, where `validate()` produces
//! one. The control at the end of that test is `validate()` itself, called on the same
//! fixture, answering with exactly one of the three.
//!
//! The other half is that the rows carry mint's own bytes, and this file goes out of its way
//! not to spell those bytes itself. Every expected message is obtained by **calling the
//! library function the panel calls** — `resolve_target` for a name, `check_collisions` for
//! a pair, `probe_source` for a file, `delivery_blocker` for a cell — and compared whole.
//! A substring assertion would pass against a row that kept a recognisable fragment and
//! reworded the rest, which is precisely the failure rule 4 is about; and a message written
//! out here would pin the GUI to a wording the library had already moved on from. Neither
//! can happen if the comparison has mint on both sides of it.
//!
//! Both halves are then asserted again against the accessibility tree, because a problem
//! collected and not drawn is a problem nobody has.
//!
//! What could break this without breaking anything else: a step that starts guessing past
//! the step above it (steps 1 and 2 are terminal on purpose), or `collect` swallowing a
//! narrow question's `Err` because the whole-config `validate()` already said something.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::{io_read, io_write};
use mint::config;
use mint_gui::app::MintApp;
use mint_gui::problems::{Scope, Severity};
use mint_gui::state::AppState;

/// Two targets that will not resolve — `format` is legal to inherit on a DSD target and
/// fatal to state — plus one pair that resolves and lands on a single output path, plus one
/// target with nothing wrong with it at all. Three faults across five targets, and the
/// innocent one is there so "three" cannot be "one per target".
const THREE_FAULTS: &str = r#"
[target.sacd-a]
codec = "dsf"
dsd = "dsd64"
format = "s24"

[target.sacd-b]
codec = "dsf"
dsd = "dsd128"
format = "s24"

[target.one]
format = "s16"
naming = "{stem}.wav"

[target.two]
format = "s24"
naming = "{stem}.wav"

[target.innocent]
format = "s16"
"#;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-problems-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// A quarter-second tone at `rate`, so the header a probe reads is a real one.
fn write_master(path: &Path, rate: u32) {
    let frames = rate as usize / 4;
    let tone: Vec<f64> = (0..frames)
        .map(|n| 0.3 * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / rate as f64).sin())
        .collect();
    let buffer = AudioBuffer {
        channels: vec![tone.clone(), tone],
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
}

/// The document under test, with `[defaults] dir` pointed at the scratch directory so a
/// planned output path is a real one. Named for what it does, because `mint::config` is
/// imported here and the two must not read as the same thing.
fn write_config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("mint.toml");
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    std::fs::write(&path, format!("[defaults]\ndir = \"{escaped}\"\n{body}")).expect("write config");
    path
}

/// The panel as the user meets it: the drawer opens on Problems, so nothing is clicked to
/// get here. Deliberately the whole app rather than `problems::ui` alone — a panel that
/// renders perfectly while the drawer never calls it is the failure being ruled out.
fn panel(state: AppState) -> Harness<'static, MintApp> {
    let mut harness =
        Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), MintApp::new(state));
    harness.run();
    harness
}

/// One message on the accessibility tree, whole. `contains` rather than an exact label match
/// because a row is a glyph, a scope chip and the message, and only the last of those is
/// mint's — but the needle is the *entire* message, so a row that truncated it to fit fails
/// here rather than looking tidy.
fn assert_on_screen(harness: &Harness<'static, MintApp>, message: &str) {
    assert!(
        harness.query_all_by_label_contains(message).next().is_some(),
        "the panel must render mint's own message: {message}"
    );
}

#[test]
fn three_independent_faults_produce_three_problems() {
    let dir = scratch("three");
    let path = write_config(&dir, THREE_FAULTS);
    let state = AppState::load(&path);
    let problems = &state.problems;
    assert_eq!(
        problems.len(),
        3,
        "two unresolvable targets and one colliding pair are three problems, not one: {:#?}",
        problems.iter().map(|p| (&p.scope, &p.message)).collect::<Vec<_>>()
    );

    // What mint says, asked of mint. These are the exact calls `problems::collect` makes,
    // made again here, so the only way a row can match is by having carried the answer
    // through untouched.
    let parsed = state.parsed.as_ref().expect("the document deserializes");
    let refused = |name: &str| {
        let err = parsed.resolve_target(name).expect_err("this target must not resolve");
        format!("{err:#}")
    };
    let resolves = |name: &str| parsed.resolve_target(name).expect("this target must resolve");
    let collided = {
        let err = config::check_collisions(&[resolves("one"), resolves("two")])
            .expect_err("these two must collide");
        format!("{err:#}")
    };

    let mut expected = vec![refused("sacd-a"), refused("sacd-b"), collided];
    let mut listed: Vec<String> = problems.iter().map(|p| p.message.clone()).collect();
    // Sorted, because the enumeration's order is its own business; what is under test is
    // that the set of rows is the set of answers, byte for byte.
    expected.sort();
    listed.sort();
    assert_eq!(listed, expected, "every row is the library's own string, unedited");

    // And every row knows what it is about, which is what makes it clickable. The collision
    // is scoped to the second of the pair — the one whose arrival created the clash.
    let scopes: Vec<&Scope> = problems.iter().map(|problem| &problem.scope).collect();
    for name in ["sacd-a", "sacd-b", "two"] {
        assert!(
            scopes.contains(&&Scope::Target(name.to_string())),
            "no row scoped to [{name}]: {scopes:#?}"
        );
    }
    assert!(problems.iter().all(|problem| problem.severity == Severity::Error));

    // The control, and the reason the panel exists: `validate()` — the whole-config check a
    // CLI run would stop at — sees exactly one of the three and never learns about the rest.
    let verdict = parsed.validate().expect_err("the fixture must be a config mint refuses");
    let verdict = format!("{verdict:#}");
    assert!(
        expected.contains(&verdict),
        "validate()'s single answer must be one of the three the panel unpacked: {verdict}"
    );
}

#[test]
fn every_row_reaches_the_panel_carrying_mints_own_bytes() {
    // Collected and drawn are different claims. This one is the second: the drawer's default
    // tab, its count, and all three messages in full, with nothing clicked and nothing
    // truncated on the way to the screen.
    let dir = scratch("panel");
    let path = write_config(&dir, THREE_FAULTS);
    let state = AppState::load(&path);
    let messages: Vec<String> = state.problems.iter().map(|p| p.message.clone()).collect();

    let harness = panel(state);
    harness.get_by_label_contains("Problems (3)");
    for message in &messages {
        assert_on_screen(&harness, message);
    }
}

#[test]
fn a_file_mint_cannot_probe_is_a_row_and_not_a_removal() {
    let dir = scratch("probe");
    let path = write_config(&dir, "\n[target.cd]\nformat = \"s16\"\nrate = 44100\n");
    let broken = dir.join("not-really.wav");
    std::fs::write(&broken, b"this is not a RIFF header").expect("write the fake master");

    let mut state = AppState::load(&path);
    state.add_inputs([broken.clone()]);

    // The file keeps its place in the set — one unreadable input is one task's failure, not
    // the batch's — and the reason is mint's own, on the row it belongs to.
    assert_eq!(state.sources.len(), 1, "the file must not remove itself");
    let source = state
        .problems
        .iter()
        .find(|problem| matches!(problem.scope, Scope::Source(0)))
        .expect("the probe failure reaches the list");
    let expected = {
        let err = io_read::probe_source(&broken).expect_err("this header must not read");
        format!("{err:#}")
    };
    assert_eq!(source.message, expected, "mint's own probe failure, unedited");

    let message = source.message.clone();
    assert_on_screen(&panel(state), &message);
}

#[test]
fn a_blocker_that_needs_the_input_reaches_the_list_per_cell() {
    // FLAC above 96 kHz: a rule the config alone cannot check, because it takes the master's
    // rate to ask. It is therefore per (source × target) and not per target, which is the
    // whole reason `Scope::Cell` exists.
    let dir = scratch("blocked");
    let path = write_config(&dir, "\n[target.lossless]\ncodec = \"flac\"\nformat = \"s24\"\n");
    let master = dir.join("ultrasonic.wav");
    write_master(&master, 192_000);

    let mut state = AppState::load(&path);
    assert!(state.error().is_none(), "the config itself is fine: {:?}", state.error());
    state.add_inputs([master]);

    let cell = state
        .problems
        .iter()
        .find(|problem| matches!(&problem.scope, Scope::Cell { target, .. } if target == "lossless"))
        .expect("the blocker reaches the list");
    assert_eq!(cell.severity, Severity::Error);

    // Asked of the same target the panel asked, with the header the probe read — not a rate
    // this file believes FLAC tops out at.
    let probe = state.sources[0].probe.as_ref().expect("the master probes");
    let expected = state
        .config()
        .expect("the fixture resolves")
        .resolve_target("lossless")
        .expect("the target resolves")
        .delivery_blocker(probe.sample_rate, probe.channels)
        .expect("192 kHz into FLAC must be blocked");
    assert_eq!(cell.message, expected, "mint's own FLAC ceiling message, unedited");

    let message = cell.message.clone();
    assert_on_screen(&panel(state), &message);
}

#[test]
fn a_document_that_does_not_parse_says_so_and_stops() {
    // Step 1 is terminal on purpose: a compiler that cannot parse does not go on to run
    // borrowck, and a list that guessed past the syntax error would be guessing.
    let dir = scratch("syntax");
    let path = dir.join("mint.toml");
    let text = "[target.cd\nformat = \"s16\"\n";
    std::fs::write(&path, text).expect("write config");

    let state = AppState::load(&path);
    assert_eq!(state.problems.len(), 1, "{:#?}", state.problems);
    assert_eq!(state.problems[0].scope, Scope::Document);

    // toml_edit's own message, caret and all — the same producer `editor.rs` underlines
    // from, so the panel and the squiggle cannot point at different bytes.
    let expected = text.parse::<toml_edit::DocumentMut>().expect_err("this must not parse");
    assert_eq!(state.problems[0].message, expected.to_string());
}

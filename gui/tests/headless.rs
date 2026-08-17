//! Spike gate S2, driven without a window.
//!
//! `egui_kittest` runs the real frame function against a real egui context and exposes the
//! accessibility tree, which is how a UI can be asserted on rather than looked at. Two
//! things that buys beyond "it compiles":
//!
//! 1. The whole widget tree is *built* — every `ComboBox`, `DragValue` and `Grid` in the
//!    form runs its layout. An id collision, a panicking unwrap in a rarely-shown branch,
//!    or a borrow held across a closure shows up here and nowhere else.
//! 2. The provenance actually reaches the screen. The badges are the payoff of Tier 2, and
//!    asserting that `/` and `!` appear for the right target is the difference between "the
//!    library computes it" and "the user can see it".
//!
//! It also means this gate is answered on a machine with no display, which is how the
//! spike was largely exercised, and how a CI smoke job would run it.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint_gui::app::{Drawer, MintApp};
use mint_gui::state::AppState;

/// The proposal's demo config: a DSD target under a PCM-shaped `[defaults]`, so every fate
/// is on screen at once.
const DEMO: &str = r#"
[defaults]
dir = "./out"
format = "s24"
rate = 48000
bwf = true
preset = "cd"

[target.sacd]
preset = "sacd"
lufs = -16.0

[target.cd]
preset = "cd"
lufs = -12.0
"#;

/// An ordinary config with no traps in it. The negative control needs its own document:
/// `DEMO`'s `[defaults] preset` is dropped on *every* target, `cd` included, so a "nothing
/// is flagged here" assertion against `DEMO` would be asserting the wrong thing.
const PLAIN: &str = r#"
[defaults]
dir = "./out"

[target.cd]
preset = "cd"
lufs = -12.0
"#;

/// `format` stated on a DSD target — the own-layer error, which is legal to inherit and
/// fatal to state. Written out rather than appended to `DEMO`, because appending would put
/// the line under whichever target happens to be last.
const BROKEN: &str = r#"
[defaults]
dir = "./out"

[target.sacd]
preset = "sacd"
format = "s24"
"#;

fn harness(name: &str, config: &str, target: &str) -> Harness<'static, MintApp> {
    // A real file, because `AppState` is the thing under test and it loads one. Named per
    // test so the parallel harness cannot have two of these tread on each other.
    let dir = std::env::temp_dir().join(format!("mint-gui-headless-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(&path, config).expect("write config");

    let mut state = AppState::load(&path);
    state.selected = Some(target.to_string());
    let mut app = MintApp::new(state);
    // The form is the drawer's "Target" tab now, and Problems is the drawer's default.
    app.select_drawer(Drawer::Target);
    Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), app)
}

#[test]
fn the_target_form_builds_a_frame_for_a_dsd_target_under_pcm_defaults() {
    let mut harness = harness("form", DEMO, "sacd");
    harness.run();

    // The five S2 fields are all on screen, by name.
    for field in ["preset", "codec", "format", "lufs", "dir"] {
        harness.get_by_label(field);
    }
    // ...and the resolved values next to them come from the library, including the two
    // that no plain merge would produce.
    harness.get_by_label_contains("1-bit");
    // `query_all`, not `get`: the Targets pane's own summary line for this target ends in
    // `dsf` too, and two screens agreeing is not an ambiguity worth failing over.
    assert!(
        harness.query_all_by_label_contains("dsf").next().is_some(),
        "the resolved codec must reach the form"
    );
}

#[test]
fn the_provenance_badges_reach_the_screen() {
    let mut harness = harness("badges", DEMO, "sacd");
    harness.run();

    // `/` — `[defaults] preset = "cd"` parses and is never applied (report.md defect #14),
    //       so the preset row carries a dropped value.
    // `~` — `format` is inherited, carried, and read by nothing on a DSD target.
    // `!` — stating `format` here would be a load-time error rather than a warning.
    // All three are Tier 2 output. If they are missing, the badges are decoration.
    for glyph in ["/", "~", "!"] {
        assert!(
            harness.query_all_by_label(glyph).next().is_some(),
            "no {glyph} badge on screen"
        );
    }
}

#[test]
fn a_plain_pcm_target_is_not_covered_in_warnings() {
    // The negative control. If every target showed badges, the badges would say nothing.
    //
    // Only `/` is asserted absent. `!` is not: since Phase 2 the drawer holds the whole
    // 22-row inspector rather than five fields, and `dsd` is legitimately illegal to state
    // on a PCM target — `tests/explain.rs` pins that it is the only such field, and
    // `gui/tests/inspector.rs` pins that the inspector shows exactly one of them.
    let mut harness = harness("plain", PLAIN, "cd");
    harness.run();
    assert!(
        harness.query_all_by_label("/").next().is_none(),
        "a plain target should have nothing dropped"
    );
}

#[test]
fn every_mode_and_drawer_tab_builds_a_frame() {
    // Each mode and each drawer tab is a separate widget tree, and most of them are never
    // entered by the tests above. A screen that panics on first paint is a bad way to find
    // out at a listening session, and this is a two-line guard against it.
    let mut harness = harness("tabs", DEMO, "sacd");
    for tab in ["Plan", "Run", "Report", "Problems", "mint.toml", "Target"] {
        harness.run();
        // `contains`, because the Problems tab carries its count in the label.
        harness.get_by_label_contains(tab).click();
        harness.run();
    }
}

#[test]
fn an_invalid_document_renders_mints_own_error_rather_than_a_paraphrase() {
    // `format` stated on a DSD target — the own-layer error. The status bar must show what
    // mint said, because a GUI that rewords a diagnostic has invented a second one.
    let mut harness = harness("broken", BROKEN, "sacd");
    harness.run();
    // `query_all`, not `get`: the text appears both as the status line and as its hover
    // text, and "found it twice" is not a failure.
    assert!(
        harness.query_all_by_label_contains("no meaning for a DSD target").next().is_some(),
        "mint's own diagnostic must be what the status bar shows"
    );
}

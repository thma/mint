//! The Target Inspector, driven without a window.
//!
//! Phase 2's claim is that provenance is *visible*, and the only way to check a claim like
//! that is to build the frame and go looking for it in the accessibility tree. Two properties
//! this file is built around:
//!
//! - **The expected content is asked of the library, not written here.** The "every field is
//!   on screen" test iterates `resolve_target_explained`'s own rows, so adding a field to
//!   `RawTarget` extends the test rather than leaving it passing against a stale list of 22
//!   names. Same for the legend, which is `config::ORIGIN_LEGEND`.
//! - **The negative controls matter as much as the positives.** If every target showed a `/`
//!   the badge would carry no information, so a plain PCM target is asserted to have none —
//!   and to have exactly *one* `!`, which is `dsd`, the one field a PCM target may not state.
//!
//! One thing that cannot be asserted headlessly: strike-through is a `RichText` attribute and
//! does not reach the a11y tree. What is asserted instead is that the struck text and the
//! reason for it are both on screen; the strike itself is one line in `short_notes`.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint::config::Config;
use mint_gui::app::{Drawer, MintApp};
use mint_gui::inspector;
use mint_gui::state::AppState;

/// The proposal's demo config (Part III §3): one DSD target under a PCM-shaped `[defaults]`,
/// which is the realistic way a heterogeneous config goes wrong and the one arrangement that
/// exercises every fate at once.
const DEMO: &str = r#"
[defaults]
dir = "./out"
quality = "vhq"
format = "s24"
rate = 48000
bwf = true
dither = "psychoacoustic"
preset = "cd"

[target.sacd]
preset = "sacd"
lufs = -16.0
"#;

/// No traps: an ordinary PCM target, for the negative controls.
const PLAIN: &str = r#"
[defaults]
dir = "./out"

[target.cd]
preset = "cd"
lufs = -12.0
"#;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-inspector-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(format!("{name}.toml"))
}

fn harness(name: &str, config: &str, target: &str) -> Harness<'static, MintApp> {
    let path = scratch(name);
    std::fs::write(&path, config).expect("write config");

    let mut state = AppState::load(&path);
    state.selected = Some(target.to_string());
    let mut app = MintApp::new(state);
    app.select_drawer(Drawer::Target);
    // Deliberately tall: the inspector is 22 rows plus a "why" panel, and a harness sized for
    // a five-field form would be testing what fits rather than what is drawn.
    Harness::builder()
        .with_size([1400.0, 1600.0])
        .build_ui_state(|ui, app: &mut MintApp| app.frame(ui), app)
}

fn explanation(config: &str, target: &str) -> mint::config::TargetExplanation {
    Config::from_toml_str(config)
        .expect("fixture loads")
        .resolve_target_explained(target)
        .expect("target resolves")
        .1
}

#[test]
fn every_field_the_library_explains_reaches_the_table() {
    let mut harness = harness("all-fields", DEMO, "sacd");
    harness.run();

    // The list comes from the library, so a new `RawTarget` field is covered the day it is
    // added rather than the day someone remembers this file.
    for field in &explanation(DEMO, "sacd").fields {
        assert!(
            harness.query_all_by_label(field.field).next().is_some(),
            "the inspector must have a row for `{}`",
            field.field
        );
        // And the effective value beside it — the whole point of the row. `contains`, because
        // several of these are also the value of some other row (`false` is on four of them).
        assert!(
            harness.query_all_by_label_contains(&field.value).next().is_some(),
            "`{}` = {} must be on screen",
            field.field,
            field.value
        );
    }

    // Grouped, in the library's own words.
    for group in ["DELIVERABLE", "POLICY", "QUALITY", "DESTINATION"] {
        harness.get_by_label(group);
    }
    // ...under the legend `mint --explain` prints, from the same constant.
    assert!(
        harness.query_all_by_label(mint::config::ORIGIN_LEGEND).next().is_some(),
        "the origin legend must be the library's"
    );
    assert!(
        harness.query_all_by_label(mint::config::FATE_LEGEND).next().is_some(),
        "and so must the fate legend"
    );
}

#[test]
fn the_why_panel_shows_every_layer_including_the_ones_that_said_nothing() {
    // Part III §3's money shot: "why is rate = 88200 Hz?" — the four layers, what each said,
    // and the one whose answer was not read. A panel that showed only the layers that spoke
    // would leave out `E this target — not set`, which is half the explanation.
    let mut harness = harness("why", DEMO, "sacd");
    harness.run();
    // Through the same door `select_mode` is: `rate` is the fifth row of a table that
    // scrolls, and what is under test is what the panel says rather than whether a row is
    // currently under the pointer. That a row *is* clickable is pinned separately, below.
    harness.state_mut().inspector_mut().expand("rate");
    harness.run();

    assert!(
        harness.query_all_by_label_contains("why is  rate").next().is_some(),
        "clicking a row opens its why panel"
    );
    // Every layer of the stack, named — including `preset 'sacd'`, which sets no rate at all,
    // and `this target`, which sets none either.
    for layer in &explanation(DEMO, "sacd").layers {
        assert!(
            harness.query_all_by_label_contains(&layer.to_string()).next().is_some(),
            "the why panel must list {layer}"
        );
    }
    // The dropped one, its value, and the verdict — the exception the facility exists for.
    assert!(harness.query_all_by_label_contains("rate = 48000").next().is_some());
    harness.get_by_label("✗ NOT READ");
    // ...and the library's own sentence about why, in full rather than as the column's
    // shortened `D rate=48000 dropped`.
    assert!(
        harness.query_all_by_label_contains("power of two").next().is_some(),
        "the why panel carries the whole reason, not the short form"
    );
}

#[test]
fn a_dropped_defaults_preset_is_surfaced_rather_than_reproduced() {
    // §7 item 12. `[defaults] preset = "cd"` parses and is never applied (report.md defect
    // #14). The GUI must not apply it either, and must not hide it: the row says the value is
    // there and struck through, and mint's own sentence says why.
    let mut harness = harness("defaults-preset", DEMO, "sacd");
    harness.run();

    // The effective preset is the target's own, not `[defaults]`'.
    assert!(
        harness.query_all_by_label_contains("'sacd' sets").next().is_some(),
        "the header names the preset that was actually applied"
    );
    // The short form in the table: `D preset=cd`, struck through (the strike is a RichText
    // attribute and does not reach the a11y tree; the text does).
    harness.get_by_label("D preset=cd");

    // The first row of the table, so this one is genuinely clicked — the row-opens-its-panel
    // wiring is pinned here once, and the other tests use `expand` for rows further down.
    harness.get_by_label("preset").click();
    harness.run();
    assert_eq!(harness.state().inspector_expanded(), Some("preset"), "a row click expands it");
    assert!(
        harness.query_all_by_label_contains("never applied").next().is_some(),
        "and the reason is mint's, in the why panel"
    );
}

#[test]
fn a_plain_pcm_target_is_not_covered_in_badges() {
    let mut harness = harness("plain", PLAIN, "cd");
    harness.run();

    assert!(
        harness.query_all_by_label("/").next().is_none(),
        "nothing is dropped on a plain target, so no `/` may appear"
    );
    // Exactly one `!`, and it is `dsd`: legal to inherit, and needing `codec = \"dsf\"` to
    // state. `tests/explain.rs` pins that it is the only one; this pins that the inspector
    // shows it and nothing else.
    assert_eq!(
        harness.query_all_by_label("!").count(),
        1,
        "only the field a PCM target may not state carries a `!`"
    );
}

#[test]
fn ticking_set_here_writes_the_value_that_was_already_in_effect() {
    // The override affordance. It writes what is already effective — an explicit no-op the
    // user asked for — rather than an invented number, and it writes exactly one line.
    let mut harness = harness("override", PLAIN, "cd");
    harness.run();
    let before = harness.state().state().text.clone();
    let effective = explanation(PLAIN, "cd")
        .field("ceiling_dbtp")
        .expect("row")
        .value
        .clone();

    harness.state_mut().inspector_mut().expand("ceiling_dbtp");
    harness.run();
    harness.get_by_label("set here").click();
    harness.run();

    let after = &harness.state().state().text;
    assert_eq!(
        added_lines(&before, after),
        vec!["ceiling_dbtp = -0.1"],
        "one key, and the value the preset had already put in effect ({effective})"
    );
    // The preset is still one line: the tool never expands `preset = \"cd\"` into the fields
    // it sets (anti-feature #2).
    assert!(after.contains("preset = \"cd\""), "{after}");
    assert!(harness.state().state().dirty, "the document is now unsaved");

    // ...and removing it puts the document back exactly as it was, byte for byte.
    harness.get_by_label("remove from this target").click();
    harness.run();
    assert_eq!(harness.state().state().text, before, "the key goes, the formatting stays");
}

#[test]
fn the_tally_is_the_compact_form_of_the_badges() {
    // Part III §3's "compact form for the matrix". Counted off the explanation, so it cannot
    // disagree with the badges it summarizes.
    let sacd = inspector::tally(&explanation(DEMO, "sacd"));
    assert_eq!(sacd.own, 1, "only `lufs` is stated on the target itself, `preset` excluded");
    assert!(sacd.dropped >= 3, "preset, rate and bwf are all dropped here: {sacd:?}");
    assert!(sacd.inert > 0, "format and the dither knobs are carried and unread");
    assert!(sacd.to_string().contains("dropped"), "{sacd}");

    let cd = inspector::tally(&explanation(PLAIN, "cd"));
    assert_eq!(cd.dropped, 0, "nothing is dropped on a plain target");
    assert!(!cd.to_string().contains("dropped"), "and the summary does not say `0 dropped`");
}

/// Lines present in `after` and not in `before`, trimmed. A crude diff, which is the right
/// tool here: what is being asserted is that a field edit adds one line and disturbs nothing.
fn added_lines(before: &str, after: &str) -> Vec<String> {
    let old: Vec<&str> = before.lines().collect();
    after
        .lines()
        .filter(|line| !old.contains(line))
        .map(|line| line.trim().to_string())
        .collect()
}

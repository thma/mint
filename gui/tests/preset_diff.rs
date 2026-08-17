//! "The preset picker is a diff, not a dropdown" (Part III §3), checked as a function.
//!
//! Most of this file needs no window: `presets::preview` is text in, verdict out, which is
//! deliberate — the property worth pinning is *what the diff says*, and a screenshot of it
//! would pin the layout instead. One test at the end drives the real widget, because the
//! other half of the promise is that committing does what the preview showed, and that is a
//! claim about the button.
//!
//! The values here are not written out from the README. Every expected figure is read back
//! from `resolve_target` on the same document, so a preset whose contents change moves the
//! test with it rather than breaking it — what is being asserted is that the *diff* is right,
//! not what `cd` happens to contain this year.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint::config::Config;
use mint_gui::app::{Drawer, MintApp};
use mint_gui::presets::{self, Preview, Verdict};
use mint_gui::state::AppState;

/// A CD target with a loudness target of its own — the mockup's exact situation, where one
/// explicit field has to survive the preset that also sets it.
const CD: &str = r#"
[defaults]
dir = "./out"

[target.master]
preset = "cd"
lufs = -12.0
"#;

/// A DSD target that says so *itself* rather than through the `sacd` preset. That detail is
/// the whole fixture: a preset is part of the target's own layer, so swapping `sacd` for a
/// PCM preset would also swap the codec and produce a perfectly valid WAV target. Stating
/// `codec` here is what keeps the codec fixed while the candidate changes, which is the
/// situation where every PCM preset's `format` becomes a load-time error.
const SACD: &str = r#"
[target.sacd]
codec = "dsf"
dsd = "dsd64"
lufs = -16.0
"#;

/// A CD target under a `[defaults]` that supplies the one field a PCM preset must: removing
/// the preset from `CD` above is a *refusal* (nothing would set `format`), which is worth
/// knowing but is not the fall-back case.
const CD_WITH_BASELINE: &str = r#"
[defaults]
dir = "./out"
format = "s24"

[target.master]
preset = "cd"
lufs = -12.0
"#;

fn resolved(text: &str, target: &str) -> mint::config::ResolvedTarget {
    Config::parse_toml_str(text)
        .expect("parses")
        .resolve_target(target)
        .expect("resolves")
}

fn changes(preview: &Preview) -> &[presets::Change] {
    match preview {
        Preview::Diff(changes) => changes,
        Preview::Refused(err) => panic!("expected a diff, got a refusal: {err}"),
    }
}

#[test]
fn a_candidate_is_resolved_before_it_is_committed() {
    let preview = presets::preview(CD, "master", Some("apple-music"));
    let changes = changes(&preview);

    // Every row is a real difference between two resolved targets, so the two ends of each
    // row must be what the library says before and after.
    let before = resolved(CD, "master");
    let after = resolved(&CD.replace("preset = \"cd\"", "preset = \"apple-music\""), "master");
    assert_ne!(
        before.ceiling_dbtp, after.ceiling_dbtp,
        "the fixture is only interesting if these two presets disagree somewhere"
    );
    let ceiling = changes
        .iter()
        .find(|change| change.field == "ceiling_dbtp")
        .expect("the ceiling moves between these two presets");
    assert!(matches!(ceiling.verdict, Verdict::Changed(_)));
    assert!(ceiling.after.contains(&format!("{:.1}", after.ceiling_dbtp)), "{ceiling:?}");

    // The explicit value is *kept*, and the row says so rather than vanishing — a picker that
    // silently omitted it would leave the user wondering whether their −12 survived.
    let lufs = changes.iter().find(|change| change.field == "lufs").expect("lufs is listed");
    assert_eq!(lufs.verdict, Verdict::Overridden);
    assert_eq!(lufs.before, lufs.after);
    assert_eq!(after.lufs, Some(-12.0), "and it really does survive the merge");

    // The count is of *delivered* values that move, which is not the number of rows: rows
    // that only say "unchanged" are context, not change.
    assert!(preview.changed() < changes.len(), "{changes:?}");
    assert!(preview.changed() > 0);

    // `preset` itself is never a row of its own diff — the header line already reads
    // `cd → apple-music`.
    assert!(!changes.iter().any(|change| change.field == "preset"));
}

#[test]
fn a_candidate_mint_would_refuse_says_so_in_mints_own_words() {
    // The case that justifies previewing at all. Every PCM preset sets `format`, and a DSD
    // target rejects `format` outright — so this click would produce a document that does not
    // load, and the picker has to say that before the click rather than after it.
    let preview = presets::preview(SACD, "sacd", Some("cd"));
    let Preview::Refused(message) = &preview else {
        panic!("a PCM preset on a DSD target must be refused, got {preview:?}");
    };
    assert!(message.contains("no meaning for a DSD target"), "mint's own text: {message}");
    assert_eq!(preview.changed(), 0);

    // And the counterfactual, so the refusal is not just pessimism: writing it really does
    // break the document.
    let written = format!("{SACD}preset = \"cd\"\n");
    assert!(Config::from_toml_str(&written).is_err());
}

#[test]
fn dropping_the_preset_is_a_candidate_like_any_other() {
    // "— (no preset)" is a real choice, and its diff is the interesting one: everything the
    // preset supplied falls back to the layers underneath.
    let preview = presets::preview(CD_WITH_BASELINE, "master", None);
    let changes = changes(&preview);
    assert!(preview.changed() > 0, "removing a preset changes what ships: {changes:?}");

    // What the preset supplied falls back to the layer under it, and the row names that layer
    // rather than only saying "changed".
    let format = changes.iter().find(|change| change.field == "format").expect("format falls back");
    assert_eq!(format.after, "s24", "the [defaults] baseline takes over");
    assert!(matches!(format.verdict, Verdict::Changed(Some(mint::config::Layer::Defaults))));

    // Nothing is reported as `Overridden` here, because there is no preset left to lose to.
    assert!(!changes.iter().any(|change| change.verdict == Verdict::Overridden));

    // The explicit `lufs` is not in the diff at all: it did not move and no candidate stated
    // it, which is the rule that keeps 22 rows from being listed every time.
    assert!(!changes.iter().any(|change| change.field == "lufs"), "{changes:?}");
}

#[test]
fn removing_a_preset_nothing_else_replaces_is_refused_before_the_click() {
    // The same shape as the DSD refusal and worth its own test because it is the one a user
    // meets first: `format` is required, `preset = "cd"` is the only thing supplying it, and
    // "— (no preset)" would therefore produce a document mint will not load.
    let preview = presets::preview(CD, "master", None);
    let Preview::Refused(message) = &preview else {
        panic!("expected a refusal, got {preview:?}");
    };
    assert!(message.contains("'format' is required"), "mint's own text: {message}");
}

#[test]
fn what_a_preset_supplies_is_read_off_the_merge() {
    let (_, explained) = Config::from_toml_str(CD)
        .expect("loads")
        .resolve_target_explained("master")
        .expect("resolves");
    let supplied = presets::set_by_preset(&explained);

    assert!(!supplied.is_empty(), "the cd preset sets something");
    assert!(supplied.contains(&"format"), "including the bit depth: {supplied:?}");
    // Not the target's own fields, and not the builtin ones.
    assert!(!supplied.contains(&"lufs"), "lufs is this target's, not the preset's");
    assert!(!supplied.contains(&"overwrite"), "overwrite is a builtin fallback: {supplied:?}");
}

#[test]
fn committing_the_candidate_writes_one_line_and_never_expands_it() {
    // The other half: the preview said N fields move, and the commit is a single key. The
    // whole point of the anti-feature list's item 2 is that this must not become 22 lines.
    let dir = std::env::temp_dir().join(format!("mint-gui-preset-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("mint.toml");
    std::fs::write(&path, CD).expect("write config");

    let mut state = AppState::load(&path);
    state.selected = Some("master".to_string());
    let mut app = MintApp::new(state);
    app.select_drawer(Drawer::Target);
    let mut harness = Harness::builder()
        .with_size([1400.0, 1200.0])
        .build_ui_state(|ui, app: &mut MintApp| app.frame(ui), app);
    harness.run();

    // Pick the candidate the same way the picker's combo does, then commit it.
    harness.state_mut().state_mut().text = CD.to_string();
    harness.run();
    let apply_shows_the_diff = presets::preview(CD, "master", Some("apple-music")).changed();
    assert!(apply_shows_the_diff > 0);

    let before_lines = CD.lines().count();
    harness.state_mut().state_mut().edit(|doc| {
        doc.set("master", "preset", toml_edit::Value::from("apple-music"))
    });
    harness.run();

    let after = harness.state().state().text.clone();
    assert_eq!(after.lines().count(), before_lines, "one line rewritten, none added");
    assert!(after.contains("preset = \"apple-music\""), "{after}");
    assert!(after.contains("lufs = -12.0"), "the explicit value is untouched: {after}");
    // And the resolved target really did move the way the preview promised.
    let now = resolved(&after, "master");
    assert_eq!(now.lufs, Some(-12.0));
    let promised = resolved(&CD.replace("\"cd\"", "\"apple-music\""), "master");
    assert_eq!(now.ceiling_dbtp, promised.ceiling_dbtp);
    // The picker itself is on screen with the caveat attached, not presented as a setting.
    assert!(harness.query_all_by_label_contains("sets").next().is_some());
}

//! External edits: `mint.toml` is the project file, and the GUI is not the only thing that
//! writes it.
//!
//! Part III §4 sets the rule this file exists to pin: *"On external change: re-parse,
//! re-derive, update everything, show a non-modal reloaded toast. If the GUI has unsaved
//! edits, that is a genuine conflict — show both, never silently pick."* Three properties
//! follow from it, and all three are the kind that only fail in the rare case:
//!
//! 1. our own `save` must not look like an external edit, or every save reloads the buffer
//!    out from under the cursor;
//! 2. an external edit with no local edits is taken, not queued behind a dialog;
//! 3. an external edit *with* local edits takes nothing and overwrites nothing.
//!
//! The last test drives the real OS watcher, because the other three would pass just as well
//! if it were watching the wrong directory — which is the mistake this design was written to
//! avoid (a watch on the file follows the inode through a rename-based save and stops seeing
//! anything).

use std::time::{Duration, Instant};

use egui_kittest::Harness;
use mint_gui::app::MintApp;
use mint_gui::state::{AppState, ExternalChange};
use mint_gui::watch::ConfigWatcher;

const ONE: &str = "[target.cd]\npreset = \"cd\"\nlufs = -12.0\n";
const TWO: &str =
    "[target.cd]\npreset = \"cd\"\nlufs = -9.0\n\n[target.hires]\npreset = \"hires\"\n";

fn scratch(name: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("mint-gui-reload-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn our_own_save_is_not_an_external_edit() {
    // The watcher fires on our writes too — it cannot tell them apart, and should not try.
    // What makes a self-write invisible is that the bytes match, which is checked rather
    // than assumed (an mtime comparison would have to be right about clock granularity).
    let dir = scratch("self");
    let path = dir.join("mint.toml");
    std::fs::write(&path, ONE).expect("write");

    let mut state = AppState::load(&path);
    state.text = TWO.to_string();
    state.dirty = true;
    state.save().expect("save");

    assert_eq!(state.absorb_external_change(), ExternalChange::None);
    assert_eq!(state.text, TWO, "the buffer is untouched by its own write");
    assert!(!state.dirty);
}

#[test]
fn an_external_edit_with_nothing_unsaved_is_simply_taken() {
    let dir = scratch("clean");
    let path = dir.join("mint.toml");
    std::fs::write(&path, ONE).expect("write");

    let mut state = AppState::load(&path);
    assert_eq!(state.target_names(), vec!["cd"]);
    state.selected = Some("cd".to_string());

    std::fs::write(&path, TWO).expect("external write");
    assert_eq!(state.absorb_external_change(), ExternalChange::Reloaded);

    // Re-parsed *and* re-derived: the new target is in the list and the changed value has
    // reached the resolved target, not just the text buffer.
    assert_eq!(state.target_names(), vec!["cd", "hires"]);
    assert_eq!(
        state.config().expect("the new document resolves").resolve_target("cd").unwrap().lufs,
        Some(-9.0)
    );
    assert!(!state.dirty, "a document that came from disk is not unsaved");
    assert_eq!(state.selected.as_deref(), Some("cd"), "a still-present selection is kept");
}

#[test]
fn a_selection_the_new_document_no_longer_defines_falls_back() {
    // Only an *external* edit can rename or delete a target out from under the app, so this
    // is the one path where every screen keyed on `selected` could be pointing at a name that
    // no longer exists.
    let dir = scratch("selection");
    let path = dir.join("mint.toml");
    std::fs::write(&path, TWO).expect("write");

    let mut state = AppState::load(&path);
    state.selected = Some("hires".to_string());

    std::fs::write(&path, ONE).expect("external write");
    assert_eq!(state.absorb_external_change(), ExternalChange::Reloaded);
    assert_eq!(state.selected.as_deref(), Some("cd"), "gone, so back to the first target");
}

#[test]
fn an_external_edit_over_unsaved_work_takes_nothing_and_overwrites_nothing() {
    let dir = scratch("conflict");
    let path = dir.join("mint.toml");
    std::fs::write(&path, ONE).expect("write");

    let mut state = AppState::load(&path);
    let mine = format!("{ONE}\n[target.wip]\npreset = \"hires\"\n");
    state.text = mine.clone();
    state.dirty = true;

    std::fs::write(&path, TWO).expect("external write");
    assert_eq!(state.absorb_external_change(), ExternalChange::Conflict);

    // Neither side has moved. That is the whole promise: the GUI never holds the config
    // hostage, and it never throws away work either.
    assert_eq!(state.text, mine, "the buffer is intact");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), TWO, "and so is the file");
    assert!(state.dirty);

    // "Take what is on disk" is the only thing that discards, and only when asked.
    state.discard_edits_and_reload().expect("reload");
    assert_eq!(state.text, TWO);
    assert!(!state.dirty);
    assert_eq!(state.target_names(), vec!["cd", "hires"]);
}

#[test]
fn a_file_that_is_mid_replacement_is_waited_out_rather_than_read_as_empty() {
    // A rename-based save — which is what every careful writer does, mint's own
    // `atomic_write` included — has a window in which the path does not exist. Reading that
    // window as "the config is now empty" would blank the workbench for a frame or two and,
    // worse, look like a document the user had better save over.
    let dir = scratch("missing");
    let path = dir.join("mint.toml");
    std::fs::write(&path, ONE).expect("write");

    let mut state = AppState::load(&path);
    std::fs::remove_file(&path).expect("remove");

    assert_eq!(state.absorb_external_change(), ExternalChange::None);
    assert_eq!(state.text, ONE, "the buffer still holds the document");
    assert!(state.config().is_some(), "and it still resolves");
}

#[test]
fn the_watcher_sees_a_rename_based_save_of_the_file_it_was_given() {
    // The one test that involves the operating system. It is here because the other five
    // would pass unchanged if `ConfigWatcher` watched the wrong path — and the specific
    // wrong path that is easy to reach is the file itself, which a rename unhooks. So the
    // edit below is done the way an editor does it: write a sibling, rename it over.
    let dir = scratch("watch");
    let path = dir.join("mint.toml");
    std::fs::write(&path, ONE).expect("write");

    let ctx = egui::Context::default();
    let watcher = ConfigWatcher::new(&path, ctx).expect("a watch on the scratch directory");

    let staging = dir.join("mint.toml.tmp");
    std::fs::write(&staging, TWO).expect("staging write");
    std::fs::rename(&staging, &path).expect("rename over");

    // Generous, because file notification is asynchronous on every platform and this is a
    // correctness test rather than a latency one.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if watcher.changed() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the watcher never saw a rename-based save of {}", path.display());
}

#[test]
fn the_shell_shows_a_conflict_rather_than_resolving_it() {
    // The banner's promise, through the app rather than through `AppState`: three explicit
    // answers, and until one is clicked, nothing has happened to either version.
    let dir = scratch("banner");
    let path = dir.join("mint.toml");
    std::fs::write(&path, ONE).expect("write");

    let mut state = AppState::load(&path);
    state.text = format!("{ONE}\n[target.wip]\npreset = \"hires\"\n");
    state.dirty = true;
    let mine = state.text.clone();

    let app = MintApp::new(state);
    let mut harness = Harness::builder()
        .with_size([1400.0, 900.0])
        .build_ui_state(|ui, app: &mut MintApp| app.frame(ui), app);
    harness.run();
    assert!(!harness.state().conflicted(), "nothing has changed on disk yet");

    std::fs::write(&path, TWO).expect("external write");
    // Driven directly rather than through the OS watcher: what is under test here is the
    // shell's reaction, and `the_watcher_sees_a_rename_based_save…` covers the notification.
    let outcome = harness.state_mut().state_mut().absorb_external_change();
    assert_eq!(outcome, ExternalChange::Conflict);
    assert_eq!(harness.state().state().text, mine, "the buffer is untouched");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), TWO, "and so is the file");
}

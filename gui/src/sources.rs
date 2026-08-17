//! The Sources pane: the input set, with the header mint read from each file.
//!
//! One row per file — name, then `rate · channels · duration · size` from
//! `SourceProbe` and `util::{fmt_bytes, duration_seconds}`, so the figures match the ones
//! the CLI prints rather than being rounded a second way here. That is also why the
//! duration reads `312.4 s` and not the wireframe's `5:12`: seconds with one decimal is
//! what `cli.rs`'s report line prints, and `util` has no mm:ss formatter to share.
//!
//! Two things this pane must get right, both of them about not hiding a fact:
//!
//! - **A file whose probe failed stays in the list.** `probe: Err` is per-file and is not
//!   fatal to the batch (`cli.rs`'s `ProbedInput` says so explicitly), so the row shows
//!   mint's own error inline and keeps its place. Silently dropping it would turn "one bad
//!   file" into "a file I am sure I added is gone".
//! - **Duration needs `frames`.** A container that does not declare a length gets `—`, not
//!   a guess. `SourceProbe::frames` is an `Option` for exactly this reason, and so is
//!   `channels` — the em dash is hoverable and says which figure the header withheld.
//!
//! Selecting a row sets `selected_source`, which with the selected target names the cell
//! the chain preview describes.
//!
//! ## The picker and the extension list
//!
//! `pick_files` filters on `util::SUPPORTED_INPUT_EXTENSIONS` — the library's own list,
//! never a copy of it. A native dialog cannot be driven by a predicate, so that const
//! exists precisely so this call site does not have to spell `wav|wave|aif|aiff` out a
//! second time; `is_supported_input` reads the same const, which is what keeps the dialog's
//! filter and the sieve in `AppState::add_inputs` from ever disagreeing.
//!
//! The sieve still runs regardless, because a dialog filter is advice: the user can type a
//! path, and every drag-and-drop arrives past the dialog entirely. Anything it refuses is
//! reported rather than silently eaten.

use std::path::PathBuf;

use egui::{Button, Id, RichText, Ui};
use mint::util::{duration_seconds, fmt_bytes};

use crate::colors;
use crate::state::{AppState, Source};

/// One deferred mutation of the source set.
///
/// The rows are drawn from a `&AppState` and every one of these needs a `&mut` one, so the
/// click is recorded and carried out after the list has been laid out — the same shape
/// `form.rs` uses for its queued `Edit`, and the reason a row can both read `sources` and
/// offer to remove itself.
enum Action {
    Select(usize),
    Remove(usize),
    Add(Vec<PathBuf>),
    Clear,
}

/// The pane. Rows, the `+` button, per-row remove, and clear.
pub fn ui(ui: &mut Ui, state: &mut AppState) {
    let mut action = None;
    list(ui, state, &mut action);

    match action {
        Some(Action::Select(index)) => state.selected_source = Some(index),
        Some(Action::Remove(index)) => state.remove_source(index),
        Some(Action::Clear) => state.clear_sources(),
        Some(Action::Add(paths)) => add(ui, state, paths),
        None => {}
    }
}

/// Add files and say what was dropped on the floor.
///
/// The one door for both ways in. `add_inputs` sieves, which is right for it and silent,
/// which is wrong for a user: a file you picked — or dragged onto the window — and cannot
/// find afterwards needs an explanation. Drag-and-drop is the *documented* primary route
/// ("drop masters on the window, or add them with +"), so it is the half that must not be
/// the silent one; `app.rs` routes its drop through here for exactly that reason.
///
/// The count is the library predicate's own verdict, handed back — neither surface holds an
/// opinion about which extensions mint decodes.
pub fn add(ui: &Ui, state: &mut AppState, paths: Vec<PathBuf>) {
    let picked = paths.len();
    let refused = state.add_inputs(paths);
    remember_skipped(
        ui,
        (refused > 0)
            .then(|| format!("{refused} of {picked} added files are not ones mint can decode")),
    );
}

/// Everything that only reads. Split out so the closure below borrows `AppState` shared
/// while the caller still holds the `&mut` it needs to act on a click.
fn list(ui: &mut Ui, state: &AppState, action: &mut Option<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("SOURCES").strong());
        ui.weak(format!("({})", state.sources.len()));
        if ui.small_button("+").on_hover_text("add master files…").clicked() {
            *action = Some(Action::Add(pick_files()));
        }
        if ui
            .add_enabled(!state.sources.is_empty(), Button::new("clear").small())
            .clicked()
        {
            *action = Some(Action::Clear);
        }
    });

    if let Some(note) = skipped_note(ui) {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(colors::WARN, format!("⚠ {note}"));
            // Dismissible: the next trip through the picker may never come, and a warning
            // that cannot be put down is one that stops being read.
            if ui.small_button("✕").clicked() {
                remember_skipped(ui, None);
            }
        });
    }
    if state.sources.is_empty() {
        ui.weak("drop masters on the window, or add them with +");
        return;
    }

    egui::ScrollArea::vertical().id_salt("sources").auto_shrink([false, false]).show(ui, |ui| {
        for (index, source) in state.sources.iter().enumerate() {
            // Ids are derived from the *path*, not the index: removing a row would
            // otherwise hand its widget ids to the row that shifted up into its place.
            ui.push_id(&source.path, |ui| {
                row(ui, index, source, state.selected_source == Some(index), action);
            });
            ui.separator();
        }
    });
}

fn row(ui: &mut Ui, index: usize, source: &Source, selected: bool, action: &mut Option<Action>) {
    let name = source.path.file_name().unwrap_or_default().to_string_lossy().to_string();

    ui.horizontal(|ui| {
        // Glyph as well as highlight: which cell the chain preview describes is worth
        // reading without comparing two shades of background.
        let dot = if selected { "●" } else { "○" };
        let label = RichText::new(format!("{dot} {name}")).monospace();
        if ui
            .selectable_label(selected, label)
            .on_hover_text(source.path.display().to_string())
            .clicked()
        {
            *action = Some(Action::Select(index));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✕").on_hover_text("remove from the set").clicked() {
                *action = Some(Action::Remove(index));
            }
        });
    });

    match &source.probe {
        Ok(probe) => {
            let rate = format!("{} Hz", probe.sample_rate);
            let channels = probe.channels.map_or_else(|| "—".to_string(), |n| format!("{n} ch"));
            let duration = probe.frames.map_or_else(
                || "—".to_string(),
                |frames| format!("{:.1} s", duration_seconds(frames, probe.sample_rate)),
            );
            let size = source.file_bytes.map_or_else(|| "—".to_string(), fmt_bytes);
            ui.weak(format!("{rate} · {channels} · {duration} · {size}"))
                // The em dashes are the header's silence, not a rounding: say which figure
                // is missing rather than letting it read as zero.
                .on_hover_text(match (probe.channels, probe.frames) {
                    (Some(_), Some(_)) => "from the file header".to_string(),
                    (None, _) => "the container does not declare a channel count".to_string(),
                    (_, None) => "the container does not declare a frame count".to_string(),
                });
        }
        // mint's own message, in full and un-truncated. The row keeps its place: one
        // unreadable input is one task's failure, not the end of the set.
        Err(error) => {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(colors::BAD, "✖");
                ui.colored_label(colors::BAD, error);
            });
        }
    }
}

/// Native multi-select open dialog. Empty when the user cancelled.
///
/// No extension filter, and no `set_directory` either — the last-used directory is the
/// platform dialog's own memory, which is better than any guess this pane could make.
pub fn pick_files() -> Vec<PathBuf> {
    rfd::FileDialog::new()
        .set_title("Add masters")
        .add_filter("masters", mint::util::SUPPORTED_INPUT_EXTENSIONS)
        .pick_files()
        .unwrap_or_default()
}

/// Where the "these were not added" note lives between frames.
///
/// egui's per-id temp store rather than a field on `AppState`: no other screen reads this,
/// it is discarded with the window, and a field would invite something else to write it.
fn skipped_id() -> Id {
    Id::new("sources-skipped")
}

fn skipped_note(ui: &Ui) -> Option<String> {
    ui.ctx().data(|data| data.get_temp::<String>(skipped_id()))
}

/// `None` clears it — a picker run that dropped nothing must not leave the last one's
/// warning standing.
fn remember_skipped(ui: &Ui, note: Option<String>) {
    ui.ctx().data_mut(|data| match note {
        Some(note) => {
            data.insert_temp(skipped_id(), note);
        }
        None => {
            data.remove_temp::<String>(skipped_id());
        }
    });
}

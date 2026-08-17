//! Spike gate S4: the plan matrix — inputs × targets, one cell per job.
//!
//! Every cell is four library calls and no judgement of its own: `delivery_blocker` for
//! whether the job is possible at all, `render_output_path` for the filename,
//! `estimated_output_bytes` for the size *and its qualifier*, and `output_conflict` for
//! whether writing it would destroy something. The qualifier is the part worth keeping:
//! WAV and DSF are exact, FLAC is an upper bound, MP3 is approximate, and a matrix that
//! printed all four as plain numbers would be lying in two of the four cases.
//!
//! `delivery_blocker` needs the *input* to answer — FLAC above 96 kHz, DSD above 5.1 —
//! which is exactly why this screen cannot be filled in from the config alone, and why it
//! is the screen a CLI cannot show you before it starts working.
//!
//! ## The one verdict a cell cannot reach on its own
//!
//! Three of [`Status`]'s four states are per-cell questions with per-cell answers. The
//! fourth — another job in this batch wants the same output path — is a property of the
//! whole plan, and `check_planned_outputs` is what knows it. `problems::collect` step 8
//! already walks it, so this module *looks its answer up* by (source, target) rather than
//! reimplementing the accept-list walk for the benefit of one glyph.
//!
//! ## Cost
//!
//! `egui_table`'s `Table` is virtualized, and that is the whole reason the per-cell work can
//! be this real: only the cells on screen are built. `tests/plan_matrix.rs` measures a fully
//! visible 20×8 at **~6400 µs/frame in a debug build**, against the 16 600 µs a 60 Hz frame
//! allows — unmoved by the conflict query and the selection this phase added. The one
//! syscall in the loop is `output_conflict`'s `symlink_metadata`, once per visible cell per
//! frame: that syscall *is* the `⟳` glyph, and virtualization is what keeps the count
//! bounded by the window rather than by the batch.
//!
//! Per-row and per-column totals are deliberately absent. A column total cannot be
//! virtualized (it has to touch rows that are off screen) and a bare number next to a
//! filename reads as that file's size rather than as a sum. The batch total belongs in the
//! project bar, where [`batch_estimate`] puts it and where it is labelled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use egui::{Color32, Response, RichText, Sense, Ui};
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use mint::audio::io_read::SourceProbe;
use mint::config::{ResolvedTarget, SizeEstimate};
use mint::pipeline::{self, Conflict};
use mint::util::fmt_bytes;

use crate::colors;
use crate::problems::{Problem, Scope, Severity};
use crate::state::{AppState, Source};

/// One cell's predicted output size, or `None` when the header did not declare enough to
/// ask.
///
/// `estimated_output_bytes` needs a frame count and a channel count, and `SourceProbe`
/// holds both as `Option` because some containers withhold them. Four surfaces want the
/// same answer — this matrix, the project bar's total, the chain preview's header and the
/// run's per-job prediction — and four copies of the same `zip`/`and_then` is four places
/// for the "missing figure means zero" mistake to be made once.
pub fn cell_estimate(probe: &SourceProbe, target: &ResolvedTarget) -> Option<SizeEstimate> {
    let (frames, channels) = probe.frames.zip(probe.channels)?;
    target.estimated_output_bytes(probe.sample_rate, frames, channels)
}

/// The whole batch's predicted size, for the project bar.
pub struct BatchEstimate {
    /// The targets a run would really use — `AppState::resolved_targets().len()`, which is
    /// the *same* list [`Self::jobs`] is a product of. Carried rather than left to the
    /// caller to count a second way: the caller has `included_names()` to hand and the two
    /// differ whenever the document does not validate (`resolved_targets` is then empty),
    /// which is how the bar came to print `2 sources × 3 targets = 0 jobs`.
    pub targets: usize,
    /// Every (source × included target) pair — what a run would **attempt**, which is what
    /// `N sources × M targets` multiplies out to and what the Run view lists. Blocked cells
    /// are counted here and reported separately in [`Self::blocked`]: `run.rs` deliberately
    /// does not filter them out, because `process_with` refusing one with mint's own reason
    /// is how that reason reaches the failures list at all.
    pub jobs: usize,
    /// Cells `delivery_blocker` refuses. Surfaced beside the count rather than subtracted
    /// from it — a job that will fail is still a job that will run.
    pub blocked: usize,
    /// Sum of every cell's `estimated_output_bytes`.
    pub bytes: u64,
    /// The weakest promise in the sum, kept as the library's own **variant** rather than as
    /// its rendered prefix. A total that mixes an exact WAV with an upper-bound FLAC is an
    /// upper bound, and printing it bare would be the "progress bar that lies" in another
    /// costume — but which characters say so is `SizeEstimate::qualifier`'s business, and a
    /// `"<= "` written out here is a spelling table for a library enum living in `gui/`.
    /// `None` while nothing has been added up.
    weakest: Option<SizeEstimate>,
    /// Cells whose size the header could not answer (`frames`/`channels` missing). Counted
    /// rather than folded into the sum as a zero.
    pub unknown: usize,
}

impl BatchEstimate {
    /// How much [`Self::bytes`] is worth, in `SizeEstimate`'s own spelling. One call site,
    /// so the bar's prefix and the matrix cells' prefixes cannot come from two tables.
    pub fn qualifier(&self) -> &'static str {
        self.weakest.map_or("", SizeEstimate::qualifier)
    }
}

/// Walk the matrix and add up what it would write.
///
/// Same library calls the cells make, in the same order and with the same skips, so the
/// bar's total and the matrix's cells cannot disagree about which jobs exist.
///
/// Called once per frame from the project bar and deliberately not cached: measured at
/// **620 µs for 160 cells in a debug build** (`tests/plan_matrix.rs`, 20×8), so a few tens
/// of µs in release. Unlike the matrix it is not virtualized — it has to touch every cell
/// to total them — so if a session ever runs to thousands of cells this is the first thing
/// to key on the same change hook `AppState::problems` uses.
pub fn batch_estimate(state: &AppState) -> BatchEstimate {
    let mut estimate =
        BatchEstimate { targets: 0, jobs: 0, blocked: 0, bytes: 0, weakest: None, unknown: 0 };
    let targets = state.resolved_targets();
    // The attempt count is the plain product, and it is counted here rather than derived
    // from the loop below so that a source whose header could not be read still contributes
    // its jobs — the run will attempt them, and mint will say why each one failed.
    estimate.targets = targets.len();
    estimate.jobs = state.sources.len() * targets.len();
    for source in &state.sources {
        let Ok(probe) = &source.probe else {
            estimate.unknown += targets.len();
            continue;
        };
        for target in &targets {
            if target.delivery_blocker(probe.sample_rate, probe.channels).is_some() {
                estimate.blocked += 1;
                continue;
            }
            match cell_estimate(probe, target) {
                Some(size) => {
                    estimate.bytes += size.bytes();
                    if estimate.weakest.is_none_or(|so_far| precision(size) < precision(so_far)) {
                        estimate.weakest = Some(size);
                    }
                }
                None => estimate.unknown += 1,
            }
        }
    }
    estimate
}

/// How much a cell's prediction is worth, as an order rather than as a spelling: `About`
/// (a nominal bitrate) is a weaker claim than `AtMost` (a real upper bound), which is weaker
/// than `Exact`. The aggregate carries the lowest one it saw.
///
/// Exhaustive on purpose, like every other match on a library enum in this tree: a fourth
/// `SizeEstimate` variant must stop this compiling rather than silently rank as the
/// strongest claim in the batch.
fn precision(size: SizeEstimate) -> u8 {
    match size {
        SizeEstimate::About(_) => 0,
        SizeEstimate::AtMost(_) => 1,
        SizeEstimate::Exact(_) => 2,
    }
}

/// What a cell's glyph says.
///
/// The enumeration is closed on purpose and the legend under the matrix is generated from
/// [`Status::ALL`], the same trick the config enums' `ALL` arrays play in
/// `tests/enum_domains.rs`: a state missing from the legend is not broken, it is
/// *unreadable*, which is worse because nobody notices.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    /// The job runs and writes a file that is not there yet.
    Ok,
    /// Something is already at the output path and this target has not consented to
    /// replacing it (`Conflict::Existing`). The run fails on this cell, but the remedy is a
    /// checkbox — and it is mint's own message that names it.
    Replaces,
    /// `delivery_blocker` said no, or the input has no name to render an output from.
    Blocked,
    /// The output path is refused outright: another job in the batch wants it, or it *is*
    /// the input master.
    Clash,
}

impl Status {
    const ALL: [Self; 4] = [Self::Ok, Self::Replaces, Self::Blocked, Self::Clash];

    /// Glyph **and** colour, together, always (capability #11) — roughly one man in twelve
    /// cannot tell this green from this red, so colour never carries the meaning alone.
    fn glyph(self) -> (&'static str, Color32) {
        match self {
            Self::Ok => ("✓", colors::OK),
            Self::Replaces => ("⟳", colors::WARN),
            Self::Blocked => ("!", colors::BAD),
            Self::Clash => ("✗", colors::BAD),
        }
    }

    /// The legend's wording. Describes the *state*, never the diagnostic: the diagnostic is
    /// mint's and lives in the cell's tooltip.
    fn legend(self) -> &'static str {
        match self {
            Self::Ok => "will write",
            Self::Replaces => "replaces an existing file — fails without force",
            Self::Blocked => "target will fail for this input",
            Self::Clash => "output path refused",
        }
    }
}

/// Everything one cell has to say, decided before anything is drawn so the tooltip can be
/// hung on the cell-wide click target rather than on whichever label happens to carry it.
struct Verdict {
    status: Status,
    /// mint's own prose for the status, when the status has any. Never a paraphrase.
    note: Option<String>,
    /// `None` when there is no file to name.
    output: Option<PathBuf>,
    size: Option<SizeEstimate>,
}

/// Ask the library about one (source × target) pair, in the order the answers foreclose
/// each other.
fn verdict(
    input: &Path,
    probe: &SourceProbe,
    target: &ResolvedTarget,
    collision: Option<&str>,
) -> Verdict {
    // Asked first and asked with the input, because a job that cannot run has no filename
    // and no size worth showing.
    if let Some(reason) = target.delivery_blocker(probe.sample_rate, probe.channels) {
        return Verdict { status: Status::Blocked, note: Some(reason), output: None, size: None };
    }
    let out_rate = target.delivery_rate(probe.sample_rate);
    let output = match target.render_output_path(input, out_rate) {
        Ok(path) => path,
        // Still `Blocked`: it is the same sentence — this target will fail for this input —
        // and the reason is mint's.
        Err(err) => {
            return Verdict {
                status: Status::Blocked,
                note: Some(format!("{err:#}")),
                output: None,
                size: None,
            };
        }
    };
    let size = cell_estimate(probe, target);

    // The batch's verdict outranks the cell's own: a collision is fatal and no flag fixes
    // it, while an occupied output path is one checkbox away from being fine.
    if let Some(message) = collision {
        return Verdict {
            status: Status::Clash,
            note: Some(message.to_string()),
            output: Some(output),
            size,
        };
    }

    // With `overwrite` (or the run's `--force`) this answers `None` and the cell reads
    // `✓` — which is correct: consent is precisely what the query is about, and the job
    // will run.
    let (status, note) = match pipeline::output_conflict(input, &output, target.overwrite) {
        None => (Status::Ok, None),
        Some(conflict) => {
            let status = match conflict {
                // Not the friendly `⟳`: writing over the master is the one conflict
                // `problems` refuses to offer a one-click force for, and the glyph says so.
                Conflict::Input => Status::Clash,
                Conflict::Existing => Status::Replaces,
            };
            (status, Some(conflict.message(input, &output)))
        }
    };
    Verdict { status, note, output: Some(output), size }
}

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    if state.config().is_none() {
        ui.label("the document does not resolve; nothing to plan");
        return;
    }
    if state.sources.is_empty() {
        ui.label("drop .wav or .aiff masters on the window to fill the matrix");
        return;
    }
    let targets = state.resolved_targets();
    if targets.is_empty() {
        ui.label("no targets resolve");
        return;
    }

    // Reserved before the table so the table takes what is left rather than the legend
    // being pushed off the bottom of a full matrix.
    egui::Panel::bottom("plan-legend").show(ui, legend);

    // The delegate borrows the state it draws, so a click is recorded and applied after.
    let (clicked_row, clicked_col) = {
        let collisions = collision_index(&state.problems);
        let mut delegate = Matrix {
            sources: &state.sources,
            targets: &targets,
            collisions: &collisions,
            selected_source: state.selected_source,
            selected_target: state.selected.as_deref(),
            clicked_row: None,
            clicked_col: None,
        };
        let columns = std::iter::once(Column::new(200.0))
            .chain(delegate.targets.iter().map(|_| Column::new(230.0)))
            .collect::<Vec<_>>();

        Table::new()
            .id_salt("plan-matrix")
            .num_sticky_cols(1)
            .num_rows(delegate.sources.len() as u64)
            .columns(columns)
            .headers(vec![HeaderRow::new(24.0)])
            .show(ui, &mut delegate);

        (delegate.clicked_row, delegate.clicked_col)
    };

    // A cell click sets both halves of the selection and therefore the chain preview; a row
    // or column header click moves only its own half, which is what a header is for.
    if let Some(row) = clicked_row {
        state.selected_source = Some(row);
    }
    if let Some(col) = clicked_col {
        state.selected = Some(targets[col].name.clone());
    }
}

/// The glyph key, under the matrix. Generated from [`Status::ALL`] so it cannot fall behind
/// the cells.
fn legend(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        for status in Status::ALL {
            let (glyph, colour) = status.glyph();
            ui.colored_label(colour, glyph);
            ui.weak(status.legend());
            ui.add_space(10.0);
        }
    });
}

/// The collector's cell-scoped *errors*, keyed by cell and built once per frame instead of
/// scanned per cell.
///
/// Errors only, and that filter is what makes the lookup mean "collision": step 7's
/// delivery blocker is asked directly one line above the lookup and so never reaches it,
/// and step 9's output conflict is a warning. Step 8's batch collision is what is left.
fn collision_index(problems: &[Problem]) -> HashMap<(usize, &str), &str> {
    let mut index: HashMap<(usize, &str), &str> = HashMap::new();
    for problem in problems {
        if problem.severity != Severity::Error {
            continue;
        }
        if let Scope::Cell { source, target } = &problem.scope {
            index.entry((*source, target.as_str())).or_insert(&problem.message);
        }
    }
    index
}

struct Matrix<'a> {
    sources: &'a [Source],
    targets: &'a [ResolvedTarget],
    collisions: &'a HashMap<(usize, &'a str), &'a str>,
    selected_source: Option<usize>,
    selected_target: Option<&'a str>,
    clicked_row: Option<usize>,
    clicked_col: Option<usize>,
}

impl TableDelegate for Matrix<'_> {
    fn header_cell_ui(&mut self, ui: &mut Ui, cell: &HeaderCellInfo) {
        match cell.col_range.start {
            0 => {
                ui.label(RichText::new("source").strong());
            }
            n => {
                // Cloned rather than borrowed because the click below writes to `self`.
                // Header cells are as many as there are targets, once per frame.
                let index = n - 1;
                let name = self.targets[index].name.clone();
                let ext = self.targets[index].codec.ext();
                let selected = self.selected_target == Some(name.as_str());
                // The Targets pane's own summary as the hover, so what a column delivers is
                // one look away from the column rather than a pane away — and so the two
                // surfaces describe the same target in the same words.
                let summary = crate::targets::summary_line(&self.targets[index]);
                let response = ui
                    .selectable_label(selected, RichText::new(&name).strong())
                    .on_hover_text(summary);
                ui.weak(ext);
                if response.clicked() {
                    self.clicked_col = Some(index);
                }
            }
        }
    }

    fn cell_ui(&mut self, ui: &mut Ui, cell: &CellInfo) {
        let row = cell.row_nr as usize;
        if cell.col_nr == 0 {
            if self.source_cell(ui, row) {
                self.clicked_row = Some(row);
            }
            return;
        }
        let col = cell.col_nr - 1;
        if self.job_cell(ui, row, col) {
            self.clicked_row = Some(row);
            self.clicked_col = Some(col);
        }
    }

    fn default_row_height(&self) -> f32 {
        22.0
    }
}

impl Matrix<'_> {
    /// The sticky first column: which take this row is.
    fn source_cell(&self, ui: &mut Ui, row: usize) -> bool {
        let source = &self.sources[row];
        let response = begin_cell(ui, self.selected_source == Some(row));
        let name = source.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let hover = match &source.probe {
            Ok(probe) => {
                ui.label(RichText::new(name).monospace());
                ui.weak(format!("{} Hz", probe.sample_rate));
                source.path.display().to_string()
            }
            // mint's own message, on the row it belongs to — the file keeps its place in
            // the set, because one unreadable input is one task's failure and not the
            // batch's, exactly as `cli.rs`'s `ProbedInput` documents.
            Err(error) => {
                ui.colored_label(colors::BAD, format!("{name} ?"));
                error.clone()
            }
        };
        let clicked = response.clicked();
        response.on_hover_text(hover);
        clicked
    }

    fn job_cell(&self, ui: &mut Ui, row: usize, col: usize) -> bool {
        let source = &self.sources[row];
        let target = &self.targets[col];
        let selected =
            self.selected_source == Some(row) && self.selected_target == Some(target.name.as_str());
        let response = begin_cell(ui, selected);

        let Ok(probe) = &source.probe else {
            // Nothing about this pair is answerable without a header, and the row already
            // carries mint's reason for that.
            ui.weak("—");
            return response.clicked();
        };

        let collision = self.collisions.get(&(row, target.name.as_str())).copied();
        let verdict = verdict(&source.path, probe, target, collision);

        let (glyph, colour) = verdict.status.glyph();
        ui.colored_label(colour, glyph);
        if let Some(path) = &verdict.output {
            let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            ui.label(RichText::new(name).monospace());
        }
        match verdict.size {
            // The qualifier is not decoration: `~` and `<=` are different promises from the
            // bare number, and `tests/output_size.rs` holds each one to its own standard.
            Some(size) => {
                ui.weak(format!("{}{}", size.qualifier(), fmt_bytes(size.bytes())));
            }
            None if verdict.output.is_some() => {
                ui.weak("size unknown");
            }
            None => {}
        }

        // One tooltip for the whole cell: mint's prose first, then the full path. Hanging
        // it here rather than on a label is what makes it one — `begin_cell` took the
        // labels' own senses away, so none of them can put a second tooltip on screen.
        let mut hover = verdict.note.unwrap_or_default();
        if let Some(path) = &verdict.output {
            if !hover.is_empty() {
                hover.push('\n');
            }
            hover.push_str(&path.display().to_string());
        }
        let clicked = response.clicked();
        if !hover.is_empty() {
            response.on_hover_text(hover);
        }
        clicked
    }
}

/// The whole cell as one click target, registered *before* its contents.
///
/// Before, so that `ui.max_rect()` is still the entire cell and so the selection or hover
/// wash can be painted underneath the labels rather than over them. That ordering is only
/// safe because of the first line of the body: egui's `selectable_labels` is on by default,
/// which quietly gives every `ui.label` a click-and-drag sense so its text can be selected,
/// and a label sitting on top of the click target then *takes the click* — the cell reads as
/// hovered by nothing and clickable nowhere. Dragging a filename out of a table cell is not
/// a feature; selecting the cell is. Turning it off also makes the "one tooltip per cell"
/// arrangement above true rather than merely intended.
fn begin_cell(ui: &mut Ui, selected: bool) -> Response {
    let rect = ui.max_rect();
    ui.style_mut().interaction.selectable_labels = false;

    let response = ui.interact(rect, ui.id().with("cell"), Sense::click());
    if selected {
        let fill = ui.visuals().selection.bg_fill.gamma_multiply(0.35);
        ui.painter().rect_filled(rect, 0.0, fill);
    } else if response.hovered() {
        let fill = ui.visuals().widgets.hovered.bg_fill.gamma_multiply(0.25);
        ui.painter().rect_filled(rect, 0.0, fill);
    }
    response
}

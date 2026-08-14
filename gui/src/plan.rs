//! Spike gate S4: the plan matrix — inputs × targets, one cell per job.
//!
//! Every cell is three library calls and no judgement of its own: `render_output_path` for
//! the filename, `delivery_blocker` for whether the job is possible at all, and
//! `estimated_output_bytes` for the size *and its qualifier*. The qualifier is the part
//! worth keeping: WAV and DSF are exact, FLAC is an upper bound, MP3 is approximate, and a
//! matrix that printed all four as plain numbers would be lying in two of the four cases.
//!
//! `delivery_blocker` needs the *input* to answer — FLAC above 96 kHz, DSD above 5.1 — which
//! is exactly why this screen cannot be filled in from the config alone, and why it is the
//! screen a CLI cannot show you before it starts working.

use std::path::PathBuf;

use egui::{Color32, RichText, Ui};
use egui_table::{CellInfo, Column, HeaderCellInfo, HeaderRow, Table, TableDelegate};
use mint::audio::io_read::{self, SourceProbe};
use mint::config::ResolvedTarget;
use mint::util::fmt_bytes;

use crate::state::AppState;

pub struct Source {
    pub path: PathBuf,
    pub probe: Option<SourceProbe>,
}

/// Probe each input once. Cheap (a header read) but not free, so it is done when the input
/// set changes rather than per frame.
pub fn probe(inputs: &[PathBuf]) -> Vec<Source> {
    inputs
        .iter()
        .map(|path| Source { path: path.clone(), probe: io_read::probe_source(path).ok() })
        .collect()
}

pub fn ui(ui: &mut Ui, state: &AppState, sources: &[Source], stress: usize) {
    let Some(config) = state.config() else {
        ui.label("the document does not resolve; nothing to plan");
        return;
    };
    let targets: Vec<ResolvedTarget> = state
        .target_names()
        .iter()
        .filter_map(|name| config.resolve_target(name).ok())
        .collect();

    if sources.is_empty() {
        ui.label("drop .wav or .aiff masters on the window to fill the matrix");
        return;
    }
    if targets.is_empty() {
        ui.label("no targets resolve");
        return;
    }

    // `stress` repeats the real sources so the scroll can be exercised at a realistic row
    // count without inventing fake probe data — every cell stays a genuine computation.
    let rows = sources.len().max(stress);
    let mut delegate = Matrix { sources, targets: &targets };
    let columns = std::iter::once(Column::new(220.0))
        .chain(delegate.targets.iter().map(|_| Column::new(210.0)))
        .collect::<Vec<_>>();

    Table::new()
        .id_salt("plan-matrix")
        .num_sticky_cols(1)
        .num_rows(rows as u64)
        .columns(columns)
        .headers(vec![HeaderRow::new(24.0)])
        .show(ui, &mut delegate);
}

struct Matrix<'a> {
    sources: &'a [Source],
    targets: &'a [ResolvedTarget],
}

impl TableDelegate for Matrix<'_> {
    fn header_cell_ui(&mut self, ui: &mut Ui, cell: &HeaderCellInfo) {
        ui.horizontal(|ui| match cell.col_range.start {
            0 => {
                ui.label(RichText::new("source").strong());
            }
            n => {
                let target = &self.targets[n - 1];
                ui.label(RichText::new(&target.name).strong());
            }
        });
    }

    fn cell_ui(&mut self, ui: &mut Ui, cell: &CellInfo) {
        let source = &self.sources[cell.row_nr as usize % self.sources.len()];
        ui.horizontal(|ui| {
            if cell.col_nr == 0 {
                let name = source.path.file_name().unwrap_or_default().to_string_lossy();
                match &source.probe {
                    Some(probe) => {
                        ui.label(RichText::new(name).monospace());
                        ui.weak(format!("{} Hz", probe.sample_rate));
                    }
                    None => {
                        ui.colored_label(Color32::from_rgb(220, 90, 80), format!("{name} ?"));
                    }
                }
                return;
            }
            self.job_cell(ui, source, &self.targets[cell.col_nr - 1]);
        });
    }

    fn default_row_height(&self) -> f32 {
        22.0
    }
}

impl Matrix<'_> {
    fn job_cell(&self, ui: &mut Ui, source: &Source, target: &ResolvedTarget) {
        let Some(probe) = &source.probe else {
            ui.weak("—");
            return;
        };
        // The blocker is asked first and asked with the input, because a job that cannot
        // run has no filename and no size worth showing.
        if let Some(reason) = target.delivery_blocker(probe.sample_rate, probe.channels) {
            ui.colored_label(Color32::from_rgb(220, 90, 80), "✖").on_hover_text(reason);
            return;
        }
        let out_rate = target.delivery_rate(probe.sample_rate);
        let Ok(path) = target.render_output_path(&source.path, out_rate) else {
            ui.colored_label(Color32::from_rgb(220, 90, 80), "✖ path");
            return;
        };
        ui.label(RichText::new("●").color(Color32::from_rgb(120, 200, 140)));
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        ui.label(RichText::new(name).monospace()).on_hover_text(path.display().to_string());
        match probe
            .frames
            .zip(probe.channels)
            .and_then(|(frames, channels)| target.estimated_output_bytes(probe.sample_rate, frames, channels))
        {
            // The qualifier is not decoration: `~` and `<=` are different promises from the
            // bare number, and `tests/output_size.rs` holds each one to its own standard.
            Some(estimate) => {
                ui.weak(format!("{}{}", estimate.qualifier(), fmt_bytes(estimate.bytes())));
            }
            None => {
                ui.weak("size unknown");
            }
        }
    }
}

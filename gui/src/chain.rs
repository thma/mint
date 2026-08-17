//! The live chain preview: what mint will actually do to the selected cell.
//!
//! Every line on this screen comes from `ResolvedTarget::chain(source_rate)`, rendered by
//! `ChainStep::line`. Not one step is derived here, and that is the entire point of the
//! module: the derivation is a separate code path from `pipeline::process` (CLAUDE.md says
//! so, and says the two drift silently), so a surface that renders it verbatim next to the
//! plan is the place a divergence between them becomes visible. If a step shown here is not
//! the step the pipeline runs, that is a library bug, and this screen is the bug report. The
//! only honest reaction to a wrong line here is to go and fix `chain()` or the pipeline —
//! never to correct it on the way out.
//!
//! It reads `chain()` rather than `describe()` for the two things a `Vec<String>` cannot
//! answer: which category a step belongs to (`ChainStepKind`, for the column that lets the
//! eye find the quantizer without reading five sentences) and whether a line is a *step* at
//! all — `note: dsd512 has limited player support` sits in the same list and used to take a
//! step number. Both answers are the library's; deciding either from the sentence would be
//! the substring-matching this whole facility exists to delete.
//!
//! It re-renders every frame rather than caching. `chain()` is microseconds of work over an
//! already-resolved target, the status bar prints what a frame costs, and a cache keyed on a
//! selection is a staleness bug waiting for the one frame the key does not change but the
//! document does.
//!
//! Under the steps, when present: the `delivery_blocker` line and the `output_conflict`
//! line rendered by `Conflict::message` — mint's own prose in both cases, because they are
//! the same sentences the run would fail with.

use std::path::Path;

use egui::{RichText, Ui};
use mint::pipeline::{self, Conflict};
use mint::util::fmt_bytes;

use crate::colors;
use crate::state::AppState;

/// The preview for `(state.selected_source, state.selected)`. Says what to click when
/// nothing is selected — an empty pane is indistinguishable from a broken one.
pub fn ui(ui: &mut Ui, state: &AppState) {
    let Some(config) = state.config() else {
        ui.weak("the document does not resolve; there is no chain to describe");
        return;
    };
    let (Some(index), Some(name)) = (state.selected_source, state.selected.as_deref()) else {
        ui.weak("click a cell in the matrix to see the steps mint derives for it");
        return;
    };
    let Some(source) = state.sources.get(index) else {
        ui.weak("that source is gone; pick another row");
        return;
    };

    // Preferred from the run's own set, because those carry the run-scoped `--force` that
    // decides the conflict line below. A target excluded from the run is still worth
    // previewing — it is usually the one being edited — and is then asked its own
    // `overwrite`, since `--force` is a property of a run this cell is not part of.
    let target = match state.resolved_targets().into_iter().find(|target| target.name == name) {
        Some(target) => target,
        None => match config.resolve_target(name) {
            Ok(target) => target,
            Err(err) => {
                ui.colored_label(colors::BAD, format!("{err:#}"));
                return;
            }
        },
    };

    let probe = match &source.probe {
        Ok(probe) => probe,
        // Every step below is derived from the source rate, and there is none. mint's own
        // message, and nothing invented to fill the space.
        Err(error) => {
            ui.colored_label(colors::BAD, error);
            return;
        }
    };

    let out_rate = target.delivery_rate(probe.sample_rate);
    let output = target.render_output_path(&source.path, out_rate);

    // `<source> → [<target>] → <output>   <qualified size>`, the wireframe's header line.
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(file_name(&source.path)).monospace());
        ui.weak("→");
        ui.label(RichText::new(format!("[{}]", target.name)).strong());
        ui.weak("→");
        match &output {
            Ok(path) => {
                ui.label(RichText::new(path.display().to_string()).monospace());
            }
            Err(err) => {
                ui.colored_label(colors::BAD, format!("{err:#}"));
            }
        }
        match crate::plan::cell_estimate(probe, &target) {
            Some(size) => {
                ui.weak(format!("{}{}", size.qualifier(), fmt_bytes(size.bytes())));
            }
            None => {
                ui.weak("size unknown");
            }
        }
        // Said out loud, because it changes what the conflict line below is answering.
        if !state.is_included(name) {
            ui.weak("· not in the run");
        }
    });

    // The two things that stop this chain from running, in mint's words. Both are shown
    // above the steps: they are the reason the steps would never happen.
    if let Some(reason) = target.delivery_blocker(probe.sample_rate, probe.channels) {
        ui.colored_label(colors::BAD, format!("! {reason}"));
    }
    if let Ok(path) = &output
        && let Some(conflict) = pipeline::output_conflict(&source.path, path, target.overwrite)
    {
        // Exhaustive on mint's own enum, so a third kind of conflict is a compile error
        // rather than a silently mis-coloured line. `Conflict::Input` is not the friendly
        // `⟳` for the same reason `problems` offers it no one-click force: the file it
        // would replace is the master.
        let (glyph, colour) = match conflict {
            Conflict::Existing => ("⟳", colors::WARN),
            Conflict::Input => ("✗", colors::BAD),
        };
        ui.colored_label(colour, format!("{glyph} {}", conflict.message(&source.path, path)));
    }

    ui.add_space(6.0);
    egui::ScrollArea::both().id_salt("chain-steps").show(ui, |ui| {
        egui::Grid::new("chain-steps-grid").num_columns(3).spacing([10.0, 3.0]).show(ui, |ui| {
            // Numbered, because the order is the contract: CLAUDE.md's "canonical order is
            // fixed and steps are skipped, not configured". A note is not a step and so
            // takes no number — `is_note` is the library's answer to which is which.
            let mut step_number = 0;
            for step in target.chain(probe.sample_rate) {
                let note = step.is_note();
                if note {
                    ui.label("");
                } else {
                    step_number += 1;
                    ui.weak(format!("{step_number:>2}"));
                }
                ui.weak(RichText::new(step.kind().label()).small());
                let text = RichText::new(step.line()).monospace();
                if note {
                    ui.colored_label(colors::WARN, text);
                } else {
                    ui.label(text);
                }
                ui.end_row();
            }
        });
    });
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or_default().to_string_lossy().into_owned()
}

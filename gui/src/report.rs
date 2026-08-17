//! The Report view: what was delivered, and mint's verdict on it.
//!
//! The verdict column is the reason this screen exists. `pipeline` already computes the
//! judgement — `LUFS_TOLERANCE`, ±0.1 LU — and then buries it in a warning string that the
//! console prints once and nobody keeps. `ProcessingReport::lufs_verdict` and
//! `::ceiling_verdict` hand it over as a boolean, computed in the library against the
//! `declared_lufs` / `declared_ceiling_dbtp` the report now carries, so that a run persisted
//! to `.mint-gui/runs/` is still judgeable months later with no config in reach — and so
//! that this file does not own a second copy of where the line is.
//!
//! Nothing here decides a pass or a fail. The deviation figure beside each verdict
//! (`delivered − declared`) is arithmetic on two numbers the report already states; the
//! *verdict* is always the library's boolean. Deriving the tick from the deviation — one
//! `abs() <= 0.1` — is precisely the second copy of the rule that `meets_target` exists to
//! prevent, and it would drift from the warning sitting on the same row.
//!
//! Two rules the layout is built around, both from §7:
//!
//! - **Warnings are rendered in full.** Never collapsed, never behind a disclosure triangle,
//!   never summarized as a count in place of the text. A run with warnings is not a
//!   success, and the whole failure mode this tool exists to prevent is a green checkmark
//!   over a delivery that missed its target by 0.4 LU. They therefore get their own section
//!   listing *every* task's warnings, rather than only the selected row's: a warning that
//!   appears when you click the right row is a warning behind a disclosure.
//! - **Failures are their own section**, non-collapsible, next to the successes rather than
//!   behind them.
//!
//! Two figures here are ones no existing mint surface shows. `LRA spent` is
//! `delivered − source` loudness range: the console prints both numbers on one line and
//! leaves the subtraction to the reader, and it is the single figure that says what
//! hitting the target cost in dynamics. And the deviation beside the loudness verdict is
//! printed to two decimals on purpose — the tolerance is 0.1 LU, so one decimal cannot tell
//! a pass from a fail at the boundary, which is the one place a reader looks.
//!
//! `[ Export JSON ]` writes `json_report::render(...)`, unmodified. The GUI does not own
//! the `--json` schema, does not add keys to it, and does not reformat it: whatever `jq`
//! does to the CLI's output it must do to this file.

use std::cmp::Ordering;

use egui::{Grid, RichText, ScrollArea, Ui};
use mint::json_report;
use mint::diagnostics::FailureKind;
use mint::pipeline::{ProcessingReport, TaskFailure};
use mint::util::{duration_seconds, fmt_bytes, fmt_db};

use crate::colors;
use crate::state::AppState;

/// Which column orders the table.
///
/// `Run` is the default and is not a sort at all — it is the order the batch ran in, which
/// is the order the user watched happen. Every other column sorts *around* it: the sort is
/// stable and the direction is applied to the key rather than by reversing the result, so
/// rows with equal keys stay in run order whichever way the column points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Sort {
    #[default]
    Run,
    Target,
    Input,
    Lufs,
    Peak,
    Gr,
    Lra,
    Plr,
    Warnings,
}

/// Sort column and row selection — the view's own state, kept out of `AppState` because
/// nothing here can change a sample or survive a restart.
#[derive(Default)]
pub struct ReportView {
    /// Row index into the reports slice.
    selected: Option<usize>,
    sort: Sort,
    descending: bool,
    /// What the last export did, in this crate's own words — it is mint-gui's own I/O, not
    /// a mint diagnostic, so there is nothing here to paraphrase.
    notice: Option<String>,
}

impl ReportView {
    /// The table, the detail card for the selected row, and the failures section.
    ///
    /// `state` is here for the two things a report cannot answer about itself: the
    /// `[meta].name` that goes into `json_report::render`, and where on disk to offer to
    /// export.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        reports: &[ProcessingReport],
        failures: &[TaskFailure],
    ) {
        // A selection made in the previous batch would index into this one. Heal it here
        // rather than guarding every read of it.
        if self.selected.is_none_or(|row| row >= reports.len()) {
            self.selected = (!reports.is_empty()).then_some(0);
        }

        self.summary(ui, state, reports, failures);
        if reports.is_empty() && failures.is_empty() {
            ui.weak("no run yet — this fills in when a render finishes");
            return;
        }

        ScrollArea::vertical().show(ui, |ui| {
            if !reports.is_empty() {
                self.table(ui, reports);
                ui.add_space(8.0);
                if let Some(report) = self.selected.and_then(|row| reports.get(row)) {
                    detail(ui, report);
                }
                ui.add_space(8.0);
                warnings(ui, reports);
            }
            failure_section(ui, failures);
        });
    }

    /// The one-line batch verdict, and the export button.
    fn summary(
        &mut self,
        ui: &mut Ui,
        state: &AppState,
        reports: &[ProcessingReport],
        failures: &[TaskFailure],
    ) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Report").strong());
            ui.weak(format!("{} task(s)", reports.len()));

            // Counted the way `cli.rs` counts them, and never folded into the task count:
            // "8 tasks" with three of them warned is not the same run as eight clean ones.
            let warned = reports.iter().filter(|report| !report.warnings.is_empty()).count();
            if warned > 0 {
                ui.colored_label(colors::WARN, format!("⚠ {warned} with warnings"));
            }
            if !failures.is_empty() {
                ui.colored_label(colors::BAD, format!("✖ {} failed", failures.len()));
            }

            // Bytes are `file_bytes`, stat'd after writing rather than predicted — the plan
            // matrix's estimate is the guess, this is the outcome. Deliberately no total
            // elapsed beside it: jobs overlap under the pool, so a sum of `elapsed_ms` is
            // not how long the batch took and would read as if it were.
            if !reports.is_empty() {
                let bytes: u64 = reports.iter().map(|report| report.file_bytes).sum();
                ui.weak("·");
                ui.weak(fmt_bytes(bytes));
            }

            let anything = !reports.is_empty() || !failures.is_empty();
            if ui.add_enabled(anything, egui::Button::new("Export JSON")).clicked() {
                self.export(state, reports, failures);
            }
        });
        if let Some(notice) = &self.notice {
            ui.weak(notice.clone());
        }
    }

    /// One row per task. The first cell selects; the rest are figures with their verdicts.
    fn table(&mut self, ui: &mut Ui, reports: &[ProcessingReport]) {
        Grid::new("report-table").num_columns(8).striped(true).spacing([14.0, 3.0]).show(
            ui,
            |ui| {
                for (sort, label, hover) in [
                    (Sort::Target, "target", "the target that produced the file"),
                    (Sort::Input, "input", "the master it was rendered from"),
                    (Sort::Lufs, "LUFS", "delivered integrated loudness, against the \
                                          target's declared lufs"),
                    (Sort::Peak, "dBTP", "delivered true peak, against the target's \
                                          declared ceiling"),
                    (Sort::Gr, "GR", "limiter gain reduction: peak / mean, over the share \
                                      of samples it touched"),
                    (Sort::Lra, "LRA", "delivered loudness range"),
                    (Sort::Plr, "PLR", "delivered peak-to-loudness ratio"),
                    (Sort::Warnings, "warn", "warnings on this task; every one of them is \
                                              printed below in full"),
                ] {
                    let arrow = match (self.sort == sort, self.descending) {
                        (true, false) => " ▲",
                        (true, true) => " ▼",
                        (false, _) => "",
                    };
                    let header = ui.selectable_label(self.sort == sort, format!("{label}{arrow}"));
                    if header.on_hover_text(hover).clicked() {
                        // Same column flips direction; a different one starts ascending.
                        self.descending = self.sort == sort && !self.descending;
                        self.sort = sort;
                    }
                }
                ui.end_row();

                for row in self.order(reports) {
                    self.row(ui, row, &reports[row]);
                    ui.end_row();
                }
            },
        );
    }

    /// Row indices in display order.
    ///
    /// `sort_by` is stable, and the direction is applied inside the comparator rather than
    /// by reversing the vector, so ties keep the run order in both directions — a table
    /// whose equal rows shuffle when the arrow flips is a table nobody trusts twice.
    fn order(&self, reports: &[ProcessingReport]) -> Vec<usize> {
        let mut rows: Vec<usize> = (0..reports.len()).collect();
        if self.sort == Sort::Run {
            return rows;
        }
        rows.sort_by(|&a, &b| {
            let ordering = compare(&reports[a], &reports[b], self.sort);
            if self.descending { ordering.reverse() } else { ordering }
        });
        rows
    }

    fn row(&mut self, ui: &mut Ui, row: usize, report: &ProcessingReport) {
        if ui.selectable_label(self.selected == Some(row), &report.target).clicked() {
            self.selected = Some(row);
        }
        ui.label(file_name(&report.input)).on_hover_text(&report.input);

        // The verdict is the library's boolean; the deviation beside it is only the
        // distance, and is never what decides the glyph.
        match (report.lufs_verdict(), report.declared_lufs) {
            (Some(pass), Some(declared)) => {
                let (glyph, color) = verdict(pass);
                ui.colored_label(
                    color,
                    format!(
                        "{glyph} {} {}",
                        fmt_db(report.delivered.integrated_lufs),
                        deviation(report.delivered.integrated_lufs, Some(declared)),
                    ),
                )
                .on_hover_text(format!("declared {declared:.1} LUFS"));
            }
            // Nothing was promised, so there is nothing to pass or fail. Not a green tick.
            _ => {
                ui.weak(format!("{} n/a", fmt_db(report.delivered.integrated_lufs)));
            }
        }

        let (glyph, color) = verdict(report.ceiling_verdict());
        ui.colored_label(color, format!("{glyph} {}", fmt_db(report.delivered.true_peak_dbtp)))
            .on_hover_text(format!("declared ceiling {:.1} dBTP", report.declared_ceiling_dbtp));

        ui.label(gain_reduction(report)).on_hover_text(
            "peak / mean gain reduction, over the percentage of samples the limiter touched",
        );
        ui.label(fmt_db(report.delivered.loudness_range_lu));
        ui.label(fmt_db(report.delivered.plr()));

        match report.warnings.len() {
            0 => {
                ui.weak("—");
            }
            n => {
                ui.colored_label(colors::WARN, format!("⚠ {n}"));
            }
        }
    }

    /// Write the batch out in the `--json` schema, wherever the native dialog says.
    ///
    /// `json_report::render` produces the bytes and `atomic_write` puts them down, exactly
    /// as `history::persist` does — one schema, one writer, two destinations.
    fn export(&mut self, state: &AppState, reports: &[ProcessingReport], failures: &[TaskFailure]) {
        let json = match json_report::render(state.meta_name(), reports, failures) {
            Ok(json) => json,
            Err(err) => {
                self.notice = Some(format!("export failed: {err:#}"));
                return;
            }
        };
        let stem = state
            .meta_name()
            .map(str::to_string)
            .or_else(|| Some(state.path.file_stem()?.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "mint".to_string());
        let dialog = rfd::FileDialog::new()
            .set_title("Export the run report")
            .add_filter("JSON", &["json"])
            .set_file_name(format!("{stem}-report.json"));
        let dialog = match state.path.parent() {
            Some(dir) => dialog.set_directory(dir),
            None => dialog,
        };
        let Some(path) = dialog.save_file() else { return };

        self.notice = match mint::audio::atomic_write::write(&path, json.as_bytes()) {
            Ok(()) => Some(format!("exported to {}", path.display())),
            Err(err) => Some(format!("export failed: {err:#}")),
        };
    }
}

/// The selected task in full: what was measured, what was declared, and what it cost.
fn detail(ui: &mut Ui, report: &ProcessingReport) {
    ui.separator();
    ui.label(RichText::new(&report.output).monospace())
        .on_hover_text(format!("{} → {}", report.input, report.output));

    let source = &report.source;
    let delivered = &report.delivered;
    Grid::new("report-detail").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
        let mut row = |key: &str, value: String| {
            ui.weak(key);
            ui.label(RichText::new(value).monospace());
            ui.end_row();
        };

        row(
            "file",
            format!(
                "{}/{} · {} ch · {:.1} s · {} · in {:.2} s",
                report.codec,
                report.format,
                report.channels,
                duration_seconds(report.frames, report.out_rate),
                fmt_bytes(report.file_bytes),
                report.elapsed_ms as f64 / 1000.0,
            ),
        );
        row(
            "rate",
            match report.src_quality {
                Some(quality) => {
                    format!("{} → {} Hz · src {quality}", report.in_rate, report.out_rate)
                }
                None => format!("{} → {} Hz", report.in_rate, report.out_rate),
            },
        );
        row(
            "source",
            format!(
                "I {} LUFS · LRA {} LU · TP {} dBTP · PLR {} · PSR {}",
                fmt_db(source.integrated_lufs),
                fmt_db(source.loudness_range_lu),
                fmt_db(source.true_peak_dbtp),
                fmt_db(source.plr()),
                fmt_db(source.psr()),
            ),
        );
        row(
            "delivered",
            format!(
                "I {} LUFS · LRA {} LU · TP {} dBTP · PLR {} · PSR {}",
                fmt_db(delivered.integrated_lufs),
                fmt_db(delivered.loudness_range_lu),
                fmt_db(delivered.true_peak_dbtp),
                fmt_db(delivered.plr()),
                fmt_db(delivered.psr()),
            ),
        );
        row(
            "declared",
            match report.declared_lufs {
                Some(lufs) => {
                    format!("I {lufs:.1} LUFS · ceiling {:.1} dBTP", report.declared_ceiling_dbtp)
                }
                None => {
                    format!("no loudness target · ceiling {:.1} dBTP", report.declared_ceiling_dbtp)
                }
            },
        );
    });

    // The verdict block, on its own row so the glyphs are not buried in a monospace line.
    ui.horizontal(|ui| {
        ui.weak("verdict");
        ui.add_space(8.0);
        match report.lufs_verdict() {
            Some(pass) => {
                let (glyph, color) = verdict(pass);
                ui.colored_label(
                    color,
                    format!(
                        "{glyph} loudness {}",
                        deviation(delivered.integrated_lufs, report.declared_lufs)
                    ),
                );
            }
            None => {
                ui.weak("loudness n/a — the target declared none");
            }
        }
        let (glyph, color) = verdict(report.ceiling_verdict());
        ui.colored_label(
            color,
            format!(
                "{glyph} ceiling {} of {:.1} dBTP",
                fmt_db(delivered.true_peak_dbtp),
                report.declared_ceiling_dbtp
            ),
        );
    });

    Grid::new("report-detail-cost").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
        let mut row = |key: &str, value: String| {
            ui.weak(key);
            ui.label(RichText::new(value).monospace());
            ui.end_row();
        };

        // What hitting the target cost in dynamics: `delivered − source`, so a negative
        // figure is the usual direction — range was spent — and a positive one says the
        // delivery came back *more* dynamic than the master. The console prints both LRA
        // figures on one line and leaves the subtraction to the reader, which is why this
        // is the number no existing mint surface shows.
        row(
            "LRA",
            format!(
                "{} → {} LU ({} LU spent)",
                fmt_db(source.loudness_range_lu),
                fmt_db(delivered.loudness_range_lu),
                signed(delivered.loudness_range_lu - source.loudness_range_lu),
            ),
        );
        row("limiting", limiting(report));
        row(
            "gain",
            match (report.loudness_gain_db, report.post_resample_gain_db) {
                // `None` and `Some(0.0)` are different answers — "that pass never ran"
                // against "it ran and found nothing" — and collapsing them loses the only
                // evidence that the post-SRC check happened at all.
                (Some(gain), Some(post)) => {
                    format!("{} dB · post-SRC {} dB", signed(gain), signed(post))
                }
                (Some(gain), None) => format!("{} dB · post-SRC not run", signed(gain)),
                (None, Some(post)) => format!("none · post-SRC {} dB", signed(post)),
                (None, None) => "none — the target set no lufs".to_string(),
            },
        );
        row("quantize", quantize(report));

        // The DSD stage's own QC figures, in the same shape the console prints them.
        if let Some(dsd) = &report.dsd {
            row(
                "dsd",
                format!(
                    "{} via {} Hz x{} · order {} · peak modulation {:.1}% · overload {} · \
                     resets {}",
                    dsd.rate,
                    dsd.intermediate_pcm_rate,
                    dsd.interpolation_factor,
                    dsd.modulator_order,
                    dsd.max_modulation_percent,
                    dsd.overload_samples,
                    dsd.resets,
                ),
            );
        }
    });
}

/// Every warning of every task, in full and unconditionally.
///
/// Not per selected row and not behind a count: §7 item 9 is that a warning may never be
/// collapsed into a checkmark, and a warning you have to click the right row to read is
/// collapsed by another name.
fn warnings(ui: &mut Ui, reports: &[ProcessingReport]) {
    if reports.iter().all(|report| report.warnings.is_empty()) {
        return;
    }
    ui.separator();
    ui.label(RichText::new("Warnings").strong());
    for report in reports {
        // In the order the render emitted them, which is chain order — the loudness stage's
        // warnings before the resample's before the encoder's. Grouping them by subject would
        // read tidier and would throw that away.
        for warning in &report.warnings {
            // `horizontal_wrapped`, not a `Grid`: these sentences run to 200 characters and a
            // grid cell does not wrap, so the tidier layout silently clips the end of every
            // long warning off the right edge. §7 rule 9 is that a warning may not be
            // collapsed into a count — one you can only read half of is collapsed by another
            // name.
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(colors::WARN, "⚠");
                // `subject` is what a warning is *about*, from the library. It is here so the
                // sigma-delta lines are findable among the loudness ones without reading all
                // of them — never as a substitute for the sentence.
                ui.weak(RichText::new(warning.subject().label()).small());
                // mint's own text, whole. The target name is a prefix, not a rewording.
                ui.colored_label(colors::WARN, format!("[{}] {warning}", report.target));
            });
        }
    }
}

/// The failures, as their own section and never folded away.
fn failure_section(ui: &mut Ui, failures: &[TaskFailure]) {
    if failures.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new("Failures").strong());
    for failure in failures {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(colors::BAD, "✖");
            ui.label(RichText::new(format!("[{}]", failure.target)).monospace());
            ui.label(file_name(&failure.input)).on_hover_text(&failure.input);
            // mint's own message, verbatim: this is the text the CLI would have bailed with.
            ui.colored_label(colors::BAD, &failure.error);
            // ...and mint's own verdict on what kind of failure it was, which the message
            // does not spell out and which decides whether `--force` is even relevant. Shown
            // only when there is one worth naming, so an ordinary failure gains no noise.
            if failure.kind != FailureKind::Other {
                ui.weak(RichText::new(failure.kind.tag()).small());
            }
            if failure.kind.force_can_fix() {
                ui.weak(RichText::new("· --force would allow it").small());
            }
        });
    }
}

/// Glyph *and* colour for a library verdict — colour alone fails capability #11.
fn verdict(pass: bool) -> (&'static str, egui::Color32) {
    if pass { ("✓", colors::OK) } else { ("✗", colors::BAD) }
}

/// How far the delivery landed from what it declared. Two decimals because the tolerance is
/// 0.1 LU: at one decimal, a 0.14 LU miss prints as `+0.1` and reads like the pass beside it.
fn deviation(delivered: f64, declared: Option<f64>) -> String {
    let Some(declared) = declared else { return "n/a".to_string() };
    let delta = delivered - declared;
    if !delta.is_finite() {
        return "n/a".to_string();
    }
    // `±0.00` rather than a sign that flips with the last bit of a figure that is on target.
    if delta.abs() < 0.005 { "±0.00".to_string() } else { format!("{delta:+.2}") }
}

/// A signed dB/LU figure, or "n/a" when the meter had nothing to measure. `fmt_db`'s own
/// job, minus the sign it does not carry.
fn signed(value: f64) -> String {
    if value.is_finite() { format!("{value:+.2}") } else { "n/a".to_string() }
}

/// Peak / mean gain reduction and how much of the file it touched. The three together are
/// what separate one shaved needle from wall-to-wall limiting; the peak alone does not.
fn gain_reduction(report: &ProcessingReport) -> String {
    let Some(peak) = report.limiter_gain_reduction_db else { return "—".to_string() };
    match (report.limiter_avg_gain_reduction_db, report.limiter_active_percent) {
        (Some(avg), Some(percent)) => format!("-{peak:.1} / -{avg:.1} dB · {percent:.1}%"),
        _ => format!("-{peak:.1} dB"),
    }
}

/// The same three figures as the table's `GR` cell, spelled out. Nothing is printed when
/// the limiter did not engage — an explicit "-0.0 dB" would read as if it had.
fn limiting(report: &ProcessingReport) -> String {
    let peak = match report.limiter_gain_reduction_db {
        Some(peak) if peak > 0.0 => peak,
        _ => return "none".to_string(),
    };
    match (report.limiter_avg_gain_reduction_db, report.limiter_active_percent) {
        (Some(avg), Some(percent)) => {
            format!("peak -{peak:.1} dB · avg -{avg:.1} dB over {percent:.1}% of samples")
        }
        _ => format!("peak -{peak:.1} dB"),
    }
}

/// What the bit-depth stage did.
///
/// The report's `dither_mode` is what was *asked for* and `shaping_curve` is what
/// *happened*; both are printed because the pair is the only thing that distinguishes
/// `adaptive` degraded to flat TPDF from a target that asked for TPDF. `effective_dither_tag`
/// would say it in one phrase, but it takes the three config enums and a report carries only
/// their tags — turning those back into enums here would be this crate parsing mint's own
/// spellings, and reading them off the *current* config would describe a document that may
/// have been edited since the render.
fn quantize(report: &ProcessingReport) -> String {
    let dither = match (report.dither_mode, report.dither_strength) {
        (Some(mode), Some(strength)) => format!("dither {mode} ({strength})"),
        (Some(mode), None) => format!("dither {mode}"),
        (None, _) => "dither —".to_string(),
    };
    let shaping = match report.shaping_curve {
        Some(curve) => format!("shaping {curve}"),
        None if report.dithered => "shaping none".to_string(),
        None => "not dithered".to_string(),
    };
    let seed = match report.seed {
        Some(seed) => format!("seed {seed}"),
        // The CLI says the same thing about an unpinned seed, and it is worth saying.
        None => "seed unpinned — not reproducible".to_string(),
    };
    format!("{} · {dither} · {shaping} · {seed}", report.format)
}

/// The last component of a path that is already a string in the report. Purely cosmetic —
/// the full path is always one hover away.
fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Order two rows by one column.
///
/// `total_cmp` rather than `partial_cmp`: a silent file meters as `-inf` and a clip shorter
/// than the integration time as `NaN`, and a comparator that returns `None` for either one
/// would panic the sort.
fn compare(a: &ProcessingReport, b: &ProcessingReport, sort: Sort) -> Ordering {
    match sort {
        Sort::Run => Ordering::Equal,
        Sort::Target => a.target.cmp(&b.target),
        Sort::Input => a.input.cmp(&b.input),
        Sort::Lufs => a.delivered.integrated_lufs.total_cmp(&b.delivered.integrated_lufs),
        Sort::Peak => a.delivered.true_peak_dbtp.total_cmp(&b.delivered.true_peak_dbtp),
        // "No limiting" is 0 dB of gain reduction, which is where it belongs in this order.
        Sort::Gr => a
            .limiter_gain_reduction_db
            .unwrap_or(0.0)
            .total_cmp(&b.limiter_gain_reduction_db.unwrap_or(0.0)),
        Sort::Lra => a.delivered.loudness_range_lu.total_cmp(&b.delivered.loudness_range_lu),
        Sort::Plr => a.delivered.plr().total_cmp(&b.delivered.plr()),
        Sort::Warnings => a.warnings.len().cmp(&b.warnings.len()),
    }
}

//! The shell: target list, tabs, and the status bar that answers S2's timing question.

use egui::{Color32, RichText, Ui};

use crate::state::AppState;
use crate::{editor, form, plan, run};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Target,
    Config,
    Plan,
    Run,
}

pub struct MintApp {
    state: AppState,
    tab: Tab,
    run: run::RunView,
    sources: Vec<plan::Source>,
    /// Row count for the plan matrix's scroll test. Real sources are repeated to reach it
    /// (see `plan::ui`) so the S4 measurement uses genuine per-cell work.
    stress_rows: usize,
}

impl MintApp {
    pub fn new(state: AppState) -> Self {
        Self { state, tab: Tab::Target, run: run::RunView::default(), sources: Vec::new(), stress_rows: 20 }
    }

    /// The document and selection, for the headless harness to set up a scenario the way a
    /// user would have. Not a general escape hatch: the app itself never reaches around
    /// its own state this way.
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    pub fn run_view(&self) -> &run::RunView {
        &self.run
    }

    pub fn run_view_mut(&mut self) -> &mut run::RunView {
        &mut self.run
    }

    /// Start a render without a click. The harness needs this because clicking the button
    /// requires the Run tab to be laid out first; what is under test is the worker and the
    /// progress path, not the button.
    pub fn select_tab(&mut self, tab: Tab) {
        self.tab = tab;
    }

    pub fn set_plan_rows(&mut self, rows: usize) {
        self.stress_rows = rows;
    }

    pub fn start_render(&mut self, ctx: egui::Context) {
        self.run.start_now(&self.state, ctx);
    }

    /// One frame, against a bare `Ui`. Split out from the `eframe::App` impl so the
    /// headless harness can drive exactly what the window drives.
    pub fn frame(&mut self, ui: &mut Ui) {
        // Before anything is drawn, and regardless of which tab is showing: a render in
        // flight must not stall because the user went to look at the config.
        self.run.drain();
        self.take_dropped_files(ui);

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Target, "Target");
                ui.selectable_value(&mut self.tab, Tab::Config, "Config");
                ui.selectable_value(&mut self.tab, Tab::Plan, "Plan");
                ui.selectable_value(&mut self.tab, Tab::Run, "Run");
                ui.separator();
                if ui.add_enabled(self.state.dirty, egui::Button::new("Save")).clicked() {
                    let _ = self.state.save();
                }
                if self.state.dirty {
                    ui.colored_label(Color32::from_rgb(200, 170, 90), "unsaved");
                }
            });
        });

        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        egui::Panel::left("targets").resizable(true).show(ui, |ui| {
            ui.heading("targets");
            for name in self.state.target_names() {
                let selected = self.state.selected.as_deref() == Some(name.as_str());
                if ui.selectable_label(selected, &name).clicked() {
                    self.state.selected = Some(name);
                }
            }
            ui.separator();
            ui.heading("inputs");
            if self.state.inputs.is_empty() {
                ui.weak("drop masters here");
            }
            for input in &self.state.inputs {
                ui.label(
                    RichText::new(input.file_name().unwrap_or_default().to_string_lossy())
                        .monospace(),
                );
            }
        });

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Target => form::ui(ui, &mut self.state),
            Tab::Config => editor::ui(ui, &mut self.state),
            Tab::Plan => {
                ui.horizontal(|ui| {
                    ui.label("rows");
                    ui.add(egui::DragValue::new(&mut self.stress_rows).range(1..=5000));
                    ui.weak("real sources are repeated to reach this count");
                });
                plan::ui(ui, &self.state, &self.sources, self.stress_rows);
            }
            Tab::Run => self.run.ui(ui, &self.state),
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            match self.state.error() {
                // mint's own text, unparaphrased. The compiler-style Problems panel is
                // Phase 1's job; a spike gets one line.
                Some(error) => {
                    ui.colored_label(Color32::from_rgb(220, 90, 80), "✖");
                    ui.label(error.lines().next().unwrap_or_default())
                        .on_hover_text(error);
                }
                None => {
                    ui.colored_label(Color32::from_rgb(120, 200, 140), "✔");
                    ui.weak(format!("{} targets", self.state.target_names().len()));
                }
            }
            ui.separator();
            // S2's actual question, as a number: can validation run per frame?
            ui.weak(format!("validate {} µs", self.state.validate_micros));
            let frame_ms = ui.ctx().input(|i| i.stable_dt) * 1000.0;
            ui.weak(format!("frame {frame_ms:.1} ms"));
            ui.separator();
            if ui.button("Copy as CLI").clicked() {
                ui.ctx().copy_text(self.state.cli_command());
            }
        });
    }

    /// Dropped files, filtered by the library's own list of what it can decode. A second
    /// copy of `wav|wave|aif|aiff` here is exactly the drift `util::is_supported_input` was
    /// made public to prevent.
    fn take_dropped_files(&mut self, ui: &Ui) {
        let dropped: Vec<_> = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        for file in dropped {
            let path = file.path();
            if mint::util::is_supported_input(path)
                && !self.state.inputs.iter().any(|held| held == path)
            {
                self.state.inputs.push(path.to_path_buf());
            }
        }
        self.sources = plan::probe(&self.state.inputs);
    }
}

impl eframe::App for MintApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

//! Spike gate S5: a real render on a worker thread, with a real percentage and a cancel
//! that leaves nothing behind.
//!
//! The proposal planned this as Tier A — `stat()` the staging file at 4 Hz and divide by
//! `estimated_output_bytes` — because Tier B (`JobControl`) was a later phase. Tier B
//! landed first, so the spike uses the real thing: `pipeline::process_with` reports the
//! stage and its fraction from inside the loop that is actually running, which is a true
//! percentage rather than a file size standing in for one, and it works for stages that
//! write nothing (decode, analyse, resample) where the staging trick reports zero.
//!
//! Three properties this has to demonstrate, all of them from proposal §5.2/§7:
//! - the UI stays responsive while a DSD256 encode runs (worker thread, `request_repaint`);
//! - **no fake percentages** — a stage with nothing to count says so;
//! - a cancelled job leaves the previous render intact and no partial file, which is
//!   `pipeline`'s guarantee and is pinned by `tests/job_control.rs`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, ProgressBar, RichText, Ui};
use mint::config::ResolvedTarget;
use mint::pipeline::{self, ProcessingReport};
use mint::progress::{Cancelled, JobControl, Stage};
use mint::util::{duration_seconds, fmt_bytes};

use crate::state::AppState;

pub enum JobEvent {
    Progress { stage: Stage, fraction: f64 },
    Done(Box<ProcessingReport>),
    Cancelled,
    Failed(String),
}

#[derive(Default)]
pub struct RunView {
    job: Option<Job>,
    last: Option<Outcome>,
}

struct Job {
    events: Receiver<JobEvent>,
    cancel: Arc<AtomicBool>,
    stage: Stage,
    fraction: f64,
    label: String,
}

enum Outcome {
    Done(Box<ProcessingReport>),
    Cancelled,
    Failed(String),
}

impl RunView {
    /// Whether a job is in flight. Exposed for the headless harness, which has to know
    /// when to stop pumping frames.
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    /// The finished job's outcome, as the test needs to see it: the output path on success.
    pub fn finished_output(&self) -> Option<&str> {
        match &self.last {
            Some(Outcome::Done(report)) => Some(report.output.as_str()),
            _ => None,
        }
    }

    /// Stage and fraction of the job in flight, for the same reason.
    pub fn progress(&self) -> Option<(Stage, f64)> {
        self.job.as_ref().map(|job| (job.stage, job.fraction))
    }

    /// Start a render without a click, so the harness can drive it.
    pub fn start_now(&mut self, state: &AppState, ctx: egui::Context) {
        self.start(state, ctx);
    }

    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) {
        let ready = state.config().is_some() && !state.inputs.is_empty() && state.selected.is_some();
        ui.horizontal(|ui| {
            if self.job.is_some() {
                if ui.button("Cancel").clicked()
                    && let Some(job) = &self.job
                {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            } else {
                ui.add_enabled_ui(ready, |ui| {
                    if ui.button("Render").clicked() {
                        self.start(state, ui.ctx().clone());
                    }
                });
                if !ready {
                    ui.weak("needs a resolving config, a selected target and a dropped input");
                }
            }
        });

        if let Some(job) = &self.job {
            ui.add_space(4.0);
            ui.label(RichText::new(&job.label).monospace());
            ui.add(
                ProgressBar::new(job.fraction as f32)
                    .text(format!("{} {:.0}%", job.stage.label(), job.fraction * 100.0)),
            );
            return;
        }

        match &self.last {
            Some(Outcome::Done(report)) => self.report_ui(ui, report),
            Some(Outcome::Cancelled) => {
                ui.colored_label(
                    Color32::from_rgb(200, 170, 90),
                    "cancelled — the previous render is untouched and no partial file was left",
                );
            }
            Some(Outcome::Failed(err)) => {
                ui.colored_label(Color32::from_rgb(220, 90, 80), err);
            }
            None => {}
        }
    }

    fn report_ui(&self, ui: &mut Ui, report: &ProcessingReport) {
        ui.separator();
        ui.label(RichText::new(&report.output).monospace());
        egui::Grid::new("run-report").num_columns(2).spacing([16.0, 2.0]).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.weak(k);
                ui.label(RichText::new(v).monospace());
                ui.end_row();
            };
            row("rate", format!("{} -> {} Hz", report.in_rate, report.out_rate));
            row("shape", format!("{} ch, {:.1} s", report.channels, duration_seconds(report.frames, report.out_rate)));
            row("size", fmt_bytes(report.file_bytes));
            row("elapsed", format!("{:.2} s", report.elapsed_ms as f64 / 1000.0));
            row("delivered", format!("{:.2} LUFS", report.delivered.integrated_lufs));
            if let Some(gain) = report.loudness_gain_db {
                row("gain", format!("{gain:+.2} dB"));
            }
            row("seed", report.seed.map_or_else(|| "unpinned".to_string(), |s| s.to_string()));
        });
        // Warnings are the product (§7 item 9): never collapsed into a green checkmark.
        for warning in &report.warnings {
            ui.colored_label(Color32::from_rgb(200, 170, 90), format!("⚠ {warning}"));
        }
    }

    /// Take whatever the worker has posted since the last frame.
    ///
    /// Called from `MintApp::frame`, not from `RunView::ui`, and that is the point: a job's
    /// state machine must not stall because the user switched to another tab. It did, once.
    pub fn drain(&mut self) {
        let Some(job) = &mut self.job else { return };
        let mut finished = None;
        while let Ok(event) = job.events.try_recv() {
            match event {
                JobEvent::Progress { stage, fraction } => {
                    job.stage = stage;
                    job.fraction = fraction;
                }
                JobEvent::Done(report) => finished = Some(Outcome::Done(report)),
                JobEvent::Cancelled => finished = Some(Outcome::Cancelled),
                JobEvent::Failed(err) => finished = Some(Outcome::Failed(err)),
            }
        }
        if let Some(outcome) = finished {
            self.last = Some(outcome);
            self.job = None;
        }
    }

    fn start(&mut self, state: &AppState, ctx: egui::Context) {
        let (Some(config), Some(name), Some(input)) =
            (state.config(), state.selected.clone(), state.inputs.first().cloned())
        else {
            return;
        };
        let Ok(target) = config.resolve_target(&name) else { return };

        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        self.job = Some(Job {
            events: rx,
            cancel: cancel.clone(),
            stage: Stage::Probe,
            fraction: 0.0,
            label: format!("{} -> {name}", input.display()),
        });
        self.last = None;

        // A plain thread rather than the batch's rayon pool: this is one job, and the pool
        // is for the many-job case where the worker cap has to come from free RAM.
        std::thread::spawn(move || render(input, target, tx, cancel, ctx));
    }
}

fn render(
    input: PathBuf,
    target: ResolvedTarget,
    tx: Sender<JobEvent>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    // The sink must be `Sync` (jobs run under rayon in the batch case) and must never see
    // audio — a stage and a fraction, nothing else.
    let sink = |stage: Stage, fraction: f64| {
        let _ = tx.send(JobEvent::Progress { stage, fraction });
        ctx.request_repaint();
    };
    let ctl = JobControl { cancel: Some(&cancel), progress: Some(&sink) };

    let outcome = match pipeline::process_with(&input, &target, None, ctl) {
        Ok(report) => JobEvent::Done(Box::new(report)),
        Err(err) if err.downcast_ref::<Cancelled>().is_some() => JobEvent::Cancelled,
        Err(err) => JobEvent::Failed(format!("{err:#}")),
    };
    let _ = tx.send(outcome);
    ctx.request_repaint();
}

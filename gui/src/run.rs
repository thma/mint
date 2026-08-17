//! The batch runner: every (source × included target) pair, on a private pool, with a
//! progress display that counts jobs instead of promising minutes.
//!
//! This is `cli.rs::run`'s fan-out with a window in front of it, and it is deliberately the
//! *same* fan-out. `cli.rs` is the sibling implementation — read its `worker_count` / rayon
//! section beside this file — and the two agree on all four decisions that matter:
//!
//! - the cross product is input-major, every input into every selected target;
//! - `fail_fast` or a single worker takes the **sequential** path, because strict ordering
//!   and "stop after the first failure" are the same requirement;
//! - the worker count is `--jobs`, else `available_parallelism()`;
//! - a blocked pair is *not* filtered out before it runs. `process_with` refuses it after
//!   one header read, with mint's own reason, and that is how the reason reaches the
//!   failures list at all. Deciding here which jobs are impossible would be the GUI
//!   re-deriving `delivery_blocker`'s verdict into a second answer.
//!
//! **The pool is private**, built with `ThreadPoolBuilder` and `install`ed exactly as
//! `cli.rs` builds and installs its own. The global rayon pool is process-wide shared
//! state; a GUI that installs a batch onto it makes every later caller's parallelism a
//! function of what the last render did — and outside an `install`, `par_iter` *is* the
//! global pool. Work stealing buys nothing here (the jobs are coarse, seconds each); the
//! *bound* is the whole point, because each job holds one whole file as f64 and the worker
//! count is therefore the memory ceiling. A pool that fails to build falls back to running
//! the batch sequentially rather than dropping it: a batch that silently did not happen is
//! the worse failure. One coordinator thread owns all of this, and the UI thread never
//! blocks on any of it.
//!
//! **Nothing on screen is an extrapolation.** The batch bar counts jobs done out of jobs
//! total — a real quantity — and there is no ETA anywhere, by §7's own rule. A running job
//! shows its stage, and a percentage only once that stage has actually reported one: which
//! stages can count is `pipeline`'s business (the loudness meters and the WAV/FLAC/MP3
//! writers are single blocking calls), so rather than keeping a list of them here, a row
//! earns its bar by having sent a number above zero. A stage with nothing to count says so.
//!
//! **The memory cap is surfaced, not enforced.** Proposal §5.2 rule 5 wants the worker count
//! capped by free RAM — each job holds the whole file in `f64` and peaked at 485 MB (700 MB
//! with a rate conversion, roughly twice that under `on_clip = "maximize"`) on a five-minute
//! 96 kHz stereo master. Reading how much memory a machine has left needs a dependency this
//! build deliberately does not carry, so the arithmetic sits next to the worker count and
//! the engineer does the multiplication. Deferred, not forgotten.
//!
//! **The batch is pre-flighted before a thread is spawned.** `cli.rs` calls `probe_inputs` →
//! `plan_outputs` → `config::check_planned_outputs` *before any audio is touched*, and it is
//! not an optimization: two jobs fighting over one path is invisible from inside
//! `pipeline::process`, which sees one job at a time, and the rule that catches one job's
//! output landing on another job's *input* is refused even under `--force` because whichever
//! job reads first decides what gets rendered. A GUI that showed that in a panel and started
//! the batch anyway would execute — silently, reporting success — the batch mint structurally
//! refuses, and in that second case destroy a master. So [`RunView::start`] asks the same
//! question of the same plan (`problems::plan_cells`, the list the Problems panel walks) and
//! refuses with mint's own message, which the Run view then prints instead of a job list.
//!
//! Cancellation is one shared `AtomicBool` for the whole batch: in-flight jobs abort
//! cooperatively (`pipeline` returns `Cancelled` as an ordinary `Err`, so the previous
//! render survives and no partial file is left) and queued jobs never start. The window
//! closing is one of the things that raises it — see [`RunView::cancel_and_join`], because a
//! process that exits mid-render leaves `atomic_write`'s staging files behind and nothing
//! sweeps them.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use egui::{ProgressBar, RichText, ScrollArea, Ui};
use mint::config::{ResolvedTarget, SizeEstimate};
use mint::pipeline::{self, ProcessingReport, TaskFailure};
use mint::diagnostics::FailureKind;
use mint::progress::{JobControl, Stage};
use mint::util::fmt_bytes;

use crate::colors;
use crate::state::{AppState, RunOptions};

/// What a worker tells the UI. Every variant carries the job index: with several jobs in
/// flight, a progress tick with no return address belongs to nobody.
///
/// There is no `BatchFinished`: the coordinator owns the only surviving `Sender`, so the
/// channel disconnecting *is* the end of the batch. That end cannot be forgotten, which a
/// sentinel message can.
pub enum JobEvent {
    /// A worker picked this job up. Sent explicitly rather than inferred from the first
    /// progress tick, so the queued/running split is honest even for a job that dies inside
    /// `probe_source`.
    Started { job: usize },
    Progress { job: usize, stage: Stage, fraction: f64 },
    Done { job: usize, report: Box<ProcessingReport> },
    /// `kind` travels with the message because the live `anyhow::Error` does not survive
    /// the channel: once it is a `String`, the only route back to "was that a refused
    /// overwrite?" is reading the sentence.
    Failed { job: usize, error: String, kind: FailureKind },
    /// Cancelled: either in flight when the flag went up, or never started because of it.
    Cancelled { job: usize },
    /// Never ran: `fail_fast` stopped the batch before this one came up.
    Skipped { job: usize },
}

#[derive(Default)]
pub struct RunView {
    /// `Some` exactly while a batch is in flight; dropping it is what `busy()` reads.
    batch: Option<Batch>,
    /// The job list, kept after the batch ends — the Run view is also where a finished run
    /// is looked at before Report mode is opened.
    rows: Vec<JobRow>,
    /// What the last batch delivered, in the shape `json_report::render` and the Report view
    /// both take. Accumulated here rather than in `AppState` because it is an outcome, not
    /// part of the document.
    reports: Vec<ProcessingReport>,
    failures: Vec<TaskFailure>,
    /// One-shot edge: set when the batch drains its last job, cleared by
    /// [`RunView::take_just_finished`]. The shell reads it to persist the history and switch
    /// to Report exactly once, rather than every frame after a run.
    just_finished: bool,
    /// The worker count the batch in flight was started with. Held because
    /// `run_options.jobs` can be changed mid-run and the number on screen must describe the
    /// run that is happening.
    workers: usize,
    /// A cancel has been requested and the batch is winding down.
    cancelling: bool,
    /// Wall-clock time of the last finished batch. A measurement, never the input to a
    /// prediction about the next one.
    elapsed: Option<Duration>,
    /// Why the last Render did not start a batch: mint's own refusal from the pre-flight,
    /// verbatim. Cleared by the next start that gets through.
    refusal: Option<String>,
}

struct Batch {
    events: Receiver<JobEvent>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    /// The coordinator, kept rather than detached so [`RunView::cancel_and_join`] can wait
    /// for it. Nothing else joins: the channel disconnecting is what ends a batch normally.
    handle: std::thread::JoinHandle<()>,
}

/// One row of the job list. `target` and `input` are kept in `TaskFailure`'s own spelling so
/// a failure can be recorded without going back to the document for it.
struct JobRow {
    target: String,
    input: String,
    /// File name only, for the row; the full path is the hover.
    label: String,
    /// What the target *would* write, from `estimated_output_bytes` with its qualifier
    /// intact. A job that turns out to be impossible says so in its own row when it fails.
    predicted: Option<SizeEstimate>,
    state: JobState,
}

enum JobState {
    Queued,
    Running {
        started: Instant,
        stage: Stage,
        /// `None` until the stage has reported a number above zero — see the module doc.
        fraction: Option<f64>,
    },
    Done {
        bytes: u64,
        elapsed_ms: u64,
    },
    /// mint's own message, verbatim.
    Failed(String),
    Cancelled,
    Skipped,
}

/// One unit of work, owned rather than borrowed: `cli.rs`'s `Job` holds `&Path` and
/// `&ResolvedTarget` because its batch outlives nothing, while these cross a thread
/// boundary and outlive the frame that built them.
struct PendingJob {
    input: PathBuf,
    target: ResolvedTarget,
}

/// How a job ended, as far as the coordinator's `fail_fast` decision cares.
enum Terminal {
    Done,
    Failed,
    Cancelled,
}

/// The worker count a run would use: `--jobs` if pinned, else every core — `cli.rs`'s own
/// default, in `cli.rs`'s own words.
///
/// Public because the project bar's jobs control should be able to print the same number it
/// is about to cause. Cached because it is asked once per frame and `available_parallelism`
/// is a syscall.
pub fn worker_count(options: &RunOptions) -> usize {
    static CORES: OnceLock<usize> = OnceLock::new();
    options.jobs.unwrap_or_else(|| {
        *CORES.get_or_init(|| {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
        })
    })
}

impl RunView {
    /// Whether a batch is in flight. Exposed for the headless harness, which has to know
    /// when to stop pumping frames.
    pub fn busy(&self) -> bool {
        self.batch.is_some()
    }

    /// The last output the batch wrote. The harness's hook — with a batch of one it is that
    /// job's file — and deliberately not a summary: the Report view is what reads all of it.
    pub fn finished_output(&self) -> Option<&str> {
        self.reports.last().map(|report| report.output.as_str())
    }

    /// Stage and fraction of the earliest job still running.
    ///
    /// One job's state, not the batch's: the batch's own measure is jobs done out of jobs
    /// total, and averaging stage fractions across workers would invent a number that
    /// describes nothing. A stage that has reported no percentage reads as `0.0` here — the
    /// list is where that distinction is drawn, because it has room to say it.
    pub fn progress(&self) -> Option<(Stage, f64)> {
        self.rows.iter().find_map(|row| match row.state {
            JobState::Running { stage, fraction, .. } => Some((stage, fraction.unwrap_or(0.0))),
            _ => None,
        })
    }

    /// What the last batch delivered. The Report view renders these and
    /// `history::persist` writes them; both are read-only consumers.
    pub fn reports(&self) -> &[ProcessingReport] {
        &self.reports
    }

    pub fn failures(&self) -> &[TaskFailure] {
        &self.failures
    }

    /// Whether a batch finished since this was last asked. Consumed, so the shell's
    /// "persist, switch to Report, re-probe the sources" happens once per batch.
    pub fn take_just_finished(&mut self) -> bool {
        std::mem::take(&mut self.just_finished)
    }

    /// Start a batch without a click, so the harness can drive it.
    pub fn start_now(&mut self, state: &AppState, ctx: egui::Context) {
        self.start(state, ctx);
    }

    /// Why the last Render did not start, in mint's own words. `None` once one has.
    ///
    /// Read by the shell, which switches to this mode when it is `Some`: the button is in
    /// the project bar and the explanation is here, and a refusal nobody is shown is a
    /// Render that looks like it did nothing.
    pub fn refusal(&self) -> Option<&str> {
        self.refusal.as_deref()
    }

    pub fn ui(&mut self, ui: &mut Ui, state: &AppState) {
        self.controls(ui, state);
        if let Some(refusal) = &self.refusal {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(colors::BAD, "✖");
                // mint's own text, in full. The batch was not started, which is worth saying
                // out loud beside it: the pre-flight runs before a thread exists, so nothing
                // was written and nothing was half-written.
                ui.colored_label(colors::BAD, refusal);
            });
            ui.weak(
                "the batch was refused before it started — no job ran and nothing was \
                 written. The same rows are in the Problems drawer.",
            );
            return;
        }
        if self.rows.is_empty() {
            ui.add_space(4.0);
            ui.weak("no jobs yet — every source × included target pair becomes one job");
            return;
        }
        ui.add_space(4.0);
        self.batch_bar(ui);
        ui.separator();
        self.job_list(ui);
    }

    fn controls(&mut self, ui: &mut Ui, state: &AppState) {
        ui.horizontal(|ui| {
            if self.batch.is_some() {
                if ui.button("Cancel").clicked() {
                    self.cancel();
                }
                if self.cancelling {
                    // pipeline's documented guarantee, stated where it is being relied on.
                    ui.colored_label(
                        colors::WARN,
                        "cancelling — queued jobs will not start, and the jobs in flight leave \
                         the previous render intact and no partial file",
                    );
                }
            } else {
                let ready = state.config().is_some()
                    && !state.sources.is_empty()
                    && !state.included_names().is_empty();
                ui.add_enabled_ui(ready, |ui| {
                    if ui.button("Render ▸").clicked() {
                        self.start(state, ui.ctx().clone());
                    }
                });
                if !ready {
                    ui.weak(
                        "needs a resolving config, at least one source and at least one \
                         included target",
                    );
                }
            }

            // Only while a batch is in flight, and it is `self.workers` rather than the
            // current option: `--jobs` can be dragged mid-run, and the number here describes
            // the run that is happening. Before a run, the project bar's jobs control says
            // the same thing beside the widget that sets it — one statement per fact.
            if self.batch.is_some() {
                ui.separator();
                ui.weak(format!("{} workers", self.workers));
            }
        });
    }

    /// Jobs done out of jobs total. A count, not a clock.
    fn batch_bar(&self, ui: &mut Ui) {
        let total = self.rows.len();
        let mut done = 0;
        let mut failed = 0;
        let mut stopped = 0;
        for row in &self.rows {
            match row.state {
                JobState::Done { .. } => done += 1,
                JobState::Failed(_) => failed += 1,
                JobState::Cancelled | JobState::Skipped => stopped += 1,
                JobState::Queued | JobState::Running { .. } => {}
            }
        }
        let settled = done + failed + stopped;
        let bytes: u64 = self.reports.iter().map(|report| report.file_bytes).sum();

        ui.add(
            ProgressBar::new(settled as f32 / total.max(1) as f32)
                .text(format!("{settled} / {total} jobs")),
        );
        ui.horizontal(|ui| {
            ui.colored_label(colors::OK, format!("✔ {done} done"));
            if failed > 0 {
                ui.colored_label(colors::BAD, format!("✖ {failed} failed"));
            }
            if stopped > 0 {
                ui.colored_label(colors::WARN, format!("⊘ {stopped} not run"));
            }
            ui.weak(format!("· {} remaining", total - settled));
            ui.weak(format!("· {} written", fmt_bytes(bytes)));
            // Elapsed is measured, not extrapolated: there is no "of about N s" after it.
            let elapsed = self.batch.as_ref().map(|batch| batch.started.elapsed()).or(self.elapsed);
            if let Some(elapsed) = elapsed {
                ui.weak(format!("· {:.1} s elapsed", elapsed.as_secs_f64()));
            }
        });
    }

    fn job_list(&self, ui: &mut Ui) {
        ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for row in &self.rows {
                ui.horizontal(|ui| {
                    // Glyph *and* colour, never colour alone (capability #11).
                    let (glyph, color) = match row.state {
                        JobState::Queued => ("○", ui.visuals().weak_text_color()),
                        JobState::Running { .. } => ("◐", ui.visuals().strong_text_color()),
                        JobState::Done { .. } => ("✔", colors::OK),
                        JobState::Failed(_) => ("✖", colors::BAD),
                        JobState::Cancelled | JobState::Skipped => ("⊘", colors::WARN),
                    };
                    ui.colored_label(color, glyph);
                    ui.label(RichText::new(&row.target).strong());
                    ui.label(RichText::new(&row.label).monospace())
                        .on_hover_text(&row.input);
                    if let Some(predicted) = row.predicted {
                        ui.weak(format!(
                            "{}{}",
                            predicted.qualifier(),
                            fmt_bytes(predicted.bytes())
                        ));
                    }
                    Self::job_state_ui(ui, &row.state);
                });
            }
        });
    }

    fn job_state_ui(ui: &mut Ui, state: &JobState) {
        match state {
            JobState::Queued => {
                ui.weak("queued");
            }
            JobState::Running { started, stage, fraction } => {
                match fraction {
                    // A percentage only where there is one to report.
                    Some(fraction) => {
                        ui.add(
                            ProgressBar::new(*fraction as f32)
                                .desired_width(120.0)
                                .text(format!("{} {:.0}%", stage.label(), fraction * 100.0)),
                        );
                    }
                    None => {
                        ui.weak(format!("{} …", stage.label())).on_hover_text(
                            "this stage is one blocking call: it reports started and finished, \
                             not a percentage",
                        );
                    }
                }
                ui.weak(format!("{:.1} s", started.elapsed().as_secs_f64()));
            }
            JobState::Done { bytes, elapsed_ms } => {
                // What was actually written, beside what was predicted: the pair is the
                // only place `estimated_output_bytes` is ever checked by eye.
                ui.label(fmt_bytes(*bytes));
                ui.weak(format!("{:.1} s", *elapsed_ms as f64 / 1000.0));
            }
            // mint's own text, in full and not behind a disclosure.
            JobState::Failed(error) => {
                ui.colored_label(colors::BAD, error);
            }
            JobState::Cancelled => {
                ui.colored_label(
                    colors::WARN,
                    "cancelled — the previous render is untouched and no partial file was left",
                );
            }
            JobState::Skipped => {
                ui.colored_label(
                    colors::WARN,
                    "not run — the batch stopped at the first failure",
                );
            }
        }
    }

    fn cancel(&mut self) {
        if let Some(batch) = &self.batch {
            batch.cancel.store(true, Ordering::Relaxed);
            self.cancelling = true;
        }
    }

    /// Raise the cancel flag and wait, for a bounded time, for the coordinator to return.
    ///
    /// Called from `on_exit`. Without it, closing the window during a batch killed the
    /// renders mid-write: `run_native` returns, `main` returns, the process exits, and every
    /// job in flight dies somewhere between staging and rename. `atomic_write`'s own doc is
    /// explicit that "a killed process leaves its staging file behind, and nothing sweeps
    /// them" — at DSD256 that is gigabytes of `.mint-tmp` per abandoned job. Cancelled, the
    /// jobs unwind through `Staged::drop` and clean up after themselves.
    ///
    /// Bounded, because quit must not hang on a job that is inside a long block: a worker
    /// that has not reached its next `ctl.check()` within `grace` is left to the exiting
    /// process, which is exactly the old behaviour and no worse than it.
    pub fn cancel_and_join(&mut self, grace: Duration) {
        let Some(batch) = self.batch.take() else { return };
        batch.cancel.store(true, Ordering::Relaxed);
        self.cancelling = false;

        let deadline = Instant::now() + grace;
        while !batch.handle.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if batch.handle.is_finished() {
            let _ = batch.handle.join();
        }
    }

    /// Take whatever the workers have posted since the last frame.
    ///
    /// Called from `MintApp::frame`, not from [`RunView::ui`], and that is the point: a
    /// batch's state machine must not stall because the user switched to another mode. It
    /// did, once.
    pub fn drain(&mut self) {
        let Some(batch) = self.batch.as_ref() else { return };
        // Buffered first, applied second: the receiver is borrowed from `self` and applying
        // an event needs all of `self`. The `Vec` does not allocate on an idle frame.
        let mut events = Vec::new();
        let mut disconnected = false;
        loop {
            match batch.events.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) => break,
                // The coordinator dropped the last sender: the batch is over, and every
                // message it queued has already been handed over above.
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        for event in events {
            self.apply(event);
        }
        if disconnected {
            self.finish();
        }
    }

    fn apply(&mut self, event: JobEvent) {
        let index = match &event {
            JobEvent::Started { job }
            | JobEvent::Progress { job, .. }
            | JobEvent::Done { job, .. }
            | JobEvent::Failed { job, .. }
            | JobEvent::Cancelled { job }
            | JobEvent::Skipped { job } => *job,
        };
        let Some(row) = self.rows.get_mut(index) else { return };

        match event {
            JobEvent::Started { .. } => {
                // Started on the UI's clock rather than the worker's: it is only ever shown
                // as a running row's age, and a frame of skew does not change that reading.
                row.state = JobState::Running {
                    started: Instant::now(),
                    stage: Stage::Probe,
                    fraction: None,
                };
            }
            JobEvent::Progress { stage, fraction, .. } => {
                if let JobState::Running { stage: current, fraction: shown, .. } = &mut row.state {
                    if *current != stage {
                        *current = stage;
                        // A new stage has proved nothing yet; it earns its bar below.
                        *shown = None;
                    }
                    // Taken as given, including when it is *lower* than the last one. A
                    // stage is not one sweep (`progress::Stage`'s doc says so): `Ceiling`
                    // covers up to six limiter verification passes and `Loudness` four to
                    // nine whole-file meter passes, and each reports 0→1 of itself. Clamping
                    // this to a running maximum would look tidier and would be the lying bar
                    // §7 rule 8 forbids — it would sit at 100 % through every pass but the
                    // first.
                    if fraction > 0.0 {
                        *shown = Some(fraction);
                    }
                }
            }
            JobEvent::Done { report, .. } => {
                row.state =
                    JobState::Done { bytes: report.file_bytes, elapsed_ms: report.elapsed_ms };
                self.reports.push(*report);
            }
            JobEvent::Failed { error, kind, .. } => {
                self.failures.push(TaskFailure {
                    target: row.target.clone(),
                    input: row.input.clone(),
                    error: error.clone(),
                    kind,
                });
                row.state = JobState::Failed(error);
            }
            // Neither a report nor a failure: nothing was written and nothing was refused.
            JobEvent::Cancelled { .. } => row.state = JobState::Cancelled,
            JobEvent::Skipped { .. } => row.state = JobState::Skipped,
        }
    }

    fn finish(&mut self) {
        self.elapsed = self.batch.take().map(|batch| batch.started.elapsed());
        self.cancelling = false;

        // Defensive: `catch_unwind` gives every job a terminal event, so a row still queued
        // or running here means the worker thread itself died. mint-gui's own words about
        // mint-gui's own thread — there is no mint diagnostic to quote, because mint never
        // got to speak. Better an honest failure than a row that spins for the session.
        for index in 0..self.rows.len() {
            if !matches!(self.rows[index].state, JobState::Queued | JobState::Running { .. }) {
                continue;
            }
            let error = "the worker thread ended without reporting a result".to_string();
            self.failures.push(TaskFailure {
                target: self.rows[index].target.clone(),
                input: self.rows[index].input.clone(),
                error: error.clone(),
                // mint never ran, so mint has no verdict on this one either.
                kind: FailureKind::Other,
            });
            self.rows[index].state = JobState::Failed(error);
        }

        // A batch that was cancelled before its first job finished delivered nothing: no
        // history entry worth keeping, and no reason to pull the user out of the view they
        // just cancelled from.
        self.just_finished = !self.reports.is_empty() || !self.failures.is_empty();
    }

    fn start(&mut self, state: &AppState, ctx: egui::Context) {
        if self.batch.is_some() {
            return;
        }
        let targets = state.resolved_targets();
        if targets.is_empty() || state.sources.is_empty() {
            return;
        }

        // Pre-flight, before a thread exists — see the module doc. `cli.rs` bails here and
        // so does this: the plan is not executable, and a batch that cannot be executed must
        // not be started as if it could.
        if let Err(refusal) = preflight(state, &targets) {
            self.refusal = Some(refusal);
            return;
        }
        self.refusal = None;

        // Input-major, exactly as `cli.rs` builds its cross product. Iterating `sources`
        // rather than `inputs()` only because the header is what the size prediction needs;
        // it is the same list in the same order.
        let mut jobs = Vec::with_capacity(state.sources.len() * targets.len());
        let mut rows = Vec::with_capacity(jobs.capacity());
        for source in &state.sources {
            let label = source.path.file_name().unwrap_or_default().to_string_lossy().to_string();
            for target in &targets {
                let predicted = source
                    .probe
                    .as_ref()
                    .ok()
                    .and_then(|probe| crate::plan::cell_estimate(probe, target));
                rows.push(JobRow {
                    target: target.name.clone(),
                    input: source.path.display().to_string(),
                    label: label.clone(),
                    predicted,
                    state: JobState::Queued,
                });
                jobs.push(PendingJob { input: source.path.clone(), target: target.clone() });
            }
        }

        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let workers = worker_count(&state.run_options);
        let fail_fast = state.run_options.fail_fast;
        let seed = state.run_options.seed;

        self.rows = rows;
        self.reports.clear();
        self.failures.clear();
        self.just_finished = false;
        self.cancelling = false;
        self.elapsed = None;
        self.workers = workers;

        // One coordinator thread: it owns the worker set, and its own return is what tells
        // the UI the batch is over. The UI thread waits on nothing — except at quit.
        let handle = std::thread::spawn({
            let cancel = cancel.clone();
            move || coordinate(jobs, seed, workers, fail_fast, tx, cancel, ctx)
        });
        self.batch = Some(Batch { events: rx, cancel, started: Instant::now(), handle });
    }
}

/// Ask the library the questions that are about the *batch* rather than about a job, exactly
/// as `cli.rs::run` asks them, and refuse with mint's own message.
///
/// Two of them, and neither is answerable from inside `pipeline::process`: a path that cannot
/// be rendered at all (`cli.rs::plan_outputs` propagates that with `?` and never starts), and
/// `check_planned_outputs` — two jobs writing one file, or one job's output landing on
/// another job's input. The plan is `problems::plan_cells`', so this and the Problems panel
/// are asking about one set of cells and cannot come to two verdicts.
fn preflight(state: &AppState, targets: &[ResolvedTarget]) -> Result<(), String> {
    let refs: Vec<&ResolvedTarget> = targets.iter().collect();
    let cells = crate::problems::plan_cells(&state.sources, &refs);
    if let Some(err) = cells.iter().find_map(|cell| cell.output.as_ref().err()) {
        return Err(err.clone());
    }
    let planned = crate::problems::planned_outputs(&state.sources, &cells);
    mint::config::check_planned_outputs(&planned, &state.inputs())
        .map_err(|err| format!("{err:#}"))
}

/// The fan-out, off the UI thread. Mirrors `cli.rs::run`'s job section — see the module doc
/// for what the two agree on and where they differ.
fn coordinate(
    jobs: Vec<PendingJob>,
    seed: Option<u64>,
    workers: usize,
    fail_fast: bool,
    tx: Sender<JobEvent>,
    cancel: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    // A pool that will not build is not a reason to lose the batch: run it in order on this
    // thread instead. `workers <= 1` takes the same path for the same reason `cli.rs` does —
    // one worker *is* sequential, and building a one-thread pool to say so is ceremony.
    let pool = (!fail_fast && workers > 1)
        .then(|| rayon::ThreadPoolBuilder::new().num_threads(workers).build().ok())
        .flatten();

    match pool {
        // Parallel path: each (input, target) pair is an independent pipeline, so `for_each`
        // over the cross product is the whole coordination. `install` keeps the work on this
        // pool — outside it, `par_iter` would run on rayon's process-wide global one, which
        // is exactly the shared state a GUI must not install a batch onto.
        Some(pool) => pool.install(|| {
            use rayon::prelude::*;
            jobs.par_iter().enumerate().for_each(|(index, job)| {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(JobEvent::Cancelled { job: index });
                    return;
                }
                run_one(index, job, seed, &tx, &cancel, &ctx);
            });
        }),
        // Sequential path: strict ordering and fail-fast support, `cli.rs`'s own words.
        None => {
            for (index, job) in jobs.iter().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    let _ = tx.send(JobEvent::Cancelled { job: index });
                    continue;
                }
                if matches!(run_one(index, job, seed, &tx, &cancel, &ctx), Terminal::Failed)
                    && fail_fast
                {
                    // `cli.rs` simply breaks; here the jobs that will never run need a
                    // terminal state, or they sit in the list as queued forever.
                    for skipped in index + 1..jobs.len() {
                        let _ = tx.send(JobEvent::Skipped { job: skipped });
                    }
                    break;
                }
            }
        }
    }

    // Dropped *before* the repaint, not after: the disconnect is what ends the batch, and a
    // repaint that arrives first would leave the UI drawing a finished batch as busy until
    // the user happened to move the mouse.
    drop(tx);
    ctx.request_repaint();
}

fn run_one(
    index: usize,
    job: &PendingJob,
    seed: Option<u64>,
    tx: &Sender<JobEvent>,
    cancel: &AtomicBool,
    ctx: &egui::Context,
) -> Terminal {
    let _ = tx.send(JobEvent::Started { job: index });
    ctx.request_repaint();

    // The sink must be `Sync` — several jobs call it at once — and it must never see audio:
    // a stage and a fraction, nothing else. It also allocates nothing per frame of audio,
    // which is why `StageProgress`' half-percent throttle is enough to make this free.
    let sink = |stage: Stage, fraction: f64| {
        let _ = tx.send(JobEvent::Progress { job: index, stage, fraction });
        ctx.request_repaint();
    };
    let ctl = JobControl { cancel: Some(cancel), progress: Some(&sink) };

    // §5.2 rule 4: a panicking job is that job's failure, not a dead app. The default panic
    // hook still prints it — a silently swallowed panic is a lost bug report.
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
        pipeline::process_with(&job.input, &job.target, seed, ctl)
    }));

    let (event, terminal) = match outcome {
        Ok(Ok(report)) => {
            (JobEvent::Done { job: index, report: Box::new(report) }, Terminal::Done)
        }
        // mint's own classifier rather than a bare downcast of the outermost error, so
        // "the user stopped it" and "the render failed" cannot be confused for one another
        // — and so this keeps working if a cancel ever arrives wrapped in a `context`.
        Ok(Err(err)) => match FailureKind::classify(&err) {
            FailureKind::Cancelled => (JobEvent::Cancelled { job: index }, Terminal::Cancelled),
            kind => (
                JobEvent::Failed { job: index, error: format!("{err:#}"), kind },
                Terminal::Failed,
            ),
        },
        // A panic is mint-gui's own crash, not a mint verdict on the job.
        Err(payload) => (
            JobEvent::Failed {
                job: index,
                error: panic_message(payload.as_ref()),
                kind: FailureKind::Other,
            },
            Terminal::Failed,
        ),
    };
    let _ = tx.send(event);
    ctx.request_repaint();
    terminal
}

/// A panic payload as a line of text. mint-gui's own wording about mint-gui's own crash —
/// there is no mint diagnostic here to quote, because mint never got to produce one.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_string());
    format!("the render panicked: {detail}")
}

//! The shell: the project bar, three modes, two panes, a drawer, and the status bar.
//!
//! Part III §2's screen inventory and the §6 wireframes, arranged so that the three things
//! a mastering engineer needs at a glance are never more than a look away:
//!
//! ```text
//! ┌ PROJECT BAR ────────────────────────────────────────────────────────────┐
//! │ mint · <[meta].name>   <config path> ●edited   [Plan] Run Report        │
//! │ N sources × M targets = K jobs · <total predicted bytes>                │
//! │ seed <n> · jobs <n> · ☐force ☐fail-fast     [Render ▸] [Copy as CLI]    │
//! ├──────────────┬──────────────────────────────────────────────────────────┤
//! │ SOURCES +    │  CENTRE — Plan | Run | Report                            │
//! ├──────────────┤                                                          │
//! │ TARGETS      │                                                          │
//! ├──────────────┴──────────────────────────────────────────────────────────┤
//! │ DRAWER (collapsible): [Problems (n)] [mint.toml] [Target]               │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │ STATUS: ✔/✖ mint's verdict · validate NNN µs · frame N.N ms             │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Three properties of this file are load-bearing rather than cosmetic:
//!
//! - **The run worker is drained unconditionally, before anything is drawn.** It used to be
//!   drained from inside the Run tab's `ui`, which meant a job stalled the moment the user
//!   went to look at the config. That bug is fixed; do not reintroduce it.
//! - **The drawer's tab strip stays visible when the drawer is closed.** A collapsed drawer
//!   that also hides its Problems count would hide the count exactly when it matters.
//! - **The status bar keeps `validate NNN µs` / `frame N.N ms`.** It is the standing answer
//!   to spike gate S2 — "can validation run per frame?" — and it costs one line. A number
//!   answers that better than an opinion, permanently rather than once.
//!
//! Nothing here decides anything about audio. The bar's job count, size total and CLI line
//! are all read back from the library through `AppState`; the modes are three arrangements
//! of other modules' `ui` functions.

use std::time::{Duration, Instant};

use egui::{RichText, Ui};

use crate::colors;
use crate::state::{AppState, ExternalChange};
use crate::{
    chain, editor, history, inspector, plan, problems, report, run, sources, targets, watch,
};

/// The three centre screens. Replaces the spike's `Tab`: a mode is *what you are doing*,
/// while the drawer below holds *what you are looking at while doing it*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Plan,
    Run,
    Report,
}

/// The drawer's tabs: every problem at once, the document itself, and the inspector.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drawer {
    Problems,
    Toml,
    Target,
}

/// How often the project sidecar may be written. It holds nothing mint cannot be told on the
/// command line (§5.3), so losing the last couple of seconds of it costs a convenience —
/// while a write per frame would be an `fsync` per frame.
const PROJECT_FLUSH_INTERVAL: Duration = Duration::from_secs(2);

/// How long quitting waits for a cancelled batch to unwind. Long enough for a job to reach
/// its next `JobControl::check()` — the DSD block loop checks ~100 000 times per render, the
/// limiter once per block — and short enough that quit is still quit if one does not.
const EXIT_CANCEL_GRACE: Duration = Duration::from_secs(3);

/// How long a "reloaded" toast stays up. Long enough to be read by someone looking at the
/// other monitor when the edit landed, short enough not to become furniture.
const TOAST_LINGER: Duration = Duration::from_secs(6);

pub struct MintApp {
    state: AppState,
    mode: Mode,
    drawer: Drawer,
    drawer_open: bool,
    run: run::RunView,
    report: report::ReportView,
    inspector: inspector::InspectorView,
    /// `None` until the first frame, which is where the `egui::Context` the watcher needs to
    /// wake the app comes from. `Err` once, if the platform will not give us a watch.
    watcher: Option<watch::ConfigWatcher>,
    watch_failed: bool,
    /// The file changed on disk while this window had unsaved edits. Nothing has been taken
    /// and nothing overwritten until the user says which version wins.
    conflict: bool,
    /// A non-modal notice with an expiry. The one thing on screen that fades by itself.
    toast: Option<(String, Instant)>,
    /// A history or sidecar write that failed. Surfaced in the status bar and never
    /// swallowed — but never in the way of the next render either, because losing the
    /// bookkeeping is not losing the run.
    warning: Option<String>,
    project_flushed: Instant,
}

impl MintApp {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            mode: Mode::Plan,
            drawer: Drawer::Problems,
            drawer_open: true,
            run: run::RunView::default(),
            report: report::ReportView::default(),
            inspector: inspector::InspectorView::default(),
            watcher: None,
            watch_failed: false,
            conflict: false,
            toast: None,
            warning: None,
            project_flushed: Instant::now(),
        }
    }

    /// The document and selections, for the headless harness to set up a scenario the way a
    /// user would have. Not a general escape hatch: the app itself never reaches around its
    /// own state this way.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// The batch's state, for the harness to wait on and assert against. Shared, not
    /// exclusive: a test drives a render through [`Self::start_render`], the same door the
    /// button uses, rather than reaching in and setting a job's state by hand.
    pub fn run_view(&self) -> &run::RunView {
        &self.run
    }

    /// The inspector, for the harness to open a "why" panel the way a click would — see
    /// [`inspector::InspectorView::expand`].
    pub fn inspector_mut(&mut self) -> &mut inspector::InspectorView {
        &mut self.inspector
    }

    /// Which field's "why" panel is open, so a test can assert that clicking a row is what
    /// opened it rather than that the panel exists.
    pub fn inspector_expanded(&self) -> Option<&str> {
        self.inspector.expanded()
    }

    /// Whether the file on disk and this window's buffer have diverged and the user has not
    /// yet said which wins. Exposed so the harness can assert that nothing was picked for
    /// them, which is the whole promise of the banner.
    pub fn conflicted(&self) -> bool {
        self.conflict
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Switch centre screens. Also the harness's way in, because clicking the mode button
    /// requires the bar to have been laid out first and what is under test is usually what
    /// the mode *shows*, not the button.
    pub fn select_mode(&mut self, mode: Mode) {
        // Entering Plan is when a stale header would show: in an in-place workflow a render
        // may have landed on top of a source since the last look.
        if mode == Mode::Plan && self.mode != Mode::Plan {
            self.state.refresh_sources();
        }
        self.mode = mode;
    }

    pub fn select_drawer(&mut self, drawer: Drawer) {
        self.drawer = drawer;
        self.drawer_open = true;
    }

    /// Start a render without a click, for the same reason [`Self::select_mode`] exists.
    pub fn start_render(&mut self, ctx: egui::Context) {
        // The settings that produced this run are worth keeping even if the app never gets
        // a clean exit.
        self.flush_project();
        self.run.start_now(&self.state, ctx);
        // The Render button is in the project bar and the pre-flight's refusal is written in
        // the Run view, so a batch mint would not start must bring the user to where it says
        // why. Otherwise the click looks like it did nothing at all.
        if self.run.refusal().is_some() {
            self.select_mode(Mode::Run);
        }
    }

    /// One frame, against a bare `Ui`. Split out from the `eframe::App` impl so the
    /// headless harness can drive exactly what the window drives.
    pub fn frame(&mut self, ui: &mut Ui) {
        // Before anything is drawn, and regardless of which mode is showing: a render in
        // flight must not stall because the user went to look at the config.
        self.run.drain();
        self.take_dropped_files(ui);
        self.take_external_change(ui.ctx());
        self.finish_batch(ui.ctx());
        if self.state.project_dirty() && self.project_flushed.elapsed() >= PROJECT_FLUSH_INTERVAL {
            self.flush_project();
        }

        egui::Panel::top("project-bar").show(ui, |ui| self.project_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        // Sized for the tab that needs the most room: the inspector is 22 rows and a details
        // pane. A drawer that opens onto four visible rows is one nobody leaves open.
        egui::Panel::bottom("drawer").resizable(true).default_size(340.0).show(ui, |ui| {
            self.drawer(ui);
        });

        egui::Panel::left("side").resizable(true).default_size(260.0).show(ui, |ui| {
            // Sources above targets, both resizable against each other: which one needs the
            // room depends entirely on whether the session is 3 takes × 8 targets or the
            // other way round.
            egui::Panel::top("sources").resizable(true).default_size(240.0).show(ui, |ui| {
                sources::ui(ui, &mut self.state);
            });
            targets::ui(ui, &mut self.state);
        });

        egui::CentralPanel::default().show(ui, |ui| match self.mode {
            Mode::Plan => {
                // The chain preview sits under the matrix rather than beside it: it
                // describes the selected *cell*, so it belongs on the same axis as the row
                // that selected it.
                egui::Panel::bottom("chain").resizable(true).default_size(200.0).show(ui, |ui| {
                    chain::ui(ui, &self.state);
                });
                plan::ui(ui, &mut self.state);
            }
            Mode::Run => self.run.ui(ui, &self.state),
            Mode::Report => {
                egui::Panel::right("history").resizable(true).default_size(240.0).show(ui, |ui| {
                    history::ui(ui, &self.state);
                });
                self.report.ui(ui, &self.state, self.run.reports(), self.run.failures());
            }
        });
    }

    fn project_bar(&mut self, ui: &mut Ui) {
        // Once per frame, not once per row: it walks every cell of the matrix.
        let estimate = plan::batch_estimate(&self.state);

        ui.horizontal(|ui| {
            ui.label(RichText::new("mint").strong());
            if let Some(name) = self.state.meta_name() {
                ui.weak("·");
                ui.label(name);
            }
            ui.weak(self.state.path.display().to_string());
            if self.state.dirty {
                ui.colored_label(colors::WARN, "● edited");
                if ui.button("Save").clicked() {
                    let _ = self.state.save();
                }
            }
            ui.separator();
            for (mode, label) in
                [(Mode::Plan, "Plan"), (Mode::Run, "Run"), (Mode::Report, "Report")]
            {
                if ui.selectable_label(self.mode == mode, label).clicked() {
                    self.select_mode(mode);
                }
            }
        });

        ui.horizontal(|ui| {
            // The batch as a quantity, not as a promise: jobs are counted, bytes are the
            // library's own predictions, and the qualifier says what the sum is worth. The
            // count is what the run will *attempt*, so the arithmetic on the left of the
            // `=` actually produces the number on its right — blocked cells are named
            // separately rather than quietly subtracted, because `run.rs` runs them and
            // mint's own refusal is the point of running them.
            //
            // `estimate.targets`, not `included_names().len()`: they are the same number
            // whenever the document validates, and when it does not `resolved_targets()` is
            // empty, so the ticked count would multiply out to a product the `= K jobs` on
            // its right contradicts. The whole document being refused is the reason, and it
            // is said out loud below rather than left to be inferred from a zero.
            ui.weak(format!(
                "{} sources × {} targets = {} jobs",
                self.state.sources.len(),
                estimate.targets,
                estimate.jobs,
            ));
            if self.state.config().is_none() {
                ui.colored_label(colors::BAD, "— the document does not resolve")
                    .on_hover_text(
                        "mint refuses the config as a whole, so no target is planned. The \
                         Problems drawer lists every reason at once.",
                    );
            }
            if estimate.blocked > 0 {
                ui.colored_label(colors::BAD, format!("! {} blocked", estimate.blocked))
                    .on_hover_text(
                        "these will run and fail with mint's own reason — see Problems, or \
                         hover the cell in the matrix",
                    );
            }
            if estimate.jobs > 0 {
                ui.weak("·");
                ui.label(format!(
                    "{}{}",
                    estimate.qualifier(),
                    mint::util::fmt_bytes(estimate.bytes)
                ));
                if estimate.unknown > 0 {
                    ui.weak(format!("+ {} of unknown size", estimate.unknown));
                }
            }
        });

        ui.horizontal(|ui| {
            // Any of these changes what `problems` says (step 9 asks `output_conflict` with
            // `force`) and what the sidecar holds, so the whole row is diffed at the end
            // rather than each widget carrying its own bookkeeping.
            let before = self.state.run_options.clone();
            let options = &mut self.state.run_options;

            let mut pinned = options.seed.is_some();
            ui.checkbox(&mut pinned, "seed");
            match (pinned, options.seed) {
                (true, None) => options.seed = Some(0),
                (false, Some(_)) => options.seed = None,
                _ => {}
            }
            if let Some(seed) = options.seed.as_mut() {
                ui.add(egui::DragValue::new(seed));
            } else {
                ui.weak("unpinned — this run is not reproducible");
            }

            ui.separator();
            let mut capped = options.jobs.is_some();
            ui.checkbox(&mut capped, "jobs");
            match (capped, options.jobs) {
                (true, None) => options.jobs = Some(1),
                (false, Some(_)) => options.jobs = None,
                _ => {}
            }
            // Read before the `DragValue` borrows `jobs`, and it is the *same* function the
            // runner sizes its pool with, so the number here cannot be a different number.
            let workers = run::worker_count(options);
            match options.jobs.as_mut() {
                Some(jobs) => {
                    ui.add(egui::DragValue::new(jobs).range(1..=256));
                }
                // Named, not left as "all": what "all cores" comes to on this machine is the
                // number the memory arithmetic beside it multiplies.
                None => {
                    ui.weak(format!("all cores ({workers})"));
                }
            }
            // Proposal §5.2 rule 5, next to the control that sets it: surfaced as
            // arithmetic rather than enforced as a cap, because reading how much memory is
            // free needs a dependency this build deliberately does not carry.
            ui.weak("× ~0.5–0.7 GB/job").on_hover_text(
                "Peak resident memory measured on a five-minute 96 kHz stereo master: 485 MB \
                 per job, 700 MB when a rate conversion runs, and roughly twice that under \
                 on_clip = \"maximize\". Nothing caps the worker count on free memory — the \
                 multiplication is shown instead of enforced.",
            );

            ui.separator();
            // Both tooltips are `cli.rs`'s own `--force` / `--fail-fast` help text, quoted.
            // The flags mean what mint says they mean, and `force` is the same widening of
            // `overwrite` that CLAUDE.md's gotcha list warns about.
            ui.checkbox(&mut options.force, "force").on_hover_text(crate::FORCE_HELP);
            ui.checkbox(&mut options.fail_fast, "fail-fast").on_hover_text(
                "Stop after the first failure (default: collect all failures and continue).",
            );

            if self.state.run_options != before {
                self.state.mark_project_dirty();
                self.state.recollect_problems();
            }

            ui.separator();
            let ready = !self.run.busy() && estimate.jobs > 0;
            if ui.add_enabled(ready, egui::Button::new("Render ▸")).clicked() {
                let ctx = ui.ctx().clone();
                self.start_render(ctx);
            }
            if ui.button("Copy as CLI").clicked() {
                ui.ctx().copy_text(self.state.cli_command());
            }
        });

        self.conflict_banner(ui);
    }

    /// Two versions of one document, both offered, neither taken.
    ///
    /// Part III §4: *"If the GUI has unsaved edits, that is a genuine conflict — show both,
    /// never silently pick."* The three buttons are the three answers, all of them explicit,
    /// and dismissing the banner without choosing simply leaves the buffer as it is — which
    /// is the fourth answer and the safest one.
    fn conflict_banner(&mut self, ui: &mut Ui) {
        if !self.conflict {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(colors::BAD, "⚠");
            ui.label(format!(
                "{} changed on disk, and this window has unsaved edits.",
                self.state.path.display()
            ));
            if ui
                .button("Keep mine")
                .on_hover_text("leave the buffer alone; the file on disk is untouched either way")
                .clicked()
            {
                self.conflict = false;
            }
            if ui
                .button("Take what is on disk")
                .on_hover_text("discard the edits in this window and reload the file")
                .clicked()
            {
                self.conflict = false;
                match self.state.discard_edits_and_reload() {
                    Ok(()) => self.notify("reloaded from disk"),
                    Err(err) => self.warning = Some(format!("could not reload: {err:#}")),
                }
            }
            if ui
                .button("Save mine over it")
                .on_hover_text("write this window's version, replacing what is on disk")
                .clicked()
            {
                self.conflict = false;
                match self.state.save() {
                    Ok(()) => self.notify("saved over the file on disk"),
                    Err(err) => self.warning = Some(format!("could not save: {err:#}")),
                }
            }
        });
    }

    /// Notice the file moving under us, and decide nothing that is the user's to decide.
    ///
    /// The watcher is started here rather than in [`Self::new`] because this is the first
    /// place with an `egui::Context` to wake the app with — and because a headless harness
    /// that never calls `frame` never starts one, which is what keeps the tests from
    /// depending on the platform's file-notification API.
    fn take_external_change(&mut self, ctx: &egui::Context) {
        if self.watcher.is_none() && !self.watch_failed {
            match watch::ConfigWatcher::new(&self.state.path, ctx.clone()) {
                Ok(watcher) => self.watcher = Some(watcher),
                // Once, and then never again: a watch is a convenience, and retrying every
                // frame would turn a missing facility into a stutter.
                Err(err) => {
                    self.watch_failed = true;
                    self.warning = Some(format!("not watching the config for edits: {err:#}"));
                }
            }
        }
        if !self.watcher.as_ref().is_some_and(watch::ConfigWatcher::changed) {
            return;
        }
        match self.state.absorb_external_change() {
            ExternalChange::None => {}
            ExternalChange::Reloaded => self.notify("the config changed on disk — reloaded"),
            ExternalChange::Conflict => self.conflict = true,
        }
    }

    fn notify(&mut self, message: &str) {
        self.toast = Some((message.to_string(), Instant::now()));
    }

    /// The drawer: a tab strip that stays put, and contents that fold away.
    fn drawer(&mut self, ui: &mut Ui) {
        // Somebody asked the TOML drawer to show a byte range — the inspector's
        // `[ open [defaults] ]`. The request is honoured here rather than by the asker
        // because the drawer's tab and its open/shut state are the shell's, and `editor.rs`
        // takes the range on the frame it scrolls to it.
        if self.state.reveal.is_some() {
            self.drawer = Drawer::Toml;
            self.drawer_open = true;
        }
        ui.horizontal(|ui| {
            let arrow = if self.drawer_open { "▾" } else { "▸" };
            if ui.small_button(arrow).clicked() {
                self.drawer_open = !self.drawer_open;
            }
            // The count travels with the tab label, so it is legible with the drawer shut.
            let errors = problems::error_count(&self.state.problems);
            let total = self.state.problems.len();
            let problems_label =
                if total == 0 { "Problems".to_string() } else { format!("Problems ({total})") };
            let response = ui.selectable_label(self.drawer == Drawer::Problems, problems_label);
            if response.clicked() {
                self.select_drawer(Drawer::Problems);
            }
            if errors > 0 {
                ui.colored_label(colors::BAD, "✖");
            } else if total > 0 {
                ui.colored_label(colors::WARN, "⚠");
            }
            for (tab, label) in [(Drawer::Toml, "mint.toml"), (Drawer::Target, "Target")] {
                if ui.selectable_label(self.drawer == tab, label).clicked() {
                    self.select_drawer(tab);
                }
            }
        });
        if !self.drawer_open {
            return;
        }
        ui.separator();
        match self.drawer {
            Drawer::Problems => problems::ui(ui, &mut self.state),
            Drawer::Toml => editor::ui(ui, &mut self.state),
            Drawer::Target => self.inspector.ui(ui, &mut self.state),
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            match self.state.error() {
                // mint's own text, unparaphrased. The first line here, the whole thing on
                // hover, and every one of them at once in the Problems drawer.
                Some(error) => {
                    ui.colored_label(colors::BAD, "✖");
                    ui.label(error.lines().next().unwrap_or_default()).on_hover_text(error);
                }
                None => {
                    ui.colored_label(colors::OK, "✔");
                    ui.weak(format!("{} targets", self.state.target_names().len()));
                }
            }
            ui.separator();
            // S2's actual question, as a number: can validation run per frame?
            ui.weak(format!("validate {} µs", self.state.validate_micros));
            // Its companion, and the one that can run away. Both are paid on every keystroke
            // in the TOML drawer, so showing only the first would understate the cost of a
            // keypress by the whole collection — 2.3 ms at 220 cells, and two orders of
            // magnitude more while the batch has an unfixed output collision.
            ui.weak(format!("problems {} µs", self.state.problems_micros)).on_hover_text(
                "the Problems panel is collected on change, not per frame — this is what one \
                 document or source change costs",
            );
            let frame_ms = ui.ctx().input(|i| i.stable_dt) * 1000.0;
            ui.weak(format!("frame {frame_ms:.1} ms"));
            if let Some(warning) = &self.warning {
                ui.separator();
                ui.colored_label(colors::WARN, format!("⚠ {warning}"));
            }
            self.toast(ui);
        });
    }

    /// The non-modal notice from §4's "show a reloaded toast".
    ///
    /// In the status bar rather than floating over the document: a reload is news, not an
    /// interruption, and the one thing it must not do is cover the text it is telling you
    /// about. It needs a repaint scheduled to disappear on its own — egui draws on demand,
    /// so without one the toast would linger until the next mouse move.
    fn toast(&mut self, ui: &mut Ui) {
        let Some((message, shown)) = &self.toast else { return };
        let left = TOAST_LINGER.checked_sub(shown.elapsed());
        match left {
            Some(left) => {
                ui.separator();
                ui.colored_label(colors::OK, format!("↻ {message}"));
                ui.ctx().request_repaint_after(left);
            }
            None => self.toast = None,
        }
    }

    /// A finished batch: keep it, show it, and re-read the headers it may have overwritten.
    fn finish_batch(&mut self, ctx: &egui::Context) {
        if !self.run.take_just_finished() {
            return;
        }
        let persisted = history::persist(
            &self.state.path,
            self.state.meta_name(),
            self.run.reports(),
            self.run.failures(),
        );
        if let Err(err) = persisted {
            self.warning = Some(format!("run history not written: {err:#}"));
        }
        // Told rather than left to the panel's staleness window: the run the user just
        // watched happen must be in the list the moment they look at it.
        history::invalidate(ctx, &self.state.path);
        self.select_mode(Mode::Report);
        self.state.refresh_sources();
        // Unconditionally, and *not* left to `refresh_sources`: that only recollects when a
        // **source** moved, while a batch changes the filesystem on the *output* side — which
        // is what steps 8 and 9 read. Render into a separate `dir` and no source moves at all,
        // so the panel would go on saying "mint has no objection" while every cell in the
        // matrix beside it had turned into `⟳ replaces an existing file` and a second Render
        // would fail on all of them.
        self.state.recollect_problems();
    }

    fn flush_project(&mut self) {
        self.project_flushed = Instant::now();
        if let Err(err) = self.state.flush_project() {
            self.warning = Some(format!("project sidecar not written: {err:#}"));
        }
    }

    /// Dropped files, through the same door as the file picker — `sources::add`, not
    /// `AppState::add_inputs` directly.
    ///
    /// The filtering is `util::is_supported_input`'s either way (a second copy of
    /// `wav|wave|aif|aiff` here is the drift that function was made public to prevent), but
    /// the *notice* about what the filter refused lives in `sources.rs`, and going straight
    /// to `add_inputs` skipped it: an `.mp3` or a folder dragged onto the window vanished
    /// without a word, on the route the empty Sources pane advertises first.
    fn take_dropped_files(&mut self, ui: &Ui) {
        let dropped: Vec<_> = ui.ctx().input(|i| i.raw.dropped_files.clone());
        if dropped.is_empty() {
            return;
        }
        let paths: Vec<_> = dropped.iter().map(|file| file.path().to_path_buf()).collect();
        sources::add(ui, &mut self.state, paths);
    }
}

impl eframe::App for MintApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }

    /// Two things the process exiting would otherwise take with it.
    ///
    /// The last debounce interval, which the timer in [`Self::frame`] drops on the floor:
    /// tick a target and quit inside two seconds and the sidecar never learns about it.
    /// Nothing in that file is anything mint cannot be told on the command line, so a lost
    /// write costs a convenience — but it is the convenience the file exists for.
    ///
    /// And a batch in flight, which is not a convenience: an exiting process kills its
    /// renders between staging and rename, and `atomic_write` leaves those staging files
    /// behind for nobody to sweep up. Cancelling first lets the jobs unwind properly. The
    /// wait is bounded so quit stays quit — see [`run::RunView::cancel_and_join`].
    ///
    /// The `glow` parameter is there because `gui/Cargo.toml` asks for both backends; if
    /// that ever changes, eframe's other `on_exit` signature makes it a compile error rather
    /// than a method that silently stops being called.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.run.cancel_and_join(EXIT_CANCEL_GRACE);
        self.flush_project();
    }
}

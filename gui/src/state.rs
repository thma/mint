//! What the whole app looks at: one document, and mint's verdict on it.
//!
//! The single rule this type exists to enforce is proposal §5.2's first one — **the GUI
//! never re-derives**. There is no parallel model of a target here, no cached "effective
//! rate", no mirrored validation. There is the document text, the `Config` mint made of it
//! (or the error mint refused it with), and nothing else. Every screen asks the library
//! again rather than asking this struct for a conclusion.
//!
//! Phase 1 adds three caches, and they are caches in the strict sense — each one holds
//! *the library's own answers*, keyed on the thing that invalidates them, and none of them
//! is ever consulted in place of asking again:
//!
//! - `sources`: one `probe_source` per file, keyed on path + mtime + length. A header read
//!   is cheap but not free, and the plan matrix would otherwise do 160 of them per frame.
//! - `parsed`: `Config::parse_toml_str` — parsed, *not* validated. The Problems panel needs
//!   a `Config` precisely in the case `status` is `Err`, because that is when there is
//!   something to list, and `from_toml_str` refuses to hand one over exactly then.
//! - `problems`: the collector's output. Steps 4 and 8 of `problems::collect` are quadratic
//!   in targets and cells, so it runs on change, never per frame.
//!
//! The two selections (`selected`, `selected_source`) are one thing viewed from two panes:
//! together they name the cell whose chain preview is on screen. A cell click sets both.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use mint::audio::io_read::{self, SourceProbe};
use mint::config::{Config, ResolvedTarget};

use crate::problems::Problem;
use crate::write_back::ConfigDocument;

pub struct AppState {
    pub path: PathBuf,
    /// The document as edited. `mint.toml` *is* the project file, so this is the project.
    pub text: String,
    /// Reparsed on every change; `None` while the text is not even valid TOML.
    pub doc: Option<ConfigDocument>,
    /// mint's own verdict on the whole document: parse + validate. The error is kept as
    /// mint formatted it — the GUI does not paraphrase, because the paraphrase is where a
    /// second opinion creeps in.
    pub status: Result<Config, String>,
    /// Parsed but *not* validated. Every narrow question the Problems panel asks
    /// (`resolve_target` per name, `check_collisions` per pair) needs a `Config`, and the
    /// case worth asking about is the one where `status` is `Err`.
    pub parsed: Option<Config>,
    /// How long the last parse + validate took. The spike question S2 asks is whether this
    /// can run per frame; a number answers it better than an opinion.
    pub validate_micros: u128,
    /// How long the last problem collection took, kept **separate** from `validate_micros`
    /// rather than folded into it. Two reasons: `validate_micros` is the standing answer to
    /// S2 and must keep meaning what it meant, and the collector's cost is the one that can
    /// run away — `problems::collect`'s own table records 2.3 ms for 220 cells and 125 ms
    /// while the batch has an unfixed output collision. Both are paid per keystroke, so
    /// both are on the status bar.
    pub problems_micros: u128,
    /// Probe cache, invalidated on path + mtime + length. Never per frame.
    pub sources: Vec<Source>,
    /// Selected target name.
    pub selected: Option<String>,
    /// Index into `sources`. With `selected`, names the cell the chain preview describes.
    pub selected_source: Option<usize>,
    /// Targets that participate in a run. **Empty set == all of them**, which is the CLI's
    /// own default when no `--target` is passed; a strict subset becomes repeated
    /// `--target` flags in the CLI line. Read it through [`Self::included_names`] rather
    /// than directly, so the convention has one implementation.
    pub included: BTreeSet<String>,
    pub run_options: RunOptions,
    /// Everything mint knows is wrong, at once. Recomputed on config / source / flag
    /// change by [`Self::recollect_problems`], never per frame.
    pub problems: Vec<Problem>,
    /// A byte range of `text` that another screen has asked the TOML drawer to show — the
    /// inspector's `[ open [defaults] ]`. Set by whoever asks, taken by `editor.rs` on the
    /// frame it acts on it, so a request cannot fire twice and cannot fight the user's own
    /// scrolling on every subsequent frame.
    pub reveal: Option<std::ops::Range<usize>>,
    /// The document has unsaved edits.
    pub dirty: bool,
    /// The *sidecar* has unflushed changes (inputs, inclusions, run options). Separate from
    /// `dirty` because they are different documents with different stakes: `mint.toml` is
    /// the user's and is only ever written on an explicit Save, while
    /// `.mint-gui/<stem>/project.toml` holds nothing mint cannot be told on the command line
    /// and is flushed whenever the shell gets round to it.
    project_dirty: bool,
}

/// One input file with the header mint read from it.
///
/// `probe` is an `Err` for a file whose header cannot be read, and that is deliberately
/// **not** fatal to the set — it is one task's failure with the rest of the run continuing,
/// exactly as `cli.rs`'s `ProbedInput` documents. The error is mint's own text, kept
/// per-file so the Sources pane can show it against the row it belongs to.
pub struct Source {
    pub path: PathBuf,
    pub probe: Result<SourceProbe, String>,
    pub file_bytes: Option<u64>,
    /// Cache key, with `file_bytes`. A re-probe happens when the file's mtime or length
    /// moved, never per frame — and a render *can* land on top of a source in an in-place
    /// workflow, which is why [`AppState::refresh_sources`] runs after a batch.
    pub mtime: Option<SystemTime>,
}

/// What [`AppState::absorb_external_change`] found when the file moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalChange {
    /// Nothing to do: the file matches the buffer (our own save looks like this), or it could
    /// not be read this instant because a rename is in flight.
    None,
    /// Taken. The buffer had no unsaved edits, so the disk was simply newer.
    Reloaded,
    /// Two versions, both somebody's work. Nothing has been taken and nothing overwritten.
    Conflict,
}

/// The run-scoped flags, in the CLI's own vocabulary. Nothing here can change a sample
/// except by the same route the CLI changes it, which is why the sidecar may persist them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOptions {
    pub seed: Option<u64>,
    /// `None` == `available_parallelism()`, same as the CLI.
    pub jobs: Option<usize>,
    pub force: bool,
    pub fail_fast: bool,
}

impl AppState {
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|err| format!("# could not read {}: {err}\n", path.display()));
        let mut state = Self {
            path: path.to_path_buf(),
            text,
            doc: None,
            status: Err(String::new()),
            parsed: None,
            validate_micros: 0,
            problems_micros: 0,
            sources: Vec::new(),
            selected: None,
            selected_source: None,
            included: BTreeSet::new(),
            run_options: RunOptions::default(),
            problems: Vec::new(),
            reveal: None,
            dirty: false,
            project_dirty: false,
        };
        state.revalidate();
        state.selected = state.target_names().first().cloned();

        // The sidecar restores what `mint.toml` deliberately does not hold (§5.3: inputs in
        // the config would hard-error an older mint binary under `deny_unknown_fields`).
        if let Some(project) = crate::history::load_project(path) {
            state.included = project.included;
            state.run_options = project.run_options;
            state.add_inputs(project.inputs);
        }
        // Restoring is not a change worth writing back.
        state.project_dirty = false;
        state
    }

    /// Reparse and re-ask mint whether the document is acceptable.
    ///
    /// Sets **both** verdicts, because they answer different questions: `status` is
    /// "would mint load this?" and `parsed` is "is there enough here to ask about?".
    pub fn revalidate(&mut self) {
        let started = Instant::now();
        self.doc = ConfigDocument::parse(&self.text).ok();
        self.status = match &self.doc {
            // Through the document rather than the raw text so that what is validated is
            // exactly what a Save would write.
            Some(doc) => doc.validate().map_err(|err| format!("{err:#}")),
            None => Config::from_toml_str(&self.text).map_err(|err| format!("{err:#}")),
        };
        self.parsed = Config::parse_toml_str(&self.text).ok();
        self.validate_micros = started.elapsed().as_micros();
        self.recollect_problems();
    }

    /// Re-run the problem collector. Public because a run-option checkbox changes the
    /// answer (step 9 asks `output_conflict` with `force`) without touching the document —
    /// anything that mutates `run_options` or `included` directly must call this after.
    pub fn recollect_problems(&mut self) {
        let started = Instant::now();
        let names = self.target_names();
        let included = self.included_names();
        let problems = crate::problems::collect(crate::problems::Query {
            text: &self.text,
            parsed: self.parsed.as_ref(),
            names: &names,
            included: &included,
            sources: &self.sources,
            force: self.run_options.force,
        });
        self.problems = problems;
        self.problems_micros = started.elapsed().as_micros();
    }

    /// Names in the document's own order. `Config::target_names` sorts (its `targets` is a
    /// `BTreeMap`); the document order is the user's, and it is the one to show.
    pub fn target_names(&self) -> Vec<String> {
        match &self.doc {
            Some(doc) => doc.target_names(),
            None => Vec::new(),
        }
    }

    /// The targets that participate in a run, in document order, with the "empty == all"
    /// convention already applied. Every caller wants this one, not `included`.
    pub fn included_names(&self) -> Vec<String> {
        let names = self.target_names();
        if self.included.is_empty() {
            return names;
        }
        names.into_iter().filter(|name| self.included.contains(name)).collect()
    }

    pub fn is_included(&self, name: &str) -> bool {
        self.included.is_empty() || self.included.contains(name)
    }

    /// Tick or untick one target.
    ///
    /// The empty set means "all", so the first untick has to materialize the set before it
    /// can remove anything — and once the set is back to holding every name it is emptied
    /// again, so that "all" stays "all" when a target is later added to the document.
    ///
    /// The last untick is **refused**, and that is the price of the "empty == all"
    /// convention: emptying the set does not mean "no targets", it means every one of them,
    /// so the click that looks like it turns the last target off would silently turn all of
    /// them on. The Targets pane disables that checkbox and says why; this refusal is the
    /// same rule for every other caller, and there is no third reading to pick between.
    pub fn set_included(&mut self, name: &str, included: bool) {
        // Asked before anything is materialized, so a refused untick leaves the set exactly
        // as it was — including the empty "all", which is what keeps a target added to the
        // document later included by default.
        if !included && self.included_names().len() <= 1 {
            return;
        }
        let names = self.target_names();
        if self.included.is_empty() {
            self.included = names.iter().cloned().collect();
        }
        if included {
            self.included.insert(name.to_string());
        } else {
            self.included.remove(name);
        }
        if names.iter().all(|n| self.included.contains(n)) && self.included.len() == names.len() {
            self.included.clear();
        }
        self.mark_project_dirty();
        self.recollect_problems();
    }

    pub fn config(&self) -> Option<&Config> {
        self.status.as_ref().ok()
    }

    pub fn error(&self) -> Option<&str> {
        self.status.as_ref().err().map(String::as_str)
    }

    /// `[meta].name`, read from whichever `Config` there is — a document that fails to
    /// validate still has a name, and the project bar should still show it.
    pub fn meta_name(&self) -> Option<&str> {
        self.config()
            .or(self.parsed.as_ref())
            .and_then(|config| config.meta.as_ref())
            .and_then(|meta| meta.name.as_deref())
    }

    /// The source the chain preview and the inspector are describing, if any.
    ///
    /// A stale index survives here rather than panicking: `selected_source` is adjusted on
    /// removal, but a screen holding one across a `clear_sources` would otherwise index a
    /// vector that has moved.
    pub fn selected_probe(&self) -> Option<&Source> {
        self.sources.get(self.selected_source?)
    }

    /// The source paths, in the order they were added. Both the plan and the run need them
    /// as a plain list.
    pub fn inputs(&self) -> Vec<PathBuf> {
        self.sources.iter().map(|source| source.path.clone()).collect()
    }

    /// Add input files, in order, skipping anything mint cannot decode and anything already
    /// held. Returns how many were refused as undecodable.
    ///
    /// The filter is `util::is_supported_input` and nothing else: a second copy of the
    /// extension list here is a second answer to "will mint accept this file?", which is
    /// what that function was made public to prevent. The count comes back rather than the
    /// caller re-walking the list with the same predicate, so the pane can say what it
    /// dropped without holding an opinion of its own about which files those are. A
    /// duplicate is *not* counted: the file is in the set, which is what the user wanted.
    pub fn add_inputs(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> usize {
        let mut added = false;
        let mut refused = 0;
        for path in paths {
            if !mint::util::is_supported_input(&path) {
                refused += 1;
                continue;
            }
            if self.sources.iter().any(|source| source.path == path) {
                continue;
            }
            self.sources.push(probe(path));
            added = true;
        }
        if !added {
            return refused;
        }
        if self.selected_source.is_none() {
            self.selected_source = Some(0);
        }
        self.mark_project_dirty();
        self.recollect_problems();
        refused
    }

    /// Drop one source. Indices are the `Scope::Source`/`Scope::Cell` keys in `problems`,
    /// so the selection and the problem list both have to be rebuilt around the hole.
    pub fn remove_source(&mut self, index: usize) {
        if index >= self.sources.len() {
            return;
        }
        self.sources.remove(index);
        self.selected_source = match self.selected_source {
            Some(selected) if selected == index => None,
            Some(selected) if selected > index => Some(selected - 1),
            other => other,
        };
        self.mark_project_dirty();
        self.recollect_problems();
    }

    pub fn clear_sources(&mut self) {
        self.sources.clear();
        self.selected_source = None;
        self.mark_project_dirty();
        self.recollect_problems();
    }

    /// Re-`stat` every source and re-probe only the ones that moved.
    ///
    /// Called on add/remove, on entering Plan mode, and when a run finishes: in an in-place
    /// workflow a render can land on top of a source, and a stale header is a plan that
    /// describes a file that no longer exists.
    pub fn refresh_sources(&mut self) {
        let mut changed = false;
        for source in &mut self.sources {
            let (bytes, mtime) = stat(&source.path);
            if bytes == source.file_bytes && mtime == source.mtime {
                continue;
            }
            let path = std::mem::take(&mut source.path);
            *source = probe(path);
            changed = true;
        }
        if changed {
            self.recollect_problems();
        }
    }

    /// The targets a run would actually use: every *included* name that resolves, in
    /// document order, with the run-scoped `--force` applied.
    ///
    /// This mirrors `cli.rs::resolve_selected_targets` step for step, including the one
    /// permitted deviation from "the GUI never constructs a `ResolvedTarget`": mutating
    /// `overwrite` on a target the library handed over is what `--force` *is*, and the CLI
    /// does exactly this. Names that do not resolve are skipped rather than reported here —
    /// they are already in `problems`, which is the surface that owns saying so.
    pub fn resolved_targets(&self) -> Vec<ResolvedTarget> {
        let Some(config) = self.config() else { return Vec::new() };
        self.included_names()
            .iter()
            .filter_map(|name| config.resolve_target(name).ok())
            .map(|mut target| {
                if self.run_options.force {
                    target.overwrite = true;
                }
                target
            })
            .collect()
    }

    /// Apply an edit to the document and re-validate. Every field edit in the form goes
    /// through here, so the write-back path is exercised by ordinary use rather than only
    /// by a Save button that might be tested less.
    pub fn edit(&mut self, apply: impl FnOnce(&mut ConfigDocument) -> anyhow::Result<()>) {
        let Some(doc) = self.doc.as_mut() else { return };
        if apply(doc).is_ok() {
            self.text = doc.text();
            self.dirty = true;
            self.revalidate();
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        std::fs::write(&self.path, &self.text)?;
        self.dirty = false;
        Ok(())
    }

    /// Take an external edit if it is safe to, and say what happened.
    ///
    /// Called when [`crate::watch::ConfigWatcher`] reports the file moved. The three answers
    /// are the three real situations, and the middle one is deliberately *not* automatic:
    ///
    /// - the bytes match the buffer — which is what our own [`Self::save`] looks like from
    ///   the outside, and the reason a self-write does not reload the document under the
    ///   cursor;
    /// - this window has no unsaved edits, so the disk is simply the newer truth: taken, with
    ///   a toast (proposal §5.2 — external edits are the *normal* case, not an incident);
    /// - this window has unsaved edits, so both versions are somebody's work. Nothing is
    ///   taken and nothing is overwritten. §4 is explicit: *show both, never silently pick.*
    ///
    /// A read that fails is reported as [`ExternalChange::None`] on purpose. A rename-based
    /// save has a window in which the path does not exist yet, and the rename itself is
    /// another event — so waiting for the next one is right, and panicking or blanking the
    /// buffer over a file that is mid-replacement is not. The GUI never holds the config
    /// hostage: whatever happens here, the text in the editor is still the user's.
    pub fn absorb_external_change(&mut self) -> ExternalChange {
        let Ok(disk) = std::fs::read_to_string(&self.path) else { return ExternalChange::None };
        if disk == self.text {
            return ExternalChange::None;
        }
        if self.dirty {
            return ExternalChange::Conflict;
        }
        self.text = disk;
        self.reload_state();
        ExternalChange::Reloaded
    }

    /// Throw away the buffer and take what is on disk — the conflict's "take theirs".
    pub fn discard_edits_and_reload(&mut self) -> anyhow::Result<()> {
        self.text = std::fs::read_to_string(&self.path)?;
        self.dirty = false;
        self.reload_state();
        Ok(())
    }

    /// Re-derive everything from a document that arrived from outside.
    ///
    /// Unlike an edit made here, an external one can rename or delete a target, and every
    /// screen keyed on `selected` would then be pointing at a name the document no longer
    /// defines. Falling back to the first target is what opening the file fresh would do.
    fn reload_state(&mut self) {
        self.revalidate();
        let names = self.target_names();
        if !self.selected.as_ref().is_some_and(|name| names.contains(name)) {
            self.selected = names.first().cloned();
        }
    }

    /// Mark the sidecar as needing a write. The write itself is the shell's business
    /// ([`Self::flush_project`]), because a `fsync` per checkbox is still too often.
    pub fn mark_project_dirty(&mut self) {
        self.project_dirty = true;
    }

    pub fn project_dirty(&self) -> bool {
        self.project_dirty
    }

    /// Write the sidecar if anything changed. Returns mint-gui's own error on failure —
    /// a history or sidecar write that fails is a surfaced warning, never a lost run.
    pub fn flush_project(&mut self) -> anyhow::Result<()> {
        if !self.project_dirty {
            return Ok(());
        }
        self.project_dirty = false;
        crate::history::save_project(
            &self.path,
            &crate::history::Project {
                inputs: self.inputs(),
                included: self.included.clone(),
                run_options: self.run_options.clone(),
            },
        )
    }

    /// The CLI line that reproduces what is on screen, in the CLI's own flag spelling.
    ///
    /// Nearly free, and it is what keeps a mixed CLI/GUI workflow reconciled (proposal
    /// §5.3): whatever the GUI is about to do, this is the command that does the same thing
    /// from a terminal, on a build machine, or in a script.
    pub fn cli_command(&self) -> String {
        let mut line = format!("mint --config {}", quoted(&self.path.display().to_string()));

        // Only when the selection is a strict subset — the CLI's default is every target,
        // and spelling all of them out would drift the moment one is added to the document.
        let included = self.included_names();
        if included.len() < self.target_names().len() {
            for name in &included {
                line.push_str(&format!(" --target {}", quoted(name)));
            }
        }
        if let Some(seed) = self.run_options.seed {
            line.push_str(&format!(" --seed {seed}"));
        }
        if let Some(jobs) = self.run_options.jobs {
            line.push_str(&format!(" --jobs {jobs}"));
        }
        if self.run_options.force {
            line.push_str(" --force");
        }
        if self.run_options.fail_fast {
            // `--fail-fast`, not `--fail_fast`: clap derives the long name by kebab-casing
            // the field, and this is the one field in `Args` whose name has two words in it.
            line.push_str(" --fail-fast");
        }

        if self.sources.is_empty() {
            line.push_str(" \"masters/*.wav\"");
        }
        for source in &self.sources {
            line.push_str(&format!(" {}", quoted(&source.path.display().to_string())));
        }
        line
    }
}

/// Read one file's header and the two numbers the cache is keyed on.
fn probe(path: PathBuf) -> Source {
    let (file_bytes, mtime) = stat(&path);
    let probe = io_read::probe_source(&path).map_err(|err| format!("{err:#}"));
    Source { path, probe, file_bytes, mtime }
}

fn stat(path: &Path) -> (Option<u64>, Option<SystemTime>) {
    match std::fs::metadata(path) {
        Ok(meta) => (Some(meta.len()), meta.modified().ok()),
        Err(_) => (None, None),
    }
}

/// Shell-quote a path or name that would otherwise word-split. Only spaces are handled,
/// deliberately: this line is for copying into a terminal and reading, not for `eval`.
fn quoted(text: &str) -> String {
    if text.contains(' ') { format!("\"{text}\"") } else { text.to_string() }
}

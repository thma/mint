//! Run persistence and the project sidecar — the two files the GUI owns, both beside the
//! config and neither of them inside it.
//!
//! Both live under `<config dir>/.mint-gui/<config stem>/`, and the stem is in the path
//! because these files are the *config's* and not the directory's. Two configs in one
//! project folder — `mint.toml` beside `mint-alt.toml`, which is an ordinary way to keep a
//! second set of deliverables — are two input sets, two inclusion sets and two histories;
//! one shared `project.toml` would have whichever was opened last silently restore the
//! other one's masters and its ticked target names, and one shared `runs/` would list both
//! configs' renders with nothing to tell them apart.
//!
//! ## `.mint-gui/<config stem>/runs/<ISO-8601 local>.json`
//!
//! [`persist`] writes `json_report::render(...)`'s output **byte for byte**. The GUI emits
//! the `--json` schema; it does not own it, does not add a key to it, and does not pretty-
//! print it differently. A run persisted from the window and the same run piped from
//! `mint --json` must be the same document, because the whole value of keeping them is that
//! the existing tooling — `jq`, a QC script, the next version of mint — already reads them.
//!
//! [`list`] reads a file's header back with `serde_json::Value` and checks
//! `schema_version` against [`mint::json_report::SCHEMA_VERSION`]. A file from another
//! schema is **listed and said to be from another schema**, never summarized: `SCHEMA_VERSION`
//! is bumped exactly when a key was removed, renamed, or changed meaning, so counting
//! `tasks` in a document from a different version is counting something that may not be
//! what it is called. That check is the entire reason the version field exists.
//!
//! Reading a run *back into the Report view* is deliberately not offered. It would need a
//! `Deserialize` twin of `json_report`'s DTOs, i.e. a second description of the schema
//! living in this crate — the thing the byte-for-byte rule above exists to avoid. The list
//! is a record of what was rendered and where the file is; `jq` reads the rest.
//!
//! ## `.mint-gui/<config stem>/project.toml`
//!
//! The sidecar holds **only what mint can be told on the command line**: the input paths,
//! the included target set, and the last-used `seed` / `jobs` / `force` / `fail_fast`. It is
//! written and read with `toml_edit`, like every other TOML this crate touches.
//!
//! Note what that is *not* a claim of. One of those four does reach the DSP: a restored
//! `seed` pins the next render's dither, exactly as `mint --seed` does, so `mint --config
//! ../mint.toml` with no `--seed` produces different bits. It is persisted anyway because
//! losing a pinned seed between sessions is the worse failure — and it is not a hidden
//! setting: the project bar shows it beside the checkbox that pins it, and "Copy as CLI"
//! spells it out in the line that reproduces the run.
//!
//! It is a separate file rather than a section of `mint.toml` because §5.3 is explicit
//! about why: `RawTarget` is `#[serde(deny_unknown_fields)]`, so a GUI-only key in the
//! config would make an older mint binary hard-error on a config the newer GUI wrote. The
//! config stays a mint config.
//!
//! Note which rule does *not* apply here. "Never re-serialize the document" is about
//! `mint.toml`, whose comments are the onboarding documentation; this file is written by a
//! machine for a machine, so [`save_project`] states every key it owns and lets `toml_edit`
//! keep whatever else is in the document.
//!
//! Both directories are created lazily, and a failure to write either one is a warning the
//! shell surfaces — never a lost run and never a modal in the way of the next render.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::{Context, Result};
use egui::{RichText, Ui};
use mint::json_report;
use mint::pipeline::{ProcessingReport, TaskFailure};
use serde_json::Value;
use toml_edit::{Array, DocumentMut, value};

use crate::colors;
use crate::state::{AppState, RunOptions};

/// What a run file says about itself, when it is from a schema this build understands.
#[derive(Clone, Copy, Debug)]
pub struct RunSummary {
    pub tasks: usize,
    pub failures: usize,
}

/// One entry in the runs directory.
#[derive(Clone)]
pub struct RunFile {
    pub path: PathBuf,
    /// Local civil time, as the filename spells it.
    pub label: String,
    /// The document's own `schema_version`; `None` when the file is not readable JSON.
    pub schema_version: Option<u32>,
    /// `Some` **only** when `schema_version` equals `json_report::SCHEMA_VERSION`. A
    /// mismatch is reported as a mismatch rather than counted anyway.
    pub summary: Option<RunSummary>,
    /// Cache key, with the path. Private: it exists so [`list_reusing`] can tell an entry it
    /// has already parsed from one that moved, and nothing on screen reads it.
    mtime: Option<SystemTime>,
}

/// Everything the sidecar remembers — all of it something `mint` itself can be told on the
/// command line. `run_options.seed` is the one entry that reaches the DSP; see the module
/// doc for why it is kept anyway.
pub struct Project {
    pub inputs: Vec<PathBuf>,
    pub included: BTreeSet<String>,
    pub run_options: RunOptions,
}

/// The sidecar's file name, under [`sidecar_dir`].
const PROJECT_FILE: &str = "project.toml";

/// What the sidecar says it is, for whoever finds it in a delivery folder. Written only
/// when the file is created; an existing document keeps whatever is already at its top.
const PROJECT_HEADER: &str = "\
# Written by mint-gui: which masters were open, which targets were ticked, and the run
# flags last used. Not a mint config — mint never reads this file, and there is nothing in
# it mint cannot be told on the command line. One key does reach the DSP: `seed` pins the
# dither exactly as `mint --seed` does, so deleting it makes the next render unpinned.
# Otherwise safe to delete.
";

/// Write one batch's reports and failures next to the config. Returns the file written.
pub fn persist(
    config_path: &Path,
    meta_name: Option<&str>,
    reports: &[ProcessingReport],
    failures: &[TaskFailure],
) -> Result<PathBuf> {
    // The `--json` document itself, with the same `config` name the CLI puts in it. Rendered
    // before the directory is created so a serialization failure leaves no empty folder.
    let json = json_report::render(meta_name, reports, failures)?;

    let dir = runs_dir(config_path);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not create {}", dir.display()))?;
    let path = unique_path(&dir, &stamp());

    // Through `atomic_write` rather than `fs::write`: a half-written history file looks
    // like a run that half happened, and the library already owns "stage beside it, rename".
    mint::audio::atomic_write::write(&path, json.as_bytes())?;
    Ok(path)
}

/// The run files beside `config_path`, newest first. Never errors: a missing or unreadable
/// history directory is an empty list, not a failure to show the app.
pub fn list(config_path: &Path) -> Vec<RunFile> {
    list_reusing(config_path, &[])
}

/// The same listing, keeping whatever `previous` already parsed.
///
/// The directory only grows — one file per batch, nothing prunes it — and a run file is
/// proportional to the batch it records, so a `read_to_string` plus a `serde_json` parse of
/// *every* file is a cost proportional to the whole history rather than to what changed. The
/// cache key is path + mtime, the same key `AppState::sources` keys its probes on, and it
/// leaves this a plain directory scan: one `read_dir` and one `stat` per entry, which is
/// what the panel's staleness timer can afford to pay for noticing a run some other terminal
/// wrote. Only a file that is new or has moved is opened.
pub fn list_reusing(config_path: &Path, previous: &[RunFile]) -> Vec<RunFile> {
    let Ok(entries) = std::fs::read_dir(runs_dir(config_path)) else { return Vec::new() };
    let mut files: Vec<(Option<SystemTime>, PathBuf)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .map(|path| (path.metadata().and_then(|meta| meta.modified()).ok(), path))
        .collect();
    // Ordered by the file's own mtime rather than by its name: the name is a *civil second*
    // and two runs can share one, which is exactly when the collision suffix appears — and
    // `X-2.json` sorts *before* `X.json` byte-wise, i.e. backwards. The name is the
    // tie-break, so the order stays deterministic when a filesystem has no mtime to give.
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.file_name().cmp(&a.1.file_name())));

    let cached: HashMap<&Path, &RunFile> =
        previous.iter().map(|file| (file.path.as_path(), file)).collect();
    files
        .into_iter()
        .map(|(mtime, path)| {
            match cached.get(path.as_path()).filter(|file| file.mtime == mtime) {
                Some(file) => (*file).clone(),
                None => read_run_file(path, mtime),
            }
        })
        .collect()
}

/// Read the sidecar. `None` when there is none, or when it is unreadable — a corrupt
/// sidecar restores nothing and blocks nothing.
pub fn load_project(config_path: &Path) -> Option<Project> {
    let text = std::fs::read_to_string(sidecar_dir(config_path).join(PROJECT_FILE)).ok()?;
    let doc = text.parse::<DocumentMut>().ok()?;

    // Every key is optional and every malformed one is simply absent: this file restores a
    // convenience, so half of it is better than a dialog about the other half.
    Some(Project {
        inputs: strings(&doc, "inputs").into_iter().map(PathBuf::from).collect(),
        included: strings(&doc, "targets").into_iter().collect(),
        run_options: RunOptions {
            seed: integer(&doc, "seed").and_then(|n| u64::try_from(n).ok()),
            jobs: integer(&doc, "jobs").and_then(|n| usize::try_from(n).ok()),
            force: doc.get("force").and_then(|item| item.as_bool()).unwrap_or(false),
            fail_fast: doc.get("fail_fast").and_then(|item| item.as_bool()).unwrap_or(false),
        },
    })
}

pub fn save_project(config_path: &Path, project: &Project) -> Result<()> {
    let dir = sidecar_dir(config_path);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not create {}", dir.display()))?;
    let path = dir.join(PROJECT_FILE);

    // Edit whatever is there rather than starting fresh, so a hand-added note in this file
    // survives the next checkbox — and so the header written below is written once.
    let existing = std::fs::read_to_string(&path).ok().and_then(|text| text.parse().ok());
    let fresh = existing.is_none();
    let mut doc: DocumentMut = existing.unwrap_or_default();

    // A path TOML cannot hold is dropped rather than written lossily: a mangled path
    // restores as a file that does not exist, which is worse than not restoring it.
    doc["inputs"] = value(Array::from_iter(
        project.inputs.iter().filter_map(|path| path.to_str()),
    ));
    // Empty means "all targets", which is `AppState::included`'s own convention and the
    // CLI's default — an empty array here restores as an empty set, i.e. all of them.
    doc["targets"] = value(Array::from_iter(project.included.iter().map(String::as_str)));

    // `None` means unpinned, and the way to spell that is an absent key. A seed too large
    // for TOML's i64 is dropped for the same reason a bad path is: restoring a *different*
    // seed would silently change what the next render produces.
    set_or_remove(&mut doc, "seed", project.run_options.seed.and_then(|n| i64::try_from(n).ok()));
    set_or_remove(&mut doc, "jobs", project.run_options.jobs.and_then(|n| i64::try_from(n).ok()));
    doc["force"] = value(project.run_options.force);
    doc["fail_fast"] = value(project.run_options.fail_fast);

    // Prepended as text on the first write only. Parsed into the document it would be the
    // *trailing* decor of an empty table and would come back out underneath the keys it is
    // there to introduce; once the file exists it is the first key's prefix and survives
    // every later round trip on its own.
    let text = if fresh { format!("{PROJECT_HEADER}{doc}") } else { doc.to_string() };
    mint::audio::atomic_write::write(&path, text.as_bytes())
}

/// How stale the history list may get before the directory is read again.
///
/// This panel is drawn on every frame the Report mode is up, and a `read_dir` is not a
/// per-frame cost. What the timer must *not* also be paying for is a re-parse of the whole
/// history every two seconds, which is why the re-read goes through [`list_reusing`]: a scan
/// is one `stat` per entry, and only a file that moved is opened. The one event that must
/// show up immediately — this app finishing a batch — does not wait for the timer at all:
/// the shell calls [`invalidate`]. What is left for it is the case nothing here can be told
/// about, a run a CLI wrote in another terminal, and two seconds is short enough for that.
const LIST_MAX_AGE_SECONDS: f64 = 2.0;

/// Where the panel's listing lives between frames. Keyed on the config, because two windows
/// on two configs are two histories.
fn cache_id(config_path: &Path) -> egui::Id {
    egui::Id::new(("mint-gui-history", config_path))
}

/// Drop the cached listing, so the next draw re-reads the directory.
///
/// Called by the shell the moment a batch is persisted. Without it the freshly written run
/// appears whenever the staleness window happens to expire, which is a list that is right
/// eventually — and "eventually" is not a property a record of what you just rendered
/// should have.
pub fn invalidate(ctx: &egui::Context, config_path: &Path) {
    ctx.data_mut(|data| data.remove_temp::<(f64, Arc<Vec<RunFile>>)>(cache_id(config_path)));
}

/// The history list, beside the Report view.
pub fn ui(ui: &mut Ui, state: &AppState) {
    let mut refresh = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new("History").strong());
        refresh = ui
            .small_button("↻")
            .on_hover_text(runs_dir(&state.path).display().to_string())
            .clicked();
    });

    // Cached in egui's own frame-to-frame store: the panel is read-only, so nothing here
    // needs to live in `AppState`.
    let id = cache_id(&state.path);
    let now = ui.input(|input| input.time);
    let (_, files) = ui.data_mut(|data| {
        let entry: &mut (f64, Arc<Vec<RunFile>>) =
            data.get_temp_mut_or_insert_with(id, || (f64::NEG_INFINITY, Arc::new(Vec::new())));
        if refresh || now - entry.0 > LIST_MAX_AGE_SECONDS {
            let files = list_reusing(&state.path, &entry.1);
            *entry = (now, Arc::new(files));
        }
        entry.clone()
    });

    if files.is_empty() {
        ui.weak("no runs yet — every render writes one beside the config, under .mint-gui/");
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for file in files.iter() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&file.label).monospace())
                    .on_hover_text(file.path.display().to_string());
                match (&file.summary, file.schema_version) {
                    (Some(summary), _) => {
                        ui.weak(format!("{} task(s)", summary.tasks));
                        if summary.failures > 0 {
                            ui.colored_label(colors::BAD, format!("✖ {}", summary.failures));
                        }
                    }
                    // The refusal, said out loud. This is the case the version field exists
                    // for, and a silent blank here would make it look like an empty run.
                    (None, Some(version)) => {
                        ui.colored_label(
                            colors::WARN,
                            format!(
                                "⚠ schema {version}, this build reads {}",
                                json_report::SCHEMA_VERSION
                            ),
                        )
                        .on_hover_text(
                            "a key may have been removed, renamed or changed meaning between \
                             the two, so counting its tasks would be counting something else",
                        );
                    }
                    (None, None) => {
                        ui.colored_label(colors::WARN, "⚠ not a readable run report");
                    }
                }
            });
        }
        // Says the quiet part: these rows do not open. Loading one back would need a
        // `Deserialize` twin of the `--json` schema living in this crate (see the module
        // doc), so the file is the artefact and `jq` is the reader.
        ui.weak("the run files are the --json document; read one with jq");
    });
}

/// `<config dir>/.mint-gui/<config stem>/runs`. One producer, so `persist` and `list` cannot
/// look in two different places.
pub fn runs_dir(config_path: &Path) -> PathBuf {
    sidecar_dir(config_path).join("runs")
}

/// `<config dir>/.mint-gui/<config stem>`.
///
/// The stem is what keeps two configs in one folder apart — see the module doc. A path with
/// no stem at all (`.`, `..`) falls back to `mint`, which is the name the CLI writes its
/// bootstrap config under, so the degenerate case lands somewhere nameable rather than in a
/// directory called nothing.
pub fn sidecar_dir(config_path: &Path) -> PathBuf {
    let stem = config_path.file_stem().unwrap_or_else(|| OsStr::new("mint"));
    config_path.parent().unwrap_or(Path::new(".")).join(".mint-gui").join(stem)
}

/// Local civil time, ISO-8601 with `-` where the time would use `:`.
///
/// The colons are the only deviation and they are not negotiable: a colon in a file name is
/// legal on APFS but is the path separator in the Finder's own display, and it is illegal on
/// Windows. `jiff` does the civil-from-timestamp conversion and the timezone lookup —
/// hand-rolling either is the second implementation this codebase refuses everywhere else.
fn stamp() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%dT%H-%M-%S").to_string()
}

/// `<dir>/<stem>.json`, or `<stem>-2.json` and up if that is taken.
///
/// Two batches can finish inside one second — a handful of short files, or a test — and the
/// rule here is that a run is never lost, which an overwrite would break. The loop is
/// bounded by the number of files that can share one second.
fn unique_path(dir: &Path, stem: &str) -> PathBuf {
    let first = dir.join(format!("{stem}.json"));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}.json")))
        .find(|candidate| !candidate.exists())
        .expect("the range is unbounded")
}

/// One run file, and what it says about itself. The `mtime` is passed in rather than
/// re-`stat`ed, so the key stored is the one the listing compared against.
fn read_run_file(path: PathBuf, mtime: Option<SystemTime>) -> RunFile {
    let label = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let document = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let schema_version = document
        .as_ref()
        .and_then(|document| document.get("schema_version"))
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok());

    // Summarized only on an exact version match — see the module doc. Anything else is
    // listed with its version and left uncounted.
    let summary = document
        .as_ref()
        .filter(|_| schema_version == Some(json_report::SCHEMA_VERSION))
        .map(|document| RunSummary {
            tasks: array_len(document, "tasks"),
            failures: array_len(document, "failures"),
        });

    RunFile { path, label, schema_version, summary, mtime }
}

fn array_len(document: &Value, key: &str) -> usize {
    document.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

/// A TOML array of strings, or nothing. Non-string entries are skipped rather than
/// stringified — this file is machine-written, so anything else in it is not ours.
fn strings(doc: &DocumentMut, key: &str) -> Vec<String> {
    doc.get(key)
        .and_then(|item| item.as_array())
        .map(|array| array.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
        .unwrap_or_default()
}

fn integer(doc: &DocumentMut, key: &str) -> Option<i64> {
    doc.get(key)?.as_integer()
}

/// Write a key, or remove it when there is nothing to write. An absent key is how this file
/// spells `None`, so leaving a stale value behind would turn "unpinned" back into a number.
fn set_or_remove(doc: &mut DocumentMut, key: &str, number: Option<i64>) {
    match number {
        Some(number) => doc[key] = value(number),
        None => {
            doc.remove(key);
        }
    }
}

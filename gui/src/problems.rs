//! Everything mint knows is wrong, at once — the compiler-style list.
//!
//! Every check underneath is a `bail!`: `Config::validate` stops at the first target that
//! will not resolve, `check_collisions` at the first colliding pair, `check_planned_outputs`
//! at the first clash. That is right for a CLI which is about to exit, and wrong for a
//! person editing a config, who then fixes one thing, re-runs, and learns about the next
//! one. Closing that gap is the highest value-per-effort item in the whole proposal (§12).
//!
//! It is closed **only by asking the library narrower questions**, never by reimplementing a
//! check. Every message in this list is mint's own bytes: `resolve_target` for one name,
//! `check_collisions` for one pair, `check_planned_outputs` for one candidate,
//! `delivery_blocker` for one cell, `Conflict::message` for one path. Where a rule needs
//! decomposing, the decomposition is over the *inputs*, never over the rule.
//!
//! For the same reason there are **no synthesized fix buttons**. The wireframe sketches
//! "set rate = 96000 on flac"; producing that means parsing mint's prose to guess what it
//! wanted, which is rule 4 (never paraphrase a diagnostic) wearing a hat. [`Fix`] therefore
//! only ever *navigates* — select the target, select the cell — with the single exception of
//! `EnableForce`, which is not a guess: `overwrite` is the one thing `Conflict::message`
//! itself names as the remedy.
//!
//! ## The enumeration
//!
//! In order, each step answerable only if the one before it succeeded:
//!
//! 1. TOML syntax — `toml_edit::DocumentMut`'s parse error and its `span()`.
//! 2. Deserialize — `Config::parse_toml_str` (where `deny_unknown_fields` surfaces a typo).
//! 3. Per-target resolution — `resolve_target` for **every** name, so N bad targets are N
//!    problems.
//! 4. Target-vs-target collisions — `check_collisions` per unordered pair.
//! 5. Whole-config catch-all — `validate()`, added only if 3–4 found nothing (this is how
//!    "config must define at least one [target.NAME] entry" reaches the list).
//! 6. Per-source probe failures, from the cache.
//! 7. Per-cell delivery blockers — `delivery_blocker(rate, channels)` per (source × target).
//! 8. Batch output collisions — an accept-list walk over `check_planned_outputs`, asked
//!    about the plan [`plan_cells`] builds, which is `cli.rs::plan_outputs`' set exactly.
//! 9. Output conflicts — `output_conflict` per cell, rendered by `Conflict::message`.
//!
//! Steps 7–9 all read one list, and it is a **public** one: `run.rs`'s pre-flight asks
//! `check_planned_outputs` about the same plan this panel does, because two independent
//! walks of the same cross product is how a runner ends up executing a batch its own
//! Problems panel called clean.
//!
//! Steps 1 and 2 are terminal: a document that does not parse has no targets to ask about,
//! and a list that guessed past the parse error would be guessing. That includes the steps
//! which do not strictly need a `Config` — a probe failure is a fact about a file, not about
//! the document — because a compiler that cannot parse does not go on to run borrowck, and
//! those problems reappear unchanged the moment the syntax is fixed.
//!
//! ## Cost
//!
//! Two steps are quadratic — 4 in *targets* (every unordered pair) and 8 in *cells* — and
//! two touch the filesystem: 8 asks `current_dir` and re-hashes its accept list per call, 9
//! does one `symlink_metadata` per cell. Measured against the real `examples/mint.toml`
//! (11 targets), one `collect`, **debug build** (a release build is roughly an order of
//! magnitude faster):
//!
//! | scenario | cells | `collect` |
//! |---|---|---|
//! | document only, no sources | 0 | 0.92 ms |
//! | 4 sources | 44 | 1.06 ms |
//! | 20 sources | 220 | 2.27 ms |
//! | 21 sources, two of them sharing a stem | 231 | **125 ms** |
//!
//! The first three are comfortable, which matters because `AppState::revalidate` recollects
//! and `editor.rs` revalidates per keystroke. At that size the money does not go where the
//! shape suggests: for 220 cells `delivery_blocker` costs 2 µs in total, `output_conflict`
//! 100 µs and `render_output_path` 460 µs, while the two *fixed* costs — 368 µs re-parsing
//! the document for step 1, 286 µs for step 4's 55 target pairs — are most of the document-
//! only figure. (That re-parse is the one duplicated piece of work in here: `revalidate` has
//! just parsed the same text into `AppState::doc`, and `Query` hands over the text rather
//! than the result. Worth a `Query` field if the fixed cost ever matters.)
//!
//! The last row is the honest bad news and it is step 8's accept-list walk: 116 ms of the
//! 125, against 1.0 ms to ask about the same batch once. It runs **only** when the batch
//! really does have a colliding output — the one-shot call in front of it is what keeps the
//! other three rows flat — but while the collision is unfixed, every keypress in the config
//! drawer pays it. Two ways out if it ever bites, neither of them a cache in here (this
//! function is pure, and a cache keyed on "something moved" is the staleness bug `editor.rs`
//! already refuses): chunk the walk, so only the chunk containing an offender is examined
//! candidate by candidate; or split the invalidation in `state.rs`, since steps 1–5 depend
//! only on the text and steps 6–9 only on the sources, the inclusions and `force`, and
//! neither half needs recollecting when the other one changes. Both are deferred rather than
//! taken: the second is not this module's to make, and the first buys nothing until someone
//! is actually sitting in the broken state.

use std::path::PathBuf;

use egui::{RichText, Ui};
use mint::config::{self, Config, DeliveryBlocker, PlannedOutput, ResolvedTarget};
use mint::diagnostics::FailureKind;
use mint::pipeline::{self};

use crate::colors;
use crate::state::{AppState, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// The run will not happen, or this target will not.
    Error,
    /// The run will fail on this cell, but the remedy is a checkbox rather than an edit.
    Warning,
}

/// What the problem is about, and therefore what clicking it should select.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Document,
    Target(String),
    /// Index into `AppState::sources`.
    Source(usize),
    Cell {
        source: usize,
        target: String,
    },
}

/// The only three things a row may offer to do. All navigation but one, and that one is
/// named by mint's own message rather than inferred from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    EnableForce,
    SelectTarget(String),
    SelectCell { source: usize, target: String },
}

#[derive(Clone, Debug)]
pub struct Problem {
    pub severity: Severity,
    pub scope: Scope,
    /// mint's own text, verbatim. Never reworded, never truncated on the way in.
    pub message: String,
    /// mint's own verdict on *what kind* of fault this is, when the row came from a real
    /// `anyhow::Error` and [`FailureKind::classify`] could read its chain. `None` for the
    /// rows this panel assembles from a *query* rather than a failure — `output_conflict`
    /// hands back a `Conflict` directly, and there is no error to classify.
    ///
    /// It exists so a reader does not have to recover the kind by elimination over
    /// `severity` and `scope`, which is what `plan.rs` used to do to find the batch
    /// collisions: correct only for as long as no other step produces a cell-scoped error.
    pub kind: Option<FailureKind>,
    pub fix: Option<Fix>,
}

/// Everything the collector is allowed to look at.
///
/// Explicit rather than `&AppState` so the list of inputs *is* the signature: a step that
/// needs something not in here needs a conversation first. Named fields rather than six
/// positional arguments because two of them are `&[String]` and a swap would compile.
pub struct Query<'a> {
    /// The document text. Step 1 is the only step that can run when nothing else parses.
    pub text: &'a str,
    /// `Config::parse_toml_str`'s output — parsed, *not* validated. `None` when even the
    /// deserialize failed, which is when steps 3–5 have nothing left to ask.
    pub parsed: Option<&'a Config>,
    /// Every `[target.NAME]` in document order. Step 3 asks about all of them, including
    /// the ones excluded from the run: a target that cannot resolve is broken whether or
    /// not it is ticked.
    pub names: &'a [String],
    /// The subset that participates in a run, with `AppState`'s "empty == all" convention
    /// already applied. Steps 7–9 are about cells, and an excluded target has no cells.
    pub included: &'a [String],
    pub sources: &'a [Source],
    /// `RunOptions::force`. Step 9 must ask `output_conflict` with the `overwrite` the run
    /// would really use, which is `target.overwrite || force` — the same widening
    /// `cli.rs::resolve_selected_targets` applies.
    pub force: bool,
}

/// Run the enumeration. Pure: it reads headers and file metadata, never audio, and never
/// writes anything.
pub fn collect(query: Query<'_>) -> Vec<Problem> {
    let mut problems = Vec::new();

    // 1. Syntax. toml_edit's error rather than the `toml` crate's, because it is the one
    // that carries a `span()` — and it is the same producer `editor.rs` underlines from, so
    // the panel and the squiggle cannot end up pointing at different bytes. The span is not
    // carried on `Problem` (there is nowhere for it to go and nothing else would read it);
    // `TomlError`'s own `Display` already names the line and column, in mint's dependency's
    // words rather than ours.
    if let Err(err) = query.text.parse::<toml_edit::DocumentMut>() {
        problems.push(document(err.to_string()));
        return problems;
    }

    // 2. Deserialize — `deny_unknown_fields`, a bad enum spelling, a string where a number
    // belongs. `parsed` is `None` exactly when this errs, and it is the *message* that is
    // wanted, so the question is asked again rather than reconstructed from the absence.
    let Some(parsed) = query.parsed else {
        if let Err(err) = Config::parse_toml_str(query.text) {
            problems.push(document(format!("{err:#}")));
        }
        return problems;
    };

    // 3. One question per target, so N broken targets are N rows. `validate()` asks the
    // same thing in a loop and keeps only the first answer.
    let mut resolved: Vec<ResolvedTarget> = Vec::with_capacity(query.names.len());
    for name in query.names {
        match parsed.resolve_target(name) {
            Ok(target) => resolved.push(target),
            Err(err) => problems.push(Problem {
                severity: Severity::Error,
                scope: Scope::Target(name.clone()),
                message: format!("{err:#}"),
                kind: Some(FailureKind::classify(&err)),
                fix: Some(Fix::SelectTarget(name.clone())),
            }),
        }
    }

    // 4. Collisions, one unordered pair at a time.
    //
    // Pairwise is the correct decomposition rather than a convenient one: `check_collisions`
    // compares targets *two at a time* internally and bails on the first hit, so handing it
    // exactly one pair asks precisely the question it already answers — no rule is restated
    // here, only the input is narrowed. Every pair therefore reports itself instead of the
    // first one hiding the rest, and the prose stays mint's.
    //
    // Quadratic, and affordable: `resolved` is the number of `[target.NAME]` tables in one
    // config (11 in `examples/mint.toml`), and this runs on change rather than per frame.
    for (index, first) in resolved.iter().enumerate() {
        for second in &resolved[index + 1..] {
            if let Err(err) = config::check_collisions(&[first.clone(), second.clone()]) {
                // Scoped to the second of the pair — the message names both, and the second
                // is the one whose arrival created the clash.
                problems.push(Problem {
                    severity: Severity::Error,
                    scope: Scope::Target(second.name.clone()),
                    message: format!("{err:#}"),
                    kind: Some(FailureKind::classify(&err)),
                    fix: Some(Fix::SelectTarget(second.name.clone())),
                });
            }
        }
    }

    // 5. Whatever `validate()` knows that steps 3 and 4 cannot ask about — today that is
    // "config must define at least one [target.NAME] entry", which is a fact about the
    // document rather than about any target in it. Only when nothing above fired, because
    // `validate()` is the union of steps 3 and 4 and would otherwise repeat one of them.
    if problems.is_empty()
        && let Err(err) = parsed.validate()
    {
        problems.push(document(format!("{err:#}")));
    }

    // 6. A header mint could not read. One task's failure, not the batch's — the file keeps
    // its place in the set (`cli.rs`'s `ProbedInput`), so this is a row, not a removal.
    // `Fix` has no `SelectSource`, so the row navigates nowhere; the Sources pane shows the
    // same message against the same file.
    for (index, source) in query.sources.iter().enumerate() {
        if let Err(err) = &source.probe {
            problems.push(Problem {
                severity: Severity::Error,
                scope: Scope::Source(index),
                message: err.clone(),
                // The probe failure was flattened to a `String` when the source was cached,
                // so there is no chain left to classify. Honest `None` rather than a guessed
                // `Other`.
                kind: None,
                fix: None,
            });
        }
    }

    // The cells a run would really attempt: included targets that resolve, crossed with
    // sources whose header could be read. Built once by the shared planner and walked three
    // times (7, 8, 9), so `render_output_path` is asked once per cell, the list stays in
    // enumeration order, and — the part that matters — `run.rs`'s pre-flight asks the
    // batch-level question about *this* set rather than about one it built itself.
    let targets: Vec<&ResolvedTarget> = resolved
        .iter()
        .filter(|target| query.included.contains(&target.name))
        .collect();
    let cells = plan_cells(query.sources, &targets);

    // 7. What the config alone cannot answer: FLAC above 96 kHz, DSD above 5.1. It takes the
    // *input* to ask, which is why this is per cell and not per target. A blocked cell gets
    // one row, not two: the missing output path of a target that will refuse the input
    // anyway is a consequence, not a second fault.
    for planned in &cells {
        match (&planned.blocked, &planned.output) {
            (Some(blocker), _) => {
                problems.push(cell(
                    Severity::Error,
                    planned.source,
                    &planned.target.name,
                    blocker.to_string(),
                ));
            }
            (None, Err(err)) => {
                problems.push(cell(
                    Severity::Error,
                    planned.source,
                    &planned.target.name,
                    err.clone(),
                ));
            }
            (None, Ok(_)) => {}
        }
    }

    // 8. Two jobs fighting over one path — the batch-level question `pipeline::process`
    // structurally cannot see, and the one that also catches an output landing on a
    // *different* input of the same run.
    //
    // The trick: `check_planned_outputs` bails on the first clash in the slice it is given,
    // so it is fed an **accept list** that grows one candidate at a time. The invariant is
    // that `accepted` has always just been approved as a whole, so a failure after pushing
    // one more entry is attributable to that entry alone — which is what licenses attaching
    // mint's own message to this cell. The offender is then popped back off, so one bad cell
    // does not make every later cell look broken.
    //
    // It terminates because the loop is over a fixed, finite `cells` and neither branch
    // touches the iteration: each pass pushes exactly once and pops at most once, so
    // `accepted` never exceeds `cells.len()` and no candidate is ever re-queued.
    //
    // The one thing it gives up: if a cell is rejected, a later cell that would have clashed
    // only with the rejected one is accepted silently. The pair is still represented in the
    // list — by the rejected cell's row, whose message names both sides.
    //
    // And it is expensive — quadratic in *cells*, not in targets, because every call rebuilds
    // the whole hash map: 116 ms for 231 cells against 1.0 ms to ask about the same batch
    // once (the module doc's table). So the batch is asked once first, exactly as `cli.rs`
    // asks it, and the walk only runs when that says there is something to enumerate. The
    // short cut rests on the same property as the walk itself — the check is a conjunction of
    // per-entry and per-pair predicates, so a batch that passes as a whole has no failing
    // subset — and a rule that broke that assumption would already have broken the walk's
    // attribution.
    let inputs: Vec<PathBuf> = query.sources.iter().map(|source| source.path.clone()).collect();
    let planned_batch = planned_outputs(query.sources, &cells);
    // Which cells the batch check threw out, so step 9 knows not to offer a one-click force
    // on top of one. A `Vec`, not a set: this only ever holds the offenders, and there is
    // one lookup per cell against a list that is empty in every healthy batch.
    let mut refused: Vec<(usize, &str)> = Vec::new();
    if config::check_planned_outputs(&planned_batch, &inputs).is_err() {
        let mut accepted: Vec<PlannedOutput<'_>> = Vec::with_capacity(planned_batch.len());
        // `planned_outputs` keeps exactly the cells whose path is known, in order, so the
        // zip pairs each candidate with the cell it came from.
        let attributable = cells.iter().filter(|planned| planned.output.is_ok());
        for (candidate, planned) in planned_batch.into_iter().zip(attributable) {
            accepted.push(candidate);
            if let Err(err) = config::check_planned_outputs(&accepted, &inputs) {
                problems.push(cell_of_kind(
                    Severity::Error,
                    planned.source,
                    &planned.target.name,
                    format!("{err:#}"),
                    Some(FailureKind::classify(&err)),
                ));
                accepted.pop();
                refused.push((planned.source, planned.target.name.as_str()));
            }
        }
    }

    // 9. Something already at the output path. A warning rather than an error because the
    // remedy is a checkbox and not an edit — the run *will* fail on this cell, and
    // `Conflict::message` is the only thing that says so out loud. Blocked cells are skipped:
    // the target refuses that input outright, so what is sitting at a path it will never
    // write to is not the thing wrong with it.
    for planned in cells.iter().filter(|planned| planned.blocked.is_none()) {
        let Ok(output) = &planned.output else { continue };
        let input = &query.sources[planned.source].path;
        // The `overwrite` the run would really use: the target's own, widened by the
        // run-scoped `--force`, exactly as `cli.rs::resolve_selected_targets` widens it.
        let overwrite = planned.target.overwrite || query.force;
        let Some(conflict) = pipeline::output_conflict(input, output, overwrite) else {
            continue;
        };
        let select =
            Fix::SelectCell { source: planned.source, target: planned.target.name.clone() };
        let kind = FailureKind::OutputConflict(conflict);
        problems.push(Problem {
            severity: Severity::Warning,
            scope: Scope::Cell {
                source: planned.source,
                target: planned.target.name.clone(),
            },
            message: conflict.message(input, output),
            kind: Some(kind.clone()),
            // The remedy mint's own message names, and the only non-navigating fix in the
            // panel. Whether `--force` even *reaches* this conflict is `force_can_fix`'s
            // answer, not this panel's: writing over the master is refused under `--force`
            // too, and a button that promised otherwise would be offering a fix mint
            // declines to perform.
            //
            // The second condition is this panel's own, and is about a *different* fact: on
            // a cell step 8 has already refused, the file in the way belongs to this same
            // batch, `check_planned_outputs` rejects that even under `--force`, and the
            // button would sit one row under mint's refusal offering to overwrite a master
            // it will not overwrite anyway.
            fix: if kind.force_can_fix()
                && !refused.contains(&(planned.source, planned.target.name.as_str()))
            {
                Some(Fix::EnableForce)
            } else {
                Some(select)
            },
        });
    }

    problems
}

/// One (source × target) pair a run would attempt, and everything the batch-level questions
/// need from it.
///
/// Public because `run.rs` must ask `check_planned_outputs` about the *same* set this panel
/// asks about. Two independent walks of the cross product is how a runner comes to execute a
/// batch its own Problems panel called clean.
pub struct PlannedCell<'a> {
    /// Index into the `sources` slice — the same index `Scope::Source`/`Scope::Cell` use.
    pub source: usize,
    pub target: &'a ResolvedTarget,
    /// Which rule refused this pair, when one did. The typed refusal rather than its
    /// sentence: a caller that only prints gets the sentence from `Display`, and one that
    /// has to count the two rules apart or offer the right fix no longer reads prose to do
    /// it.
    pub blocked: Option<DeliveryBlocker>,
    /// `render_output_path`'s answer, or mint's own message for why there is none.
    pub output: Result<PathBuf, String>,
}

/// Every cell a run would attempt, in `cli.rs::plan_outputs`'s own order and with its own
/// skips.
///
/// An input whose header could not be read contributes nothing, exactly as `plan_outputs`
/// skips it: its delivery rate is unknown, so its output path is not computable and it
/// cannot be shown to collide with anything. It still becomes a job, and `process_with`
/// re-raises the probe error as that job's own failure.
pub fn plan_cells<'a>(
    sources: &'a [Source],
    targets: &[&'a ResolvedTarget],
) -> Vec<PlannedCell<'a>> {
    let mut cells = Vec::with_capacity(sources.len() * targets.len());
    for (index, source) in sources.iter().enumerate() {
        let Ok(probe) = &source.probe else { continue };
        for target in targets {
            cells.push(PlannedCell {
                source: index,
                target,
                blocked: target.delivery_blocker(probe.sample_rate, probe.channels),
                output: target
                    .render_output_path(&source.path, target.delivery_rate(probe.sample_rate))
                    .map_err(|err| format!("{err:#}")),
            });
        }
    }
    cells
}

/// The plan in the library's own shape, for `config::check_planned_outputs`.
///
/// **Blocked cells are in it**, and that is not an oversight: `cli.rs::plan_outputs` plans
/// every cell of every probed input, blocked or not, so a plan that quietly dropped them
/// would ask the batch question over a strict subset of the set the CLI asks it over — and
/// a batch mint refuses outright (two 192 kHz masters sharing a stem into a FLAC target)
/// would come back clean.
pub fn planned_outputs<'a>(
    sources: &'a [Source],
    cells: &'a [PlannedCell<'a>],
) -> Vec<PlannedOutput<'a>> {
    cells
        .iter()
        .filter_map(|planned| {
            Some(PlannedOutput {
                input: &sources[planned.source].path,
                target: &planned.target.name,
                output: planned.output.as_ref().ok()?.clone(),
            })
        })
        .collect()
}

fn document(message: String) -> Problem {
    Problem { severity: Severity::Error, scope: Scope::Document, message, kind: None, fix: None }
}

/// A cell-scoped problem, whose fix is always "show me". Steps 7 and 8 both produce these;
/// step 9 builds its own because `Conflict::Existing` is the one row that offers more.
fn cell(severity: Severity, source: usize, target: &str, message: String) -> Problem {
    cell_of_kind(severity, source, target, message, None)
}

/// [`cell`], for a row that came from a live error mint could classify.
fn cell_of_kind(
    severity: Severity,
    source: usize,
    target: &str,
    message: String,
    kind: Option<FailureKind>,
) -> Problem {
    Problem {
        severity,
        scope: Scope::Cell { source, target: target.to_string() },
        message,
        kind,
        fix: Some(Fix::SelectCell { source, target: target.to_string() }),
    }
}

/// How many of them are errors. One producer, so the drawer tab's count, the status bar's
/// verdict and the panel's own header cannot disagree.
pub fn error_count(problems: &[Problem]) -> usize {
    problems.iter().filter(|problem| problem.severity == Severity::Error).count()
}

/// Carry out a row's [`Fix`]. Beside the enum rather than inside the panel's `ui`, so that
/// what a variant *means* has one implementation — a second surface offering the same
/// click-through calls this rather than writing its own three-arm match.
pub fn apply(fix: &Fix, state: &mut AppState) {
    match fix {
        Fix::EnableForce => {
            state.run_options.force = true;
            state.mark_project_dirty();
            state.recollect_problems();
        }
        Fix::SelectTarget(name) => state.selected = Some(name.clone()),
        Fix::SelectCell { source, target } => {
            state.selected_source = Some(*source);
            state.selected = Some(target.clone());
        }
    }
}

/// The panel: severity glyph + scope chip + mint's message, one row each, click-through via
/// [`Fix`].
///
/// Laid out the way a compiler prints: the location and the action on one line, the
/// diagnostic itself on the next, in full. Truncating it to fit a row and hiding the rest
/// behind a tooltip would make the panel's own summary the thing on screen and mint's
/// message the thing you have to go looking for — which is the paraphrase rule losing on
/// points instead of losing outright.
pub fn ui(ui: &mut Ui, state: &mut AppState) {
    // The cache is lifted out for the length of the draw. A row needs a `&mut AppState` to
    // act on and the list lives inside it; taking it is what avoids cloning every message
    // once per frame. It goes back *before* any fix runs, because `apply` may recollect and
    // a restore afterwards would overwrite the fresh list with this frame's stale one.
    let problems = std::mem::take(&mut state.problems);
    let mut chosen: Option<Fix> = None;

    // The header counts, and says which kind. A warning is not an error and the two are
    // never added up into one number — a batch that will fail on nine cells and a document
    // that will not load are different situations.
    ui.horizontal_wrapped(|ui| {
        let errors = error_count(&problems);
        let warnings = problems.len() - errors;
        if problems.is_empty() {
            ui.colored_label(colors::OK, "✔");
            ui.weak("mint has no objection, and every planned job has a path to write to");
            return;
        }
        if errors > 0 {
            ui.colored_label(colors::BAD, format!("✖ {}", counted(errors, "error")));
        }
        if warnings > 0 {
            ui.colored_label(colors::WARN, format!("⚠ {}", counted(warnings, "warning")));
        }
    });

    egui::ScrollArea::vertical().id_salt("problems").auto_shrink([false, false]).show(ui, |ui| {
        for problem in &problems {
            ui.horizontal(|ui| {
                match problem.severity {
                    Severity::Error => ui.colored_label(colors::BAD, "✖"),
                    Severity::Warning => ui.colored_label(colors::WARN, "⚠"),
                };
                ui.label(RichText::new(chip(&problem.scope, &state.sources)).monospace().weak());
                if let Some(fix) = &problem.fix {
                    let (label, hint) = fix_button(fix);
                    if ui.small_button(label).on_hover_text(hint).clicked() {
                        chosen = Some(fix.clone());
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.add_space(18.0);
                // Monospace only when the message brought its own layout with it: a
                // `toml_edit` parse error draws a caret under the offending byte, and that
                // caret lines up in one font family and not the other.
                let text = RichText::new(problem.message.as_str());
                ui.label(if problem.message.contains('\n') { text.monospace() } else { text });
            });
            ui.add_space(4.0);
        }
    });

    state.problems = problems;
    if let Some(fix) = chosen {
        apply(&fix, state);
    }
}

/// Where the problem is, in one chip. A stale index (the sources moved between collection
/// and this frame) prints the index rather than panicking or silently blaming file 0.
fn chip(scope: &Scope, sources: &[Source]) -> String {
    let file = |index: usize| match sources.get(index) {
        Some(source) => source.path.file_name().unwrap_or_default().to_string_lossy().to_string(),
        None => format!("source #{index}"),
    };
    match scope {
        Scope::Document => "[document]".to_string(),
        Scope::Target(name) => format!("[{name}]"),
        Scope::Source(index) => format!("[{}]", file(*index)),
        Scope::Cell { source, target } => format!("[{} → {target}]", file(*source)),
    }
}

/// The button's label and its tooltip. `EnableForce`'s tooltip is [`crate::FORCE_HELP`] —
/// `cli.rs`'s own `--force` help text, shared with the project bar's checkbox because the
/// two tick the same flag and had better promise the same thing in the same words.
fn fix_button(fix: &Fix) -> (&'static str, &'static str) {
    match fix {
        Fix::EnableForce => ("Enable force", crate::FORCE_HELP),
        Fix::SelectTarget(_) => ("Show target", "Select this target in the Targets pane."),
        Fix::SelectCell { .. } => {
            ("Show cell", "Select this cell; the chain preview follows the selection.")
        }
    }
}

fn counted(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

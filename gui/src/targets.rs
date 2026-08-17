//! The Targets pane: one row per `[target.NAME]`, in the document's own order.
//!
//! Document order, not `Config::target_names`'s — that sorts, because `Config.targets` is a
//! `BTreeMap`, and the order the user wrote is the order that means something. The list
//! comes from `AppState::target_names`, which reads the `toml_edit` document.
//!
//! The one-line summary under each name (`44100 Hz · s16 · -12.0 LUFS · -0.1 dBTP · wav`) is
//! assembled **exclusively** from the library's own tag helpers — `format_tag`,
//! `effective_dither_tag`, `OutputCodec::ext`, `format_placeholder`, and `ResolvedTarget`'s
//! own fields. There is no spelling table in this file and there must never be one: a
//! hand-written "s16" is a second name for a value the config parser already names, and the
//! two drift the first time a variant is added. The DSD case is the proof that this is not
//! pedantry — a DSD target's summary has to read `dsd64 · 1bit · …`, which falls out of
//! `format_placeholder()` and would not fall out of `format_tag(target.format)`, whose
//! `format` field on a DSD target is an inherited value that nothing reads.
//!
//! What the line deliberately leaves out is as load-bearing as what it carries: `dither`,
//! `mp3_bitrate`, `bwf` and `quality` are all fields whose *applicability* depends on the
//! codec, and the library that knows which of them are inert on a given target is
//! `resolve_target_explained`, not `ResolvedTarget`. Deciding it from a `&ResolvedTarget`
//! here would be re-deriving a codec rule, so those figures stay on the surfaces that have
//! the explanation to hand: the Target inspector and the chain preview.
//!
//! The status dot is glyph **and** colour, never colour alone (capability checklist #11).
//! What it reports is the target's standing in `AppState::problems` — it does not re-ask
//! anything the collector already asked. The one exception is the row's own
//! `resolve_target`, which it has to call anyway to have a summary at all: when that comes
//! back `Err`, mint's message is right there and the row shows it rather than waiting for
//! the collector to say the same thing.
//!
//! Presets are shown as a chip and carry the README's own caveat ([`PRESET_CAVEAT`]),
//! quoted rather than restated, because a preset is a starting point and a GUI that
//! presents it as a setting invites treating it as one.

use std::collections::BTreeSet;

use egui::{Checkbox, Color32, RichText, Ui};
use mint::config::{Config, ResolvedTarget};

use crate::colors;
use crate::inspector;
use crate::problems::{Scope, Severity};
use crate::state::AppState;

/// The README's wording on presets, verbatim (`README.md`, the preset table's footnote).
/// Quoted rather than paraphrased for the same reason every diagnostic is.
pub const PRESET_CAVEAT: &str =
    "Streaming targets drift over time; treat these as sensible starting points, not gospel.";

/// One deferred mutation, for the same reason `sources.rs` has one: the rows are drawn from
/// a shared borrow and both of these need the exclusive one.
enum Action {
    Select(String),
    SetIncluded { name: String, included: bool },
}

/// The pane: include checkbox, name, status dot, preset chip, resolved summary.
pub fn ui(ui: &mut Ui, state: &mut AppState) {
    let mut action = None;
    list(ui, state, &mut action);

    match action {
        Some(Action::Select(name)) => state.selected = Some(name),
        Some(Action::SetIncluded { name, included }) => state.set_included(&name, included),
        None => {}
    }
}

fn list(ui: &mut Ui, state: &AppState, action: &mut Option<Action>) {
    let names = state.target_names();
    let included = state.included_names();

    ui.horizontal(|ui| {
        ui.label(RichText::new("TARGETS").strong());
        ui.weak(format!("({} of {} in the run)", included.len(), names.len()));
    });

    if names.is_empty() {
        // Not a verdict — `validate` has one ("at least one [target.NAME] entry") and the
        // Problems panel is where it belongs. This is just an empty list saying it is one.
        ui.weak("no [target.NAME] blocks in this document");
        return;
    }
    // `parsed` as the fallback, because that is the case worth covering: one target that
    // will not resolve makes `status` an `Err`, and the other seven still have summaries.
    let Some(config) = state.config().or(state.parsed.as_ref()) else {
        ui.weak("the document does not parse — see Problems");
        return;
    };

    // Above the list rather than under it: the list takes the rest of the pane, so a
    // footer would be the first thing scrolled out of sight — and this is the one line
    // that keeps a preset from reading as a setting.
    let any_preset = names
        .iter()
        .any(|name| config.targets.get(name).is_some_and(|raw| raw.preset.is_some()));
    if any_preset {
        ui.weak("P = preset").on_hover_text(PRESET_CAVEAT);
    }

    egui::ScrollArea::vertical().id_salt("targets").auto_shrink([false, false]).show(ui, |ui| {
        for name in &names {
            // Ids from the name, not the row index: a target renamed in the TOML drawer
            // would otherwise inherit the widget state of whatever now sits in its slot.
            ui.push_id(name, |ui| row(ui, state, config, name, &included, action));
            ui.separator();
        }
    });
}

fn row(
    ui: &mut Ui,
    state: &AppState,
    config: &Config,
    name: &str,
    included: &[String],
    action: &mut Option<Action>,
) {
    let resolved = config.resolve_target(name);
    let is_included = included.iter().any(|other| other == name);
    let selected = state.selected.as_deref() == Some(name);
    // The target's *own* preset, from the document. Not the merged one: `[defaults] preset`
    // parses and is never applied (report.md defect #14), so a chip fed from the merge
    // would claim a preset that nothing read.
    let preset = config.targets.get(name).and_then(|raw| raw.preset.clone());

    ui.horizontal(|ui| {
        let mut ticked = is_included;
        // "Empty set == all" (see `AppState::included`) means *no* targets is not a state
        // the run options can represent — unticking the last one would silently mean all of
        // them again. The checkbox refuses instead of lying.
        let last_one = is_included && included.len() == 1;
        let tick = ui
            .add_enabled(!last_one, Checkbox::without_text(&mut ticked))
            .on_hover_text("include this target in the run")
            .on_disabled_hover_text("a run needs at least one target");

        let (glyph, colour, hover) = standing(state, name, &resolved);
        ui.colored_label(colour, glyph).on_hover_text(hover);

        if ui.selectable_label(selected, name).clicked() {
            *action = Some(Action::Select(name.to_string()));
        }
        if tick.changed() {
            *action = Some(Action::SetIncluded { name: name.to_string(), included: ticked });
        }

        if let Some(preset) = &preset {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("P {preset}"))
                    .on_hover_text(format!("preset \"{preset}\" — {PRESET_CAVEAT}"));
            });
        }
    });

    match &resolved {
        Ok(target) => {
            ui.horizontal_wrapped(|ui| {
                ui.weak(summary_line(target))
                    // Where the file lands, before the matrix renders one filename per source.
                    .on_hover_text(format!("{}/{}", target.dir.display(), target.naming));
                // Part III §3's "compact form for the matrix": full badges belong in the
                // inspector, but a target with a dropped or inert value should not look
                // identical to one without from the list that chooses between them. Only the
                // two surprising counts are shown here — `2 set here · 14 inherited` on every
                // row would be furniture.
                let tally = config
                    .resolve_target_explained(name)
                    .map(|(_, explained)| inspector::tally(&explained));
                if let Ok(tally) = tally
                    && (tally.dropped > 0 || tally.own > 0)
                {
                    let mut parts = Vec::new();
                    if tally.own > 0 {
                        parts.push(format!("+{} set here", tally.own));
                    }
                    if tally.dropped > 0 {
                        parts.push(format!("{} dropped", tally.dropped));
                    }
                    ui.weak(format!("· {}", parts.join(" · "))).on_hover_text(format!(
                        "{tally} — the Target inspector has the badges and the reasons"
                    ));
                }
            });
        }
        // mint's own text. The Problems panel will carry it too; a row that showed nothing
        // here would leave the pane looking like the target simply has no settings.
        Err(err) => {
            ui.colored_label(colors::BAD, format!("{err:#}"));
        }
    }
}

/// The status dot: glyph, colour and the reason, for one target.
///
/// Everything but the resolve failure comes out of `AppState::problems`, which is the
/// collector's cache — the row does not walk the sources itself, because "for how many
/// inputs does this target fail?" is a question `problems::collect` has already answered
/// with mint's own wording per cell.
fn standing(
    state: &AppState,
    name: &str,
    resolved: &anyhow::Result<ResolvedTarget>,
) -> (&'static str, Color32, String) {
    if let Err(err) = resolved {
        return ("✖", colors::BAD, format!("{err:#}"));
    }

    // Inputs, not problems: one cell can carry both a blocker and a collision, and "blocked
    // for 4 of 3 inputs" is how a count of messages reads.
    let mut blocked: BTreeSet<usize> = BTreeSet::new();
    let mut reasons: Vec<&str> = Vec::new();
    let mut warned: Vec<&str> = Vec::new();
    for problem in &state.problems {
        match &problem.scope {
            Scope::Target(target) if target == name && problem.severity == Severity::Error => {
                return ("✖", colors::BAD, problem.message.clone());
            }
            Scope::Cell { source, target } if target == name => match problem.severity {
                Severity::Error => {
                    blocked.insert(*source);
                    reasons.push(&problem.message);
                }
                Severity::Warning => warned.push(&problem.message),
            },
            _ => {}
        }
    }

    if !blocked.is_empty() {
        return (
            "▲",
            colors::BAD,
            format!(
                "blocked for {} of {} inputs:\n{}",
                blocked.len(),
                state.sources.len(),
                distinct(&reasons)
            ),
        );
    }
    // Never folded into the green dot: a run that is about to refuse a path is not an ok
    // run, and §7 item 9 is explicit that a warning may not become a checkmark.
    if !warned.is_empty() {
        return ("⟳", colors::WARN, distinct(&warned));
    }
    ("●", colors::OK, "resolves, and nothing in the plan blocks it".to_string())
}

/// mint's messages, in order, without the repeats.
///
/// One blocker usually applies to every input the same way (a FLAC target over 96 kHz says
/// the same sentence per file), and a hover that prints it twenty times is a hover nobody
/// finishes reading. The text itself is never edited — only the duplicates are dropped.
fn distinct(messages: &[&str]) -> String {
    let mut unique: Vec<&str> = Vec::new();
    for message in messages {
        if !unique.contains(message) {
            unique.push(message);
        }
    }
    unique.join("\n")
}

/// The one-line resolved summary, from the library's own tag helpers only.
///
/// Shared with the Plan matrix's column-header hover, so a target reads the same way in the
/// pane that lists it and in the column that renders it.
pub fn summary_line(target: &ResolvedTarget) -> String {
    let mut parts = Vec::with_capacity(5);

    // Rate. A DSD target delivers at the 1-bit rate and its `rate` field is only the
    // intermediate PCM stage the modulator is fed from, so the row shows `DsdRate::tag` —
    // the same label the TOML accepts and `describe()` prints. A PCM target with no rate
    // of its own has none to show until a source supplies one; `delivery_rate` needs that
    // source rate, and the plan matrix is where it becomes concrete.
    parts.push(match (target.dsd, target.rate) {
        (Some(dsd), _) => dsd.tag().to_string(),
        (None, Some(rate)) => format!("{rate} Hz"),
        (None, None) => "source rate".to_string(),
    });

    // `format_placeholder` rather than `format_tag(target.format)`: it is the library's own
    // answer to what this target's format *means*, which is `1bit` on DSD, and it is
    // literally the string mint substitutes for `{format}` in the output filename.
    parts.push(target.format_placeholder().to_string());

    // `describe()`'s own word for `lufs = None` ("transcode (no loudness normalization)").
    // An omitted slot would read as a rendering gap rather than as a deliberate absence of
    // the loudness stage, and those are very different deliverables.
    parts.push(match target.lufs {
        Some(lufs) => format!("{lufs:.1} LUFS"),
        None => "transcode".to_string(),
    });

    parts.push(format!("{:.1} dBTP", target.ceiling_dbtp));
    parts.push(target.codec.ext().to_string());
    parts.join(" · ")
}

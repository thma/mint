//! The Target Inspector: all 22 fields, where each came from, and what became of it.
//!
//! Phase 2's centrepiece and the successor to `form.rs`'s five-field spike. The proposal
//! calls it "the highest-value screen in the whole design and it is impossible in every stack
//! today", and the reason is `Fate`: a value can be *carried and never read*, or *stated by a
//! layer nobody consults here*, or *legal to inherit and fatal to write down* — three
//! properties that are invisible in `--dry-run`, in `--json` and in every error message, and
//! that are the difference between a config that works and one that works by accident.
//!
//! Every one of those verdicts comes from `Config::resolve_target_explained`. This file
//! computes none of them, and the test of that claim is easy to apply while reading: there is
//! no `if codec == Dsf` anywhere below, no list of which fields a preset sets, no default
//! value, and no sentence about *why* something is inert. The glyphs are
//! `Layer::glyph`/`Fate::glyph`, the legend is `config::ORIGIN_LEGEND`/`FATE_LEGEND`, the
//! prose is `FieldExplanation::notes` — the same producer `mint --explain` prints from, so
//! the two front ends cannot word one explanation two ways.
//!
//! Three deliberate shapes:
//!
//! - **Read first, edit on expand.** The table is the mockup's: name, value, origin, fate.
//!   Clicking a row opens the "why" panel underneath, and the control that edits the field
//!   lives *there* — next to the four layers that argued over it, rather than above them.
//!   Part III §3 calls that panel the money shot for a reason: it is where the exception gets
//!   taught, at the moment of confusion.
//! - **A dropped value is struck through wherever it appears.** §7 item 12 asks for that of
//!   `[defaults] preset` specifically; it generalizes, because "written down and not read" is
//!   exactly what strike-through means and `Dropped` is exactly that set.
//! - **The widget is seeded from the document, the provenance from the library.**
//!   `ConfigDocument::get` returns the bytes this target actually states; the explanation's
//!   rendered value is prose for a reader (`-14.4 LUFS`, `— (1-bit, no bit depth)`) and
//!   seeding a control from it would write the prose's rounding back into the file.

use egui::{Color32, ComboBox, Response, RichText, Ui};
use mint::config::{
    self, BitDepthFormat, DitherCorrelation, DitherMode, DitherStrength, FieldExplanation,
    Layer, LimiterCharacter, MP3_BITRATES, OnClipPolicy, OutputCodec, PRESET_NAMES,
    ResampleQuality, ResolvedTarget, TargetExplanation,
};
use mint::ops::dsd::DsdRate;
use toml_edit::Value;

use crate::colors;
use crate::presets::{self, Preview, Verdict};
use crate::state::AppState;
use crate::targets::PRESET_CAVEAT;
use crate::write_back::enum_value;

/// Where a `lufs` override starts when the target has none and the user asks for one.
///
/// The one number on this screen that is not mint's. It cannot be: `lufs` is genuinely
/// optional — no layer, builtin included, supplies a loudness target — so "start editing"
/// has to start somewhere. −14 is the figure the README's own streaming presets cluster
/// around, it lands in the document where it is visible and editable rather than staying a
/// GUI-side opinion, and the chain preview says what it did the moment it is written.
const LUFS_SEED: f64 = -14.0;

/// The inspector's own two pieces of memory. Neither can change a sample: one is which row is
/// open, the other is a preset the user is *looking at* and has not committed.
#[derive(Default)]
pub struct InspectorView {
    /// The field whose "why" panel is open. One at a time — the panel is a paragraph, not a
    /// badge, and a drawer full of open paragraphs is a drawer with no table in it.
    expanded: Option<String>,
    /// A preset being previewed. `Some(None)` is the "no preset" candidate, which is a real
    /// choice and not the same as "not previewing".
    candidate: Option<Option<String>>,
}

/// Everything a click in here can ask for. Queued rather than done on the spot, because the
/// rows are drawn from a shared borrow of `AppState` and every one of these needs the
/// exclusive one — the same shape `sources.rs` and `targets.rs` use.
enum Action {
    /// Write one key on the selected target.
    Set { field: &'static str, value: Value },
    /// Remove one key, so the field falls back to the layers below it.
    Clear { field: &'static str },
    /// Show a candidate preset's diff without committing it.
    Preview(Option<String>),
    /// Commit the candidate. The only edit here that moves more than one value.
    ChoosePreset(Option<String>),
    Expand(String),
    Collapse,
    /// Send the TOML drawer to a byte range — the mockup's `[ open [defaults] ]`.
    Reveal(std::ops::Range<usize>),
}

impl InspectorView {
    pub fn ui(&mut self, ui: &mut Ui, state: &mut AppState) {
        let mut action = None;
        self.draw(ui, state, &mut action);
        if let Some(action) = action {
            self.apply(action, state);
        }
    }

    /// Open a field's "why" panel without a click, for the same reason
    /// [`crate::app::MintApp::select_mode`] exists: clicking the row requires the table to
    /// have been laid out *and* scrolled to it, and what is under test is what the panel
    /// says, not whether a row twenty places down is under the pointer.
    pub fn expand(&mut self, field: &str) {
        self.expanded = Some(field.to_string());
    }

    pub fn expanded(&self) -> Option<&str> {
        self.expanded.as_deref()
    }

    fn apply(&mut self, action: Action, state: &mut AppState) {
        let Some(name) = state.selected.clone() else { return };
        match action {
            Action::Set { field, value } => state.edit(|doc| doc.set(&name, field, value)),
            Action::Clear { field } => state.edit(|doc| doc.clear(&name, field).map(|_| ())),
            Action::Preview(candidate) => self.candidate = Some(candidate),
            Action::ChoosePreset(candidate) => {
                self.candidate = None;
                match candidate {
                    // One line written, never the preset expanded into the fields it sets —
                    // anti-feature #2, and the reason `set` takes a single key.
                    Some(preset) => {
                        state.edit(|doc| doc.set(&name, "preset", Value::from(preset)))
                    }
                    None => state.edit(|doc| doc.clear(&name, "preset").map(|_| ())),
                }
            }
            Action::Expand(field) => self.expanded = Some(field),
            Action::Collapse => self.expanded = None,
            Action::Reveal(span) => state.reveal = Some(span),
        }
    }

    fn draw(&mut self, ui: &mut Ui, state: &AppState, action: &mut Option<Action>) {
        let Some(name) = state.selected.clone() else {
            ui.weak("no target selected — pick one in the Targets pane");
            return;
        };
        // `parsed` as the fallback for the same reason the Targets pane uses it: one broken
        // target makes `status` an `Err`, and the other ten are still worth inspecting —
        // including, usually, the one being fixed.
        let Some(config) = state.config().or(state.parsed.as_ref()) else {
            ui.colored_label(colors::BAD, "the document does not parse:");
            ui.label(state.error().unwrap_or_default());
            return;
        };
        let (resolved, explained) = match config.resolve_target_explained(&name) {
            Ok(pair) => pair,
            // mint's own refusal, which is the whole content of the screen in that case.
            Err(err) => {
                ui.colored_label(colors::BAD, format!("{err:#}"));
                return;
            }
        };

        self.header(ui, state, &explained, action);
        legend(ui);
        ui.separator();

        // The "why" panel is a pane of its own rather than a block appended under the last
        // row: the table is 22 rows and scrolls, and a details panel that scrolls away from
        // the row that opened it is a details panel you have to hunt for. Same arrangement
        // the Report view's DETAIL pane uses, for the same reason.
        if let Some(field) = self.expanded.as_deref().and_then(|field| explained.field(field)) {
            egui::Panel::bottom("inspector-why").resizable(true).default_size(240.0).show(
                ui,
                |ui| {
                    egui::ScrollArea::vertical().id_salt("why").auto_shrink([false, false]).show(
                        ui,
                        |ui| why(ui, state, &explained, &resolved, field, action),
                    );
                },
            );
        }

        egui::ScrollArea::vertical().id_salt("inspector").auto_shrink([false, false]).show(
            ui,
            |ui| self.table(ui, &explained, action),
        );
    }

    /// Target name, the source the derived values are derived *from*, the compact provenance
    /// tally, and the preset picker with its diff.
    fn header(
        &self,
        ui: &mut Ui,
        state: &AppState,
        explained: &TargetExplanation,
        action: &mut Option<Action>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("TARGET").strong());
            ui.label(RichText::new(&explained.target).strong().monospace());
            ui.weak("·");
            ui.weak(tally(explained).to_string()).on_hover_text(
                "how the 22 fields divide: set on this target, inherited from a lower layer, \
                 carried but read by nothing here, and stated by a layer this target does not \
                 consult",
            );
            match source_line(state) {
                // Named because half this screen is derived *from* it: with no source, `rate`
                // on a DSD target has no number and the chain has no steps.
                Some(line) => {
                    ui.weak("·");
                    ui.weak(line);
                }
                None => {
                    ui.weak("·");
                    ui.weak(
                        "no source selected — nothing for the derived values to derive from",
                    );
                }
            }
        });
        self.preset_picker(ui, state, explained, action);
    }

    fn preset_picker(
        &self,
        ui: &mut Ui,
        state: &AppState,
        explained: &TargetExplanation,
        action: &mut Option<Action>,
    ) {
        let current = explained.preset.clone();
        // Not previewing == previewing what is already there, which keeps the combo showing
        // the truth when nothing is pending.
        let candidate = self.candidate.clone().unwrap_or_else(|| current.clone());

        ui.horizontal_wrapped(|ui| {
            // `preset:` with the colon, not bare: the table below has a row whose name *is*
            // `preset`, and two widgets with one label is an ambiguity for anything reading
            // the screen — a test harness first, a screen reader after it.
            ui.label("preset:");
            let mut chosen = candidate.clone();
            ComboBox::from_id_salt("preset-picker")
                .selected_text(chosen.clone().unwrap_or_else(|| "— none".to_string()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut chosen, None, "— (no preset)");
                    // `PRESET_NAMES`, never a list written here: that constant exists because
                    // a second copy went stale once already.
                    for name in PRESET_NAMES {
                        ui.selectable_value(&mut chosen, Some(name.to_string()), name);
                    }
                });
            if chosen != candidate {
                *action = Some(Action::Preview(chosen));
            }

            let supplied = presets::set_by_preset(explained);
            if let Some(preset) = &current {
                ui.weak(format!(
                    "'{preset}' sets {} of {}: {}",
                    supplied.len(),
                    explained.fields.len(),
                    supplied.join(", ")
                ))
                .on_hover_text(PRESET_CAVEAT);
            } else {
                ui.weak("no preset — every value is a layer of its own");
            }
        });

        if candidate == current {
            return;
        }
        self.diff(ui, state, &explained.target, &current, &candidate, action);
    }

    /// The picker's whole reason for existing: which *resolved* values move, before the
    /// document is touched.
    fn diff(
        &self,
        ui: &mut Ui,
        state: &AppState,
        target: &str,
        current: &Option<String>,
        candidate: &Option<String>,
        action: &mut Option<Action>,
    ) {
        let preview = presets::preview(&state.text, target, candidate.as_deref());
        ui.group(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("changing preset:").strong());
                ui.monospace(current.clone().unwrap_or_else(|| "—".to_string()));
                ui.weak("→");
                ui.monospace(candidate.clone().unwrap_or_else(|| "—".to_string()));
            });

            match &preview {
                // Not a broken preview — the most useful thing it can say. Every PCM preset
                // sets `format`, so offering one on a DSD target is offering a load-time
                // error, and mint says so in its own words before the click rather than after.
                Preview::Refused(err) => {
                    ui.colored_label(colors::BAD, format!("mint refuses this combination: {err}"));
                }
                Preview::Diff(changes) if changes.is_empty() => {
                    ui.weak("nothing this target delivers would change");
                }
                Preview::Diff(changes) => {
                    egui::Grid::new("preset-diff").num_columns(4).spacing([12.0, 3.0]).show(
                        ui,
                        |ui| {
                            for change in changes {
                                ui.monospace(change.field);
                                ui.label(RichText::new(&change.before).monospace().weak());
                                match &change.verdict {
                                    Verdict::Changed(_) => {
                                        ui.label(RichText::new(&change.after).monospace());
                                    }
                                    _ => {
                                        ui.weak("—");
                                    }
                                }
                                match &change.verdict {
                                    Verdict::Changed(Some(layer)) => {
                                        ui.weak(layer.to_string());
                                    }
                                    Verdict::Changed(None) => {
                                        ui.weak("no layer — derived");
                                    }
                                    Verdict::Overridden => {
                                        ui.weak("unchanged — your explicit value wins");
                                    }
                                    Verdict::Unchanged => {
                                        ui.weak("unchanged");
                                    }
                                }
                                ui.end_row();
                            }
                        },
                    );
                    ui.weak(counted(preview.changed(), "field"));
                }
            }

            ui.horizontal(|ui| {
                let usable = !matches!(preview, Preview::Refused(_));
                if ui
                    .add_enabled(usable, egui::Button::new("Apply preset"))
                    .on_disabled_hover_text("mint will not load the target this way")
                    .clicked()
                {
                    *action = Some(Action::ChoosePreset(candidate.clone()));
                }
                if ui.button("Cancel").clicked() {
                    *action = Some(Action::Preview(current.clone()));
                }
            });
        });
    }

    fn table(&self, ui: &mut Ui, explained: &TargetExplanation, action: &mut Option<Action>) {
        egui::Grid::new("inspector-fields")
            .num_columns(5)
            .spacing([12.0, 3.0])
            .striped(true)
            .show(ui, |ui| {
                let mut group = None;
                for field in &explained.fields {
                    if group != Some(field.group) {
                        group = Some(field.group);
                        ui.label(RichText::new(field.group.to_string()).small().strong());
                        ui.end_row();
                    }
                    self.row(ui, field, action);
                }
            });
    }

    fn row(&self, ui: &mut Ui, field: &FieldExplanation, action: &mut Option<Action>) {
        let open = self.expanded.as_deref() == Some(field.field);
        if ui
            .selectable_label(open, RichText::new(field.field).monospace())
            .on_hover_text("why is it this? — the four layers, and what each of them said")
            .clicked()
        {
            *action = Some(if open {
                Action::Collapse
            } else {
                Action::Expand(field.field.to_string())
            });
        }
        ui.label(RichText::new(&field.value).monospace());
        origin_badge(ui, field);
        marks(ui, field);
        short_notes(ui, field);
        ui.end_row();
    }
}

/// `B` / `D` / `P` / `E`, or `—` when no layer set the field at all.
fn origin_badge(ui: &mut Ui, field: &FieldExplanation) {
    let glyph = field.origin.layer.as_ref().map_or('—', Layer::glyph);
    // Colour carries emphasis, never meaning: the glyph says which layer, and the legend
    // above says what the glyph means (capability checklist #11).
    let colour = match &field.origin.layer {
        Some(Layer::Explicit) => colors::OK,
        Some(Layer::Preset(_)) => Color32::from_rgb(140, 170, 230),
        _ => Color32::GRAY,
    };
    let label = match &field.origin.layer {
        Some(layer) => format!("{glyph} {layer}"),
        None => format!("{glyph} nothing set it"),
    };
    ui.label(RichText::new(label).monospace().color(colour));
}

/// The fate column: `·` or `~`, then `/` and `!` when they apply — the same three marks
/// `--explain` prints, from the same three sources.
fn marks(ui: &mut Ui, field: &FieldExplanation) {
    let notes = field.notes();
    ui.horizontal(|ui| {
        let fate = field.origin.fate.glyph();
        let colour = match field.origin.fate.reason() {
            Some(_) => colors::WARN,
            None => Color32::GRAY,
        };
        let response = ui.label(RichText::new(fate.to_string()).monospace().color(colour));
        if let Some(note) = notes.iter().find(|note| note.glyph == fate) {
            response.on_hover_text(&note.text);
        }
        for note in &notes {
            if note.glyph == fate {
                continue;
            }
            let colour = if note.glyph == mint::config::Dropped::GLYPH {
                Color32::from_rgb(200, 130, 90)
            } else {
                colors::BAD
            };
            ui.label(RichText::new(note.glyph.to_string()).monospace().color(colour))
                .on_hover_text(&note.text);
        }
    });
}

/// The mockup's short forms — `inert`, `D rate=48000 dropped`, `error if stated` — each with
/// the library's whole sentence one hover away and all of it again in the "why" panel.
/// Shortened for the column, never reworded: a table wide enough for the full prose is a
/// table nobody can read a row of.
fn short_notes(ui: &mut Ui, field: &FieldExplanation) {
    let notes = field.notes();
    ui.horizontal_wrapped(|ui| {
        if field.origin.fate.reason().is_some()
            && let Some(note) = notes.iter().find(|note| note.glyph == field.origin.fate.glyph())
        {
            ui.weak("inert").on_hover_text(&note.text);
        }
        if let Some(dropped) = &field.origin.dropped {
            // Struck through, which is §7 item 12's instruction for `[defaults] preset` and
            // is the honest rendering of every dropped value: written down, not read.
            let text = format!("{} {}={}", dropped.layer.glyph(), field.field, dropped.value);
            ui.label(RichText::new(text).weak().strikethrough()).on_hover_text(dropped.why);
        }
        if field.origin.illegal_here {
            ui.colored_label(colors::BAD, "error if stated")
                .on_hover_text("legal to inherit, a load-time error if stated on this target");
        }
    });
}

/// Both legends, from the library, so this table and `mint --explain`'s cannot drift.
fn legend(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.weak("origin");
        ui.weak(RichText::new(config::ORIGIN_LEGEND).monospace());
    });
    ui.horizontal_wrapped(|ui| {
        ui.weak("fate  ");
        ui.weak(RichText::new(config::FATE_LEGEND).monospace());
    });
}

/// The money shot (Part III §3): the four layers, what each said, who won, and what it costs
/// to state this field here — followed by the control that states it.
fn why(
    ui: &mut Ui,
    state: &AppState,
    explained: &TargetExplanation,
    resolved: &ResolvedTarget,
    field: &FieldExplanation,
    action: &mut Option<Action>,
) {
    ui.add_space(8.0);
    ui.group(|ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!("why is  {} = {}?", field.field, field.value)).strong(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("dismiss").clicked() {
                    *action = Some(Action::Collapse);
                }
            });
        });

        // Highest precedence first, which is the order the merge is *read* in even though it
        // is folded the other way — "what wins" is the question, so the winner is at the top.
        egui::Grid::new("why-layers").num_columns(4).spacing([12.0, 3.0]).show(ui, |ui| {
            for layer in explained.layers.iter().rev() {
                let stated = field.origin.stated_by(layer);
                let is_winner = field.origin.layer.as_ref() == Some(layer);
                let is_dropped =
                    field.origin.dropped.as_ref().is_some_and(|dropped| &dropped.layer == layer);

                ui.label(RichText::new(layer.glyph().to_string()).monospace());
                ui.label(layer.to_string());
                match stated {
                    Some(value) => {
                        let text = RichText::new(format!("{} = {value}", field.field)).monospace();
                        ui.label(if is_dropped { text.strikethrough() } else { text });
                    }
                    None => {
                        ui.weak("not set");
                    }
                }
                if is_winner {
                    ui.colored_label(colors::OK, "✓ used");
                } else if is_dropped {
                    let why = field.origin.dropped.as_ref().map(|d| d.why).unwrap_or_default();
                    ui.colored_label(colors::BAD, "✗ NOT READ").on_hover_text(why);
                } else if stated.is_some() {
                    match &field.origin.layer {
                        Some(winner) => {
                            ui.weak(format!("passed over by {winner}"));
                        }
                        None => {
                            ui.colored_label(colors::BAD, "✗ not read");
                        }
                    }
                } else {
                    ui.weak("");
                }
                ui.end_row();
            }
        });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            ui.weak("⇒");
            ui.label(RichText::new(&field.value).monospace().strong());
            if field.origin.layer.is_none() {
                ui.weak("— no layer states it");
            }
        });
        derived(ui, state, resolved, field);

        // The library's own sentences, in full this time.
        for note in field.notes() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(note.glyph.to_string()).monospace());
                ui.label(&note.text);
            });
        }

        ui.separator();
        editor_row(ui, state, resolved, explained, field, action);
    });
}

/// The one field whose effective value is a function of the *source*, spelled out with the
/// source in hand. `pcm_stage_rate` and `delivery_rate` are the library's own functions —
/// the same two the pipeline and `describe()` call — so this line cannot claim a rate the
/// render will not use.
fn derived(ui: &mut Ui, state: &AppState, resolved: &ResolvedTarget, field: &FieldExplanation) {
    if field.field != "rate" {
        return;
    }
    let Some(source) = state.selected_probe() else {
        ui.weak("· select a source to see what this derives to");
        return;
    };
    let Ok(probe) = &source.probe else { return };
    let stage = resolved.pcm_stage_rate(probe.sample_rate);
    let delivered = resolved.delivery_rate(probe.sample_rate);
    let line = if stage == delivered {
        format!("· with a {} Hz source: {stage} Hz", probe.sample_rate)
    } else {
        format!(
            "· with a {} Hz source: {stage} Hz into the modulator, {delivered} Hz delivered",
            probe.sample_rate
        )
    };
    ui.weak(line).on_hover_text(
        "ResolvedTarget::pcm_stage_rate / delivery_rate — the same two the pipeline and the \
         dry-run read. The steps between them are in the chain preview.",
    );
}

/// The control, the "remove it" button, and the jump to whichever layer is being argued with.
fn editor_row(
    ui: &mut Ui,
    state: &AppState,
    resolved: &ResolvedTarget,
    explained: &TargetExplanation,
    field: &FieldExplanation,
    action: &mut Option<Action>,
) {
    let stated = state.doc.as_ref().and_then(|doc| doc.get(&explained.target, field.field));

    ui.horizontal_wrapped(|ui| {
        if field.field == "preset" {
            ui.weak("chosen at the top of this panel, where the diff is");
        } else {
            let response = ui
                .add_enabled_ui(!field.origin.illegal_here, |ui| {
                    control(ui, state, resolved, field, stated, action);
                })
                .response;
            if field.origin.illegal_here {
                response.on_disabled_hover_text(
                    "legal to inherit, a load-time error if stated on this target",
                );
            }
        }

        if ui
            .add_enabled(stated.is_some(), egui::Button::new("remove from this target"))
            .on_hover_text(
                "removes the key — the field falls back to the layers below it. Never replaced \
                 by the value it happened to have.",
            )
            .clicked()
        {
            *action = Some(Action::Clear { field: field.field });
        }

        // Only when `[defaults]` has something to say about this field — which is the case
        // the panel above has just spent four rows on.
        let in_defaults = field.origin.stated_by(&Layer::Defaults).is_some();
        let span = state.doc.as_ref().and_then(|doc| doc.table_span("defaults"));
        if in_defaults
            && let Some(span) = span
            && ui
                .button("open [defaults]")
                .on_hover_text("show the block this value came from, in the TOML drawer")
                .clicked()
        {
            *action = Some(Action::Reveal(span));
        }
    });
}

/// How one field is edited.
///
/// A widget choice, not a rule: the *domain* of every enum below is that enum's own `ALL`
/// array, the MP3 bitrates are `config::MP3_BITRATES`, and nothing here decides whether a
/// value is legal — `resolve_target` does, per keystroke, and its refusal is on the status
/// bar and in the Problems panel a frame later.
enum Control {
    Enum(Vec<String>),
    /// A closed set of integers (`MP3_BITRATES`), written back as an integer rather than as
    /// its spelling.
    IntChoice(&'static [u32]),
    Bool,
    Int,
    Float,
    Text,
    Dir,
    /// A field this file has grown no control for. Visible rather than silently missing: a
    /// new `RawTarget` field lands here, and an inspector row that says so is a to-do a user
    /// can report.
    Unsupported,
}

fn control_for(field: &str) -> Control {
    let spellings = |values: Vec<String>| Control::Enum(values);
    match field {
        "codec" => spellings(OutputCodec::ALL.iter().map(ToString::to_string).collect()),
        "dsd" => spellings(DsdRate::ALL.iter().map(|rate| rate.tag().to_string()).collect()),
        "format" => spellings(BitDepthFormat::ALL.iter().map(ToString::to_string).collect()),
        "on_clip" => spellings(OnClipPolicy::ALL.iter().map(ToString::to_string).collect()),
        "limiter_character" => {
            spellings(LimiterCharacter::ALL.iter().map(ToString::to_string).collect())
        }
        "quality" => spellings(ResampleQuality::ALL.iter().map(ToString::to_string).collect()),
        "dither" => spellings(DitherMode::ALL.iter().map(ToString::to_string).collect()),
        "dither_strength" => {
            spellings(DitherStrength::ALL.iter().map(ToString::to_string).collect())
        }
        "dither_correlation" => {
            spellings(DitherCorrelation::ALL.iter().map(ToString::to_string).collect())
        }
        "mp3_bitrate" => Control::IntChoice(MP3_BITRATES),
        "limiter_soft_clip" | "limiter_link_channels" | "bwf" | "overwrite" => Control::Bool,
        "rate" => Control::Int,
        "lufs" | "ceiling_dbtp" | "warn_limiting_db" | "max_limiting_db" => Control::Float,
        "naming" => Control::Text,
        "dir" => Control::Dir,
        _ => Control::Unsupported,
    }
}

/// The "state it here" toggle plus the value widget.
///
/// The toggle is what makes the *absence* of a key editable, and absence is a real state:
/// `lufs` unset is a transcode rather than a normalization, and `rate` unset on a DSD target
/// is the derived intermediate rate rather than a number the user chose. Ticking it writes
/// the value that is already in effect — an explicit no-op the user asked for, and visible in
/// the document a moment later, which is a different thing from the tool quietly expanding
/// defaults into the file (anti-feature #2).
fn control(
    ui: &mut Ui,
    state: &AppState,
    resolved: &ResolvedTarget,
    field: &FieldExplanation,
    stated: Option<&Value>,
    action: &mut Option<Action>,
) {
    let kind = control_for(field.field);
    if matches!(kind, Control::Unsupported) {
        ui.weak(format!("no control for `{}` yet — edit it in the TOML drawer", field.field));
        return;
    }

    let mut on = stated.is_some();
    let seed = seed_value(state, resolved, field.field);
    let toggle = ui
        .add_enabled(seed.is_some() || on, egui::Checkbox::new(&mut on, "set here"))
        .on_disabled_hover_text("there is no value to start from — this target derives it");
    if toggle.changed() {
        *action = Some(match (on, seed) {
            (true, Some(value)) => Action::Set { field: field.field, value },
            _ => Action::Clear { field: field.field },
        });
        return;
    }

    ui.add_enabled_ui(stated.is_some(), |ui| {
        let Some(value) = stated else {
            ui.weak(&field.value);
            return;
        };
        let id = field.field;
        match kind {
            Control::Enum(options) => {
                let current = value.as_str().unwrap_or_default().to_string();
                let mut chosen = current.clone();
                ComboBox::from_id_salt(id).selected_text(&current).show_ui(ui, |ui| {
                    for option in &options {
                        // The one availability question the library answers rather than this
                        // file: an MP3 target in a build without the encoder resolves,
                        // validates, runs the whole chain and dies in the writer.
                        let usable = id != "codec"
                            || option != OutputCodec::Mp3.to_string().as_str()
                            || mint::MP3_AVAILABLE;
                        ui.add_enabled_ui(usable, |ui| {
                            ui.selectable_value(&mut chosen, option.clone(), option)
                                .on_disabled_hover_text("this build has no MP3 encoder");
                        });
                    }
                });
                if chosen != current {
                    *action = Some(Action::Set { field: id, value: Value::from(chosen) });
                }
            }
            Control::IntChoice(choices) => {
                let current = value.as_integer().unwrap_or_default();
                let mut chosen = current;
                ComboBox::from_id_salt(id).selected_text(current.to_string()).show_ui(ui, |ui| {
                    for choice in choices {
                        let choice = i64::from(*choice);
                        ui.selectable_value(&mut chosen, choice, choice.to_string());
                    }
                });
                if chosen != current {
                    *action = Some(Action::Set { field: id, value: Value::from(chosen) });
                }
            }
            Control::Bool => {
                let current = value.as_bool().unwrap_or_default();
                let mut chosen = current;
                ui.selectable_value(&mut chosen, true, "true");
                ui.selectable_value(&mut chosen, false, "false");
                if chosen != current {
                    *action = Some(Action::Set { field: id, value: Value::from(chosen) });
                }
            }
            Control::Int => {
                let mut number = value.as_integer().unwrap_or_default();
                // No range: `rate = 0` is mint's error to give, in mint's words, and a widget
                // that makes the mistake unreachable also makes the lesson unreachable.
                if committed(&ui.add(egui::DragValue::new(&mut number).speed(50.0))) {
                    *action = Some(Action::Set { field: id, value: Value::from(number) });
                }
            }
            Control::Float => {
                let mut number = value.as_float().unwrap_or_default();
                if committed(&ui.add(egui::DragValue::new(&mut number).speed(0.1))) {
                    *action = Some(Action::Set { field: id, value: Value::from(number) });
                }
            }
            Control::Text | Control::Dir => {
                let mut text = value.as_str().unwrap_or_default().to_string();
                let response =
                    ui.add(egui::TextEdit::singleline(&mut text).desired_width(240.0));
                if response.changed() {
                    *action = Some(match text.is_empty() {
                        // An empty box is not `dir = ""`; it is the key gone.
                        true => Action::Clear { field: id },
                        false => Action::Set { field: id, value: Value::from(text.clone()) },
                    });
                }
                if matches!(kind, Control::Dir)
                    && ui.button("Browse…").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    *action = Some(Action::Set {
                        field: id,
                        value: Value::from(dir.display().to_string()),
                    });
                }
            }
            Control::Unsupported => {}
        }
    });
}

/// Whether an edit is worth writing to the document this frame.
///
/// A `DragValue` reports `changed()` on every frame of a drag, and each write re-parses the
/// document, re-validates it and re-collects the Problems panel. Typing into the box commits
/// on the spot; dragging commits when the mouse comes up.
fn committed(response: &Response) -> bool {
    (response.changed() && !response.dragged()) || response.drag_stopped()
}

/// The value an override starts from: whatever is in effect right now, as a TOML value.
///
/// Read off `ResolvedTarget` rather than parsed back out of the explanation's rendered
/// string, which is prose for a reader (`-1.0 dBTP`, `88200 Hz`, `— (1-bit, no bit depth)`).
/// `None` means there is nothing to start from and the toggle stays disabled.
fn seed_value(state: &AppState, resolved: &ResolvedTarget, field: &str) -> Option<Value> {
    let float = |value: f64| Some(Value::from(value));
    match field {
        "rate" => {
            // The rate this target *works at*, which for a DSD target with no explicit rate
            // is derived from the source — so with no source there is nothing to seed.
            let rate = match resolved.rate {
                Some(rate) => rate,
                None => resolved.pcm_stage_rate(source_rate(state)?),
            };
            Some(Value::from(i64::from(rate)))
        }
        "format" => Some(Value::from(config::format_tag(resolved.format))),
        "lufs" => float(resolved.lufs.unwrap_or(LUFS_SEED)),
        "ceiling_dbtp" => float(resolved.ceiling_dbtp),
        "warn_limiting_db" => float(resolved.warn_limiting_db),
        "max_limiting_db" => float(resolved.max_limiting_db),
        "on_clip" => Some(enum_value(resolved.on_clip)),
        "limiter_character" => Some(enum_value(resolved.limiter_character)),
        "limiter_soft_clip" => Some(Value::from(resolved.limiter_soft_clip)),
        "limiter_link_channels" => Some(Value::from(resolved.limiter_link_channels)),
        "quality" => Some(enum_value(resolved.quality)),
        "dither" => Some(enum_value(resolved.dither)),
        "dither_strength" => Some(enum_value(resolved.dither_strength)),
        "dither_correlation" => Some(enum_value(resolved.dither_correlation)),
        "codec" => Some(enum_value(resolved.codec)),
        "dsd" => resolved.dsd.map(|rate| Value::from(rate.tag())),
        "bwf" => Some(Value::from(resolved.bwf)),
        "mp3_bitrate" => Some(Value::from(i64::from(resolved.mp3_bitrate))),
        "dir" => Some(Value::from(resolved.dir.display().to_string())),
        "naming" => Some(Value::from(resolved.naming.clone())),
        "overwrite" => Some(Value::from(resolved.overwrite)),
        // `preset` is the picker's, and an unknown field has no control to seed.
        _ => None,
    }
}

fn source_rate(state: &AppState) -> Option<u32> {
    state.selected_probe()?.probe.as_ref().ok().map(|probe| probe.sample_rate)
}

fn source_line(state: &AppState) -> Option<String> {
    let source = state.selected_probe()?;
    let name = source.path.file_name()?.to_string_lossy().into_owned();
    Some(match &source.probe {
        Ok(probe) => format!("source: {name} @ {} Hz", probe.sample_rate),
        // The header is not the place to repeat mint's message — the Sources pane carries it
        // against the row it belongs to — but a source that cannot be read must not look like
        // a source that can.
        Err(_) => format!("source: {name} — unreadable"),
    })
}

/// How this target's 22 fields divide. The compact form Part III §3 asks for in the target
/// list, where full badges would not fit: `2 set here · 4 inherited · 3 inert · 1 dropped`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    /// Fields this target states itself, `preset` excluded — it is named separately, and
    /// counting it would make every preset-using target look like it has an override.
    pub own: usize,
    pub inherited: usize,
    pub inert: usize,
    pub dropped: usize,
}

pub fn tally(explained: &TargetExplanation) -> Tally {
    let mut tally = Tally::default();
    for field in &explained.fields {
        match &field.origin.layer {
            Some(Layer::Explicit) if field.field != "preset" => tally.own += 1,
            Some(Layer::Builtin | Layer::Defaults | Layer::Preset(_)) => tally.inherited += 1,
            _ => {}
        }
        if field.origin.fate.reason().is_some() {
            tally.inert += 1;
        }
        if field.origin.dropped.is_some() {
            tally.dropped += 1;
        }
    }
    tally
}

impl std::fmt::Display for Tally {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = vec![
            format!("{} set here", self.own),
            format!("{} inherited", self.inherited),
        ];
        // The two that are only worth the space when they are not zero — and always worth it
        // when they are not, because they are the states no other surface shows.
        if self.inert > 0 {
            parts.push(format!("{} inert", self.inert));
        }
        if self.dropped > 0 {
            parts.push(format!("{} dropped", self.dropped));
        }
        f.write_str(&parts.join(" · "))
    }
}

fn counted(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun} changes") } else { format!("{n} {noun}s change") }
}

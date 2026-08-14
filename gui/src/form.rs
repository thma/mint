//! Spike gate S2: a live-validating form over five representative fields.
//!
//! The gate's fail signal is *"any need to reimplement a rule in the GUI"*, so read this
//! file with that in mind. It contains no rules. It does not know that FLAC stops at
//! 96 kHz, that a DSD target rejects `format`, that `[defaults] rate` is dropped on one, or
//! what any field's default is. Every one of those comes from
//! `Config::resolve_target_explained`, and every error string is mint's own.
//!
//! The five fields are chosen to cover the distinct shapes: an optional float (`lufs`), two
//! enums that constrain each other (`format`, `codec`), a path (`dir`), and the picker that
//! rewrites the meaning of everything else (`preset`).
//!
//! Two things the origin badges buy immediately, both from Tier 2:
//! - a field the target does not state shows where it *did* come from, so "inherited" and
//!   "set here" stop looking identical;
//! - a field that would be a load-time error here is disabled rather than offered, which is
//!   what `illegal_here` is for.

use egui::{Color32, ComboBox, RichText, Ui};
use mint::config::{
    BitDepthFormat, FieldExplanation, Layer, OutputCodec, PRESET_NAMES, TargetExplanation,
};
use toml_edit::Value;

use crate::state::AppState;

/// One edit the form wants applied to the document, queued so the borrow of `state` ends
/// before the write does. `None` clears the field.
struct Edit {
    field: &'static str,
    value: Option<Value>,
}

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    let Some(name) = state.selected.clone() else {
        ui.label("no target selected");
        return;
    };
    let Some(config) = state.config() else {
        ui.colored_label(Color32::from_rgb(220, 90, 80), "the document does not resolve:");
        ui.label(state.error().unwrap_or_default());
        return;
    };

    // The explained resolve is the form's entire data source. Note it is asked again every
    // frame — the timing in the status bar is what says whether that is affordable.
    let explained = match config.resolve_target_explained(&name) {
        Ok((_, explanation)) => explanation,
        Err(err) => {
            ui.colored_label(Color32::from_rgb(220, 90, 80), format!("{err:#}"));
            return;
        }
    };

    let mut edit: Option<Edit> = None;
    egui::Grid::new("target-form")
        .num_columns(4)
        .spacing([12.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            preset_row(ui, &explained, &mut edit);
            codec_row(ui, &explained, &mut edit);
            format_row(ui, &explained, &mut edit);
            lufs_row(ui, &explained, &mut edit);
            dir_row(ui, &explained, &mut edit);
        });

    if let Some(edit) = edit {
        state.edit(|doc| match edit.value {
            Some(value) => doc.set(&name, edit.field, value),
            None => doc.clear(&name, edit.field).map(|_| ()),
        });
    }
}

/// Origin badge + fate marks + the inert/dropped reason, straight from the explanation.
fn badges(ui: &mut Ui, field: &FieldExplanation) {
    let origin = field.origin.layer.as_ref().map_or('—', Layer::glyph);
    let colour = match &field.origin.layer {
        Some(Layer::Explicit) => Color32::from_rgb(120, 200, 140),
        Some(Layer::Preset(_)) => Color32::from_rgb(140, 170, 230),
        _ => Color32::GRAY,
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(origin).monospace().color(colour))
            .on_hover_text(match &field.origin.layer {
                Some(layer) => format!("from {layer}"),
                None => "no layer set this".to_string(),
            });
        if let Some(reason) = field.origin.fate.reason() {
            ui.label(RichText::new("~").monospace().color(Color32::from_rgb(200, 170, 90)))
                .on_hover_text(format!("inert here — {reason}"));
        }
        if let Some(dropped) = &field.origin.dropped {
            ui.label(RichText::new("/").monospace().color(Color32::from_rgb(200, 130, 90)))
                .on_hover_text(format!(
                    "{} says {} = {}, and it is not read here — {}",
                    dropped.layer, field.field, dropped.value, dropped.why
                ));
        }
        if field.origin.illegal_here {
            ui.label(RichText::new("!").monospace().color(Color32::from_rgb(220, 90, 80)))
                .on_hover_text("legal to inherit, a load-time error if stated on this target");
        }
    });
}

/// A row is: label, control, badges, effective value. The effective value is shown even
/// when the control is empty, because "inherited −14 LUFS" and "no loudness step" are
/// different states that an empty box cannot distinguish.
fn row(
    ui: &mut Ui,
    explained: &TargetExplanation,
    field: &'static str,
    control: impl FnOnce(&mut Ui, &FieldExplanation),
) {
    let Some(explanation) = explained.field(field) else { return };
    ui.label(field);
    let stated_here = matches!(explanation.origin.layer, Some(Layer::Explicit));
    ui.add_enabled_ui(!explanation.origin.illegal_here, |ui| control(ui, explanation));
    badges(ui, explanation);
    let effective = RichText::new(&explanation.value).monospace();
    ui.label(if stated_here { effective } else { effective.weak() });
    ui.end_row();
}

fn preset_row(ui: &mut Ui, explained: &TargetExplanation, edit: &mut Option<Edit>) {
    row(ui, explained, "preset", |ui, explanation| {
        let current = explained.preset.clone();
        let mut chosen = current.clone();
        ComboBox::from_id_salt("preset")
            .selected_text(current.clone().unwrap_or_else(|| "—".to_string()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, "— (no preset)");
                // From `PRESET_NAMES`, never a hardcoded list: that constant exists
                // precisely because a second copy went stale.
                for name in PRESET_NAMES {
                    ui.selectable_value(&mut chosen, Some(name.to_string()), name);
                }
            });
        let _ = explanation;
        if chosen != current {
            *edit = Some(Edit { field: "preset", value: chosen.map(Value::from) });
        }
    });
}

fn codec_row(ui: &mut Ui, explained: &TargetExplanation, edit: &mut Option<Edit>) {
    row(ui, explained, "codec", |ui, explanation| {
        let current = explanation.value.clone();
        let mut chosen = current.clone();
        ComboBox::from_id_salt("codec")
            .selected_text(&current)
            .show_ui(ui, |ui| {
                for codec in OutputCodec::ALL {
                    // An MP3 target in a build without the feature resolves, validates,
                    // passes `delivery_blocker` and dies in the writer after the whole
                    // chain has run. `MP3_AVAILABLE` is the library saying so.
                    let available = codec != OutputCodec::Mp3 || mint::MP3_AVAILABLE;
                    ui.add_enabled_ui(available, |ui| {
                        ui.selectable_value(&mut chosen, codec.to_string(), codec.to_string())
                            .on_disabled_hover_text("this build has no MP3 encoder");
                    });
                }
            });
        if chosen != current {
            *edit = Some(Edit { field: "codec", value: Some(Value::from(chosen)) });
        }
    });
}

fn format_row(ui: &mut Ui, explained: &TargetExplanation, edit: &mut Option<Edit>) {
    row(ui, explained, "format", |ui, explanation| {
        let stated = explanation
            .origin
            .stated
            .iter()
            .rev()
            .find(|(layer, _)| matches!(layer, Layer::Explicit))
            .map(|(_, value)| value.clone());
        let mut chosen = stated.clone();
        ComboBox::from_id_salt("format")
            .selected_text(stated.clone().unwrap_or_else(|| "—".to_string()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut chosen, None, "— (inherit)");
                for format in BitDepthFormat::ALL {
                    ui.selectable_value(
                        &mut chosen,
                        Some(format.to_string()),
                        format.to_string(),
                    );
                }
            });
        if chosen != stated {
            *edit = Some(Edit { field: "format", value: chosen.map(Value::from) });
        }
    });
}

fn lufs_row(ui: &mut Ui, explained: &TargetExplanation, edit: &mut Option<Edit>) {
    row(ui, explained, "lufs", |ui, explanation| {
        let stated: Option<f64> = explanation
            .origin
            .stated
            .iter()
            .rev()
            .find(|(layer, _)| matches!(layer, Layer::Explicit))
            .and_then(|(_, value)| value.parse().ok());
        let mut enabled = stated.is_some();
        let mut value = stated.unwrap_or(-14.0);
        ui.horizontal(|ui| {
            let toggled = ui.checkbox(&mut enabled, "").changed();
            let dragged = ui
                .add_enabled(
                    enabled,
                    egui::DragValue::new(&mut value).speed(0.1).range(-40.0..=0.0).suffix(" LUFS"),
                )
                .changed();
            if toggled || dragged {
                *edit = Some(Edit {
                    field: "lufs",
                    // Unticking removes the key rather than writing a default: no loudness
                    // step at all is a different deliverable from one targeting −14.
                    value: enabled.then(|| Value::from(value)),
                });
            }
        });
    });
}

fn dir_row(ui: &mut Ui, explained: &TargetExplanation, edit: &mut Option<Edit>) {
    row(ui, explained, "dir", |ui, explanation| {
        let stated = explanation
            .origin
            .stated
            .iter()
            .rev()
            .find(|(layer, _)| matches!(layer, Layer::Explicit))
            .map(|(_, value)| value.clone());
        let mut text = stated.clone().unwrap_or_default();
        let response = ui.add(
            egui::TextEdit::singleline(&mut text)
                .hint_text(&explanation.value)
                .desired_width(220.0),
        );
        if response.changed() {
            *edit = Some(Edit {
                field: "dir",
                value: (!text.is_empty()).then(|| Value::from(text.clone())),
            });
        }
    });
}

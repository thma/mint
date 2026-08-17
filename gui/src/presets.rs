//! The preset picker is a diff, not a dropdown (Part III §3).
//!
//! A preset is the one edit in this whole application that changes twenty other values at
//! once, and a dropdown that simply applies it is the least honest widget mint could grow: a
//! `cd → apple-music` click silently moves the ceiling, the loudness target and the rate,
//! and the only way to find out which is to read the README and the merge rules. So the
//! candidate is *resolved* before it is committed, and what the picker shows is the
//! difference between two answers the library gave.
//!
//! Nothing here knows what any preset contains. The diff is computed by writing the
//! candidate into a **copy** of the document, handing that copy back to
//! `Config::parse_toml_str` + `resolve_target_explained`, and comparing the two explanations
//! field by field. That costs one extra resolve per candidate — the proposal's own estimate,
//! and the status bar prints what a resolve costs — and it buys the property that the diff
//! cannot disagree with what committing actually does, because committing runs the same
//! write against the real document.
//!
//! Deliberately egui-free: the whole file is a function from text to a verdict, which is
//! what makes `gui/tests/preset_diff.rs` able to assert on the diff rather than on pixels.

use mint::config::{Config, Layer, TargetExplanation};
use toml_edit::Value;

use crate::write_back::ConfigDocument;

/// One row of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub field: &'static str,
    /// Rendered effective value now, and under the candidate. The library's own rendering
    /// in both cases — the same string the inspector's value column shows.
    pub before: String,
    pub after: String,
    pub verdict: Verdict,
}

/// What the candidate does to one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The delivered value moves. `Layer` is what wins it afterwards, so a row can say
    /// *whose* value it now is rather than only that it changed.
    Changed(Option<Layer>),
    /// The candidate states this field and loses: something the target says itself wins.
    /// The row that stops a picker from looking like it did nothing.
    Overridden,
    /// The candidate states it and lands on the value that was already effective.
    Unchanged,
}

/// What the picker has to say about a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// mint refuses the target under this candidate, in mint's own words. Not a failure of
    /// the preview — it is the most useful thing the preview can say, and it is the reason
    /// the candidate is resolved before it is written: every PCM preset sets `format`, so
    /// picking one on a DSD target is a load-time error, and finding that out *after* the
    /// click means finding it out from a document that no longer loads.
    Refused(String),
    Diff(Vec<Change>),
}

impl Preview {
    /// How many delivered values move. The line under the diff, and the number that decides
    /// whether committing is a small edit or a different deliverable.
    pub fn changed(&self) -> usize {
        match self {
            Preview::Refused(_) => 0,
            Preview::Diff(changes) => {
                changes.iter().filter(|c| matches!(c.verdict, Verdict::Changed(_))).count()
            }
        }
    }
}

/// Resolve `target` twice — as the document stands, and with `candidate` as its preset — and
/// report the difference. `candidate` of `None` is the "no preset" entry in the picker.
pub fn preview(text: &str, target: &str, candidate: Option<&str>) -> Preview {
    let before = match explained(text, target) {
        Ok(before) => before,
        Err(err) => return Preview::Refused(err),
    };

    // A copy of the real document, written through the real writer: the preview and the
    // commit differ only in which `ConfigDocument` they were handed, so a preview that says
    // "1 field changes" cannot be followed by a commit that changes two.
    let mut doc = match ConfigDocument::parse(text) {
        Ok(doc) => doc,
        Err(err) => return Preview::Refused(format!("{err:#}")),
    };
    let written = match candidate {
        Some(name) => doc.set(target, "preset", Value::from(name)),
        None => doc.clear(target, "preset").map(|_| ()),
    };
    if let Err(err) = written {
        return Preview::Refused(format!("{err:#}"));
    }

    let after = match explained(&doc.text(), target) {
        Ok(after) => after,
        Err(err) => return Preview::Refused(err),
    };
    Preview::Diff(diff(&before, &after))
}

/// The fields a preset supplies to this target, by name, in inspector order.
///
/// Read off the explanation rather than out of `preset()` (which is private, and rightly so):
/// a field is "set by the preset" exactly when the preset layer stated it, which is what
/// `FieldOrigin::stated` records. So this cannot drift from what the merge saw.
pub fn set_by_preset(explanation: &TargetExplanation) -> Vec<&'static str> {
    explanation
        .fields
        .iter()
        .filter(|field| {
            field.origin.stated.iter().any(|(layer, _)| matches!(layer, Layer::Preset(_)))
        })
        .map(|field| field.field)
        .collect()
}

fn diff(before: &TargetExplanation, after: &TargetExplanation) -> Vec<Change> {
    let mut changes = Vec::new();
    for field in &after.fields {
        // `preset` itself is the thing being chosen; the picker's own line already reads
        // `cd → apple-music`, and repeating it as the first row of its own diff is noise.
        if field.field == "preset" {
            continue;
        }
        let Some(was) = before.field(field.field) else { continue };
        let states_it =
            field.origin.stated.iter().any(|(layer, _)| matches!(layer, Layer::Preset(_)));

        let verdict = if was.value != field.value {
            Verdict::Changed(field.origin.layer.clone())
        } else if !states_it {
            // Neither touched by the candidate nor moved by it. Most of the 22 rows.
            continue;
        } else if matches!(field.origin.layer, Some(Layer::Explicit)) {
            Verdict::Overridden
        } else {
            Verdict::Unchanged
        };
        changes.push(Change {
            field: field.field,
            before: was.value.clone(),
            after: field.value.clone(),
            verdict,
        });
    }
    changes
}

/// Parse-without-validating, then explain one target. `parse_toml_str` rather than
/// `from_toml_str` for the same reason the Problems panel uses it: another target in the
/// document being broken is not this picker's business, and refusing to preview because of
/// it would hide the answer exactly when the config is mid-edit.
fn explained(text: &str, target: &str) -> Result<TargetExplanation, String> {
    Config::parse_toml_str(text)
        .and_then(|config| config.resolve_target_explained(target))
        .map(|(_, explanation)| explanation)
        .map_err(|err| format!("{err:#}"))
}

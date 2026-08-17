//! Spike gate S3: the config drawer — a TOML editor with syntax colour and an error
//! squiggle located at the byte the parser objected to.
//!
//! The gate's fail signal is *"the config drawer is a from-scratch text engine"*. It is
//! not: `TextEdit::multiline` plus a `layouter` closure does the whole job, and the
//! highlighter below is forty lines of line-splitting because TOML's line grammar is
//! simple enough not to need a parser.
//!
//! The span comes from `toml_edit::TomlError::span()` — the same crate the writer uses, so
//! no second TOML implementation enters the build for the sake of an underline. Semantic
//! errors (mint's own: an unresolvable target, a colliding output path) have no span by
//! nature and are shown as text under the editor, which is the honest placement.
//!
//! Phase 2 adds one more span source: `AppState::reveal`, which another screen sets to say
//! "show them this". The inspector's `[ open [defaults] ]` is the caller — a "why" panel that
//! names the block a value came from and then leaves you to find it is half an answer.

use egui::text::{LayoutJob, TextFormat};
use egui::text::CCursor;
use egui::text_selection::CCursorRange;
use egui::{Color32, FontId, TextStyle, Ui};

use crate::state::AppState;

const COMMENT: Color32 = Color32::from_rgb(120, 130, 120);
const KEY: Color32 = Color32::from_rgb(150, 190, 240);
const STRING: Color32 = Color32::from_rgb(200, 175, 120);
const NUMBER: Color32 = Color32::from_rgb(190, 160, 220);
const HEADER: Color32 = Color32::from_rgb(130, 210, 170);
const PLAIN: Color32 = Color32::from_rgb(215, 215, 215);
const BAD: Color32 = Color32::from_rgb(230, 100, 90);

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    // Where the parser stopped, if it stopped. Recomputed per frame from the current text
    // rather than cached against it — a cache keyed on a string is a staleness bug waiting
    // for the one frame the key does not change but the text does.
    let bad_span = state.text.parse::<toml_edit::DocumentMut>().err().and_then(|e| e.span());

    let font = FontId::monospace(12.0);
    let mut layouter = |ui: &Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = highlight(buffer.as_str(), &bad_span, font.clone());
        job.wrap.max_width = wrap_width;
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    };

    // Taken, not read: acting on it twice would fight the user's scrolling on every frame
    // after the one they asked on.
    let reveal = state.reveal.take();

    let mut text = std::mem::take(&mut state.text);
    let output = egui::ScrollArea::vertical()
        .id_salt("config-editor")
        .show(ui, |ui| {
            egui::TextEdit::multiline(&mut text)
                .code_editor()
                .desired_width(f32::INFINITY)
                .desired_rows(24)
                .font(TextStyle::Monospace)
                .layouter(&mut layouter)
                .show(ui)
        })
        .inner;
    let changed = output.response.response.changed();
    if let Some(span) = reveal {
        scroll_to(ui, &output, &text, span);
    }
    state.text = text;
    if changed {
        // Per keystroke. `Config::from_toml_str` exists so this does not need a temp file,
        // and the status bar prints how long it took so the claim is checkable.
        state.dirty = true;
        state.revalidate();
    }
}

/// Select a byte range and bring it into view.
///
/// egui scrolls to the cursor by itself only when the *widget* moved it (a keypress, a
/// click); a range stored from outside looks unchanged to it, so the scroll is done here from
/// the galley the widget just laid out. The conversion is the fiddly part and the reason it
/// is one function: `toml_edit` spans are byte offsets and egui cursors are **character**
/// indices, which differ the moment a config has a comment with an em dash in it.
fn scroll_to(
    ui: &mut Ui,
    output: &egui::text_edit::TextEditOutput,
    text: &str,
    span: std::ops::Range<usize>,
) {
    let chars = |byte: usize| text.get(..byte).map_or(0, |head| head.chars().count());
    let (start, end) = (chars(span.start), chars(span.end));

    let mut state = output.state.clone();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(start), CCursor::new(end))));
    state.store(ui.ctx(), output.response.response.id);

    let rect = output
        .galley
        .pos_from_cursor(CCursor::new(start))
        .translate(output.galley_pos.to_vec2());
    ui.scroll_to_rect(rect, Some(egui::Align::Center));
}

/// Colour a TOML document, underlining the byte range the parser rejected.
///
/// Line-based on purpose: the alternative is a second parse per frame to get a token
/// stream, and every construct that this gets slightly wrong (a `#` inside a string) is
/// cosmetic, whereas a second parser is a second opinion about what the document means.
fn highlight(text: &str, bad_span: &Option<std::ops::Range<usize>>, font: FontId) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut offset = 0usize;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        if indent > 0 {
            push(&mut job, &line[..indent], PLAIN, &font, false);
        }
        if trimmed.starts_with('#') {
            push(&mut job, trimmed, COMMENT, &font, in_span(bad_span, offset + indent, trimmed));
        } else if trimmed.starts_with('[') {
            push(&mut job, trimmed, HEADER, &font, in_span(bad_span, offset + indent, trimmed));
        } else if let Some(eq) = trimmed.find('=') {
            let (key, rest) = trimmed.split_at(eq);
            push(&mut job, key, KEY, &font, false);
            push(&mut job, "=", PLAIN, &font, false);
            let value = &rest[1..];
            // A trailing comment is part of the value's line, not of the value.
            let (value, comment) = match value.find('#') {
                Some(hash) => (&value[..hash], Some(&value[hash..])),
                None => (value, None),
            };
            let colour = if value.trim_start().starts_with('"') { STRING } else { NUMBER };
            push(&mut job, value, colour, &font, in_span(bad_span, offset + indent + eq + 1, value));
            if let Some(comment) = comment {
                push(&mut job, comment, COMMENT, &font, false);
            }
        } else {
            push(&mut job, trimmed, PLAIN, &font, in_span(bad_span, offset + indent, trimmed));
        }
        offset += line.len();
    }
    job
}

fn in_span(bad: &Option<std::ops::Range<usize>>, start: usize, piece: &str) -> bool {
    match bad {
        Some(span) => start < span.end && span.start < start + piece.len().max(1),
        None => false,
    }
}

fn push(job: &mut LayoutJob, text: &str, colour: Color32, font: &FontId, bad: bool) {
    if text.is_empty() {
        return;
    }
    let mut format = TextFormat::simple(font.clone(), colour);
    if bad {
        // The squiggle. egui has no wavy underline, so a solid one in the error colour is
        // the closest honest thing — it marks the same range either way.
        format.underline = egui::Stroke::new(1.5, BAD);
    }
    job.append(text, 0.0, format);
}

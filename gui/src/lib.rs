//! `mint-gui` — the egui workbench from `docs/gui-proposal.md`.
//!
//! A library as well as a binary so the integration tests can drive the same code the
//! window does. Everything visual lives above one hard rule (proposal §5.2): **the GUI
//! never re-derives.** It calls `resolve_target`, `resolve_target_explained`, `describe`,
//! `estimated_output_bytes`, `delivery_blocker`, `render_output_path`, `probe_source`,
//! `output_conflict`, `check_collisions`, `check_planned_outputs` and `process_with`, and it
//! renders what comes back. It never models the four-layer merge, the FLAC 96 kHz rule, the
//! DSD 5.1 rule or the own-layer exception, it never constructs a `ResolvedTarget`, it never
//! re-serializes the document, and every diagnostic on screen is mint's own bytes.

pub mod app;
pub mod chain;
pub mod editor;
pub mod history;
pub mod inspector;
pub mod plan;
pub mod presets;
pub mod problems;
pub mod report;
pub mod run;
pub mod sources;
pub mod state;
pub mod targets;
pub mod watch;
pub mod write_back;

/// `cli.rs`'s own `--force` help text, quoted.
///
/// One const rather than one per widget: the project bar's checkbox and the Problems
/// panel's fix button tick the *same* flag, and two copies of a promise drift the first
/// time one of them is edited. It is quoted rather than written because the widening it
/// describes is the surprising one CLAUDE.md's gotcha list warns about — `overwrite` means
/// the input master too — and paraphrasing that is how the surprise gets lost.
pub const FORCE_HELP: &str = "Replace files that are already at the output paths, the input \
                              file included (equivalent to overwrite = true on every target).";

/// The three states anything on screen can be in, as one palette.
///
/// Shared rather than re-typed per module so that "ok" is the same green in the Targets
/// pane, the Plan matrix and the Report table — a status glyph that changes shade between
/// screens reads as a different status. Colour is never the only carrier: capability
/// checklist #11 requires a glyph beside it everywhere, because roughly one man in twelve
/// cannot tell this green from this red.
pub mod colors {
    use egui::Color32;

    pub const OK: Color32 = Color32::from_rgb(120, 200, 140);
    pub const WARN: Color32 = Color32::from_rgb(200, 170, 90);
    pub const BAD: Color32 = Color32::from_rgb(220, 90, 80);
}

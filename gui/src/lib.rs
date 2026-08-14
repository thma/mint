//! `mint-gui` — the egui workbench from `docs/gui-proposal.md`.
//!
//! A library as well as a binary so the integration tests can drive the same code the
//! window does. Everything visual lives above one hard rule (proposal §5.2): **the GUI
//! never re-derives.** It calls `resolve_target`, `resolve_target_explained`, `describe`,
//! `estimated_output_bytes`, `delivery_blocker`, `render_output_path`, `probe_source`,
//! `output_conflict` and `process_with`, and it renders what comes back. It never models
//! the four-layer merge, the FLAC 96 kHz rule, the DSD 5.1 rule or the own-layer exception,
//! and it never constructs a `ResolvedTarget`.

pub mod app;
pub mod editor;
pub mod form;
pub mod plan;
pub mod run;
pub mod state;
pub mod write_back;

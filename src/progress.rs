//! Cancellation and progress for a single job, as two borrowed pointers.
//!
//! The constraint this design is built around is CLAUDE.md's: *the buffer is the budget*.
//! [`JobControl`] allocates nothing, is sized in pointers rather than frames, and the
//! callback is handed a stage and a fraction — never audio. With [`JobControl::default`]
//! both fields are `None` and every hook collapses to two `Option` checks per block, which
//! is why `pipeline::process` can keep its old signature and the CLI can keep calling it.
//!
//! The sink is `Sync`, not merely `Send`: jobs run under a rayon pool, so several of them
//! call it concurrently.
//!
//! **Cancellation semantics.** A cancelled stage returns [`Cancelled`] as an ordinary
//! `Err`, which unwinds through the same `?` path as a real failure — past
//! `audio::atomic_write::Staged`'s `Drop`, which removes the staging file. So a cancelled
//! job leaves the previous render intact and no partial file behind. That is what a
//! mastering engineer will assume, and it is worth not breaking.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

/// The stages a job passes through, in the order `pipeline::process` runs them. Not every
/// stage runs for every target — the chain is derived — and two of them are deliberately
/// absent: the loudness *meters* feed the whole buffer to `ebur128` in one call, and the
/// WAV/FLAC/MP3 writers are single blocking calls into encoder crates. Neither can report
/// anything but "started", so neither pretends to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Probe,
    Decode,
    AnalyzeSource,
    Loudness,
    Resample,
    PostSrcVerify,
    Ceiling,
    AnalyzeDelivered,
    Quantize,
    Encode,
}

impl Stage {
    pub fn label(self) -> &'static str {
        match self {
            Stage::Probe => "probing",
            Stage::Decode => "decoding",
            Stage::AnalyzeSource => "analyzing source",
            Stage::Loudness => "loudness",
            Stage::Resample => "resampling",
            Stage::PostSrcVerify => "verifying after resample",
            Stage::Ceiling => "enforcing ceiling",
            Stage::AnalyzeDelivered => "analyzing delivered",
            Stage::Quantize => "quantizing",
            Stage::Encode => "encoding",
        }
    }
}

/// Returned when a job was asked to stop. A distinct type so a caller can tell "the user
/// cancelled" from "the render failed" without matching on message text.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("cancelled")]
pub struct Cancelled;

/// Borrowed cancellation flag and progress sink. Two pointers; nothing owned.
#[derive(Default, Clone, Copy)]
pub struct JobControl<'a> {
    pub cancel: Option<&'a AtomicBool>,
    pub progress: Option<&'a (dyn Fn(Stage, f64) + Sync)>,
}

impl<'a> JobControl<'a> {
    pub fn new(cancel: &'a AtomicBool, progress: &'a (dyn Fn(Stage, f64) + Sync)) -> Self {
        Self { cancel: Some(cancel), progress: Some(progress) }
    }

    /// `Err(Cancelled)` if the job has been asked to stop.
    ///
    /// `Relaxed` is the right ordering: the flag guards nothing but itself, and a check
    /// that observes the cancel one block late is indistinguishable from one that arrived
    /// one block later.
    pub fn check(&self) -> Result<()> {
        match self.cancel {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(Cancelled.into()),
            _ => Ok(()),
        }
    }

    /// Report a stage as `fraction` complete (0.0–1.0), or as indeterminate by passing the
    /// fraction the caller has — a stage with nothing to count reports 0.0 and then 1.0.
    pub fn report(&self, stage: Stage, fraction: f64) {
        if let Some(sink) = self.progress {
            sink(stage, fraction.clamp(0.0, 1.0));
        }
    }

    /// A counter for one stage's loop: checks cancellation on every tick and reports only
    /// when the number has moved enough to be worth a repaint.
    pub fn stage(&self, stage: Stage, total: u64) -> StageProgress<'a> {
        StageProgress { ctl: *self, stage, total, last_reported: -1.0 }
    }
}

/// Per-loop progress state. Throttled at half a percent because the hooks sit in tight
/// block loops — `encode_dsf` alone iterates ~100 000 times on a five-minute DSD256 render,
/// and a repaint request per iteration would cost more than the encode.
pub struct StageProgress<'a> {
    ctl: JobControl<'a>,
    stage: Stage,
    total: u64,
    last_reported: f64,
}

impl StageProgress<'_> {
    /// One iteration of the loop is done: `done` units of `total`. Returns `Err(Cancelled)`
    /// if the job should stop, so the call site is a bare `progress.tick(n)?;`.
    pub fn tick(&mut self, done: u64) -> Result<()> {
        self.ctl.check()?;
        if self.ctl.progress.is_none() {
            return Ok(());
        }
        let fraction = if self.total == 0 { 1.0 } else { done as f64 / self.total as f64 };
        if fraction - self.last_reported >= 0.005 {
            self.last_reported = fraction;
            self.ctl.report(self.stage, fraction);
        }
        Ok(())
    }

    /// The loop finished. Reported unconditionally so a listener always sees 1.0 for a
    /// stage that completed, whatever the throttle did to the last tick.
    pub fn finish(&self) {
        self.ctl.report(self.stage, 1.0);
    }
}

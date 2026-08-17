//! The derived chain, as data.
//!
//! `ResolvedTarget::describe` has always been the only place the derived chain was visible
//! from outside `pipeline::process`, and it was visible only as prose: a `Vec<String>` a
//! reader could print and nothing else could ask a question of. That is fine for
//! `--dry-run`, which prints it, and wrong for every other consumer — a front end wanting a
//! glyph per step, a category, or "which step is running right now" had a sentence and a
//! substring search.
//!
//! So the chain is derived once, as [`ChainStep`]s carrying the *inputs to each sentence*,
//! and [`ChainStep::line`] is the single place those inputs become the sentence.
//! `describe()` is now that map and nothing else. The rule this file exists to keep is the
//! one `docs/gui-proposal.md` §6 Tier 3 states: **one producer plus a formatter, never a
//! parallel third representation.** Concretely — no consumer may build a step's prose from
//! its fields, and no new prose may be written here that is not `line()`'s. The bytes
//! matter beyond taste: `README.md`'s dry-run sample quotes them, and ~15 assertions across
//! `tests/config_parse.rs` and `tests/dsd_pipeline.rs` read them as substrings.
//!
//! What this deliberately does **not** do is become a third description of the chain
//! alongside `pipeline::process` and `estimated_output_bytes`. It is a refactor of the
//! second one, not a new one: `ResolvedTarget::chain` holds exactly the branch structure
//! `describe()` held, and CLAUDE.md's rule that any change to the canonical order must be
//! mirrored there now means mirrored *here*.

use crate::audio::buffer::OutputSampleFormat;
use crate::config::{
    DitherMode, DitherStrength, LimiterCharacter, OnClipPolicy, ResampleQuality,
    dither_strength_tag, effective_dither_tag, format_tag, limiter_policy_tag, quality_tag,
};
use crate::ops::dsd::DsdRate;
use crate::progress::Stage;

/// The five values that decide how the ceiling is held, travelling together because every
/// sentence that mentions one mentions all of them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimiterPolicy {
    pub on_clip: OnClipPolicy,
    pub character: LimiterCharacter,
    pub soft_clip: bool,
    pub link_channels: bool,
    pub max_limiting_db: f64,
}

/// Which container the PCM tail writes into. Resolved from `codec` + `bwf` at the one place
/// that decision is made, so a reader is not handed two fields and the rule joining them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainContainer {
    Wav,
    /// WAV plus the `bext` chunk — a different sentence, not a flag on the same one.
    BroadcastWav,
    Flac,
}

/// A coarse category, for a front end that wants a glyph or a colour per row. Deliberately
/// coarser than the step list: it is a *presentation* grouping, and the two classifications
/// worth knowing are that DSD's sigma-delta loop belongs with the bit-depth decision — it is
/// the DSD quantizer, and the only place the buffer meets a one-bit grid — and that a note is
/// not a step.
///
/// `Depth` rather than `Quantize` for that group, because one of the three steps in it is
/// `write f64 (no quantization)` and a row labelled "quantize" beside that sentence
/// contradicts it. What the group actually shares is that each step decides the delivered
/// sample depth: onto an integer grid, onto a one-bit grid, or onto no grid at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainStepKind {
    Loudness,
    Rate,
    Verify,
    Depth,
    Write,
    Note,
}

impl ChainStepKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Loudness => "loudness",
            Self::Rate => "rate",
            Self::Verify => "verify",
            Self::Depth => "depth",
            Self::Write => "write",
            Self::Note => "note",
        }
    }
}

/// One step of the chain mint derived for an (input rate × target) pair.
///
/// Exhaustive on purpose and **not** `#[non_exhaustive]`: a new codec's step has to be a
/// compile error in every renderer, for the same reason the `match target.codec` arms in
/// `pipeline.rs` and `describe()` are exhaustive. A catch-all in a front end is how a step
/// silently stops being shown.
#[derive(Debug, Clone, PartialEq)]
pub enum ChainStep {
    /// Loudness normalization to a stated target; enforces the ceiling itself.
    Loudness { lufs: f64, ceiling_dbtp: f64, limiter: LimiterPolicy },
    /// No `lufs`: the ceiling is enforced on its own.
    Transcode { ceiling_dbtp: f64, limiter: LimiterPolicy },
    /// For a DSD target `to_hz` is the *intermediate* PCM rate, not the 1-bit rate.
    Resample { from_hz: u32, to_hz: u32, quality: ResampleQuality, derived: bool },
    KeepRate { rate_hz: u32 },
    /// The two passes a resample brings with it. `loudness` is false when the target stated
    /// no `lufs`, in which case only the ceiling is re-checked.
    PostSrcVerify { loudness: bool },
    Quantize { format: OutputSampleFormat, dither: DitherMode, strength: DitherStrength },
    /// A float output format: the buffer is already f64, so there is no grid to map onto.
    WriteFloat { format: OutputSampleFormat },
    WriteContainer { container: ChainContainer },
    EncodeMp3 { kbps: u32 },
    /// `factor` is `None` when the rate pair admits no power-of-two ratio — a config that
    /// will fail, described rather than hidden.
    Interpolate { factor: Option<u32>, to_hz: u32, rate: DsdRate, quality: ResampleQuality },
    Modulate { order: usize, input_scale_percent: f64 },
    WriteDsf,
    /// Not a step: a remark about one. Kept in the list because `--dry-run` prints it inline
    /// and a reader would not find it anywhere else.
    Note(ChainNote),
}

/// The remarks [`ChainStep::Note`] can carry. An enum rather than a string so the one that
/// is a bug report is distinguishable from the one that is advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainNote {
    /// DSD512 is past what the DSF spec names; player support is thin.
    Dsd512PlayerSupport,
    /// A DSD codec resolved without a `dsd` rate. Unreachable through `resolve_target`,
    /// which pairs them — so if it is ever rendered, the resolution is broken.
    DsdRateMissing,
}

impl ChainStep {
    /// The `--dry-run` line for this step. **The only producer of this prose.**
    ///
    /// `README.md`'s dry-run sample and the substring assertions in `tests/config_parse.rs`
    /// and `tests/dsd_pipeline.rs` read these bytes; a reworded line is a documentation and
    /// test change, not a cosmetic one.
    pub fn line(&self) -> String {
        match self {
            Self::Loudness { lufs, ceiling_dbtp, limiter } => format!(
                "loudness -> {lufs:.1} LUFS (ceiling {ceiling_dbtp:.1} dBTP, {})",
                limiter.tag()
            ),
            Self::Transcode { ceiling_dbtp, limiter } => format!(
                "transcode (no loudness normalization); ceiling {ceiling_dbtp:.1} dBTP \
                 enforced ({})",
                limiter.tag()
            ),
            Self::Resample { from_hz, to_hz, quality, derived } => {
                let why = if *derived { ", derived" } else { "" };
                format!("resample {from_hz} -> {to_hz} Hz ({}{why})", quality_tag(*quality))
            }
            Self::KeepRate { rate_hz } => format!("keep sample rate {rate_hz} Hz"),
            Self::PostSrcVerify { loudness } => {
                let mut line = "re-check true peak against the ceiling".to_string();
                if *loudness {
                    line.push_str(" and re-verify loudness (corrected within the headroom)");
                }
                line
            }
            Self::Quantize { format, dither, strength } => format!(
                "quantize -> {} + {} ({})",
                format_tag(*format),
                effective_dither_tag(*dither, *strength, *format),
                dither_strength_tag(*strength)
            ),
            Self::WriteFloat { format } => {
                format!("write {} (no quantization)", format_tag(*format))
            }
            Self::WriteContainer { container } => format!("write {}", container.tag()),
            Self::EncodeMp3 { kbps } => {
                format!("encode MP3 {kbps} kbps (lossy; bit depth & dither not applicable)")
            }
            Self::Interpolate { factor, to_hz, rate, quality } => {
                let ratio = factor.map_or_else(|| "?".to_string(), |f| format!("x{f}"));
                format!(
                    "interpolate {ratio} -> {to_hz} Hz ({}, {}), streaming",
                    rate.tag(),
                    quality_tag(*quality)
                )
            }
            Self::Modulate { order, input_scale_percent } => format!(
                "sigma-delta modulate to 1 bit (order {order}; 0 dBFS -> \
                 {input_scale_percent:.0} % modulation)"
            ),
            Self::WriteDsf => "write DSF (planar 4096-byte blocks, LSB first)".to_string(),
            Self::Note(note) => note.text().to_string(),
        }
    }

    pub fn kind(&self) -> ChainStepKind {
        match self {
            Self::Loudness { .. } | Self::Transcode { .. } => ChainStepKind::Loudness,
            Self::Resample { .. } | Self::KeepRate { .. } | Self::Interpolate { .. } => {
                ChainStepKind::Rate
            }
            Self::PostSrcVerify { .. } => ChainStepKind::Verify,
            // `Modulate` belongs here and not under Write: the sigma-delta loop *is* the
            // DSD quantizer, which is the whole reason the bit-depth stage is skipped in
            // front of it.
            Self::Quantize { .. } | Self::WriteFloat { .. } | Self::Modulate { .. } => {
                ChainStepKind::Depth
            }
            Self::WriteContainer { .. } | Self::EncodeMp3 { .. } | Self::WriteDsf => {
                ChainStepKind::Write
            }
            Self::Note(_) => ChainStepKind::Note,
        }
    }

    /// The pipeline stage that runs this step, for a front end holding a live [`Stage`] from
    /// a `JobControl` progress callback and wanting to point at the row it belongs to.
    ///
    /// `None` means **no stage will ever report this row**, and every case is an honest one.
    /// A note is not work. `KeepRate` and `WriteFloat` describe an *absence* — no resample,
    /// no quantization — and `WriteFloat` is the non-obvious one: `bitdepth::apply` is still
    /// called for a float target, but `reduced` is false (nothing reduces *to* f32), so it
    /// returns before `quantize_in_place`, which is where `Stage::Quantize` is reported.
    /// `WriteContainer` is `None` for the reason `progress::Stage`'s own doc gives: the
    /// WAV and FLAC writers are single blocking calls into encoder crates with nothing to
    /// count, so they report nothing at all — only the MP3 arm reports `Encode`, and that is
    /// `EncodeMp3`. Claiming a stage here would leave a front end waiting for a signal that
    /// is not coming.
    ///
    /// The reverse gap is worth knowing too and is not fixable from this side: on a target
    /// that states `lufs` the limiter inside the loudness step reports `Stage::Ceiling`, and
    /// there is no `Transcode` row for it to belong to. A front end highlighting by stage
    /// should leave the highlight where it was rather than clear it.
    ///
    /// Not a bijection in the other direction either, and saying so is the point: the three
    /// DSD steps are one `Stage::Encode`, because `encode_dsf` interpolates, modulates and
    /// writes inside one block loop and reports one fraction for all of it.
    pub fn stage(&self) -> Option<Stage> {
        match self {
            Self::Loudness { .. } => Some(Stage::Loudness),
            // The no-loudness path's ceiling enforcement is the limiter and the true-peak
            // meter in front of it, both of which report `Ceiling`.
            Self::Transcode { .. } => Some(Stage::Ceiling),
            Self::Resample { .. } => Some(Stage::Resample),
            Self::PostSrcVerify { .. } => Some(Stage::PostSrcVerify),
            Self::Quantize { .. } => Some(Stage::Quantize),
            Self::EncodeMp3 { .. }
            | Self::Interpolate { .. }
            | Self::Modulate { .. }
            | Self::WriteDsf => Some(Stage::Encode),
            Self::KeepRate { .. }
            | Self::WriteFloat { .. }
            | Self::WriteContainer { .. }
            | Self::Note(_) => None,
        }
    }

    pub fn is_note(&self) -> bool {
        matches!(self, Self::Note(_))
    }
}

impl LimiterPolicy {
    /// The parenthesised policy phrase both loudness lines carry.
    fn tag(&self) -> String {
        limiter_policy_tag(
            self.on_clip,
            self.character,
            self.soft_clip,
            self.link_channels,
            self.max_limiting_db,
        )
    }
}

impl ChainContainer {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Wav => "WAV",
            Self::BroadcastWav => "Broadcast WAV (bext + R128 metadata)",
            Self::Flac => "FLAC",
        }
    }
}

impl ChainNote {
    pub fn text(self) -> &'static str {
        match self {
            Self::Dsd512PlayerSupport => "note: dsd512 has limited player support",
            Self::DsdRateMissing => "DSD target without a dsd rate (config bug)",
        }
    }
}

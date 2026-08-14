use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::audio::buffer::OutputSampleFormat;
use crate::audio::{io_write, io_write_dsd};
use crate::dither::ShapingCurve;
use crate::ops::dsd::modulator as dsd_modulator;
use crate::ops::dsd::{self, DsdRate};

/// Top-level config: optional metadata, shared defaults, and one or more named
/// output targets. Each `[target.NAME]` describes a *deliverable* (rate, depth,
/// loudness, ceiling); the required DSP steps are derived per input file rather
/// than spelled out — see `crate::pipeline`.
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub meta: Option<MetaConfig>,
    /// Baseline applied to every target; any field is overridable per-target.
    #[serde(default)]
    pub defaults: RawTarget,
    /// Named targets: `[target.streaming]`, `[target.cd]`, ...
    #[serde(rename = "target", default)]
    pub targets: BTreeMap<String, RawTarget>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct MetaConfig {
    pub name: Option<String>,
}

/// A target (or the `[defaults]` block) as written in TOML. Every field is
/// optional so the layers (builtin < defaults < preset < explicit) can be merged
/// with simple `Option::or` precedence before resolving to a `ResolvedTarget`.
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct RawTarget {
    /// Name of a built-in preset to seed this target's spec fields.
    pub preset: Option<String>,
    pub rate: Option<u32>,
    pub format: Option<BitDepthFormat>,
    pub lufs: Option<f64>,
    pub ceiling_dbtp: Option<f64>,
    pub on_clip: Option<OnClipPolicy>,
    /// Limiter behavior when `on_clip = "limit"`.
    pub limiter_character: Option<LimiterCharacter>,
    /// Optional post-limiter soft clipping near the ceiling.
    pub limiter_soft_clip: Option<bool>,
    /// Channel envelope linking in the limiter.
    pub limiter_link_channels: Option<bool>,
    /// Warn when the loudness step's limiter applies more than this many dB of peak
    /// gain reduction to reach `lufs` under `ceiling_dbtp`. Surfaces the otherwise
    /// silent "source got crushed to hit the target" case (default 1.0 — conservative,
    /// so even mild limiting is flagged).
    pub warn_limiting_db: Option<f64>,
    /// Peak gain reduction (dB) `on_clip = "maximize"` may spend before it stops adding
    /// makeup gain (default 8.0). The safety cap that keeps a mis-set hot target from
    /// grinding a master flat; ignored by the other policies.
    pub max_limiting_db: Option<f64>,
    pub quality: Option<ResampleQuality>,
    pub dither: Option<DitherMode>,
    pub dither_strength: Option<DitherStrength>,
    /// Phase 4.4: Stereo/multi-channel dither correlation mode (default: Decorrelated).
    pub dither_correlation: Option<DitherCorrelation>,
    /// Output container/codec: `wav` (default), `flac`, `mp3` or `dsf`. Orthogonal to
    /// `format` — FLAC reuses the integer bit depth; MP3 ignores it (lossy) and DSF
    /// rejects it outright (the modulator is the quantizer).
    pub codec: Option<OutputCodec>,
    /// DSD rate for the 1-bit codecs: `dsd64` | `dsd128` | `dsd256` | `dsd512`. Required
    /// with `codec = "dsf"`, rejected without it.
    pub dsd: Option<DsdRate>,
    /// Write a Broadcast WAV `bext` chunk (with EBU R128 loudness metadata). WAV only.
    pub bwf: Option<bool>,
    /// MP3 CBR bitrate in kbps (default 320). Only used when `codec = "mp3"`.
    pub mp3_bitrate: Option<u32>,
    pub dir: Option<PathBuf>,
    pub naming: Option<String>,
    pub overwrite: Option<bool>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ResampleQuality {
    Lq,
    Mq,
    Hq,
    Vhq,
}

impl ResampleQuality {
    pub const ALL: [ResampleQuality; 4] = [
        ResampleQuality::Lq,
        ResampleQuality::Mq,
        ResampleQuality::Hq,
        ResampleQuality::Vhq,
    ];
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnClipPolicy {
    Limit,
    ReduceGain,
    Warn,
    /// Like `Limit`, but with makeup-gain convergence: after limiting eats loudness,
    /// gain is added back iteratively until the delivered LUFS actually hits the target
    /// (bounded by `max_limiting_db` of peak gain reduction). Deliberate loudness
    /// maximization — the heavy-limiting warning is suppressed; the cap is the guard.
    Maximize,
}

impl OnClipPolicy {
    pub const ALL: [OnClipPolicy; 4] = [
        OnClipPolicy::Limit,
        OnClipPolicy::ReduceGain,
        OnClipPolicy::Warn,
        OnClipPolicy::Maximize,
    ];
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LimiterCharacter {
    Balanced,
    Transient,
    /// Slow, mastering-style release for *sustained* gain reduction (loudness
    /// maximization); balanced/transient are tuned for occasional overshoots.
    Dense,
}

impl LimiterCharacter {
    pub const ALL: [LimiterCharacter; 3] = [
        LimiterCharacter::Balanced,
        LimiterCharacter::Transient,
        LimiterCharacter::Dense,
    ];
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DitherStrength {
    Low,
    Normal,
    High,
}

impl DitherStrength {
    pub const ALL: [DitherStrength; 3] = [
        DitherStrength::Low,
        DitherStrength::Normal,
        DitherStrength::High,
    ];
}

/// Phase 4.4: Stereo and multi-channel dither correlation mode.
///
/// By default, each channel gets independent (decorrelated) TPDF noise, which
/// maximizes the SNR benefit of dithering: the quantization error is uncorrelated
/// across channels, so it won't create audible artifacts in the stereo image.
///
/// Alternative modes are available for specialized use cases:
/// - `Correlated`: All channels share the same dither noise. This can be useful
///   for certain mastering workflows but reduces SNR and may create subtle
///   imaging artifacts.
/// - `MidSide`: Dither is applied to mid (L+R) and side (L-R) channels separately,
///   then reconstructed to L and R. Provides a balance between decorrelation and
///   stereo coherence.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DitherCorrelation {
    /// Default: Each channel gets independent dither (decorrelated).
    /// Pros: Maximum SNR, no imaging artifacts.
    /// Cons: Slight stereo width in the dither noise.
    Decorrelated,
    /// All channels share identical dither noise.
    /// Pros: Mono dither noise (no stereo width).
    /// Cons: Reduced SNR per channel, potential imaging artifacts.
    Correlated,
    /// Dither applied to mid (L+R) and side (L-R) channels, then reconstructed.
    /// Pros: Balanced between SNR and stereo coherence.
    /// Cons: Moderate complexity.
    #[serde(rename = "mid_side")]
    MidSide,
}

impl Default for DitherCorrelation {
    fn default() -> Self {
        DitherCorrelation::Decorrelated
    }
}

impl DitherCorrelation {
    pub const ALL: [DitherCorrelation; 3] = [
        DitherCorrelation::Decorrelated,
        DitherCorrelation::Correlated,
        DitherCorrelation::MidSide,
    ];
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DitherMode {
    Tpdf,
    None,
    /// TPDF dither + gentle `(1 - z^-1)^2` error-feedback noise shaping. Opt-in;
    /// only engages for s16 targets (where the noise floor can approach audibility)
    /// and gracefully degrades to flat `Tpdf` for s24 and to nothing for f32.
    Shaped,
    /// TPDF dither + the published Lipshitz minimally-audible psychoacoustic curve —
    /// a stronger, ear-weighted shaper than `Shaped` (deep notch at ~4 kHz). Same
    /// s16-only gating and graceful degradation.
    Psychoacoustic,
    /// Adaptive dither — comparable in approach to commercial adaptive dithers. 
    /// A per-frame psychoacoustic analysis (MPEG-1 model-1 masking)
    /// designs a minimal-phase noise-shaping curve whose noise sits under the
    /// program's masking threshold, with a per-frame fallback to a fixed ATH-optimal
    /// curve so quiet/exposed passages are never worse than a fixed shaper.
    /// Per-channel decorrelated noise. s16-only; degrades to flat TPDF above.
    /// (Frequency/simultaneous masking only — no temporal pre/post-masking model yet.)
    Adaptive,
}

impl DitherMode {
    pub const ALL: [DitherMode; 5] = [
        DitherMode::Tpdf,
        DitherMode::None,
        DitherMode::Shaped,
        DitherMode::Psychoacoustic,
        DitherMode::Adaptive,
    ];

    /// The error-feedback curve this mode applies, or `None` for the flat/no-dither
    /// modes. Single source of truth for *which* modes shape and *with which* curve —
    /// shared by `bitdepth::apply` (the run), `shapes`, and `describe` (the dry-run),
    /// so none of them can drift.
    pub fn shaping_curve(self) -> Option<ShapingCurve> {
        match self {
            DitherMode::Shaped => Some(ShapingCurve::Gentle),
            DitherMode::Psychoacoustic => Some(ShapingCurve::Psychoacoustic),
            DitherMode::Tpdf | DitherMode::None | DitherMode::Adaptive => None,
        }
    }

    /// Whether error-feedback noise shaping actually engages for `format`. Shaping
    /// only helps at s16; above it any shaped mode degrades to flat TPDF.
    pub fn shapes(self, format: OutputSampleFormat) -> bool {
        self.shaping_curve().is_some() && matches!(format, OutputSampleFormat::S16)
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BitDepthFormat {
    S16,
    S24,
    F32,
    F64,
}

impl BitDepthFormat {
    pub const ALL: [BitDepthFormat; 4] = [
        BitDepthFormat::S16,
        BitDepthFormat::S24,
        BitDepthFormat::F32,
        BitDepthFormat::F64,
    ];
}

/// Output container / codec. Separate axis from `BitDepthFormat`: WAV and FLAC carry an
/// integer bit depth (FLAC has no float), MP3 is lossy and ignores bit depth entirely, and
/// DSF is 1-bit so a bit depth is meaningless there.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputCodec {
    Wav,
    Flac,
    Mp3,
    /// DSD Stream File (Sony). Takes the DSD branch of the pipeline.
    Dsf,
}

impl OutputCodec {
    pub const ALL: [OutputCodec; 4] = [
        OutputCodec::Wav,
        OutputCodec::Flac,
        OutputCodec::Mp3,
        OutputCodec::Dsf,
    ];

    /// File extension and the value substituted for the `{cext}` naming placeholder.
    pub fn ext(self) -> &'static str {
        match self {
            OutputCodec::Wav => "wav",
            OutputCodec::Flac => "flac",
            OutputCodec::Mp3 => "mp3",
            OutputCodec::Dsf => "dsf",
        }
    }

    /// True for the 1-bit containers. They skip `ops::bitdepth` entirely — the sigma-delta
    /// modulator *is* the quantizer — and derive their PCM stage rate instead of taking it
    /// from `rate`. Written as a predicate rather than matched inline so adding DFF later
    /// is one arm here, not a hunt through the pipeline.
    pub const fn is_dsd(self) -> bool {
        matches!(self, OutputCodec::Dsf)
    }
}

/// MP3 CBR bitrates (kbps) libmp3lame accepts. Used for validation and the encoder map.
pub const MP3_BITRATES: &[u32] = &[
    8, 16, 24, 32, 40, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];

impl From<BitDepthFormat> for OutputSampleFormat {
    fn from(value: BitDepthFormat) -> Self {
        match value {
            BitDepthFormat::S16 => OutputSampleFormat::S16,
            BitDepthFormat::S24 => OutputSampleFormat::S24,
            BitDepthFormat::F32 => OutputSampleFormat::F32,
            BitDepthFormat::F64 => OutputSampleFormat::F64,
        }
    }
}

/// A predicted output size, carrying how much the prediction is worth.
///
/// The distinction is not decoration: DSD is the codec that motivated showing sizes at all
/// (a five-minute DSD512 stereo track is ~1.7 GB), and there the number is exact — an
/// approximation would be a strictly worse answer to "will this fit". FLAC cannot be
/// predicted at all before encoding, and saying "~" of an upper bound would be a lie in the
/// one direction that matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeEstimate {
    /// Byte-exact: every stage has a deterministic length contract (WAV, DSF).
    Exact(u64),
    /// Derived from a nominal bitrate; the real file lands a few kB above (MP3).
    About(u64),
    /// Upper bound only — lossless compression is content-dependent (FLAC).
    AtMost(u64),
}

impl SizeEstimate {
    pub fn bytes(self) -> u64 {
        match self {
            SizeEstimate::Exact(b) | SizeEstimate::About(b) | SizeEstimate::AtMost(b) => b,
        }
    }

    /// Prefix that says how much the number is worth: nothing, `~`, or `<=`.
    pub fn qualifier(self) -> &'static str {
        match self {
            SizeEstimate::Exact(_) => "",
            SizeEstimate::About(_) => "~",
            SizeEstimate::AtMost(_) => "<= ",
        }
    }
}

/// A fully-resolved target with every effective value materialized.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub name: String,
    /// Explicit PCM rate. `None` keeps the source rate for PCM targets, and for DSD
    /// targets means "derive the intermediate rate" — see [`Self::pcm_stage_rate`], which
    /// is what the pipeline and the dry-run actually read.
    pub rate: Option<u32>,
    /// Delivered sample format. A placeholder for the codecs that have none of their own
    /// (MP3, DSF); [`Self::format_placeholder`] is what renders it, and it never reaches
    /// a file in those cases.
    pub format: OutputSampleFormat,
    /// `None` skips loudness normalization (pure transcode).
    pub lufs: Option<f64>,
    pub ceiling_dbtp: f64,
    pub on_clip: OnClipPolicy,
    pub limiter_character: LimiterCharacter,
    pub limiter_soft_clip: bool,
    pub limiter_link_channels: bool,
    /// Limiter gain-reduction threshold (dB) above which a heavy-limiting warning fires.
    pub warn_limiting_db: f64,
    /// Peak gain-reduction budget (dB) for `on_clip = "maximize"`.
    pub max_limiting_db: f64,
    pub quality: ResampleQuality,
    pub dither: DitherMode,
    pub dither_strength: DitherStrength,
    pub dither_correlation: DitherCorrelation,
    pub codec: OutputCodec,
    /// DSD rate; `Some` exactly when `codec.is_dsd()` (enforced in `resolve_target`).
    pub dsd: Option<DsdRate>,
    /// Write a Broadcast WAV `bext` chunk (WAV only).
    pub bwf: bool,
    /// MP3 CBR bitrate (kbps); only meaningful when `codec == Mp3`.
    pub mp3_bitrate: u32,
    pub dir: PathBuf,
    pub naming: String,
    pub overwrite: bool,
}

impl ResolvedTarget {
    /// Rate the PCM chain delivers at — the rate everything up to and including the
    /// ceiling stage works in.
    ///
    /// For a DSD target this is the *intermediate* rate the modulator is fed from, not the
    /// rate of the file: an explicit `rate` if the user set one (validated as a power-of-two
    /// divisor of the DSD rate), otherwise derived from the source
    /// (`ops::dsd::intermediate_pcm_rate`).
    pub fn pcm_stage_rate(&self, source_rate: u32) -> u32 {
        match self.dsd {
            Some(dsd) => self
                .rate
                .unwrap_or_else(|| dsd::intermediate_pcm_rate(source_rate, dsd)),
            None => self.rate.unwrap_or(source_rate),
        }
    }

    /// Sample rate of the delivered file: the 1-bit rate for DSD, the PCM rate otherwise.
    /// This is what `{rate}` renders and what the report calls `out_rate`.
    pub fn delivery_rate(&self, source_rate: u32) -> u32 {
        match self.dsd {
            Some(dsd) => dsd.hz(),
            None => self.pcm_stage_rate(source_rate),
        }
    }

    /// `{format}` substitution. DSD has no sample format — the stream is one bit — so the
    /// dummy sitting in `format` must not leak into a file name.
    pub fn format_placeholder(&self) -> &'static str {
        match self.dsd {
            Some(_) => "1bit",
            None => format_tag(self.format),
        }
    }

    /// Render the output path for `input`. `out_rate` is the effective delivery
    /// rate (see [`Self::delivery_rate`]) and is used for the `{rate}` placeholder.
    pub fn render_output_path(&self, input: &Path, out_rate: u32) -> Result<PathBuf> {
        let stem = input
            .file_stem()
            .and_then(|x| x.to_str())
            .context("input file has no valid stem")?;
        let ext = input.extension().and_then(|x| x.to_str()).unwrap_or_default();

        let file_name = self
            .naming
            .replace("{stem}", stem)
            .replace("{ext}", ext)
            .replace("{target}", &self.name)
            .replace("{rate}", &out_rate.to_string())
            .replace("{format}", self.format_placeholder())
            .replace("{dsd}", self.dsd.map_or("", DsdRate::tag))
            .replace("{cext}", self.codec.ext());

        Ok(self.dir.join(file_name))
    }

    /// Size of the file this target will write, as far as it can be known in advance.
    ///
    /// `source_frames` is per channel. Returns `None` when the source could not be measured
    /// (see [`crate::audio::io_read::probe_source`]) — printing a wrong number is worse
    /// than printing none.
    ///
    /// How exact the answer is depends entirely on the codec, which is why the caller gets
    /// a [`SizeEstimate`] rather than a bare `u64`: for DSD every stage between here and
    /// the writer has a deterministic length contract, so the prediction is byte-exact,
    /// while for FLAC only an upper bound exists before the material has been encoded.
    pub fn estimated_output_bytes(
        &self,
        source_rate: u32,
        source_frames: u64,
        channels: usize,
    ) -> Option<SizeEstimate> {
        if source_rate == 0 || channels == 0 {
            return None;
        }
        let pcm_rate = self.pcm_stage_rate(source_rate);
        // The one length change in the PCM chain; loudness and limiting are in-place.
        // Deliberately the same expression as `ops::resample::apply` — a `floor` here would
        // differ by a frame and, at a DSF block boundary, by a whole 4096-byte block.
        let frames = if pcm_rate == source_rate {
            source_frames
        } else {
            (source_frames as f64 * (f64::from(pcm_rate) / f64::from(source_rate))).round() as u64
        };

        // Exhaustive for the same reason as `describe`: a new codec must not inherit a
        // plausible-looking number computed for a different container.
        Some(match self.codec {
            OutputCodec::Dsf => {
                let dsd = self.dsd?;
                let interpolation = dsd::interpolation_factor(pcm_rate, dsd)?;
                SizeEstimate::Exact(io_write_dsd::dsf_file_bytes(
                    frames * u64::from(interpolation),
                    channels,
                ))
            }
            OutputCodec::Wav => SizeEstimate::Exact(io_write::wav_file_bytes(
                frames,
                pcm_rate,
                channels,
                self.format,
                self.bwf,
            )),
            OutputCodec::Flac => SizeEstimate::AtMost(io_write::flac_max_file_bytes(
                frames,
                channels,
                self.format.bits_per_sample(),
            )),
            OutputCodec::Mp3 => {
                SizeEstimate::About(io_write::mp3_file_bytes(frames, pcm_rate, self.mp3_bitrate))
            }
        })
    }

    /// Why this target cannot be delivered from this source, if it cannot.
    ///
    /// The compatibility rules a *config* cannot be validated against, because they depend
    /// on the input file: the delivery rate when the target has no `rate` of its own, and
    /// the channel count, which nothing in the TOML mentions. Both used to surface from
    /// inside a writer, i.e. after loudness, resampling, limiting and (for DSD) an hour of
    /// modulation had already run.
    ///
    /// One function so the two callers cannot drift: `pipeline::process` turns it into the
    /// job's error before touching the audio, and `--dry-run` prints it as the "this target
    /// will fail" line. `channels` is `None` when the header does not declare it — the
    /// dry-run's case, where the rate check still applies.
    pub fn delivery_blocker(&self, source_rate: u32, channels: Option<usize>) -> Option<String> {
        if self.codec == OutputCodec::Flac {
            let rate = self.pcm_stage_rate(source_rate);
            if rate > io_write::FLAC_MAX_SAMPLE_RATE {
                return Some(flac_rate_error(rate));
            }
        }
        // The DSF channel-type table stops at 5.1, so a 7.1 source has no container to go
        // into. Nothing in the config knows the channel count, which is why this cannot be
        // a load-time check like the rest of the codec rules.
        if self.codec.is_dsd()
            && let Some(channels) = channels
            && channels > io_write_dsd::MAX_CHANNELS
        {
            return Some(format!(
                "{channels} channels exceeds the DSF limit of {}",
                io_write_dsd::MAX_CHANNELS
            ));
        }
        None
    }

    /// Human-readable summary of the auto-derived pipeline for `--dry-run`.
    pub fn describe(&self, source_rate: u32) -> Vec<String> {
        let mut steps = Vec::new();

        match self.lufs {
            Some(lufs) => steps.push(format!(
                "loudness -> {lufs:.1} LUFS (ceiling {:.1} dBTP, {})",
                self.ceiling_dbtp,
                limiter_policy_tag(
                    self.on_clip,
                    self.limiter_character,
                    self.limiter_soft_clip,
                    self.limiter_link_channels,
                    self.max_limiting_db,
                )
            )),
            None => steps.push(format!(
                "transcode (no loudness normalization); ceiling {:.1} dBTP enforced ({})",
                self.ceiling_dbtp,
                limiter_policy_tag(
                    self.on_clip,
                    self.limiter_character,
                    self.limiter_soft_clip,
                    self.limiter_link_channels,
                    self.max_limiting_db,
                )
            )),
        }

        // For DSD this is the *intermediate* rate; the interpolation to the 1-bit rate is
        // a separate step below.
        let pcm_rate = self.pcm_stage_rate(source_rate);
        if pcm_rate != source_rate {
            let why = if self.dsd.is_some() && self.rate.is_none() {
                ", derived"
            } else {
                ""
            };
            steps.push(format!(
                "resample {source_rate} -> {pcm_rate} Hz ({}{why})",
                quality_tag(self.quality)
            ));
            // The two verification passes the resample brings with it. Both are conditional
            // at run time (each is a no-op when its measurement comes back clean), but they
            // are unconditional *steps* of a resampling chain and the dry-run has to say so
            // — a static gain applied after the loudness stage is exactly the kind of thing
            // a reader of this list would otherwise not know had happened.
            let mut recheck = "re-check true peak against the ceiling".to_string();
            if self.lufs.is_some() {
                recheck.push_str(" and re-verify loudness (corrected within the headroom)");
            }
            steps.push(recheck);
        } else {
            steps.push(format!("keep sample rate {source_rate} Hz"));
        }

        // Exhaustive on purpose: a catch-all arm here would silently describe a new codec
        // as a quantized WAV, and the dry-run is the only place the derived chain is
        // visible before any audio is touched.
        match self.codec {
            // MP3 is lossy: no PCM quantization/dither — libmp3lame encodes from the
            // full-precision buffer directly.
            OutputCodec::Mp3 => steps.push(format!(
                "encode MP3 {} kbps (lossy; bit depth & dither not applicable)",
                self.mp3_bitrate
            )),
            // DSD is the second lossy-shaped branch: the sigma-delta loop is the quantizer,
            // so the bit-depth/dither stage is skipped the same way.
            OutputCodec::Dsf => steps.extend(self.describe_dsd(pcm_rate)),
            OutputCodec::Wav | OutputCodec::Flac => {
                match self.format {
                    f @ (OutputSampleFormat::F32 | OutputSampleFormat::F64) => {
                        steps.push(format!("write {} (no quantization)", format_tag(f)))
                    }
                    other => steps.push(format!(
                        "quantize -> {} + {} ({})",
                        format_tag(other),
                        effective_dither_tag(self.dither, self.dither_strength, other),
                        dither_strength_tag(self.dither_strength)
                    )),
                }
                let container = if self.codec == OutputCodec::Flac {
                    "FLAC"
                } else if self.bwf {
                    "Broadcast WAV (bext + R128 metadata)"
                } else {
                    "WAV"
                };
                steps.push(format!("write {container}"));
            }
        }

        steps
    }

    /// The DSD tail of `describe`: the three steps `ops::dsd::encode_dsf` performs.
    fn describe_dsd(&self, pcm_rate: u32) -> Vec<String> {
        let Some(dsd) = self.dsd else {
            return vec!["DSD target without a dsd rate (config bug)".to_string()];
        };
        let factor = dsd::interpolation_factor(pcm_rate, dsd);
        let ratio = factor.map_or_else(|| "?".to_string(), |f| format!("x{f}"));

        let mut steps = vec![
            format!(
                "interpolate {ratio} -> {} Hz ({}, {}), streaming",
                dsd.hz(),
                dsd.tag(),
                quality_tag(self.quality)
            ),
            format!(
                "sigma-delta modulate to 1 bit (order {}; 0 dBFS -> {:.0} % modulation)",
                dsd_modulator::ORDER,
                dsd_modulator::INPUT_SCALE * 100.0
            ),
            "write DSF (planar 4096-byte blocks, LSB first)".to_string(),
        ];
        // A de-facto extension: the DSF spec names only DSD64 and DSD128 (see the writer's
        // module doc), and support in the wild is thin. Worth saying once, here, rather
        // than after an hour of encoding.
        if matches!(dsd, DsdRate::Dsd512) {
            steps.push("note: dsd512 has limited player support".to_string());
        }
        steps
    }
}

impl Config {
    pub fn from_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config: {}", path.display()))?;
        let parsed: Self = toml::from_str(&raw)
            .with_context(|| format!("invalid TOML config: {}", path.display()))?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Parse and validate config text that is not (yet) a file — an editor buffer being
    /// typed into, say. Same two steps as `from_path` minus the read, so a front end
    /// validating an unsaved document does not have to stage a temp file to do it. The
    /// two are kept as separate three-liners rather than one wrapping the other because
    /// `from_path`'s context string names the file and its validation errors deliberately
    /// carry no context at all — several tests read those messages verbatim.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let parsed: Self = toml::from_str(text).context("invalid TOML config")?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Sorted list of all target names defined in the config.
    pub fn target_names(&self) -> Vec<String> {
        self.targets.keys().cloned().collect()
    }

    /// Resolve a single target by name, layering builtin < defaults < preset < explicit.
    pub fn resolve_target(&self, name: &str) -> Result<ResolvedTarget> {
        self.resolve_layered(name).map(|(target, _)| target)
    }

    /// Resolve a target *and* say where every value came from — which layer won, what the
    /// lower layers said, what was dropped or is inert, and which fields would be a hard
    /// error if stated here.
    ///
    /// The provenance is not recomputed from a second copy of the precedence rule: both it
    /// and the merge read the same [`Layers`] array. That matters because the rule has a
    /// documented exception (`format`, `rate` and `bwf` are judged against the target's own
    /// layer, not the merged one), and an explanation that re-derived precedence would
    /// eventually describe a merge that no longer happens.
    ///
    /// Also what backs `mint --explain <target>`; it is not a GUI-only facility.
    pub fn resolve_target_explained(&self, name: &str) -> Result<(ResolvedTarget, TargetExplanation)> {
        let (target, layers) = self.resolve_layered(name)?;
        let explanation = explain(&target, &layers);
        Ok((target, explanation))
    }

    fn resolve_layered(&self, name: &str) -> Result<(ResolvedTarget, Layers)> {
        let raw = self
            .targets
            .get(name)
            .with_context(|| format!("no such target: '{name}'"))?
            .clone();

        // A preset is a layer between `[defaults]` and the explicit fields — but for the
        // three own-layer checks below it counts as part of what *this target* says, which
        // is what makes the one-line `preset = "sacd"` work: a preset may legally supply
        // `codec` and `dsd` there. Anyone narrowing `own` to mean "explicit fields only"
        // breaks that preset.
        let preset_layer = match raw.preset.as_deref() {
            Some(preset_name) => Some((preset_name.to_string(), preset(preset_name)?)),
            None => None,
        };
        let layers = Layers::new(self.defaults.clone(), preset_layer, raw);

        // What the target says about *itself*: preset < explicit, without the [defaults]
        // baseline underneath. Kept separate because the checks below have to tell
        // "the user described this deliverable" apart from "the user set a baseline for a
        // whole set of mostly-PCM targets" — a `format` in [defaults] must not make every
        // DSD target in the file a hard error. `overlay` is field-wise `Option::or` and
        // therefore associative, so folding the layers below reproduces the documented
        // builtin < defaults < preset < explicit precedence exactly.
        let own = layers.own();
        let merged = layers.merged();

        let codec = merged.codec.unwrap();
        let dsd = resolve_dsd(name, codec, &own, &merged)?;

        // `format` is required for WAV/FLAC (it's the delivered bit depth). MP3 is lossy
        // and ignores it; DSD is one bit and has none. Both get a dummy that never reaches
        // a file — `format_placeholder` is what naming renders.
        let format: OutputSampleFormat = match merged.format.map(Into::into) {
            Some(f) => f,
            None if codec == OutputCodec::Mp3 || codec.is_dsd() => OutputSampleFormat::S16,
            None => bail!(
                "target '{name}': 'format' is required (set it directly or via a preset)"
            ),
        };
        // Strict rather than tolerated (MP3 ignores a stray `format` for historical
        // reasons; a new field can afford to be honest). Only the target's own layer
        // counts — see the comment on `own`.
        if dsd.is_some() && own.format.is_some() {
            bail!(
                "target '{name}': 'format' has no meaning for a DSD target — the \
                 sigma-delta modulator is the quantizer. Remove it; use 'dsd' to pick the \
                 rate. (Every PCM preset sets 'format', so no preset other than 'sacd' can \
                 be combined with a DSD codec.)"
            );
        }
        // The bext chunk is a WAV (RIFF) extension, so it is meaningless anywhere else —
        // but this is the same own-layer question as `format` above, and it used to be
        // asked of the merged value. `[defaults] bwf = true` is a reasonable baseline in a
        // broadcast-oriented config, and against the merged value it turned every FLAC,
        // MP3 or DSD sibling in the same file into a load-time error. Rejected here when
        // *this* target asked for it; inherited-and-irrelevant is dropped below instead.
        if own.bwf == Some(true) && codec != OutputCodec::Wav {
            bail!(
                "target '{name}': bwf=true is only valid for codec=wav (got codec={})",
                codec.ext()
            );
        }

        // Builtin fallbacks guarantee these are populated; unwrap is safe.
        let resolved = ResolvedTarget {
            name: name.to_string(),
            // Same reasoning as `format`: for a DSD target `rate` is the escape hatch that
            // overrides the derived intermediate rate, so it must be something this target
            // said, not a baseline meant for its PCM siblings.
            rate: if dsd.is_some() { own.rate } else { merged.rate },
            format,
            lufs: merged.lufs,
            ceiling_dbtp: merged.ceiling_dbtp.unwrap(),
            on_clip: merged.on_clip.unwrap(),
            limiter_character: merged.limiter_character.unwrap(),
            limiter_soft_clip: merged.limiter_soft_clip.unwrap(),
            limiter_link_channels: merged.limiter_link_channels.unwrap(),
            warn_limiting_db: merged.warn_limiting_db.unwrap(),
            max_limiting_db: merged.max_limiting_db.unwrap(),
            quality: merged.quality.unwrap(),
            dither: merged.dither.unwrap(),
            dither_strength: merged.dither_strength.unwrap(),
            dither_correlation: merged.dither_correlation.unwrap(),
            codec,
            dsd,
            // Effective, not requested: an inherited `bwf` that this target never asked for
            // is dropped for the codecs that have no bext chunk (an explicit one already
            // failed above). So `bwf` here means "a bext chunk will be written".
            bwf: merged.bwf.unwrap() && codec == OutputCodec::Wav,
            mp3_bitrate: merged.mp3_bitrate.unwrap(),
            dir: merged.dir.unwrap(),
            naming: merged.naming.unwrap(),
            overwrite: merged.overwrite.unwrap(),
        };

        resolved.validate_fields()?;
        Ok((resolved, layers))
    }

    /// Resolve every target and reject configs that cannot produce valid output.
    pub fn validate(&self) -> Result<()> {
        if self.targets.is_empty() {
            bail!("config must define at least one [target.NAME] entry");
        }

        let resolved: Vec<ResolvedTarget> = self
            .target_names()
            .iter()
            .map(|name| self.resolve_target(name))
            .collect::<Result<_>>()?;

        check_collisions(&resolved)?;
        Ok(())
    }
}

impl ResolvedTarget {
    fn validate_fields(&self) -> Result<()> {
        if let Some(rate) = self.rate
            && rate == 0
        {
            bail!("target '{}': rate must be > 0", self.name);
        }
        if let Some(lufs) = self.lufs
            && !lufs.is_finite()
        {
            bail!("target '{}': lufs must be finite", self.name);
        }
        // A ceiling above 0 dBTP is meaningless (0 dBTP = digital full scale).
        if self.ceiling_dbtp > 0.0 {
            bail!(
                "target '{}': ceiling_dbtp must be <= 0 (got {:.2})",
                self.name,
                self.ceiling_dbtp
            );
        }
        // A negative threshold would fire on every file (GR is always >= 0).
        if !self.warn_limiting_db.is_finite() || self.warn_limiting_db < 0.0 {
            bail!(
                "target '{}': warn_limiting_db must be a finite value >= 0 (got {:.2})",
                self.name,
                self.warn_limiting_db
            );
        }
        // Zero would forbid all limiting, making maximize unable to ever exceed the
        // plain-normalization result — surely a config mistake.
        if !self.max_limiting_db.is_finite() || self.max_limiting_db <= 0.0 {
            bail!(
                "target '{}': max_limiting_db must be a finite value > 0 (got {:.2})",
                self.name,
                self.max_limiting_db
            );
        }
        // FLAC is an integer codec; it has no floating-point sample format.
        if self.codec == OutputCodec::Flac
            && matches!(self.format, OutputSampleFormat::F32 | OutputSampleFormat::F64)
        {
            bail!(
                "target '{}': codec=flac requires an integer format (s16 or s24), not {}",
                self.name,
                format_tag(self.format)
            );
        }
        // The FLAC backend tops out at 96 kHz. Only an *explicit* rate can be judged here;
        // a target that follows the source rate is caught by `delivery_blocker` the moment
        // the source rate is known. Both exist because the old behaviour was to discover it
        // inside the encoder, after the whole DSP chain had run.
        if self.codec == OutputCodec::Flac
            && let Some(rate) = self.rate
            && rate > io_write::FLAC_MAX_SAMPLE_RATE
        {
            bail!("target '{}': {}", self.name, flac_rate_error(rate));
        }
        // `bwf` is checked in `resolve_target`, against the target's own layer — see the
        // comment there. By the time it reaches this struct it is already the effective
        // value, so there is nothing left to reject.
        //
        // A DSD target's `rate` is the *intermediate* PCM rate the modulator is fed from,
        // so it has to reach the 1-bit rate by an exact power of two. Anything else means
        // a fractional SRC immediately in front of the most level-sensitive stage in the
        // chain — see `ops::dsd::intermediate_pcm_rate`. Keeping ultrasonic content in a
        // low-rate file (rate = 176400 for dsd64) is legitimate and stays allowed.
        if let Some(dsd) = self.dsd
            && let Some(rate) = self.rate
            && dsd::interpolation_factor(rate, dsd).is_none()
        {
            bail!(
                "target '{}': rate {rate} cannot feed {} ({} Hz) — the interpolation ratio \
                 must be a power of two >= 2. Use a 44.1 kHz-family rate (44100, 88200, \
                 176400, 352800, 705600) or omit 'rate' to have it derived",
                self.name,
                dsd.tag(),
                dsd.hz()
            );
        }
        // MP3 bitrate must be one libmp3lame accepts.
        if self.codec == OutputCodec::Mp3 && !MP3_BITRATES.contains(&self.mp3_bitrate) {
            bail!(
                "target '{}': mp3_bitrate {} is not a valid MP3 bitrate ({:?})",
                self.name,
                self.mp3_bitrate,
                MP3_BITRATES
            );
        }
        if !self.naming.contains("{stem}") {
            bail!(
                "target '{}': naming must contain the {{stem}} placeholder",
                self.name
            );
        }
        Ok(())
    }
}

/// Pair up `codec` and `dsd`, rejecting both ways they can disagree.
///
/// The two directions read different layers on purpose. *Missing* is checked against the
/// fully merged value, so `dsd` in `[defaults]` is a usable baseline for several DSD
/// targets. *Superfluous* is checked against the target's own layer, so that same baseline
/// does not turn every PCM target in the config into an error. What is rejected either way
/// is the mismatch a single target actually describes — silently ignoring one half would
/// sit badly next to `deny_unknown_fields`.
fn resolve_dsd(
    name: &str,
    codec: OutputCodec,
    own: &RawTarget,
    merged: &RawTarget,
) -> Result<Option<DsdRate>> {
    match (codec.is_dsd(), merged.dsd, own.dsd) {
        (true, Some(rate), _) => Ok(Some(rate)),
        (true, None, _) => bail!(
            "target '{name}': codec={} needs a 'dsd' rate (dsd64 | dsd128 | dsd256 | dsd512)",
            codec.ext()
        ),
        (false, _, Some(rate)) => bail!(
            "target '{name}': dsd=\"{}\" needs codec=\"dsf\" (got codec={})",
            rate.tag(),
            codec.ext()
        ),
        (false, _, None) => Ok(None),
    }
}

/// The four config layers of one target, **in precedence order, lowest first**.
///
/// This array is the single statement of the precedence rule. `merged` folds it and
/// `stated`/`winner` read it, so "which layer won" cannot drift from "which layer was
/// applied" — there is no second copy of `builtin < defaults < preset < explicit` to keep
/// in sync. Nothing here is optional except the preset, which is simply absent from the
/// array when the target names none.
struct Layers {
    ordered: Vec<(Layer, RawTarget)>,
}

impl Layers {
    fn new(defaults: RawTarget, preset_layer: Option<(String, RawTarget)>, explicit: RawTarget) -> Self {
        let mut ordered = vec![
            (Layer::Builtin, builtin_fallback()),
            (Layer::Defaults, defaults),
        ];
        if let Some((name, raw)) = preset_layer {
            ordered.push((Layer::Preset(name), raw));
        }
        ordered.push((Layer::Explicit, explicit));
        Self { ordered }
    }

    /// All four layers folded — the effective `RawTarget`.
    fn merged(&self) -> RawTarget {
        self.ordered
            .iter()
            .fold(RawTarget::default(), |acc, (_, layer)| overlay(acc, layer.clone()))
    }

    /// What the target says about *itself*: preset < explicit, with the `[defaults]`
    /// baseline left out. See the long comment in `resolve_target` for why three fields are
    /// judged against this narrower stack rather than the merged one.
    fn own(&self) -> RawTarget {
        self.ordered
            .iter()
            .filter(|(layer, _)| matches!(layer, Layer::Preset(_) | Layer::Explicit))
            .fold(RawTarget::default(), |acc, (_, layer)| overlay(acc, layer.clone()))
    }

    /// Every layer that states this field, lowest precedence first. `render` picks the
    /// field out of a layer and spells it the way the TOML would.
    fn stated(&self, render: impl Fn(&RawTarget) -> Option<String>) -> Vec<(Layer, String)> {
        self.ordered
            .iter()
            .filter_map(|(layer, raw)| render(raw).map(|value| (layer.clone(), value)))
            .collect()
    }
}

/// Where a value came from. `Preset` carries the name because "a preset set this" is not
/// actionable — *which* preset is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer {
    Builtin,
    Defaults,
    Preset(String),
    Explicit,
}

impl Layer {
    /// Single-character badge for a dense table: `B` `D` `P` `E`.
    pub fn glyph(&self) -> char {
        match self {
            Layer::Builtin => 'B',
            Layer::Defaults => 'D',
            Layer::Preset(_) => 'P',
            Layer::Explicit => 'E',
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Layer::Builtin => f.write_str("builtin"),
            Layer::Defaults => f.write_str("[defaults]"),
            Layer::Preset(name) => write!(f, "preset '{name}'"),
            Layer::Explicit => f.write_str("this target"),
        }
    }
}

/// What becomes of a value that *is* there.
///
/// Deliberately not four states. The proposal's fourth, "would error", turned out to be
/// orthogonal rather than alternative — `format` on a DSD target is inherited-and-inert
/// *and* illegal if stated here, which is why that lives on [`FieldOrigin::illegal_here`]
/// and dropping lives on [`FieldOrigin::dropped`]. A field can be all three at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// The value reaches the deliverable.
    Applies,
    /// Materialized into `ResolvedTarget`, but no code path on this target reads it.
    Inert(&'static str),
}

impl Fate {
    pub fn glyph(self) -> char {
        match self {
            Fate::Applies => '·',
            Fate::Inert(_) => '~',
        }
    }

    /// Why it is inert, for the row's trailing note.
    pub fn reason(self) -> Option<&'static str> {
        match self {
            Fate::Applies => None,
            Fate::Inert(why) => Some(why),
        }
    }
}

/// A value that a lower layer stated and the narrow own-layer stack refused to read.
/// Distinct from inert: an inert value *is* the effective one and simply goes unread; a
/// dropped one never became the effective value at all.
#[derive(Debug, Clone)]
pub struct Dropped {
    pub layer: Layer,
    pub value: String,
    pub why: &'static str,
}

/// Provenance of one resolved field: which layer won, what became of it, what was thrown
/// away on the way, and whether stating it here at all would be a load-time error.
#[derive(Debug, Clone)]
pub struct FieldOrigin {
    /// Layer whose value survived the merge. `None` when no layer stated the field — it is
    /// either derived at run time (`rate` on DSD) or simply absent (`lufs`).
    pub layer: Option<Layer>,
    pub fate: Fate,
    pub dropped: Option<Dropped>,
    /// Stating this field in *this* target's own layer is a hard error, even though
    /// inheriting it is fine. The asymmetry that makes `[defaults] format = "s24"` legal
    /// alongside a DSD target and `format = "s24"` on that target fatal.
    pub illegal_here: bool,
    /// Every layer that stated the field, lowest precedence first — the "why" panel.
    pub stated: Vec<(Layer, String)>,
}

/// Which part of the inspector a field belongs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Deliverable,
    Policy,
    Quality,
    Destination,
}

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Group::Deliverable => "DELIVERABLE",
            Group::Policy => "POLICY",
            Group::Quality => "QUALITY",
            Group::Destination => "DESTINATION",
        })
    }
}

/// One row of the inspector: the field, what it resolved to, and where that came from.
#[derive(Debug, Clone)]
pub struct FieldExplanation {
    /// TOML key, exactly as it is spelled in a config file.
    pub field: &'static str,
    pub group: Group,
    /// Effective value, rendered. Not always a TOML value — a field with no meaning on
    /// this target says so in prose rather than reporting the placeholder it carries.
    pub value: String,
    pub origin: FieldOrigin,
}

/// Everything `--dry-run` cannot say: the per-field provenance behind one resolved target.
#[derive(Debug, Clone)]
pub struct TargetExplanation {
    pub target: String,
    /// The preset this target named, if any.
    pub preset: Option<String>,
    /// Rows in inspector order, grouped.
    pub fields: Vec<FieldExplanation>,
}

impl TargetExplanation {
    pub fn field(&self, name: &str) -> Option<&FieldExplanation> {
        self.fields.iter().find(|f| f.field == name)
    }
}

impl FieldExplanation {
    fn new(field: &'static str, group: Group, value: String, stated: Vec<(Layer, String)>) -> Self {
        // The winner is the last layer that stated it, because `stated` is in the same
        // precedence order the merge folds in. The own-layer exceptions correct this with
        // `own_layer_only` rather than by asking a different question.
        let layer = stated.last().map(|(l, _)| l.clone());
        Self {
            field,
            group,
            value,
            origin: FieldOrigin { layer, fate: Fate::Applies, dropped: None, illegal_here: false, stated },
        }
    }

    fn inert_if(mut self, condition: bool, why: &'static str) -> Self {
        if condition {
            self.origin.fate = Fate::Inert(why);
        }
        self
    }

    fn illegal_here(mut self, condition: bool) -> Self {
        self.origin.illegal_here = condition;
        self
    }

    /// This field is read from the target's own layer only, so an inherited statement never
    /// wins it — even though `stated` still lists the inherited value, which is exactly what
    /// makes the "why" panel able to show what was passed over.
    fn own_layer_only(mut self, condition: bool) -> Self {
        if condition {
            self.origin.layer = self
                .origin
                .stated
                .iter()
                .rev()
                .find(|(l, _)| matches!(l, Layer::Preset(_) | Layer::Explicit))
                .map(|(l, _)| l.clone());
        }
        self
    }

    /// Record the highest-precedence inherited layer that stated this field as dropped.
    /// Only meaningful alongside `own_layer_only`, which is what did the dropping.
    fn dropping_inherited(mut self, condition: bool, why: &'static str) -> Self {
        if !condition {
            return self;
        }
        if let Some((layer, value)) = self
            .origin
            .stated
            .iter()
            .rev()
            .find(|(l, _)| matches!(l, Layer::Builtin | Layer::Defaults))
        {
            self.origin.dropped = Some(Dropped { layer: layer.clone(), value: value.clone(), why });
        }
        self
    }
}

/// Build the per-field provenance for an already-resolved target.
///
/// Every "inert" verdict here is a claim about a code path, not a guess: each one names the
/// reason a reader can go and check. The one field that cannot be judged is `quality` — an
/// SRC runs or not depending on the *source* rate, which no amount of config inspection
/// knows, so it is reported as applying rather than guessed at.
fn explain(t: &ResolvedTarget, layers: &Layers) -> TargetExplanation {
    // Tripwire, not logic: this destructure is exhaustive, so adding a field to `RawTarget`
    // stops this function compiling and the author is standing in the one place that has to
    // decide what the new field's fate is. Nothing else enforces that — a missing row here
    // is silently invisible in the inspector rather than wrong.
    let RawTarget {
        preset: _,
        rate: _,
        format: _,
        lufs: _,
        ceiling_dbtp: _,
        on_clip: _,
        limiter_character: _,
        limiter_soft_clip: _,
        limiter_link_channels: _,
        warn_limiting_db: _,
        max_limiting_db: _,
        quality: _,
        dither: _,
        dither_strength: _,
        dither_correlation: _,
        codec: _,
        dsd: _,
        bwf: _,
        mp3_bitrate: _,
        dir: _,
        naming: _,
        overwrite: _,
    } = RawTarget::default();

    let is_dsd = t.codec.is_dsd();
    let is_mp3 = t.codec == OutputCodec::Mp3;
    let is_float = matches!(t.format, OutputSampleFormat::F32 | OutputSampleFormat::F64);
    // Whether `ops::bitdepth` maps this target onto an integer grid at all.
    let quantizes = !is_dsd && !is_mp3 && !is_float;
    // `bitdepth::quantize_in_place` reads strength and correlation in the adaptive branch
    // only; the fixed-curve and flat-TPDF paths ignore both (report.md §9).
    let adaptive = quantizes && t.dither == DitherMode::Adaptive && t.format == OutputSampleFormat::S16;
    let limits = matches!(t.on_clip, OnClipPolicy::Limit | OnClipPolicy::Maximize);
    let normalizes = t.lufs.is_some();

    let no_quantizer = if is_dsd {
        "the sigma-delta modulator is the quantizer"
    } else if is_mp3 {
        "libmp3lame quantizes internally"
    } else {
        "a float target is written without quantization"
    };

    let preset_name = layers.ordered.iter().find_map(|(l, _)| match l {
        Layer::Preset(name) => Some(name.clone()),
        _ => None,
    });

    let fields = vec![
        // ── DELIVERABLE ────────────────────────────────────────────────────────────────
        FieldExplanation::new(
            "preset",
            Group::Deliverable,
            preset_name.clone().unwrap_or_else(|| "—".to_string()),
            layers.stated(|r| r.preset.clone()),
        )
        .own_layer_only(true)
        .dropping_inherited(
            true,
            "`[defaults] preset` parses but is never applied — resolve_target reads the \
             target's own preset, not the merged one (report.md defect #14)",
        ),
        FieldExplanation::new(
            "codec",
            Group::Deliverable,
            t.codec.to_string(),
            layers.stated(|r| r.codec.map(|c| c.to_string())),
        ),
        FieldExplanation::new(
            "dsd",
            Group::Deliverable,
            t.dsd.map_or_else(|| "—".to_string(), |d| d.tag().to_string()),
            layers.stated(|r| r.dsd.map(|d| d.tag().to_string())),
        )
        .illegal_here(!is_dsd)
        // Dropped rather than inert: on a PCM target the value never reaches
        // `ResolvedTarget` at all, so there is nothing sitting there unread. The quiet one
        // of the three own-layer exceptions — an inherited `dsd` is neither applied nor
        // rejected, it is simply never looked at.
        .own_layer_only(!is_dsd)
        .dropping_inherited(
            !is_dsd,
            "an inherited `dsd` is ignored on a PCM target; only one stated on this target \
             is an error",
        ),
        FieldExplanation::new(
            "rate",
            Group::Deliverable,
            match (t.rate, is_dsd) {
                (Some(rate), _) => format!("{rate} Hz"),
                (None, true) => "derived from the source".to_string(),
                (None, false) => "follows the source".to_string(),
            },
            layers.stated(|r| r.rate.map(|v| v.to_string())),
        )
        .own_layer_only(is_dsd)
        .dropping_inherited(
            is_dsd,
            "on a DSD target `rate` is the intermediate PCM rate and is read from preset + \
             explicit only. An inherited rate would silently re-pick what the modulator is fed \
             from — and a rate that cannot reach the DSD rate by a power of two would turn every \
             DSD target in the file into a load-time error",
        ),
        FieldExplanation::new(
            "format",
            Group::Deliverable,
            if is_dsd {
                "— (1-bit, no bit depth)".to_string()
            } else if is_mp3 {
                "— (lossy, no bit depth)".to_string()
            } else {
                format_tag(t.format).to_string()
            },
            layers.stated(|r| r.format.map(|f| f.to_string())),
        )
        .inert_if(is_dsd || is_mp3, no_quantizer)
        // The asymmetry this whole facility exists to make visible: inheriting a format is
        // fine on a DSD target, stating one here is a load-time error.
        .illegal_here(is_dsd),
        FieldExplanation::new(
            "lufs",
            Group::Deliverable,
            t.lufs.map_or_else(|| "— (no normalization)".to_string(), |v| format!("{v:.1} LUFS")),
            layers.stated(|r| r.lufs.map(|v| format!("{v:.1}"))),
        ),
        FieldExplanation::new(
            "ceiling_dbtp",
            Group::Deliverable,
            format!("{:.1} dBTP", t.ceiling_dbtp),
            layers.stated(|r| r.ceiling_dbtp.map(|v| format!("{v:.1}"))),
        ),
        FieldExplanation::new(
            "bwf",
            Group::Deliverable,
            t.bwf.to_string(),
            layers.stated(|r| r.bwf.map(|v| v.to_string())),
        )
        .own_layer_only(t.codec != OutputCodec::Wav)
        .dropping_inherited(
            t.codec != OutputCodec::Wav,
            "a bext chunk is a WAV (RIFF) extension, so an inherited bwf = true is dropped \
             rather than rejected — stating it on this target is an error instead",
        )
        .illegal_here(t.codec != OutputCodec::Wav),
        FieldExplanation::new(
            "mp3_bitrate",
            Group::Deliverable,
            format!("{} kbps", t.mp3_bitrate),
            layers.stated(|r| r.mp3_bitrate.map(|v| v.to_string())),
        )
        .inert_if(!is_mp3, "the codec is not mp3"),
        // ── POLICY ─────────────────────────────────────────────────────────────────────
        FieldExplanation::new(
            "on_clip",
            Group::Policy,
            t.on_clip.to_string(),
            layers.stated(|r| r.on_clip.map(|v| v.to_string())),
        ),
        FieldExplanation::new(
            "limiter_character",
            Group::Policy,
            t.limiter_character.to_string(),
            layers.stated(|r| r.limiter_character.map(|v| v.to_string())),
        )
        .inert_if(!limits, "this on_clip policy never engages the limiter"),
        FieldExplanation::new(
            "limiter_soft_clip",
            Group::Policy,
            t.limiter_soft_clip.to_string(),
            layers.stated(|r| r.limiter_soft_clip.map(|v| v.to_string())),
        )
        .inert_if(!limits, "this on_clip policy never engages the limiter"),
        FieldExplanation::new(
            "limiter_link_channels",
            Group::Policy,
            t.limiter_link_channels.to_string(),
            layers.stated(|r| r.limiter_link_channels.map(|v| v.to_string())),
        )
        .inert_if(!limits, "this on_clip policy never engages the limiter"),
        FieldExplanation::new(
            "warn_limiting_db",
            Group::Policy,
            format!("{:.1} dB", t.warn_limiting_db),
            layers.stated(|r| r.warn_limiting_db.map(|v| format!("{v:.1}"))),
        )
        .inert_if(
            !normalizes,
            "the heavy-limiting warning only fires inside the loudness step, and no lufs is set",
        )
        .inert_if(
            normalizes && !matches!(t.on_clip, OnClipPolicy::Limit),
            "only on_clip = limit can raise the heavy-limiting warning (maximize suppresses it \
             by design — max_limiting_db is its guard instead)",
        ),
        FieldExplanation::new(
            "max_limiting_db",
            Group::Policy,
            format!("{:.1} dB", t.max_limiting_db),
            layers.stated(|r| r.max_limiting_db.map(|v| format!("{v:.1}"))),
        )
        .inert_if(
            !matches!(t.on_clip, OnClipPolicy::Maximize) || !normalizes,
            "only on_clip = maximize with a lufs target reads it",
        ),
        // ── QUALITY ────────────────────────────────────────────────────────────────────
        FieldExplanation::new(
            "quality",
            Group::Quality,
            t.quality.to_string(),
            layers.stated(|r| r.quality.map(|v| v.to_string())),
        ),
        FieldExplanation::new(
            "dither",
            Group::Quality,
            t.dither.to_string(),
            layers.stated(|r| r.dither.map(|v| v.to_string())),
        )
        .inert_if(!quantizes, no_quantizer),
        FieldExplanation::new(
            "dither_strength",
            Group::Quality,
            t.dither_strength.to_string(),
            layers.stated(|r| r.dither_strength.map(|v| v.to_string())),
        )
        .inert_if(!quantizes, no_quantizer)
        .inert_if(
            quantizes && !adaptive,
            "only the adaptive mode reads strength (it picks the FIR order); the fixed curves \
             and flat TPDF ignore it",
        ),
        FieldExplanation::new(
            "dither_correlation",
            Group::Quality,
            t.dither_correlation.to_string(),
            layers.stated(|r| r.dither_correlation.map(|v| v.to_string())),
        )
        .inert_if(!quantizes, no_quantizer)
        .inert_if(
            quantizes && !adaptive,
            "only the adaptive mode reads correlation; the fixed curves and flat TPDF ignore it",
        ),
        // ── DESTINATION ────────────────────────────────────────────────────────────────
        FieldExplanation::new(
            "dir",
            Group::Destination,
            t.dir.display().to_string(),
            layers.stated(|r| r.dir.as_ref().map(|d| d.display().to_string())),
        ),
        FieldExplanation::new(
            "naming",
            Group::Destination,
            t.naming.clone(),
            layers.stated(|r| r.naming.clone()),
        ),
        FieldExplanation::new(
            "overwrite",
            Group::Destination,
            t.overwrite.to_string(),
            layers.stated(|r| r.overwrite.map(|v| v.to_string())),
        ),
    ];

    TargetExplanation { target: t.name.clone(), preset: preset_name, fields }
}

/// Overlay `top` onto `base`: any field set in `top` wins.
fn overlay(base: RawTarget, top: RawTarget) -> RawTarget {
    RawTarget {
        preset: top.preset.or(base.preset),
        rate: top.rate.or(base.rate),
        format: top.format.or(base.format),
        lufs: top.lufs.or(base.lufs),
        ceiling_dbtp: top.ceiling_dbtp.or(base.ceiling_dbtp),
        on_clip: top.on_clip.or(base.on_clip),
        limiter_character: top.limiter_character.or(base.limiter_character),
        limiter_soft_clip: top.limiter_soft_clip.or(base.limiter_soft_clip),
        limiter_link_channels: top.limiter_link_channels.or(base.limiter_link_channels),
        warn_limiting_db: top.warn_limiting_db.or(base.warn_limiting_db),
        max_limiting_db: top.max_limiting_db.or(base.max_limiting_db),
        quality: top.quality.or(base.quality),
        dither: top.dither.or(base.dither),
        dither_strength: top.dither_strength.or(base.dither_strength),
        dither_correlation: top.dither_correlation.or(base.dither_correlation),
        codec: top.codec.or(base.codec),
        dsd: top.dsd.or(base.dsd),
        bwf: top.bwf.or(base.bwf),
        mp3_bitrate: top.mp3_bitrate.or(base.mp3_bitrate),
        dir: top.dir.or(base.dir),
        naming: top.naming.or(base.naming),
        overwrite: top.overwrite.or(base.overwrite),
    }
}

/// Hardcoded last-resort defaults so a bare target only needs a `format`.
fn builtin_fallback() -> RawTarget {
    RawTarget {
        preset: None,
        rate: None,
        format: None, // required — must come from a preset or an explicit field
        lufs: None,
        ceiling_dbtp: Some(-1.0),
        on_clip: Some(OnClipPolicy::Limit),
        limiter_character: Some(LimiterCharacter::Balanced),
        limiter_soft_clip: Some(false),
        limiter_link_channels: Some(true),
        warn_limiting_db: Some(1.0),
        max_limiting_db: Some(8.0),
        quality: Some(ResampleQuality::Vhq),
        dither: Some(DitherMode::Tpdf),
        dither_strength: Some(DitherStrength::Normal),
        // Populated rather than left to `unwrap_or_default()` in `resolve_target`: the
        // documented invariant is that every non-optional field is materialized *here*, so
        // that a forgotten entry panics loudly instead of silently defaulting.
        dither_correlation: Some(DitherCorrelation::Decorrelated),
        codec: Some(OutputCodec::Wav),
        dsd: None, // DSD-only; a default here would be a trap for every PCM target
        bwf: Some(false),
        mp3_bitrate: Some(320),
        dir: Some(PathBuf::from("./out")),
        // Per-target subdirectory keeps outputs from colliding; `{cext}` is the codec's
        // extension (wav/flac/mp3), so the default name follows the chosen container.
        naming: Some("{target}/{stem}.{cext}".to_string()),
        overwrite: Some(false),
    }
}

/// Every built-in preset, in the order the error message offers them.
///
/// [`preset`] must have an arm for each name and no arm without one; nothing in the type
/// system enforces that, so `every_listed_preset_resolves_and_the_error_offers_all_of_them`
/// in `tests/config_parse.rs` holds the two together. Before this list existed the
/// valid-name enumeration was a third
/// hand-maintained copy inside the error string, and adding a preset silently left it stale.
pub const PRESET_NAMES: [&str; 11] = [
    "spotify",
    "apple-music",
    "youtube",
    "tidal",
    "amazon-music",
    "soundcloud",
    "cd",
    "broadcast-ebu-r128",
    "48k",
    "hires",
    "sacd",
];

/// Built-in delivery presets. For the streaming platforms the values reflect each one's
/// published loudness-normalization target and can be overridden per-target; they drift
/// over time, so treat them as sensible starting points, not gospel. `cd` and `sacd` are
/// not loudness specs at all — they are physical-format bundles that deliberately leave
/// `lufs` unset.
fn preset(name: &str) -> Result<RawTarget> {
    use BitDepthFormat::{S16, S24};

    // (rate, format, lufs, ceiling)
    let spec = |rate: u32, format: BitDepthFormat, lufs: Option<f64>, ceiling: f64| RawTarget {
        rate: Some(rate),
        format: Some(format),
        lufs,
        ceiling_dbtp: Some(ceiling),
        ..RawTarget::default()
    };

    Ok(match name {
        "spotify" => spec(44_100, S16, Some(-14.0), -1.0),
        "apple-music" => spec(44_100, S16, Some(-16.0), -1.0),
        "youtube" => spec(48_000, S16, Some(-14.0), -1.0),
        "tidal" => spec(44_100, S16, Some(-14.0), -1.0),
        "amazon-music" => spec(44_100, S16, Some(-14.0), -2.0),
        "soundcloud" => spec(44_100, S16, Some(-14.0), -1.0),
        // CD has no loudness standard — leave lufs unset for the user to choose.
        "cd" => spec(44_100, S16, None, -0.1),
        "broadcast-ebu-r128" => spec(48_000, S24, Some(-23.0), -1.0),
        "48k" => spec(48_000, S16, Some(-16.0), -1.0),
        "hires" => spec(96_000, S24, Some(-16.0), -0.1),
        // The one preset that is not PCM, and the only one that cannot use `spec`: it must
        // set *neither* of the two fields `spec` always sets. `format` is a hard error on a
        // DSD target (the modulator is the quantizer), and a `rate` would suppress the
        // derivation of the intermediate PCM rate that is the whole point of the DSD
        // branch — silently, since the preset layer is folded into the target's own layer
        // and is therefore indistinguishable from something the user typed.
        // Like `cd`: a physical format, no loudness standard, so no `lufs`.
        "sacd" => RawTarget {
            codec: Some(OutputCodec::Dsf),
            dsd: Some(DsdRate::Dsd64),
            ceiling_dbtp: Some(-1.0),
            ..RawTarget::default()
        },
        other => bail!(
            "unknown preset '{other}'; valid presets: {}",
            PRESET_NAMES.join(", ")
        ),
    })
}

/// One planned (input, target) render, for the pre-flight in `cli.rs`.
#[derive(Debug, Clone)]
pub struct PlannedOutput<'a> {
    pub input: &'a Path,
    pub target: &'a str,
    pub output: PathBuf,
}

/// Reject a *batch* whose jobs would fight over files, before any audio is touched.
///
/// [`check_collisions`] answers the same question for targets and cannot answer this one:
/// it holds the stem constant on purpose, and the stem is exactly what differs here. Two
/// inputs called `song.wav` in different directories render to one output under any naming
/// mint can express — none of the placeholders (`{stem} {ext} {target} {rate} {format}
/// {dsd} {cext}`) carries the input's parent directory — so both jobs used to run, in
/// parallel, and whichever finished last won.
///
/// The second rule catches the case a per-job guard structurally cannot see: one job's
/// *output* landing on another job's *input* (`naming = "{stem}_master.{ext}"` over a batch
/// that already contains `song_master.wav`). By the time that job checks its destination the
/// other one may not have read its source yet.
pub fn check_planned_outputs(planned: &[PlannedOutput<'_>], inputs: &[PathBuf]) -> Result<()> {
    // Hashed rather than a linear scan: `planned` is inputs x targets, so an album batch is
    // already in the thousands and the quadratic version was measurable there.
    // One `current_dir` for the whole batch; `plan_key` is pure given it.
    let cwd = std::env::current_dir().ok();
    let key = |path: &Path| plan_key(path, cwd.as_deref());

    let input_set: HashSet<PathBuf> = inputs.iter().map(|p| key(p)).collect();
    let mut seen: HashMap<PathBuf, &PlannedOutput> = HashMap::with_capacity(planned.len());
    for entry in planned {
        let output = key(&entry.output);
        if let Some(other) = seen.get(&output) {
            bail!(
                "inputs '{}' and '{}' both render to '{}' for target '{}' — they share a stem \
                 and no naming placeholder carries the input's directory. Render the two \
                 sources in separate runs (with different `dir`s), or rename one of them",
                other.input.display(),
                entry.input.display(),
                entry.output.display(),
                entry.target
            );
        }
        // A job writing over its *own* input is the documented in-place render, and whether
        // it is allowed is `overwrite`'s question, asked by `pipeline::guard_output_path`.
        // Only writing over a *different* input of the same run is rejected here, and it is
        // rejected even under `--force`: with the jobs running concurrently, the other one
        // may read that file before or after this one replaces it, so the batch's output
        // would depend on scheduling.
        //
        // Compared on `plan_key`, not on the paths as written. The two sides of *this* rule
        // come from different places — the output from `dir` in the config, the input from
        // argv or a glob — so they routinely disagree about spelling while naming one file:
        // the shipped default is `dir = "./out"`, and an input given as an absolute path or
        // reached by a glob is neither `./`-prefixed nor necessarily relative.
        //
        // What this deliberately does not do is touch the filesystem. The identity question
        // — is this path *the file* that path names, through symlinks and hard links — needs
        // both ends to exist, and an output usually does not yet. `guard_output_path` asks
        // that one per job, of paths that are really there. It cannot cover this rule
        // though: under `--force` it returns before comparing, which is exactly the regime
        // where one job silently overwrites another job's source.
        if output != key(entry.input) && input_set.contains(&output) {
            bail!(
                "target '{}' would write '{}' over a different input of this same run; the \
                 job that reads it may or may not have got there first",
                entry.target,
                entry.output.display()
            );
        }
        seen.insert(output, entry);
    }
    Ok(())
}

/// The form two paths in the batch plan are compared in: absolute (against `cwd`) and
/// lexically normal.
///
/// Deliberately does not consult the filesystem — see the caller. Making paths absolute is
/// the half that matters most in practice, because the output side comes from `dir` in the
/// config and the input side from the command line, and there is nothing keeping those two
/// to the same convention. When there is no `cwd` to resolve against, relative and absolute
/// spellings simply do not match, which is the same answer the check gave before.
fn plan_key(path: &Path, cwd: Option<&Path>) -> PathBuf {
    match cwd {
        Some(cwd) if path.is_relative() => lexically_normal(&cwd.join(path)),
        _ => lexically_normal(path),
    }
}

/// `path` with `.` components dropped and `..` folded into the component before it, without
/// consulting the filesystem.
///
/// A leading `..`, or one following a component that might be a symlink, is left in place:
/// resolving those correctly requires the filesystem, and guessing is how a "normalizer"
/// starts reporting two different files as one.
fn lexically_normal(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    let mut popped_ok = 0usize; // components we put there ourselves and may take back
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if popped_ok > 0 => {
                out.pop();
                popped_ok -= 1;
            }
            Component::Normal(_) => {
                out.push(component);
                popped_ok += 1;
            }
            other => {
                out.push(other);
                popped_ok = 0; // a root or prefix: nothing before it can be popped
            }
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Reject targets that would write to the same path. Uses a probe stem so the
/// check is independent of the actual input file names: two targets collide only
/// when their `dir` + `naming` lack a distinguishing placeholder ({target},
/// {rate}, {format}).
pub fn check_collisions(targets: &[ResolvedTarget]) -> Result<()> {
    let probe = Path::new("__render_probe__.wav");
    let mut seen: Vec<(PathBuf, &str)> = Vec::new();

    for target in targets {
        // Source rate unknown here (the delivery rate may follow it) -> 0 is a safe,
        // conservative stand-in. DSD targets ignore it: their rate is the DSD rate.
        let path = target.render_output_path(probe, target.delivery_rate(0))?;
        if let Some((_, other)) = seen.iter().find(|(p, _)| *p == path) {
            bail!(
                "targets '{}' and '{}' resolve to the same output path; \
                 add {{target}} (or {{rate}}/{{format}}/{{dsd}}/{{cext}}) to naming, or give them different dirs",
                other,
                target.name
            );
        }
        seen.push((path, &target.name));
    }

    Ok(())
}

/// The one wording for "this FLAC target asks for a rate the encoder cannot reach", shared
/// by the load-time check (explicit `rate`) and the per-source one (derived `rate`). Names
/// both ways out, because "not supported" without them just moves the guessing earlier.
fn flac_rate_error(rate: u32) -> String {
    format!(
        "codec=flac cannot deliver {rate} Hz — mint's FLAC encoder tops out at {} Hz. \
         Set rate = {} (or lower) on this target, or use codec = \"wav\" to keep {rate} Hz",
        io_write::FLAC_MAX_SAMPLE_RATE,
        io_write::FLAC_MAX_SAMPLE_RATE
    )
}

pub fn format_tag(format: OutputSampleFormat) -> &'static str {
    match format {
        OutputSampleFormat::S16 => "s16",
        OutputSampleFormat::S24 => "s24",
        OutputSampleFormat::F32 => "f32",
        OutputSampleFormat::F64 => "f64",
    }
}

pub fn quality_tag(quality: ResampleQuality) -> &'static str {
    match quality {
        ResampleQuality::Lq => "lq",
        ResampleQuality::Mq => "mq",
        ResampleQuality::Hq => "hq",
        ResampleQuality::Vhq => "vhq",
    }
}

pub fn on_clip_tag(policy: OnClipPolicy) -> &'static str {
    match policy {
        OnClipPolicy::Limit => "limit",
        OnClipPolicy::ReduceGain => "reduce_gain",
        OnClipPolicy::Warn => "warn",
        OnClipPolicy::Maximize => "maximize",
    }
}

pub fn limiter_character_tag(character: LimiterCharacter) -> &'static str {
    match character {
        LimiterCharacter::Balanced => "balanced",
        LimiterCharacter::Transient => "transient",
        LimiterCharacter::Dense => "dense",
    }
}

fn limiter_policy_tag(
    policy: OnClipPolicy,
    character: LimiterCharacter,
    soft_clip: bool,
    link_channels: bool,
    max_limiting_db: f64,
) -> String {
    let limiter_detail = |verb: String| {
        format!(
            "{verb}; {} release; {}; soft_clip={}",
            limiter_character_tag(character),
            if link_channels { "linked" } else { "unlinked" },
            soft_clip
        )
    };
    match policy {
        OnClipPolicy::Limit => limiter_detail("limit".to_string()),
        OnClipPolicy::Maximize => {
            limiter_detail(format!("maximize to target (cap {max_limiting_db:.1} dB)"))
        }
        _ => on_clip_tag(policy).to_string(),
    }
}

pub fn dither_tag(dither: DitherMode) -> &'static str {
    match dither {
        DitherMode::Tpdf => "tpdf",
        DitherMode::None => "none",
        DitherMode::Shaped => "shaped",
        DitherMode::Psychoacoustic => "psychoacoustic",
        DitherMode::Adaptive => "adaptive",
    }
}

pub fn dither_strength_tag(strength: DitherStrength) -> &'static str {
    match strength {
        DitherStrength::Low => "low",
        DitherStrength::Normal => "normal",
        DitherStrength::High => "high",
    }
}

/// No dry-run line renders this today — it exists so a front end labelling the
/// `dither_correlation` dropdown spells the variants the way the TOML does.
pub fn dither_correlation_tag(correlation: DitherCorrelation) -> &'static str {
    match correlation {
        DitherCorrelation::Decorrelated => "decorrelated",
        DitherCorrelation::Correlated => "correlated",
        DitherCorrelation::MidSide => "mid_side",
    }
}

/// Dry-run label that reflects what the quantizer will *actually* do, including
/// the s16-only downgrade of the shaped modes to flat TPDF at higher bit depths.
pub fn effective_dither_tag(
    dither: DitherMode,
    strength: DitherStrength,
    format: OutputSampleFormat,
) -> String {
    if matches!(dither, DitherMode::Adaptive) {
        return if matches!(format, OutputSampleFormat::S16) {
            format!(
                "adaptive {} dither (psychoacoustic, per-frame)",
                dither_strength_tag(strength)
            )
        } else {
            format!(
                "tpdf dither (adaptive {} skipped: s16 only)",
                dither_strength_tag(strength)
            )
        };
    }

    if let Some(curve) = dither.shaping_curve() {
        return if dither.shapes(format) {
            format!("tpdf + noise-shaping ({})", curve.tag())
        } else {
            // A shaped mode whose shaping was skipped (non-s16 target).
            "tpdf dither (shaping skipped: s16 only)".to_string()
        };
    }
    format!("{} dither", dither_tag(dither))
}

/// `Display` for the config enums = the exact spelling the TOML accepts and the dry-run
/// prints. A macro rather than eight near-identical impls; the tag functions above stay
/// the single source of truth and this only forwards to them, so a dropdown label and a
/// `--dry-run` line cannot disagree.
macro_rules! display_via_tag {
    ($($ty:ty => $tag:expr),+ $(,)?) => {
        $(impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&($tag)(*self))
            }
        })+
    };
}

display_via_tag! {
    ResampleQuality => quality_tag,
    OnClipPolicy => on_clip_tag,
    LimiterCharacter => limiter_character_tag,
    DitherMode => dither_tag,
    DitherStrength => dither_strength_tag,
    DitherCorrelation => dither_correlation_tag,
    // These two carry their spelling elsewhere: bit depth through the `OutputSampleFormat`
    // the pipeline actually uses, and a codec through the extension it writes.
    BitDepthFormat => |f: BitDepthFormat| format_tag(f.into()),
    OutputCodec => OutputCodec::ext,
}

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::audio::buffer::OutputSampleFormat;
use crate::dither::ShapingCurve;

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
    pub quality: Option<ResampleQuality>,
    pub dither: Option<DitherMode>,
    /// Output container/codec: `wav` (default), `flac`, or `mp3`. Orthogonal to
    /// `format` — FLAC reuses the integer bit depth; MP3 ignores it (lossy).
    pub codec: Option<OutputCodec>,
    /// Write a Broadcast WAV `bext` chunk (with EBU R128 loudness metadata). WAV only.
    pub bwf: Option<bool>,
    /// MP3 CBR bitrate in kbps (default 320). Only used when `codec = "mp3"`.
    pub mp3_bitrate: Option<u32>,
    pub dir: Option<PathBuf>,
    pub naming: Option<String>,
    pub overwrite: Option<bool>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ResampleQuality {
    Lq,
    Mq,
    Hq,
    Vhq,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum OnClipPolicy {
    Limit,
    ReduceGain,
    Warn,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum LimiterCharacter {
    Balanced,
    Transient,
}

#[derive(Debug, Deserialize, Clone, Copy)]
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
    /// MBIT+-style adaptive dither: psychoacoustic + temporal masking driven,
    /// adaptive spectral redistribution, and stereo-correlated noise. This mode is
    /// tuned for s16 delivery and gracefully degrades to flat TPDF above s16.
    #[serde(rename = "mbit_plus")]
    MbitPlus,
}

impl DitherMode {
    /// The error-feedback curve this mode applies, or `None` for the flat/no-dither
    /// modes. Single source of truth for *which* modes shape and *with which* curve —
    /// shared by `bitdepth::apply` (the run), `shapes`, and `describe` (the dry-run),
    /// so none of them can drift.
    pub fn shaping_curve(self) -> Option<ShapingCurve> {
        match self {
            DitherMode::Shaped => Some(ShapingCurve::Gentle),
            DitherMode::Psychoacoustic => Some(ShapingCurve::Psychoacoustic),
            DitherMode::Tpdf | DitherMode::None | DitherMode::MbitPlus => None,
        }
    }

    /// Whether error-feedback noise shaping actually engages for `format`. Shaping
    /// only helps at s16; above it any shaped mode degrades to flat TPDF.
    pub fn shapes(self, format: OutputSampleFormat) -> bool {
        self.shaping_curve().is_some() && matches!(format, OutputSampleFormat::S16)
    }
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum BitDepthFormat {
    S16,
    S24,
    F32,
}

/// Output container / codec. Separate axis from `BitDepthFormat`: WAV and FLAC carry an
/// integer bit depth (FLAC has no float), MP3 is lossy and ignores bit depth entirely.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputCodec {
    Wav,
    Flac,
    Mp3,
}

impl OutputCodec {
    /// File extension and the value substituted for the `{cext}` naming placeholder.
    pub fn ext(self) -> &'static str {
        match self {
            OutputCodec::Wav => "wav",
            OutputCodec::Flac => "flac",
            OutputCodec::Mp3 => "mp3",
        }
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
        }
    }
}

/// A fully-resolved target with every effective value materialized.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub name: String,
    /// `None` keeps the source rate (no resample step is derived).
    pub rate: Option<u32>,
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
    pub quality: ResampleQuality,
    pub dither: DitherMode,
    pub codec: OutputCodec,
    /// Write a Broadcast WAV `bext` chunk (WAV only).
    pub bwf: bool,
    /// MP3 CBR bitrate (kbps); only meaningful when `codec == Mp3`.
    pub mp3_bitrate: u32,
    pub dir: PathBuf,
    pub naming: String,
    pub overwrite: bool,
}

impl ResolvedTarget {
    /// Render the output path for `input`. `out_rate` is the effective delivery
    /// rate (target rate, or the source rate when no resample happens) and is
    /// used for the `{rate}` placeholder.
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
            .replace("{format}", format_tag(self.format))
            .replace("{cext}", self.codec.ext());

        Ok(self.dir.join(file_name))
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
                )
            )),
        }

        let out_rate = self.rate.unwrap_or(source_rate);
        if out_rate != source_rate {
            steps.push(format!(
                "resample {source_rate} -> {out_rate} Hz ({})",
                quality_tag(self.quality)
            ));
        } else {
            steps.push(format!("keep sample rate {source_rate} Hz"));
        }

        match self.codec {
            // MP3 is lossy: no PCM quantization/dither — libmp3lame encodes from the
            // full-precision buffer directly.
            OutputCodec::Mp3 => steps.push(format!(
                "encode MP3 {} kbps (lossy; bit depth & dither not applicable)",
                self.mp3_bitrate
            )),
            codec => {
                match self.format {
                    OutputSampleFormat::F32 => {
                        steps.push("write f32 (no quantization)".to_string())
                    }
                    other => steps.push(format!(
                        "quantize -> {} + {}",
                        format_tag(other),
                        effective_dither_tag(self.dither, other)
                    )),
                }
                let container = match codec {
                    OutputCodec::Flac => "FLAC",
                    _ if self.bwf => "Broadcast WAV (bext + R128 metadata)",
                    _ => "WAV",
                };
                steps.push(format!("write {container}"));
            }
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

    /// Sorted list of all target names defined in the config.
    pub fn target_names(&self) -> Vec<String> {
        self.targets.keys().cloned().collect()
    }

    /// Resolve a single target by name, layering builtin < defaults < preset < explicit.
    pub fn resolve_target(&self, name: &str) -> Result<ResolvedTarget> {
        let raw = self
            .targets
            .get(name)
            .with_context(|| format!("no such target: '{name}'"))?
            .clone();

        // Precedence (lowest to highest): builtin fallbacks, [defaults], preset, explicit fields.
        let mut merged = overlay(builtin_fallback(), self.defaults.clone());
        if let Some(preset_name) = raw.preset.as_deref() {
            merged = overlay(merged, preset(preset_name)?);
        }
        merged = overlay(merged, raw);

        let codec = merged.codec.unwrap();
        // `format` is required for WAV/FLAC (it's the delivered bit depth). MP3 is lossy
        // and ignores it, so a missing format is allowed there and a dummy is used.
        let format: OutputSampleFormat = match merged.format.map(Into::into) {
            Some(f) => f,
            None if codec == OutputCodec::Mp3 => OutputSampleFormat::S16,
            None => bail!(
                "target '{name}': 'format' is required (set it directly or via a preset)"
            ),
        };

        // Builtin fallbacks guarantee these are populated; unwrap is safe.
        let resolved = ResolvedTarget {
            name: name.to_string(),
            rate: merged.rate,
            format,
            lufs: merged.lufs,
            ceiling_dbtp: merged.ceiling_dbtp.unwrap(),
            on_clip: merged.on_clip.unwrap(),
            limiter_character: merged.limiter_character.unwrap(),
            limiter_soft_clip: merged.limiter_soft_clip.unwrap(),
            limiter_link_channels: merged.limiter_link_channels.unwrap(),
            warn_limiting_db: merged.warn_limiting_db.unwrap(),
            quality: merged.quality.unwrap(),
            dither: merged.dither.unwrap(),
            codec,
            bwf: merged.bwf.unwrap(),
            mp3_bitrate: merged.mp3_bitrate.unwrap(),
            dir: merged.dir.unwrap(),
            naming: merged.naming.unwrap(),
            overwrite: merged.overwrite.unwrap(),
        };

        resolved.validate_fields()?;
        Ok(resolved)
    }

    /// Resolve every target and reject configs that cannot produce valid output.
    fn validate(&self) -> Result<()> {
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
        // FLAC is an integer codec; it has no floating-point sample format.
        if self.codec == OutputCodec::Flac && matches!(self.format, OutputSampleFormat::F32) {
            bail!(
                "target '{}': codec=flac requires an integer format (s16 or s24), not f32",
                self.name
            );
        }
        // The bext chunk is a WAV (RIFF) extension; it has no meaning in FLAC/MP3.
        if self.bwf && self.codec != OutputCodec::Wav {
            bail!("target '{}': bwf=true is only valid for codec=wav", self.name);
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
        quality: top.quality.or(base.quality),
        dither: top.dither.or(base.dither),
        codec: top.codec.or(base.codec),
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
        quality: Some(ResampleQuality::Vhq),
        dither: Some(DitherMode::Tpdf),
        codec: Some(OutputCodec::Wav),
        bwf: Some(false),
        mp3_bitrate: Some(320),
        dir: Some(PathBuf::from("./out")),
        // Per-target subdirectory keeps outputs from colliding; `{cext}` is the codec's
        // extension (wav/flac/mp3), so the default name follows the chosen container.
        naming: Some("{target}/{stem}.{cext}".to_string()),
        overwrite: Some(false),
    }
}

/// Built-in distribution presets. Values reflect each platform's published
/// loudness-normalization target and can be overridden per-target; they drift
/// over time, so treat them as sensible starting points, not gospel.
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
        other => bail!(
            "unknown preset '{other}'; valid presets: spotify, apple-music, youtube, \
             tidal, amazon-music, soundcloud, cd, broadcast-ebu-r128, 48k, hires"
        ),
    })
}

/// Reject targets that would write to the same path. Uses a probe stem so the
/// check is independent of the actual input file names: two targets collide only
/// when their `dir` + `naming` lack a distinguishing placeholder ({target},
/// {rate}, {format}).
fn check_collisions(targets: &[ResolvedTarget]) -> Result<()> {
    let probe = Path::new("__render_probe__.wav");
    let mut seen: Vec<(PathBuf, &str)> = Vec::new();

    for target in targets {
        // rate unknown here (may follow the source) -> 0 is a safe, conservative stand-in.
        let path = target.render_output_path(probe, target.rate.unwrap_or(0))?;
        if let Some((_, other)) = seen.iter().find(|(p, _)| *p == path) {
            bail!(
                "targets '{}' and '{}' resolve to the same output path; \
                 add {{target}} (or {{rate}}/{{format}}/{{cext}}) to naming, or give them different dirs",
                other,
                target.name
            );
        }
        seen.push((path, &target.name));
    }

    Ok(())
}

pub fn format_tag(format: OutputSampleFormat) -> &'static str {
    match format {
        OutputSampleFormat::S16 => "s16",
        OutputSampleFormat::S24 => "s24",
        OutputSampleFormat::F32 => "f32",
    }
}

fn quality_tag(quality: ResampleQuality) -> &'static str {
    match quality {
        ResampleQuality::Lq => "lq",
        ResampleQuality::Mq => "mq",
        ResampleQuality::Hq => "hq",
        ResampleQuality::Vhq => "vhq",
    }
}

fn on_clip_tag(policy: OnClipPolicy) -> &'static str {
    match policy {
        OnClipPolicy::Limit => "limit",
        OnClipPolicy::ReduceGain => "reduce_gain",
        OnClipPolicy::Warn => "warn",
    }
}

fn limiter_character_tag(character: LimiterCharacter) -> &'static str {
    match character {
        LimiterCharacter::Balanced => "balanced",
        LimiterCharacter::Transient => "transient",
    }
}

fn limiter_policy_tag(
    policy: OnClipPolicy,
    character: LimiterCharacter,
    soft_clip: bool,
    link_channels: bool,
) -> String {
    match policy {
        OnClipPolicy::Limit => format!(
            "limit; {} release; {}; soft_clip={}",
            limiter_character_tag(character),
            if link_channels { "linked" } else { "unlinked" },
            soft_clip
        ),
        _ => on_clip_tag(policy).to_string(),
    }
}

fn dither_tag(dither: DitherMode) -> &'static str {
    match dither {
        DitherMode::Tpdf => "tpdf",
        DitherMode::None => "none",
        DitherMode::Shaped => "shaped",
        DitherMode::Psychoacoustic => "psychoacoustic",
        DitherMode::MbitPlus => "mbit_plus",
    }
}

/// Dry-run label that reflects what the quantizer will *actually* do, including
/// the s16-only downgrade of the shaped modes to flat TPDF at higher bit depths.
fn effective_dither_tag(dither: DitherMode, format: OutputSampleFormat) -> String {
    if matches!(dither, DitherMode::MbitPlus) {
        return if matches!(format, OutputSampleFormat::S16) {
            "mbit+ adaptive dither (psychoacoustic + temporal + stereo-correlated)".to_string()
        } else {
            "tpdf dither (mbit+ skipped: s16 only)".to_string()
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

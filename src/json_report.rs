//! Machine-readable (`--json`) rendering of the batch processing report, for QC
//! automation. This is a thin DTO layer over `ProcessingReport` so the JSON schema stays
//! stable and decoupled from the internal structs — and so derived figures (PLR/PSR) and
//! unit-bearing field names are made explicit. Non-finite meter values (silence, or a clip
//! shorter than a meter's integration time) serialize as JSON `null`.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::diagnostics::WarningKind;
use crate::ops::loudness::LoudnessAnalysis;
use crate::pipeline::{DsdReport, ProcessingReport, TaskFailure};

/// Bumped when a consumer that reads this document could break: a field removed or
/// renamed, or one whose meaning changes. Adding a field does **not** bump it — every
/// reader already has to tolerate unknown keys. The GUI persists run history in exactly
/// this schema, and history without a version is unreadable in two years.
///
/// **2** (Tier 3): `warnings` went from an array of strings to an array of
/// `{kind, message}` objects, and `failures[].kind` was added. The prose is unchanged and
/// still in `message` — but the element *type* changed, which is exactly the "meaning
/// changes" case, so it bumps. A `kind` alongside a parallel `warnings` array of strings
/// would have avoided the bump and put two representations of one warning on the wire;
/// the version field exists so that trade does not have to be made. The GUI already
/// reports a version mismatch on an old run file rather than failing to list it.
pub const SCHEMA_VERSION: u32 = 2;

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u32,
    config: Option<&'a str>,
    tasks: Vec<TaskJson<'a>>,
    failures: Vec<FailureJson<'a>>,
}

#[derive(Serialize)]
struct TaskJson<'a> {
    target: &'a str,
    input: &'a str,
    output: &'a str,
    sample_rate: RateJson,
    codec: &'a str,
    /// Sample format (s16/s24/f32/f64) for PCM codecs, "<N> kbps" for MP3, "1-bit" for DSD.
    format: &'a str,
    /// True when a Broadcast WAV bext chunk was written.
    bwf: bool,
    channels: usize,
    /// Frames per channel as delivered; 1-bit `sampleCount` on a DSD task.
    frames: u64,
    /// Size of the file on disk, measured after writing rather than predicted.
    file_bytes: u64,
    elapsed_ms: u64,
    /// RNG seed for this render, or `null` when it was left to entropy — `null` means the
    /// render is not reproducible.
    seed: Option<u64>,
    /// Resampler quality tier, `null` when no sample-rate conversion ran.
    src_quality: Option<&'a str>,
    /// Static gain the normalization applied (dB); `null` when the target has no `lufs`.
    loudness_gain_db: Option<f64>,
    /// Further static gain from the post-resample correction (dB); `null` when that pass
    /// did not run (no resample, or no `lufs`), `0.0` when it ran and found nothing to do.
    post_resample_gain_db: Option<f64>,
    dither: DitherJson<'a>,
    /// DSD stage figures; `null` for every PCM and MP3 target.
    dsd: Option<DsdJson<'a>>,
    /// Peak gain reduction the limiter applied (dB); `null` when none was needed.
    limiter_gain_reduction_db: Option<f64>,
    /// Mean gain reduction (dB) over the limited portion; `null` when none was needed.
    limiter_avg_gain_reduction_db: Option<f64>,
    /// Percent (0–100) of samples the limiter attenuated; `null` when none was needed.
    limiter_active_percent: Option<f64>,
    /// The target's declared `lufs`; `null` when it asked for none. Emitted so a reader of
    /// this document alone can judge `delivered.integrated_lufs` — the config that stated
    /// the number is not part of the run history.
    declared_lufs: Option<f64>,
    /// The target's declared `ceiling_dbtp`, for the same reason: it is what turns
    /// `delivered.true_peak_dbtp` into a pass or a fail.
    declared_ceiling_dbtp: f64,
    source: AnalysisJson,
    delivered: AnalysisJson,
    warnings: Vec<WarningJson<'a>>,
}

/// A warning as both halves of what it is: the stable discriminant a machine branches on and
/// the sentence a human reads. `message` is [`WarningKind::message`]'s, so the document
/// cannot word a warning differently from the console that printed it.
#[derive(Serialize)]
struct WarningJson<'a> {
    /// Stable snake_case kind — `heavy_limiting`, `dsd_sigma_delta_overload`, …
    kind: &'a str,
    /// Coarser grouping: `loudness` | `peak` | `dither` | `dsd`.
    subject: &'a str,
    message: String,
}

impl<'a> From<&'a WarningKind> for WarningJson<'a> {
    fn from(w: &'a WarningKind) -> Self {
        Self { kind: w.tag(), subject: w.subject().label(), message: w.message() }
    }
}

#[derive(Serialize)]
struct RateJson {
    #[serde(rename = "in")]
    in_hz: u32,
    #[serde(rename = "out")]
    out_hz: u32,
}

#[derive(Serialize)]
struct DitherJson<'a> {
    applied: bool,
    /// Mode as configured; `null` on MP3 and DSD, which have no bit-depth stage. Together
    /// with `noise_shaping` this is what separates a target that asked for flat TPDF from
    /// one that asked for `adaptive` and got TPDF because the format was not s16.
    mode: Option<&'a str>,
    /// Strength as configured; `null` on the same codecs as `mode`.
    strength: Option<&'a str>,
    /// Noise-shaping curve name when shaping ran, else `null`.
    noise_shaping: Option<&'a str>,
}

#[derive(Serialize)]
struct DsdJson<'a> {
    /// "dsd64" | "dsd128" | "dsd256" | "dsd512".
    rate: &'a str,
    rate_hz: u32,
    /// PCM rate the modulator was fed from — derived unless the target set `rate`.
    intermediate_pcm_rate_hz: u32,
    interpolation_factor: u32,
    modulator_order: usize,
    /// Peak modulation index in percent; 50 % is the SACD 0 dB reference, 71.43 % the
    /// format maximum. The QC number for a DSD deliverable.
    max_modulation_percent: f64,
    /// Samples on which the sigma-delta loop hit a state clamp; 0 on a healthy run.
    overload_samples: u64,
    loop_resets: u64,
    /// `sampleCount` as written, i.e. including the silence the last block was topped up
    /// with (`src/audio/io_write_dsd.rs` documents why that counts as audio).
    samples_per_channel: u64,
    file_bytes: u64,
}

impl<'a> From<&'a DsdReport> for DsdJson<'a> {
    fn from(d: &'a DsdReport) -> Self {
        Self {
            rate: d.rate,
            rate_hz: d.rate_hz,
            intermediate_pcm_rate_hz: d.intermediate_pcm_rate,
            interpolation_factor: d.interpolation_factor,
            modulator_order: d.modulator_order,
            max_modulation_percent: d.max_modulation_percent,
            overload_samples: d.overload_samples,
            loop_resets: d.resets,
            samples_per_channel: d.samples_per_channel,
            file_bytes: d.file_bytes,
        }
    }
}

#[derive(Serialize)]
struct AnalysisJson {
    integrated_lufs: Option<f64>,
    loudness_range_lu: Option<f64>,
    max_momentary_lufs: Option<f64>,
    max_short_term_lufs: Option<f64>,
    true_peak_dbtp: Option<f64>,
    plr_db: Option<f64>,
    psr_db: Option<f64>,
}

#[derive(Serialize)]
struct FailureJson<'a> {
    target: &'a str,
    input: &'a str,
    error: &'a str,
    /// What kind of failure it was, recovered from the error's own chain rather than from
    /// its text: `cancelled` | `output_exists` | `output_is_the_input` | `delivery_blocked` |
    /// `batch_output_collision` | `batch_writes_over_an_input` | `other`. A reader can tell a
    /// cancelled job from a failed one without a regex.
    kind: &'static str,
}

/// Non-finite (`-inf`/`NaN`) meter values become `None` so the JSON shows `null`
/// deterministically, independent of how the serializer treats special floats.
fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

impl From<&LoudnessAnalysis> for AnalysisJson {
    fn from(a: &LoudnessAnalysis) -> Self {
        Self {
            integrated_lufs: finite(a.integrated_lufs),
            loudness_range_lu: finite(a.loudness_range_lu),
            max_momentary_lufs: finite(a.max_momentary_lufs),
            max_short_term_lufs: finite(a.max_short_term_lufs),
            true_peak_dbtp: finite(a.true_peak_dbtp),
            plr_db: finite(a.plr()),
            psr_db: finite(a.psr()),
        }
    }
}

/// Serialize the full batch result (successes + failures) as pretty JSON.
pub fn render(
    config_name: Option<&str>,
    reports: &[ProcessingReport],
    failures: &[TaskFailure],
) -> Result<String> {
    let tasks = reports
        .iter()
        .map(|r| TaskJson {
            target: &r.target,
            input: &r.input,
            output: &r.output,
            sample_rate: RateJson { in_hz: r.in_rate, out_hz: r.out_rate },
            codec: &r.codec,
            format: &r.format,
            bwf: r.bwf,
            channels: r.channels,
            frames: r.frames,
            file_bytes: r.file_bytes,
            elapsed_ms: r.elapsed_ms,
            seed: r.seed,
            src_quality: r.src_quality,
            loudness_gain_db: r.loudness_gain_db.and_then(finite),
            post_resample_gain_db: r.post_resample_gain_db.and_then(finite),
            dither: DitherJson {
                applied: r.dithered,
                mode: r.dither_mode,
                strength: r.dither_strength,
                noise_shaping: r.shaping_curve,
            },
            dsd: r.dsd.as_ref().map(Into::into),
            limiter_gain_reduction_db: r.limiter_gain_reduction_db,
            limiter_avg_gain_reduction_db: r.limiter_avg_gain_reduction_db,
            limiter_active_percent: r.limiter_active_percent,
            declared_lufs: r.declared_lufs,
            declared_ceiling_dbtp: r.declared_ceiling_dbtp,
            source: (&r.source).into(),
            delivered: (&r.delivered).into(),
            warnings: r.warnings.iter().map(Into::into).collect(),
        })
        .collect();

    let failures = failures
        .iter()
        .map(|f| FailureJson {
            target: &f.target,
            input: &f.input,
            error: &f.error,
            kind: f.kind.tag(),
        })
        .collect();

    let doc = JsonReport { schema_version: SCHEMA_VERSION, config: config_name, tasks, failures };
    serde_json::to_string_pretty(&doc).context("failed to serialize JSON report")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::FailureKind;

    fn analysis(integrated: f64) -> LoudnessAnalysis {
        LoudnessAnalysis {
            integrated_lufs: integrated,
            loudness_range_lu: 7.5,
            max_momentary_lufs: -12.0,
            max_short_term_lufs: -13.0,
            true_peak_dbtp: -1.0,
        }
    }

    #[test]
    fn serializes_tasks_failures_and_nulls() {
        let report = ProcessingReport {
            target: "spotify".into(),
            input: "in.wav".into(),
            output: "out/spotify/in.wav".into(),
            in_rate: 48_000,
            out_rate: 44_100,
            channels: 2,
            frames: 44_100,
            file_bytes: 176_444,
            elapsed_ms: 1_234,
            source: analysis(f64::NEG_INFINITY), // silence -> null
            delivered: analysis(-14.0),
            declared_lufs: Some(-14.0),
            declared_ceiling_dbtp: -1.0,
            limiter_gain_reduction_db: Some(2.5),
            limiter_avg_gain_reduction_db: Some(0.8),
            limiter_active_percent: Some(42.0),
            loudness_gain_db: Some(-3.5),
            post_resample_gain_db: Some(0.12),
            src_quality: Some("vhq"),
            dither_mode: Some("psychoacoustic"),
            dither_strength: Some("normal"),
            seed: Some(42),
            dithered: true,
            shaped: true,
            shaping_curve: Some("psychoacoustic"),
            codec: "wav".into(),
            bwf: true,
            format: "s16".into(),
            dsd: None,
            warnings: vec![WarningKind::HeavyLimiting {
                gain_reduction_db: 4.2,
                target_lufs: -14.0,
                ceiling_dbtp: -1.0,
                source_true_peak_dbtp: -0.2,
            }],
        };
        let failure = TaskFailure {
            target: "cd".into(),
            input: "bad.wav".into(),
            error: "boom".into(),
            kind: FailureKind::Other,
        };

        let json = render(Some("album"), &[report], &[failure]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["schema_version"].as_u64(), Some(SCHEMA_VERSION as u64));
        assert_eq!(v["config"].as_str(), Some("album"));
        assert_eq!(v["tasks"][0]["target"].as_str(), Some("spotify"));
        assert_eq!(v["tasks"][0]["channels"].as_u64(), Some(2));
        assert_eq!(v["tasks"][0]["frames"].as_u64(), Some(44_100));
        assert_eq!(v["tasks"][0]["file_bytes"].as_u64(), Some(176_444));
        assert_eq!(v["tasks"][0]["elapsed_ms"].as_u64(), Some(1_234));
        assert_eq!(v["tasks"][0]["seed"].as_u64(), Some(42));
        assert_eq!(v["tasks"][0]["src_quality"].as_str(), Some("vhq"));
        assert_eq!(v["tasks"][0]["loudness_gain_db"].as_f64(), Some(-3.5));
        assert_eq!(v["tasks"][0]["post_resample_gain_db"].as_f64(), Some(0.12));
        assert_eq!(v["tasks"][0]["dither"]["mode"].as_str(), Some("psychoacoustic"));
        assert_eq!(v["tasks"][0]["dither"]["strength"].as_str(), Some("normal"));
        assert_eq!(v["tasks"][0]["codec"].as_str(), Some("wav"));
        assert_eq!(v["tasks"][0]["bwf"].as_bool(), Some(true));
        assert_eq!(v["tasks"][0]["sample_rate"]["in"].as_u64(), Some(48_000));
        assert_eq!(v["tasks"][0]["sample_rate"]["out"].as_u64(), Some(44_100));
        assert_eq!(v["tasks"][0]["dither"]["noise_shaping"].as_str(), Some("psychoacoustic"));
        assert_eq!(v["tasks"][0]["limiter_gain_reduction_db"].as_f64(), Some(2.5));
        assert_eq!(v["tasks"][0]["limiter_avg_gain_reduction_db"].as_f64(), Some(0.8));
        assert_eq!(v["tasks"][0]["limiter_active_percent"].as_f64(), Some(42.0));
        // The declared figures travel with the task: a persisted document is read with no
        // config beside it, and without them the delivered numbers carry no verdict.
        assert_eq!(v["tasks"][0]["declared_lufs"].as_f64(), Some(-14.0));
        assert_eq!(v["tasks"][0]["declared_ceiling_dbtp"].as_f64(), Some(-1.0));
        // Non-finite source loudness must serialize as JSON null.
        assert!(v["tasks"][0]["source"]["integrated_lufs"].is_null());
        assert_eq!(v["tasks"][0]["delivered"]["integrated_lufs"].as_f64(), Some(-14.0));
        // PLR delivered = true_peak (-1.0) - integrated (-14.0) = 13.0.
        assert_eq!(v["tasks"][0]["delivered"]["plr_db"].as_f64(), Some(13.0));
        // A PCM task must carry the DSD block as an explicit null, not omit it.
        assert!(v["tasks"][0]["dsd"].is_null());
        assert_eq!(v["failures"][0]["target"].as_str(), Some("cd"));
        assert_eq!(v["failures"][0]["error"].as_str(), Some("boom"));
    }

    #[test]
    fn serializes_the_dsd_block() {
        let report = ProcessingReport {
            target: "sacd".into(),
            input: "in.wav".into(),
            output: "out/sacd/in.dsf".into(),
            in_rate: 96_000,
            out_rate: 2_822_400,
            channels: 2,
            frames: 32_768,
            file_bytes: 8_284,
            elapsed_ms: 9_000,
            source: analysis(-18.0),
            delivered: analysis(-16.0),
            // A target that declared no loudness: the verdict has to read as "nothing was
            // promised" rather than as a miss.
            declared_lufs: None,
            declared_ceiling_dbtp: -1.0,
            limiter_gain_reduction_db: None,
            limiter_avg_gain_reduction_db: None,
            limiter_active_percent: None,
            loudness_gain_db: None,
            post_resample_gain_db: None,
            src_quality: Some("vhq"),
            dither_mode: None,
            dither_strength: None,
            seed: None,
            dithered: false,
            shaped: false,
            shaping_curve: None,
            codec: "dsf".into(),
            bwf: false,
            format: "1-bit".into(),
            dsd: Some(DsdReport {
                rate: "dsd64",
                rate_hz: 2_822_400,
                intermediate_pcm_rate: 88_200,
                interpolation_factor: 32,
                modulator_order: 7,
                max_modulation_percent: 44.6,
                overload_samples: 0,
                resets: 0,
                samples_per_channel: 32_768,
                file_bytes: 8_284,
            }),
            warnings: vec![],
        };

        let json = render(None, &[report], &[]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        let dsd = &v["tasks"][0]["dsd"];
        assert_eq!(dsd["rate"].as_str(), Some("dsd64"));
        assert_eq!(dsd["rate_hz"].as_u64(), Some(2_822_400));
        assert_eq!(dsd["intermediate_pcm_rate_hz"].as_u64(), Some(88_200));
        assert_eq!(dsd["interpolation_factor"].as_u64(), Some(32));
        assert_eq!(dsd["max_modulation_percent"].as_f64(), Some(44.6));
        assert_eq!(dsd["overload_samples"].as_u64(), Some(0));
        assert_eq!(v["tasks"][0]["format"].as_str(), Some("1-bit"));
        // The 1-bit rate, not the intermediate PCM one.
        assert_eq!(v["tasks"][0]["sample_rate"]["out"].as_u64(), Some(2_822_400));
        // DSD has no bit-depth stage, so the dither settings must read as absent rather
        // than as whatever the target happened to carry.
        assert!(v["tasks"][0]["dither"]["mode"].is_null());
        assert!(v["tasks"][0]["dither"]["strength"].is_null());
        // ...but the seed still applies: it drives the modulator's comparator dither.
        assert!(v["tasks"][0]["seed"].is_null());
        // No loudness was asked for, so there is nothing to pass or fail — null, not a
        // number a reader could mistake for a target that was missed.
        assert!(v["tasks"][0]["declared_lufs"].is_null());
        assert_eq!(v["tasks"][0]["declared_ceiling_dbtp"].as_f64(), Some(-1.0));
        assert_eq!(v["tasks"][0]["src_quality"].as_str(), Some("vhq"));
    }
}

//! Machine-readable (`--json`) rendering of the batch processing report, for QC
//! automation. This is a thin DTO layer over `ProcessingReport` so the JSON schema stays
//! stable and decoupled from the internal structs — and so derived figures (PLR/PSR) and
//! unit-bearing field names are made explicit. Non-finite meter values (silence, or a clip
//! shorter than a meter's integration time) serialize as JSON `null`.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::ops::loudness::LoudnessAnalysis;
use crate::pipeline::{ProcessingReport, TaskFailure};

#[derive(Serialize)]
struct JsonReport<'a> {
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
    /// Sample format (s16/s24/f32) for PCM codecs, or "<N> kbps" for MP3.
    format: &'a str,
    /// True when a Broadcast WAV bext chunk was written.
    bwf: bool,
    dither: DitherJson<'a>,
    /// Peak gain reduction the limiter applied (dB); `null` when none was needed.
    limiter_gain_reduction_db: Option<f64>,
    source: AnalysisJson,
    delivered: AnalysisJson,
    warnings: &'a [String],
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
    /// Noise-shaping curve name when shaping ran, else `null`.
    noise_shaping: Option<&'a str>,
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
            dither: DitherJson { applied: r.dithered, noise_shaping: r.shaping_curve },
            limiter_gain_reduction_db: r.limiter_gain_reduction_db,
            source: (&r.source).into(),
            delivered: (&r.delivered).into(),
            warnings: &r.warnings,
        })
        .collect();

    let failures = failures
        .iter()
        .map(|f| FailureJson { target: &f.target, input: &f.input, error: &f.error })
        .collect();

    let doc = JsonReport { config: config_name, tasks, failures };
    serde_json::to_string_pretty(&doc).context("failed to serialize JSON report")
}

#[cfg(test)]
mod tests {
    use super::*;

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
            source: analysis(f64::NEG_INFINITY), // silence -> null
            delivered: analysis(-14.0),
            limiter_gain_reduction_db: Some(2.5),
            dithered: true,
            shaped: true,
            shaping_curve: Some("psychoacoustic"),
            codec: "wav".into(),
            bwf: true,
            format: "s16".into(),
            warnings: vec!["heavy limiting".into()],
        };
        let failure = TaskFailure {
            target: "cd".into(),
            input: "bad.wav".into(),
            error: "boom".into(),
        };

        let json = render(Some("album"), &[report], &[failure]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(v["config"].as_str(), Some("album"));
        assert_eq!(v["tasks"][0]["target"].as_str(), Some("spotify"));
        assert_eq!(v["tasks"][0]["codec"].as_str(), Some("wav"));
        assert_eq!(v["tasks"][0]["bwf"].as_bool(), Some(true));
        assert_eq!(v["tasks"][0]["sample_rate"]["in"].as_u64(), Some(48_000));
        assert_eq!(v["tasks"][0]["sample_rate"]["out"].as_u64(), Some(44_100));
        assert_eq!(v["tasks"][0]["dither"]["noise_shaping"].as_str(), Some("psychoacoustic"));
        assert_eq!(v["tasks"][0]["limiter_gain_reduction_db"].as_f64(), Some(2.5));
        // Non-finite source loudness must serialize as JSON null.
        assert!(v["tasks"][0]["source"]["integrated_lufs"].is_null());
        assert_eq!(v["tasks"][0]["delivered"]["integrated_lufs"].as_f64(), Some(-14.0));
        // PLR delivered = true_peak (-1.0) - integrated (-14.0) = 13.0.
        assert_eq!(v["tasks"][0]["delivered"]["plr_db"].as_f64(), Some(13.0));
        assert_eq!(v["failures"][0]["target"].as_str(), Some("cd"));
        assert_eq!(v["failures"][0]["error"].as_str(), Some("boom"));
    }
}

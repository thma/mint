//! Does the delivered file match what the target promised — and if it cannot, is that said
//! before the work is done rather than after?
//!
//! Two defects with the same shape, both from `report.md` § 8:
//!
//! - **Loudness was verified against the wrong buffer.** The ±0.1 LU "target not met" check
//!   read the figure `loudness::apply` measured at the *source* rate, before the resampler.
//!   Downsampling discards K-weighted energy above the new Nyquist, so an ultrasonic-heavy
//!   96 kHz master could ship 2.54 LU off target with a clean report. The check now reads the
//!   delivered measurement, and `ops::loudness::correct_loudness_after_resample` pulls the
//!   drift back out within the true-peak headroom.
//! - **FLAC above 96 kHz failed at write time**, after loudness, resampling, limiting and
//!   quantization had all run, with flacenc's `VerifyError` debug struct for a message. It is
//!   now a load-time error when the target names the rate and a pre-decode one when the rate
//!   follows the source.
//!
//! The 96 kHz sources here carry a deliberate 30 kHz tone. That is the whole mechanism: at
//! 30 kHz BS.1770's K-weighting shelf is still counting energy that a 44.1 kHz deliverable
//! physically cannot carry.

use std::path::{Path, PathBuf};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::config::{Config, ResolvedTarget};
use mint::ops::loudness;
use mint::pipeline;

const SEED: u64 = 0x105D;

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("delivery-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn tone(rate: u32, frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin())
        .collect()
}

/// A stereo master whose energy is split between an audible tone and an ultrasonic one.
/// `ultrasonic_amp = 0.0` gives the band-limited control.
fn write_master(dir: &Path, rate: u32, seconds: f64, audible_amp: f64, ultrasonic_amp: f64) -> PathBuf {
    let frames = (rate as f64 * seconds) as usize;
    let mix = |hz: f64| {
        let low = tone(rate, frames, hz, audible_amp);
        if ultrasonic_amp == 0.0 {
            return low;
        }
        let high = tone(rate, frames, 30_000.0, ultrasonic_amp);
        low.iter().zip(&high).map(|(a, b)| a + b).collect()
    };
    let buffer = AudioBuffer {
        channels: vec![mix(997.0), mix(1245.0)],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("master.wav"),
            channels: 2,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    let path = dir.join("master.wav");
    io_write::write_wav(&path, &buffer, None).expect("write master");
    path
}

fn target(dir: &Path, body: &str) -> ResolvedTarget {
    let toml = format!("[target.t]\ndir = \"{}\"\n{body}\n", dir.join("out").display());
    let path = dir.join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    Config::from_path(&path)
        .expect("config should resolve")
        .resolve_target("t")
        .expect("target should resolve")
}

fn config_error(dir: &Path, toml: &str) -> String {
    let path = dir.join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    match Config::from_path(&path) {
        Ok(_) => panic!("config should have been rejected:\n{toml}"),
        Err(e) => format!("{e:#}"),
    }
}

// ═══════════════════ 1. loudness is verified against what ships ══════════════════════

/// The measured defect, end to end. Without the post-SRC verification this delivers several
/// LU below target and reports nothing at all.
#[test]
fn an_ultrasonic_heavy_master_still_lands_on_target_after_the_resample() {
    let dir = tmp("ultrasonic");
    let input = write_master(&dir, 96_000, 4.0, 0.2, 0.2);
    let t = target(
        &dir,
        "format = \"s24\"\nrate = 44100\nlufs = -16.0\nceiling_dbtp = -1.0\n",
    );

    let report = pipeline::process(&input, &t, Some(SEED)).expect("render");
    let delivered = report.delivered.integrated_lufs;

    // The premise, measured rather than asserted from theory: the conversion really does
    // take loudness away, and that gap is what used to be delivered silently.
    let mut resampled = mint::audio::io_read::read_audio(&input).expect("re-read");
    mint::ops::resample::apply(&mut resampled, 44_100, None).expect("resample");
    let cost = report.source.integrated_lufs
        - loudness::analyze(&resampled).expect("analyze").integrated_lufs;
    assert!(
        cost > 1.0,
        "test premise: the 30 kHz content must cost more than 1 LU across 96 -> 44.1 kHz \
         (measured {cost:.2})"
    );

    assert!(
        (delivered + 16.0).abs() <= 0.1,
        "delivered {delivered:.2} LUFS against a -16.00 target"
    );
    assert!(
        report.delivered.true_peak_dbtp <= -1.0 + 0.05,
        "the correction must not have pushed the true peak past the ceiling (got {:.3})",
        report.delivered.true_peak_dbtp
    );
    // A correction this large is worth saying out loud even though it succeeded: the same
    // master at a different delivery rate will not measure the same.
    assert!(
        report.warnings.iter().any(|w| w.contains("across the resample")),
        "expected the drift to be reported, got: {:?}",
        report.warnings
    );
}

/// The control: ordinary band-limited programme drifts ~0.03 LU, which is inside the
/// correction's tolerance. Nothing must be applied and nothing must be said.
#[test]
fn a_band_limited_master_is_neither_corrected_nor_warned_about() {
    let dir = tmp("bandlimited");
    let input = write_master(&dir, 96_000, 4.0, 0.2, 0.0);
    let t = target(
        &dir,
        "format = \"s24\"\nrate = 44100\nlufs = -16.0\nceiling_dbtp = -1.0\n",
    );

    let report = pipeline::process(&input, &t, Some(SEED)).expect("render");
    assert!(
        (report.delivered.integrated_lufs + 16.0).abs() <= 0.1,
        "delivered {:.2} LUFS against a -16.00 target",
        report.delivered.integrated_lufs
    );
    assert!(
        !report.warnings.iter().any(|w| w.contains("across the resample")),
        "a 0.03 LU drift must not be reported: {:?}",
        report.warnings
    );
}

/// A correction that cannot be made must be reported honestly — with the delivered figure,
/// not the one measured before the resampler. This is the assertion the old check could not
/// make: it compared the pre-SRC number against the target and found it perfect.
#[test]
fn the_target_not_met_warning_quotes_the_delivered_loudness() {
    let dir = tmp("residual");
    let input = write_master(&dir, 96_000, 4.0, 0.2, 0.2);
    // An unreachable target: the correction is capped at the headroom under the ceiling, so
    // no amount of it can put a -1 dBTP file at -3 LUFS when the material's own crest is
    // wider than 2 dB. The miss is real, and reporting it means quoting the delivered
    // number — which is exactly what the pre-SRC check could not do.
    let t = target(
        &dir,
        "format = \"s24\"\nrate = 44100\nlufs = -1.0\nceiling_dbtp = -6.0\non_clip = \"reduce_gain\"\n",
    );

    let report = pipeline::process(&input, &t, Some(SEED)).expect("render");
    let warning = report
        .warnings
        .iter()
        .find(|w| w.contains("loudness target not met"))
        .unwrap_or_else(|| panic!("expected a target-not-met warning, got: {:?}", report.warnings));

    // The figure in the warning has to be the one in the report, or the two disagree about
    // the same file.
    let delivered = format!("{:.2}", report.delivered.integrated_lufs);
    assert!(
        warning.contains(&delivered),
        "the warning must quote the delivered loudness {delivered}: {warning}"
    );
}

/// Unit-level: the correction never spends more than the headroom, so it can never push a
/// peak past the ceiling and never needs the limiter.
#[test]
fn the_post_resample_correction_stops_at_the_ceiling() {
    let rate = 48_000;
    let frames = rate as usize;
    let mut buffer = AudioBuffer {
        channels: vec![tone(rate, frames, 997.0, 0.8)],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels: 1,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    };
    let ceiling = -1.0;
    let tp = loudness::measure_true_peak_dbtp(&buffer).expect("tp");
    let headroom = ceiling - tp;
    assert!(headroom > 0.0 && headroom < 2.0, "premise: a little headroom ({headroom:.3} dB)");

    // Ask for far more gain than the headroom can pay for.
    let before = loudness::analyze(&buffer).expect("analyze").integrated_lufs;
    let correction =
        loudness::correct_loudness_after_resample(&mut buffer, 12.0, ceiling, tp, before)
            .expect("correct");

    let after = loudness::measure_true_peak_dbtp(&buffer).expect("tp after");
    assert!(
        after <= ceiling + 1e-6,
        "the correction must stop at the ceiling: {after:.6} dBTP vs {ceiling:.6}"
    );
    assert!(
        correction.warnings.iter().any(|w| w.contains("capped by")),
        "a clamped correction must say so: {:?}",
        correction.warnings
    );
    // The reported gain is the capped one, not the shortfall it wanted — the report would
    // otherwise claim a move the ceiling never allowed.
    assert!(
        (correction.applied_db - headroom).abs() < 1e-6,
        "reported gain {:.6} dB should be the headroom {headroom:.6} dB",
        correction.applied_db
    );
}

#[test]
fn the_post_resample_correction_is_a_no_op_inside_its_tolerance() {
    let rate = 48_000;
    let frames = rate as usize;
    let mut buffer = AudioBuffer {
        channels: vec![tone(rate, frames, 997.0, 0.1)],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels: 1,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    };
    let measured = loudness::analyze(&buffer).expect("analyze").integrated_lufs;
    let before = buffer.channels[0].clone();

    // 0.02 LU away: inside the 0.05 LU tolerance.
    let correction = loudness::correct_loudness_after_resample(
        &mut buffer,
        measured + 0.02,
        -1.0,
        -20.0,
        measured,
    )
    .expect("correct");

    assert!(
        correction.warnings.is_empty(),
        "no warning expected: {:?}",
        correction.warnings
    );
    assert_eq!(correction.applied_db, 0.0, "a no-op must report no gain");
    assert_eq!(before, buffer.channels[0], "the buffer must not be touched at all");
}

// ══════════════════════ 2. FLAC above 96 kHz fails before the work ═══════════════════════

#[test]
fn an_explicit_flac_rate_above_96k_is_rejected_when_the_config_loads() {
    let dir = tmp("flac-config");
    let message = config_error(
        &dir,
        "[target.hires]\nformat = \"s24\"\ncodec = \"flac\"\nrate = 192000\n",
    );
    assert!(message.contains("192000"), "{message}");
    assert!(message.contains("96000"), "the message must name the limit: {message}");
    assert!(
        message.contains("codec = \"wav\""),
        "the message must offer the way out: {message}"
    );
}

#[test]
fn a_flac_rate_at_the_limit_is_accepted() {
    let dir = tmp("flac-96k");
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        "[target.hires]\nformat = \"s24\"\ncodec = \"flac\"\nrate = 96000\n",
    )
    .expect("write config");
    Config::from_path(&path).expect("96 kHz FLAC is fine");
}

/// The half the config cannot see: a target that follows the source rate is a *correct*
/// config, and only the master decides whether it is deliverable.
#[test]
fn a_derived_flac_rate_above_96k_fails_before_any_dsp_runs() {
    let dir = tmp("flac-derived");
    let input = write_master(&dir, 192_000, 0.5, 0.2, 0.0);
    let t = target(&dir, "format = \"s24\"\ncodec = \"flac\"\nlufs = -16.0\n");

    // The dry-run and the run must agree — they read the same predicate.
    let blocker = t
        .delivery_blocker(192_000, Some(2))
        .expect("the dry-run must predict this failure");
    assert!(blocker.contains("192000"), "{blocker}");

    let message = match pipeline::process(&input, &t, Some(SEED)) {
        Ok(r) => panic!("expected a refusal, got {}", r.output),
        Err(e) => format!("{e:#}"),
    };
    assert!(message.contains("96000"), "{message}");
    assert!(
        !dir.join("out").exists(),
        "a refused job must not even have created its output directory"
    );
}

/// A 192 kHz master into a *96 kHz* FLAC target is a normal deliverable and must still work
/// — the guard is on the delivered rate, not on the source.
#[test]
fn a_192k_master_resampled_down_to_96k_flac_is_fine() {
    let dir = tmp("flac-downsampled");
    let input = write_master(&dir, 192_000, 0.5, 0.2, 0.0);
    let t = target(&dir, "format = \"s24\"\ncodec = \"flac\"\nrate = 96000\n");

    let report = pipeline::process(&input, &t, Some(SEED)).expect("render");
    assert_eq!(report.out_rate, 96_000);
    assert!(Path::new(&report.output).exists(), "the .flac must be on disk");
}

/// The writer keeps its own guard, the way `DsfWriter::create` keeps its channel check: a
/// caller that finds a route around the config and the pipeline still gets a sentence.
#[test]
fn the_flac_writer_refuses_an_out_of_range_rate_on_its_own() {
    let dir = tmp("flac-writer");
    let buffer = AudioBuffer {
        channels: vec![tone(192_000, 4_096, 997.0, 0.2)],
        sample_rate: 192_000,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels: 1,
            sample_rate: 192_000,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    let path = dir.join("out.flac");
    let message = match io_write::write_flac(&path, &buffer, 24) {
        Ok(()) => panic!("the writer must refuse 192 kHz"),
        Err(e) => format!("{e:#}"),
    };
    assert!(message.contains("FLAC supports up to 96000 Hz"), "{message}");
    assert!(!path.exists(), "nothing may be left on disk");
}

//! What a render *says it did*, checked against what it actually did.
//!
//! Everything here was computed inside `pipeline::process` and thrown away before
//! (report.md §9 item 6): the applied gain, the dither settings, the seed, the real file
//! size. A figure that is merely reported is easy to get subtly wrong and impossible to
//! notice, so each one is pinned against an independent measurement rather than against
//! itself — the size against `fs::metadata` *and* against the dry-run's own prediction,
//! the gain against the source and target loudness, the DSD frame count against the
//! container's `sampleCount`.
//!
//! The last test is the T1-3 pair: `pipeline::output_conflict` is a query and
//! `guard_output_path` is the enforcement of the same rule, and the only thing keeping
//! them from drifting is that one is implemented in terms of the other. This checks the
//! observable consequence — the query's verdict predicts whether the render is refused.

use std::path::{Path, PathBuf};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::config::{Config, ResolvedTarget, SizeEstimate};
use mint::pipeline::{self, Conflict};

const RATE: u32 = 48_000;
/// 0.1 s. Enough for anything that does not read a loudness figure, and short enough that
/// a debug-build DSD encode stays free.
const SHORT: usize = RATE as usize / 10;
/// 1.5 s. BS.1770's integrated measurement gates on 400 ms blocks, so a shorter clip
/// meters as `-inf` and every gain assertion turns into `inf == inf`.
const METERED: usize = RATE as usize * 3 / 2;

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("report-fields-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn tone(frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / RATE as f64).sin())
        .collect()
}

/// A stereo master on disk, quiet enough that no target here needs the limiter.
fn write_master(path: &Path, frames: usize) -> PathBuf {
    let buffer = AudioBuffer {
        channels: vec![tone(frames, 997.0, 0.2), tone(frames, 1245.0, 0.2)],
        sample_rate: RATE,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: 2,
            sample_rate: RATE,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    io_write::write_wav(path, &buffer, None).expect("write master");
    path.to_path_buf()
}

/// Resolve one target out of a config whose `dir` is the scratch directory. The path is
/// spliced into TOML, so a Windows separator has to survive being a TOML basic string.
fn target_in(dir: &Path, name: &str, body: &str) -> ResolvedTarget {
    let dir = dir.display().to_string().replace('\\', "\\\\");
    let toml = format!("[target.{name}]\ndir = \"{dir}\"\n{body}");
    Config::from_toml_str(&toml)
        .unwrap_or_else(|e| panic!("config did not resolve: {e:#}"))
        .resolve_target(name)
        .expect("resolve")
}

#[test]
fn the_reported_size_is_the_file_on_disk_and_the_size_the_dry_run_promised() {
    let dir = tmp("size");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s24\"\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    let on_disk = std::fs::metadata(&report.output).expect("stat").len();
    assert_eq!(report.file_bytes, on_disk, "the report must not predict, it must measure");

    // WAV claims `Exact`, so the dry-run and the render have to agree to the byte. This is
    // the first time the two numbers can be compared without a human reading both.
    let estimate = target
        .estimated_output_bytes(RATE, SHORT as u64, 2)
        .expect("an estimate for a WAV target");
    assert!(
        matches!(estimate, SizeEstimate::Exact(_)),
        "WAV claims an exact size: {estimate:?}"
    );
    assert_eq!(estimate.bytes(), report.file_bytes, "the dry-run promised a different size");

    assert_eq!(report.channels, 2);
    assert_eq!(report.frames, SHORT as u64, "no resample, so frames are unchanged");
}

#[test]
fn a_normalized_render_reports_the_gain_it_applied() {
    let dir = tmp("gain");
    let input = write_master(&dir.join("master.wav"), METERED);
    let target = target_in(&dir, "t", "format = \"s24\"\nlufs = -20.0\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    let gain = report.loudness_gain_db.expect("a target with lufs must report its gain");
    let expected = -20.0 - report.source.integrated_lufs;
    assert!(
        (gain - expected).abs() < 0.01,
        "reported {gain:.4} dB, but the source measured {:.4} LUFS against a -20.0 target \
         (expected {expected:.4} dB)",
        report.source.integrated_lufs
    );
    // Quiet source, generous target: nothing for the limiter to do, so the gain alone has
    // to land the delivered loudness. If it does not, the reported number is not the
    // number that was applied.
    assert!(
        (report.delivered.integrated_lufs - -20.0).abs() < 0.1,
        "delivered {:.3} LUFS",
        report.delivered.integrated_lufs
    );
    assert_eq!(
        report.post_resample_gain_db, None,
        "no resample ran, so the post-SRC correction never got the chance to"
    );
    assert_eq!(report.src_quality, None, "no SRC ran");
}

#[test]
fn a_pure_transcode_reports_no_gain_at_all() {
    let dir = tmp("nogain");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s16\"\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    assert_eq!(report.loudness_gain_db, None, "no lufs means no normalization step ran");
    assert_eq!(report.post_resample_gain_db, None);
}

#[test]
fn a_resampled_render_reports_the_quality_tier_and_the_post_src_correction() {
    let dir = tmp("src");
    let input = write_master(&dir.join("master.wav"), METERED);
    let target =
        target_in(&dir, "t", "format = \"s24\"\nrate = 44100\nlufs = -20.0\nquality = \"hq\"\n");

    let report = pipeline::process(&input, &target, Some(1)).expect("process");

    assert_eq!(report.src_quality, Some("hq"), "the tier that actually ran, not the default");
    assert_eq!(report.out_rate, 44_100);
    // The correction pass ran; whether it had anything to do is the program's business,
    // but "ran and did nothing" (0.0) must be distinguishable from "never ran" (None).
    assert!(report.post_resample_gain_db.is_some(), "the correction pass ran, so it must report");
}

#[test]
fn the_dither_settings_reported_are_the_ones_that_were_asked_for() {
    let dir = tmp("dither");
    let input = write_master(&dir.join("master.wav"), SHORT);

    // s16: adaptive engages, so what was asked for and what happened agree.
    let engaged = target_in(&dir, "a", "format = \"s16\"\ndither = \"adaptive\"\n");
    let report = pipeline::process(&input, &engaged, Some(3)).expect("process");
    assert_eq!(report.dither_mode, Some("adaptive"));
    assert_eq!(report.dither_strength, Some("normal"));
    assert!(report.dithered);
    assert_eq!(report.shaping_curve, Some("adaptive"));

    // s24: the same request degrades to flat TPDF. Before this pair of fields existed, the
    // report was indistinguishable from a target that had simply asked for tpdf — which is
    // the exact case T1-2 was written for.
    let degraded = target_in(&dir, "b", "format = \"s24\"\ndither = \"adaptive\"\n");
    let report = pipeline::process(&input, &degraded, Some(3)).expect("process");
    assert_eq!(report.dither_mode, Some("adaptive"), "the request is reported as made");
    assert_eq!(report.shaping_curve, None, "...and the shaping is reported as skipped");
    assert!(report.dithered, "flat TPDF still ran");
}

#[test]
fn the_seed_is_reported_exactly_when_the_render_is_reproducible() {
    let dir = tmp("seed");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s16\"\n");

    let pinned = pipeline::process(&input, &target, Some(0xC0FFEE)).expect("process");
    assert_eq!(pinned.seed, Some(0xC0FFEE));

    let unpinned = target_in(&dir, "u", "format = \"s16\"\n");
    let report = pipeline::process(&input, &unpinned, None).expect("process");
    assert_eq!(report.seed, None, "an unseeded run must not claim to be reproducible");
}

#[test]
fn a_dsd_render_reports_the_delivered_1_bit_length_and_no_dither_settings() {
    let dir = tmp("dsd");
    let input = write_master(&dir.join("master.wav"), SHORT);
    // `dither` is set deliberately: on a DSD target it is silently ignored, and the report
    // must say so by omission rather than by echoing a setting that never ran.
    let target = target_in(&dir, "t", "codec = \"dsf\"\ndsd = \"dsd64\"\ndither = \"shaped\"\n");

    let report = pipeline::process(&input, &target, Some(5)).expect("process");
    let dsd = report.dsd.as_ref().expect("a dsf target reports its DSD figures");

    assert_eq!(report.dither_mode, None, "DSD has no bit-depth stage to configure");
    assert_eq!(report.dither_strength, None);
    assert!(!report.dithered);
    // The seed still matters here — it drives the modulator's comparator dither.
    assert_eq!(report.seed, Some(5));
    // A DSD target always resamples, even from a rate that needs no PCM conversion: the
    // interpolation up to the 1-bit rate is an SRC on the same tier.
    assert_eq!(report.src_quality, Some("vhq"));

    assert_eq!(
        report.frames, dsd.samples_per_channel,
        "the delivered length is the 1-bit one, not the intermediate PCM buffer's"
    );
    let on_disk = std::fs::metadata(&report.output).expect("stat").len();
    assert_eq!(report.file_bytes, on_disk);
    assert_eq!(dsd.file_bytes, on_disk, "the writer's own accounting must match the file");
}

#[test]
fn the_conflict_query_predicts_what_the_render_will_refuse() {
    let dir = tmp("conflict");
    let input = write_master(&dir.join("master.wav"), SHORT);
    let target = target_in(&dir, "t", "format = \"s24\"\n");
    let output = target.render_output_path(&input, RATE).expect("output path");

    // Nothing there yet: the query says clear, and the render agrees.
    assert_eq!(pipeline::output_conflict(&input, &output, false), None);
    pipeline::process(&input, &target, Some(1)).expect("the first render must succeed");

    // Now something is: the query says so, and the same render is refused.
    assert_eq!(
        pipeline::output_conflict(&input, &output, false),
        Some(Conflict::Existing),
        "an existing output is a conflict once overwrite is off"
    );
    let err = pipeline::process(&input, &target, Some(1)).expect_err("the second must be refused");
    assert!(
        format!("{err:#}").contains("output file already exists"),
        "the guard must bail with the query's own message: {err:#}"
    );
    // ...and the message the GUI would show is the message the CLI prints.
    assert!(
        Conflict::Existing
            .message(&input, &output)
            .contains("output file already exists")
    );

    // Consent removes the conflict without moving the rule anywhere else.
    assert_eq!(pipeline::output_conflict(&input, &output, true), None);
    let overwriting = target_in(&dir, "t", "format = \"s24\"\noverwrite = true\n");
    pipeline::process(&input, &overwriting, Some(1)).expect("overwrite = true must be honoured");

    // The input case is a different conflict with a different message, and it is decided by
    // file identity rather than by spelling — hence the `./`-style detour to the same file.
    let spelled_differently = dir.join(".").join("master.wav");
    assert_eq!(
        pipeline::output_conflict(&input, &spelled_differently, false),
        Some(Conflict::Input),
        "the output resolving to the input master is its own case"
    );
    assert!(
        Conflict::Input
            .message(&input, &spelled_differently)
            .contains("refusing to overwrite the input file")
    );
}

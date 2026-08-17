//! `plan_job` and `process_buffer`: the header pre-flight and the chain, as two things a
//! caller can hold separately.
//!
//! `pipeline::process` used to be one function that probed, guarded, decoded and rendered.
//! Splitting it is `docs/gui-proposal.md` §6 Tier 3's "`process_buffer` so the GUI can hand
//! over already-decoded audio", and the property that makes the split worth anything is the
//! first test here: **the same audio through either entry point produces the same file, byte
//! for byte.** A second way into the chain that renders slightly differently would be worse
//! than no second way in.
//!
//! The other three are about what the caller takes on when it arrives with a buffer instead
//! of a path. Two guards survive intact and are asserted to (the overwrite policy, including
//! the input-master case that needs the path; the delivery blocker, with a channel count that
//! is never `None`). One does not, and cannot: mint's rule that nothing costs a decode before
//! it is known to be possible is the caller's to keep, which is what `plan_job` is for. And
//! one hazard is new — a buffer that has already been quantized — so it is refused rather
//! than quantized twice.

use std::path::{Path, PathBuf};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::{io_read, io_write};
use mint::config::{Config, DeliveryBlocker, ResolvedTarget};
use mint::diagnostics::FailureKind;
use mint::pipeline::{self, Conflict};

const RATE: u32 = 48_000;
const SEED: u64 = 0xB0FFE;

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("process-buffer-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn master(path: &Path, rate: u32) -> AudioBuffer {
    let frames = rate as usize / 2;
    let tone = |hz: f64, amp: f64| -> Vec<f64> {
        (0..frames)
            .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin())
            .collect()
    };
    AudioBuffer {
        channels: vec![tone(997.0, 0.6), tone(1245.0, 0.55)],
        sample_rate: rate,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: 2,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

fn write_master(path: &Path, rate: u32) -> PathBuf {
    io_write::write_wav(path, &master(path, rate), None).expect("write master");
    path.to_path_buf()
}

fn target_in(dir: &Path, name: &str, body: &str) -> ResolvedTarget {
    let dir = dir.display().to_string().replace('\\', "\\\\");
    Config::from_toml_str(&format!("[target.{name}]\ndir = \"{dir}\"\n{body}"))
        .unwrap_or_else(|e| panic!("config: {e:#}"))
        .resolve_target(name)
        .expect("resolve")
}

/// The property the whole split rests on: one chain, two doors.
///
/// A loudness target with a resample and s16 dither, so the run exercises the normalization,
/// the limiter, the SRC, both post-SRC passes and the quantizer — every stage that could
/// plausibly behave differently for having been reached by a different route. Same seed both
/// times, because the dither streams are seeded.
#[test]
fn either_entry_point_renders_the_same_bytes() {
    let dir = tmp("equivalence");
    let input = write_master(&dir.join("master.wav"), 96_000);
    let body = "format = \"s16\"\nrate = 44100\nlufs = -14.0\ndither = \"adaptive\"\n";

    let via_path = target_in(&dir.join("a"), "t", body);
    let report_a = pipeline::process(&input, &via_path, Some(SEED)).expect("path render");

    // Decoded by the caller, exactly as an in-process consumer would hand it over.
    let decoded = io_read::read_audio(&input).expect("decode");
    let via_buffer = target_in(&dir.join("b"), "t", body);
    let report_b =
        pipeline::process_buffer(&input, decoded, &via_buffer, Some(SEED)).expect("buffer render");

    let a = std::fs::read(&report_a.output).expect("read a");
    let b = std::fs::read(&report_b.output).expect("read b");
    assert_eq!(a, b, "the two entry points produced different audio");

    // ...and the report agrees about everything that is not the path or the clock.
    assert_eq!(report_a.in_rate, report_b.in_rate);
    assert_eq!(report_a.out_rate, report_b.out_rate);
    assert_eq!(report_a.channels, report_b.channels);
    assert_eq!(report_a.frames, report_b.frames);
    assert_eq!(report_a.file_bytes, report_b.file_bytes);
    assert_eq!(
        report_a.delivered.integrated_lufs.to_bits(),
        report_b.delivered.integrated_lufs.to_bits(),
        "the delivered measurement must not depend on how the audio arrived"
    );
    assert_eq!(report_a.loudness_gain_db, report_b.loudness_gain_db);
    assert_eq!(report_a.warnings, report_b.warnings);
}

/// One decode, several targets — the thing the split makes possible. Each target still gets
/// its own copy, because every op mutates in place.
#[test]
fn one_decode_can_feed_several_targets() {
    let dir = tmp("fan-out");
    let input = write_master(&dir.join("master.wav"), 96_000);
    let decoded = io_read::read_audio(&input).expect("decode");

    let bodies = [
        ("cd", "format = \"s16\"\nrate = 44100\nlufs = -14.0\n"),
        ("hires", "format = \"s24\"\n"),
        ("lossless", "format = \"s24\"\ncodec = \"flac\"\n"),
    ];
    for (name, body) in bodies {
        let shared = target_in(&dir.join(format!("shared-{name}")), name, body);
        let separate = target_in(&dir.join(format!("separate-{name}")), name, body);

        // The clone is the price of the reuse and is stated in `process_buffer`'s own doc.
        let from_shared = pipeline::process_buffer(&input, decoded.clone(), &shared, Some(SEED))
            .expect("render from the shared decode");
        let from_own = pipeline::process(&input, &separate, Some(SEED)).expect("render alone");

        assert_eq!(
            std::fs::read(&from_shared.output).expect("shared output"),
            std::fs::read(&from_own.output).expect("own output"),
            "target '{name}' differs when fed from a shared decode"
        );
    }
}

/// The hazard the split introduces, and the only one: `bitdepth::apply` assumes it is handed
/// full precision, which was guaranteed only while `read_audio` was the sole producer of
/// buffers. Quantizing twice is the one thing CLAUDE.md calls the most load-bearing
/// invariant, so this is a hard error rather than a `debug_assert`.
#[test]
fn a_buffer_that_has_already_been_quantized_is_refused() {
    let dir = tmp("double-quantize");
    let input = dir.join("master.wav");
    let target = target_in(&dir, "t", "format = \"s16\"\n");

    let mut already = master(&input, RATE);
    already.out_format = OutputSampleFormat::S16;

    let err = pipeline::process_buffer(&input, already, &target, Some(SEED))
        .expect_err("a pre-quantized buffer must be refused");
    let message = format!("{err:#}");
    assert!(message.contains("already been quantized"), "{message}");
    assert!(message.contains("s16"), "the message must name the grid it is on: {message}");
    // Nothing was written on the way to refusing.
    assert!(!dir.join("master.wav").exists() || std::fs::metadata(&input).is_ok());

    // The same buffer at full precision goes through.
    let fine = master(&input, RATE);
    pipeline::process_buffer(&input, fine, &target, Some(SEED)).expect("full precision renders");

    // ...and so does one labelled `F64`, which is the case the check must NOT catch: f64 is
    // mint's lossless passthrough, so a buffer that has "been through" it is bit-for-bit what
    // it was. The question the guard asks is `int_grid()`, not equality with `F32`.
    let mut lossless = master(&input, RATE);
    lossless.out_format = OutputSampleFormat::F64;
    // Its own directory: the render above already occupies this target's output path, and
    // the point here is the format guard, not the overwrite policy.
    let elsewhere = target_in(&dir.join("f64"), "t", "format = \"s24\"\n");
    pipeline::process_buffer(&input, lossless, &elsewhere, Some(SEED))
        .expect("an f64 passthrough buffer is not a quantized one");
}

/// A planar buffer with channels of different lengths is representable and, before this
/// check, panicked several stages later inside a slice index in the loudness meter — with
/// nothing in the message naming the caller's mistake. `process_with` cannot produce one;
/// `process_buffer` accepts whatever it is handed, which is exactly why it has to ask.
#[test]
fn a_ragged_or_empty_buffer_is_refused_with_a_message_that_names_the_problem() {
    let dir = tmp("ragged");
    let input = dir.join("master.wav");
    let target = target_in(&dir, "t", "format = \"s24\"\n");

    let mut ragged = master(&input, RATE);
    let shorter = ragged.channels[1].len() - 7;
    ragged.channels[1].truncate(shorter);
    let err = pipeline::process_buffer(&input, ragged, &target, Some(SEED))
        .expect_err("a ragged buffer must be refused");
    let message = format!("{err:#}");
    assert!(message.contains("channel 1"), "{message}");
    assert!(message.contains("same length"), "{message}");

    let mut empty = master(&input, RATE);
    empty.channels.clear();
    let err = pipeline::process_buffer(&input, empty, &target, Some(SEED))
        .expect_err("a channel-less buffer must be refused");
    assert!(format!("{err:#}").contains("no channels"), "{err:#}");
}

/// The guards that need the *path* still run, which is why `process_buffer` insists on one.
#[test]
fn the_overwrite_policy_still_applies_to_a_buffer_render() {
    let dir = tmp("overwrite");
    let input = write_master(&dir.join("master.wav"), RATE);
    let target = target_in(&dir, "t", "format = \"s24\"\n");

    pipeline::process_buffer(&input, master(&input, RATE), &target, Some(SEED))
        .expect("first render");
    let err = pipeline::process_buffer(&input, master(&input, RATE), &target, Some(SEED))
        .expect_err("the second must refuse");
    assert_eq!(
        FailureKind::classify(&err),
        FailureKind::OutputConflict(Conflict::Existing),
        "{err:#}"
    );

    // And the case that needs the path as *identity* rather than as a name: an output that
    // resolves onto the input master. `same_file` compares (dev, ino), so a caller that
    // passed a stand-in path would get the wrong one of mint's two messages here.
    let onto_input = ResolvedTarget {
        dir: dir.to_path_buf(),
        ..target_in(&dir, "u", "format = \"s24\"\nnaming = \"{stem}.{ext}\"\n")
    };
    let err = pipeline::process_buffer(&input, master(&input, RATE), &onto_input, Some(SEED))
        .expect_err("writing onto the master must be refused");
    assert_eq!(FailureKind::classify(&err), FailureKind::OutputConflict(Conflict::Input), "{err:#}");
}

/// `delivery_blocker` is asked with a channel count that is never `None` here, so the buffer
/// path is strictly better informed than the header one.
#[test]
fn the_delivery_blocker_still_refuses_a_buffer_render() {
    let dir = tmp("blocked");
    let input = dir.join("master.wav");
    let target = target_in(&dir, "t", "format = \"s24\"\ncodec = \"flac\"\n");

    let err = pipeline::process_buffer(&input, master(&input, 192_000), &target, Some(SEED))
        .expect_err("192 kHz into FLAC must be refused");
    assert_eq!(
        FailureKind::classify(&err),
        FailureKind::DeliveryBlocked(DeliveryBlocker::FlacRate { rate: 192_000 })
    );
}

/// `plan_job` answers the impossible-job questions from the header alone, and answers them
/// the same way the render does. That equality is the point: it is what lets a caller who is
/// about to decode find out first.
#[test]
fn plan_job_settles_from_the_header_and_agrees_with_the_render() {
    let dir = tmp("plan");
    let input = write_master(&dir.join("master.wav"), 96_000);
    let target = target_in(&dir, "t", "format = \"s16\"\nrate = 44100\n");

    let plan = pipeline::plan_job(&input, &target).expect("the plan resolves");
    assert_eq!(plan.source_rate, 96_000);
    assert_eq!(plan.out_rate, 44_100);

    let report = pipeline::process(&input, &target, Some(SEED)).expect("render");
    assert_eq!(plan.output_path.display().to_string(), report.output);
    assert_eq!(plan.out_rate, report.out_rate);
    assert_eq!(plan.source_rate, report.in_rate);

    // Now that the output exists, the plan refuses it — the same question, before a decode.
    let err = pipeline::plan_job(&input, &target).expect_err("the second plan must refuse");
    assert_eq!(FailureKind::classify(&err), FailureKind::OutputConflict(Conflict::Existing));

    // A blocked target is refused from the header too, with no audio read.
    let flac = target_in(&dir, "f", "format = \"s24\"\ncodec = \"flac\"\n");
    let hires = write_master(&dir.join("hires.wav"), 192_000);
    let err = pipeline::plan_job(&hires, &flac).expect_err("192 kHz into FLAC must be refused");
    assert_eq!(
        FailureKind::classify(&err),
        FailureKind::DeliveryBlocked(DeliveryBlocker::FlacRate { rate: 192_000 })
    );
}

/// `plan_job` creates nothing. A caller may ask it about a batch of candidates without
/// leaving a tree of empty output directories behind.
#[test]
fn plan_job_creates_no_directories() {
    let dir = tmp("no-dirs");
    let input = write_master(&dir.join("master.wav"), RATE);
    let out = dir.join("deep").join("nested");
    let target = target_in(&out, "t", "format = \"s24\"\n");

    let plan = pipeline::plan_job(&input, &target).expect("plan");
    // `dir` plus the target's own subdirectory, which is the default naming.
    assert!(plan.output_path.starts_with(&out), "{}", plan.output_path.display());
    assert!(!out.exists(), "planning must not create the output directory");

    // The render does create it, so the assertion above is about `plan_job` and not about a
    // path that was never going to exist.
    pipeline::process(&input, &target, Some(SEED)).expect("render");
    assert!(plan.output_path.parent().expect("a parent").is_dir());
}

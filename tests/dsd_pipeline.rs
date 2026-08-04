//! DSD as a *configured* deliverable: rate derivation, validation, the dry-run, and the
//! whole pipeline from a `.wav` on disk to a `.dsf` on disk.
//!
//! `tests/dsd_verification.rs` proves the modulator is good and `tests/dsd_format.rs`
//! proves the container is right. Neither of them goes through `config.rs` or
//! `pipeline.rs`, which is exactly where phase 4's risk lives — it is broad rather than
//! deep, and one part of it is genuinely dangerous:
//!
//! **`describe()` and the codec dispatch in `pipeline.rs` used to have catch-all arms.** A
//! new `OutputCodec` variant therefore produced no compiler error at all; it fell through
//! to "quantize, then write a WAV", and the only symptom would have been a `.dsf` file
//! containing RIFF. Both arms are exhaustive now, and
//! [`a_dsd_target_writes_dsd_and_never_falls_through_to_the_pcm_path`] is the regression
//! test that says so from the outside.
//!
//! # Measured baseline (2026-08-04, Apple Silicon, seed 0xD5F, ~4.5 s in a debug build)
//!
//! | property | measured | gate |
//! |---|---|---|
//! | in-band SNR, 96 kHz s24 master -> dsd64 -> decoded at 44.1 kHz | 104.5 dB | > 95 dB |
//! | peak modulation, full-scale master at `ceiling_dbtp = -1` | 45.57 % | 43–47 %, < 71.43 % |
//! | overload samples, same run | 0 | 0 |
//! | same seed, two runs | byte-identical | byte-identical |
//!
//! The 104.5 dB is the same figure `tests/dsd_format.rs` reports for the container alone,
//! so config resolution and the pipeline cost nothing on top — as they should, being pure
//! plumbing around the same three stages.

use std::path::{Path, PathBuf};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::audio::io_write_dsd::{BLOCK_BYTES, BLOCK_SAMPLES, HEADER_BYTES};
use mint::config::Config;
use mint::ops::dsd::DsdRate;
use mint::ops::dsd::decimate::Decimator;
use mint::pipeline;

const SEED: u64 = 0xD5F;

/// Scratch path carrying the pid, so two overlapping runs of this binary cannot clobber
/// each other's files — the mistake that cost phase 3 an afternoon of ghost-hunting.
fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "dsd-pipeline-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn tone(rate: u32, frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin())
        .collect()
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

/// A stereo master on disk: two different tones so the file is genuinely two-channel.
fn write_master(dir: &Path, rate: u32, seconds: f64, amp: f64) -> PathBuf {
    let frames = (rate as f64 * seconds) as usize;
    let buffer = AudioBuffer {
        channels: vec![
            tone(rate, frames, 997.0, amp),
            tone(rate, frames, 1245.0, amp),
        ],
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

/// Parse a config from a string, going through `Config::from_path` so the whole-config
/// validation (which resolves every target) runs exactly as it does in production.
fn config(dir: &Path, toml: &str) -> anyhow::Result<Config> {
    let path = dir.join("config.toml");
    std::fs::write(&path, toml).expect("write config");
    Config::from_path(&path)
}

/// Same, for the cases that must fail; returns the full error chain as a string.
fn config_error(dir: &Path, toml: &str) -> String {
    match config(dir, toml) {
        Ok(_) => panic!("config should have been rejected:\n{toml}"),
        Err(e) => format!("{e:#}"),
    }
}

/// One channel's bit stream out of a DSF payload. Written independently of `pack.rs`, so
/// the two really have to agree on LSB-first.
fn unpack(payload: &[u8], channels: usize, ch: usize) -> Vec<bool> {
    let blocks = payload.len() / (BLOCK_BYTES * channels);
    let mut bits = Vec::with_capacity(blocks * BLOCK_SAMPLES as usize);
    for b in 0..blocks {
        let off = (b * channels + ch) * BLOCK_BYTES;
        for &byte in &payload[off..off + BLOCK_BYTES] {
            for i in 0..8 {
                bits.push((byte >> i) & 1 == 1);
            }
        }
    }
    bits
}

// ═══════════════════════════ 1. the intermediate rate ════════════════════════

#[test]
fn the_intermediate_rate_matches_the_table_in_the_design_note() {
    // `docs/dsd-design.md` section 2.1, reproduced field for field. Bold entries there =
    // the cap bites; here they are simply the rows where the result is below the nearest
    // 44.1-family rate. The rule is min(nearest 44.1 rate, dsd_rate / 32).
    #[rustfmt::skip]
    let table: &[(u32, [u32; 4])] = &[
        // source     dsd64    dsd128   dsd256   dsd512
        ( 44_100, [ 44_100,  44_100,  44_100,  44_100]),
        ( 48_000, [ 44_100,  44_100,  44_100,  44_100]),
        ( 88_200, [ 88_200,  88_200,  88_200,  88_200]),
        ( 96_000, [ 88_200,  88_200,  88_200,  88_200]),
        (176_400, [ 88_200, 176_400, 176_400, 176_400]),
        (192_000, [ 88_200, 176_400, 176_400, 176_400]),
        (352_800, [ 88_200, 176_400, 352_800, 352_800]),
    ];

    for &(source, expected) in table {
        for (rate, want) in DsdRate::ALL.into_iter().zip(expected) {
            let got = mint::ops::dsd::intermediate_pcm_rate(source, rate);
            assert_eq!(got, want, "{source} Hz -> {}", rate.tag());
            // The whole point of staying in the 44.1 family: an exact power of two.
            let factor = mint::ops::dsd::interpolation_factor(got, rate)
                .unwrap_or_else(|| panic!("{got} -> {} is not a power of two", rate.tag()));
            assert_eq!(factor * got, rate.hz());
        }
    }
}

#[test]
fn a_dsd_target_derives_its_rate_and_leaves_the_pcm_ones_alone() {
    let dir = tmp("derive");
    let config = config(
        &dir,
        r#"
        [target.sacd]
        codec = "dsf"
        dsd   = "dsd64"

        [target.cd]
        preset = "cd"
        "#,
    )
    .expect("config should parse");

    let sacd = config.resolve_target("sacd").unwrap();
    assert_eq!(sacd.dsd, Some(DsdRate::Dsd64));
    assert_eq!(sacd.rate, None, "nothing explicit; the rate is derived");
    // 96 kHz source: nearest 44.1 rate is 88.2 kHz, and dsd64/32 is also 88.2 kHz.
    assert_eq!(sacd.pcm_stage_rate(96_000), 88_200);
    // 352.8 kHz source: the cap bites, so the modulator still sees 88.2 kHz.
    assert_eq!(sacd.pcm_stage_rate(352_800), 88_200);
    // The delivered rate is the 1-bit rate, whatever the source was.
    assert_eq!(sacd.delivery_rate(96_000), 2_822_400);

    let cd = config.resolve_target("cd").unwrap();
    assert_eq!(cd.dsd, None);
    assert_eq!(cd.delivery_rate(96_000), 44_100);
}

#[test]
fn an_explicit_rate_overrides_the_derivation() {
    let dir = tmp("explicit-rate");
    // The documented escape hatch: keep the ultrasonic band in a dsd64 archive file by
    // feeding the modulator 176.4 kHz instead of the derived 88.2.
    let config = config(
        &dir,
        r#"
        [target.archive]
        codec = "dsf"
        dsd   = "dsd64"
        rate  = 176400
        "#,
    )
    .expect("an explicit power-of-two rate is legal");

    let t = config.resolve_target("archive").unwrap();
    assert_eq!(t.pcm_stage_rate(352_800), 176_400);
    assert_eq!(t.delivery_rate(352_800), 2_822_400);
}

// ═══════════════════════════ 2. validation ═══════════════════════════════════

#[test]
fn a_fractional_interpolation_ratio_is_rejected() {
    let dir = tmp("bad-rate");
    // 48 kHz into dsd64 is a ratio of 58.8 — a fractional SRC right in front of the most
    // level-sensitive stage in the chain, which is the whole reason for the 44.1 family.
    let err = config_error(
        &dir,
        r#"
        [target.sacd]
        codec = "dsf"
        dsd   = "dsd64"
        rate  = 48000
        "#,
    );
    assert!(err.contains("power of two"), "{err}");
    assert!(err.contains("44100"), "the message should name a way out: {err}");
}

#[test]
fn dsd_and_the_codec_have_to_agree() {
    let dir = tmp("mismatch");

    let err = config_error(
        &dir,
        r#"
        [target.x]
        format = "s24"
        dsd    = "dsd64"
        "#,
    );
    assert!(err.contains("codec"), "dsd without dsf: {err}");

    let err = config_error(
        &dir,
        r#"
        [target.x]
        codec = "dsf"
        "#,
    );
    assert!(err.contains("dsd"), "dsf without a dsd rate: {err}");
}

#[test]
fn format_is_rejected_on_the_target_but_tolerated_as_a_baseline() {
    let dir = tmp("format");

    // Written on the target itself, `format` is a misconception worth naming: the
    // sigma-delta loop is the quantizer, so there is no bit depth to pick.
    let err = config_error(
        &dir,
        r#"
        [target.sacd]
        codec  = "dsf"
        dsd    = "dsd64"
        format = "s24"
        "#,
    );
    assert!(err.contains("format"), "{err}");
    assert!(err.contains("modulator"), "the message should say why: {err}");

    // Inherited from [defaults] it is just a baseline for the PCM targets alongside it,
    // and must not make the DSD target unresolvable.
    let config = config(
        &dir,
        r#"
        [defaults]
        format = "s24"
        rate   = 48000

        [target.hires]
        lufs = -16.0

        [target.sacd]
        codec = "dsf"
        dsd   = "dsd64"
        "#,
    )
    .expect("a [defaults] baseline must not poison a DSD target");

    let sacd = config.resolve_target("sacd").unwrap();
    // Neither baseline reached it: the rate is still derived, not the 48 kHz default
    // (which is not even a legal DSD intermediate rate).
    assert_eq!(sacd.rate, None);
    assert_eq!(sacd.pcm_stage_rate(96_000), 88_200);
    assert_eq!(config.resolve_target("hires").unwrap().rate, Some(48_000));
}

#[test]
fn bwf_is_still_wav_only() {
    let dir = tmp("bwf");
    let err = config_error(
        &dir,
        r#"
        [target.sacd]
        codec = "dsf"
        dsd   = "dsd64"
        bwf   = true
        "#,
    );
    assert!(err.contains("bwf"), "{err}");
}

#[test]
fn two_dsd_targets_differing_only_in_rate_do_not_collide() {
    let dir = tmp("collide");
    // `{dsd}` is the placeholder that makes them distinguishable; without it the two
    // targets write the same file and the config must be rejected before any audio runs.
    let err = config_error(
        &dir,
        r#"
        [defaults]
        naming = "{stem}.{cext}"

        [target.a]
        codec = "dsf"
        dsd   = "dsd64"

        [target.b]
        codec = "dsf"
        dsd   = "dsd128"
        "#,
    );
    assert!(err.contains("same output path"), "{err}");

    let config = config(
        &dir,
        r#"
        [defaults]
        naming = "{stem}-{dsd}.{cext}"

        [target.a]
        codec = "dsf"
        dsd   = "dsd64"

        [target.b]
        codec = "dsf"
        dsd   = "dsd128"
        "#,
    )
    .expect("{dsd} distinguishes them");

    let path = config
        .resolve_target("b")
        .unwrap()
        .render_output_path(Path::new("song.wav"), 5_644_800)
        .unwrap();
    assert!(path.ends_with("song-dsd128.dsf"), "{}", path.display());
}

#[test]
fn the_rate_placeholder_is_the_dsd_rate_and_format_is_not_a_lie() {
    let dir = tmp("naming");
    let config = config(
        &dir,
        r#"
        [target.sacd]
        codec  = "dsf"
        dsd    = "dsd64"
        naming = "{stem}-{rate}-{format}.{cext}"
        "#,
    )
    .unwrap();

    let target = config.resolve_target("sacd").unwrap();
    let path = target
        .render_output_path(Path::new("song.wav"), target.delivery_rate(96_000))
        .unwrap();
    // Not "s16" — that field holds a placeholder for codecs that have no sample format,
    // and it must never reach a file name.
    assert!(path.ends_with("song-2822400-1bit.dsf"), "{}", path.display());
}

// ═══════════════════════════ 3. the dry-run ══════════════════════════════════

#[test]
fn the_dry_run_describes_the_dsd_chain_and_not_a_quantized_wav() {
    let dir = tmp("describe");
    let config = config(
        &dir,
        r#"
        [target.sacd]
        codec = "dsf"
        dsd   = "dsd64"
        lufs  = -16.0
        "#,
    )
    .unwrap();

    let steps = config.resolve_target("sacd").unwrap().describe(96_000);
    let joined = steps.join("\n");

    assert!(joined.contains("resample 96000 -> 88200"), "{joined}");
    assert!(joined.contains("derived"), "{joined}");
    assert!(joined.contains("interpolate x32 -> 2822400 Hz (dsd64"), "{joined}");
    assert!(joined.contains("sigma-delta modulate to 1 bit (order 7"), "{joined}");
    assert!(joined.contains("50 % modulation"), "{joined}");
    assert!(joined.contains("write DSF"), "{joined}");
    // The two things the old catch-all arm would have printed instead.
    assert!(!joined.contains("quantize"), "no PCM quantizer step: {joined}");
    assert!(!joined.contains("WAV"), "no WAV writer step: {joined}");
}

#[test]
fn the_dry_run_flags_dsd512() {
    let dir = tmp("describe-512");
    let config = config(
        &dir,
        r#"
        [target.big]
        codec = "dsf"
        dsd   = "dsd512"
        "#,
    )
    .unwrap();

    let steps = config.resolve_target("big").unwrap().describe(96_000);
    assert!(
        steps.iter().any(|s| s.contains("limited player support")),
        "{steps:?}"
    );
    // 88.2 kHz source rate class, dsd512 / 32 = 705.6 kHz, so the family wins: x64.
    assert!(steps.iter().any(|s| s.contains("interpolate x256")), "{steps:?}");
}

// ═══════════════════════════ 4. end to end ═══════════════════════════════════

/// Render one master through the whole pipeline and hand back the report plus the bytes.
fn render(
    dir: &Path,
    toml: &str,
    target: &str,
    source_rate: u32,
) -> (pipeline::ProcessingReport, Vec<u8>) {
    let input = write_master(dir, source_rate, 0.25, 0.5);
    let config = config(dir, toml).expect("config should parse");
    let resolved = config.resolve_target(target).expect("resolve");
    let report = pipeline::process(&input, &resolved, Some(SEED)).expect("process");
    let bytes = std::fs::read(&report.output).expect("read the delivered file");
    (report, bytes)
}

/// The plain SACD-style target, writing into the caller's own scratch directory — the
/// tests run concurrently and `dir` is the only thing keeping their outputs apart.
fn sacd_config(dir: &Path) -> String {
    format!(
        r#"
        [defaults]
        dir = "{}"

        [target.sacd]
        codec        = "dsf"
        dsd          = "dsd64"
        ceiling_dbtp = -1.0
        "#,
        dir.display()
    )
}

#[test]
fn a_dsd_target_writes_dsd_and_never_falls_through_to_the_pcm_path() {
    let dir = tmp("e2e-dsf");
    let (report, bytes) = render(&dir, &sacd_config(&dir), "sacd", 96_000);

    // The regression the exhaustive matches exist for: a catch-all arm would have quantized
    // the buffer and written a RIFF file under a .dsf name, and everything above this line
    // would still have passed.
    assert_eq!(&bytes[0..4], b"DSD ", "not a DSF file");
    assert_ne!(&bytes[0..4], b"RIFF", "fell through to the WAV writer");
    assert!(report.output.ends_with(".dsf"), "{}", report.output);
    assert!(!report.dithered, "the PCM dither stage must not have run");
    assert!(!report.shaped);

    // Report: the delivered rate is the 1-bit rate, the intermediate one is derived.
    assert_eq!(report.in_rate, 96_000);
    assert_eq!(report.out_rate, 2_822_400);
    assert_eq!(report.codec, "dsf");
    assert_eq!(report.format, "1-bit");
    let dsd = report.dsd.as_ref().expect("a DSD report");
    assert_eq!(dsd.rate, "dsd64");
    assert_eq!(dsd.intermediate_pcm_rate, 88_200);
    assert_eq!(dsd.interpolation_factor, 32);
    assert_eq!(dsd.modulator_order, 7);

    // The file's own size fields have to agree with the file on disk — the check WavPack
    // performs, and the reason three header fields are patched by seek at all.
    assert_eq!(dsd.file_bytes, bytes.len() as u64);
    let u64_at = |off: usize| u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap());
    assert_eq!(u64_at(12), bytes.len() as u64, "totalFileSize");
    assert_eq!(u64_at(64), dsd.samples_per_channel, "sampleCount");
    assert_eq!(
        u64_at(84),
        12 + bytes.len() as u64 - HEADER_BYTES,
        "dataChunkSize counts its own 12-byte header"
    );
    // Topping the last block up with modulated silence means whole blocks, always.
    assert_eq!(dsd.samples_per_channel % BLOCK_SAMPLES, 0);

    // A foreign parser that has never seen our writer.
    let file =
        dsf::DsfFile::open(Path::new(&report.output)).expect("the dsf crate rejected our file");
    assert_eq!(file.fmt_chunk().sampling_frequency(), 2_822_400);
    assert_eq!(file.fmt_chunk().channel_num(), 2);

    println!(
        "\n  {} -> {} ({:.1} MB, peak modulation {:.1} %)",
        report.input,
        report.output,
        dsd.file_bytes as f64 / 1e6,
        dsd.max_modulation_percent
    );
}

#[test]
fn the_delivered_file_still_carries_the_signal() {
    let dir = tmp("e2e-snr");
    let (report, bytes) = render(&dir, &sacd_config(&dir), "sacd", 96_000);

    // Decode at 44.1 kHz, not at the 88.2 kHz the modulator was fed: DSD's whole trick is
    // that the quantization noise sits *above* the audio band, so a wideband comparison
    // measures that ultrasonic noise instead of the signal (39.7 dB vs 104.4 dB from the
    // same bits — see the header of tests/dsd_format.rs).
    const OUT_RATE: u32 = 44_100;
    let bits = unpack(&bytes[HEADER_BYTES as usize..], 2, 0);
    let dec = Decimator::for_rates(DsdRate::Dsd64.hz(), OUT_RATE).expect("decimator");
    // Out of modulation units: the modulator maps PCM 0 dBFS to 50 % modulation.
    let pcm: Vec<f64> = dec.process_bits(&bits).into_iter().map(|v| v * 2.0).collect();

    // Middle half only: the ends are the decimator's transient and the silence the last
    // container block was topped up with.
    let lo = pcm.len() / 4;
    let hi = pcm.len() * 3 / 4;
    let unit = tone(OUT_RATE, pcm.len(), 997.0, 1.0);

    // Best-fit gain rather than a nominal amplitude. Everything in the chain nudges the
    // level by a fraction of a percent — the resample to 88.2 kHz, the ceiling stage, the
    // decimator's passband — and at these numbers a 0.1 % gain error on its own would read
    // as 60 dB of "noise". What is being measured here is what the modulator did to the
    // *waveform*; the level is `delivered.true_peak_dbtp`'s job, and it is checked
    // separately in `the_ceiling_stage_keeps_the_modulation_index_inside_the_format`.
    let energy: f64 = unit[lo..hi].iter().map(|v| v * v).sum();
    let gain = (lo..hi).map(|i| pcm[i] * unit[i]).sum::<f64>() / energy;
    let reference: Vec<f64> = unit[lo..hi].iter().map(|v| v * gain).collect();
    let error: Vec<f64> = (lo..hi).map(|i| pcm[i] - reference[i - lo]).collect();
    let snr = 20.0 * (rms(&reference) / rms(&error)).log10();

    // The fitted gain has to land where the metering says the signal is, or the fit has
    // quietly absorbed a real level error.
    let expected = 10f64.powf(report.delivered.true_peak_dbtp / 20.0);
    assert!(
        (gain / expected - 1.0).abs() < 0.01,
        "fitted gain {gain:.5} vs metered {expected:.5}"
    );

    println!("\n  in-band SNR through config + pipeline: {snr:.1} dB");
    assert!(snr > 95.0, "the chain lost the signal: {snr:.1} dB");
}

#[test]
fn the_ceiling_stage_keeps_the_modulation_index_inside_the_format() {
    let dir = tmp("e2e-hot");
    // A master already at full scale, transcoded rather than normalized: the ceiling stage
    // is then the *only* thing standing between the source level and the modulator, which
    // is the arrangement worth pinning down. The loop's own margin over 50 % modulation is
    // only ~2 dB (see the header of `src/ops/dsd/modulator.rs`), so "the ceiling stage
    // holds" and "the 1-bit loop stays sane" are the same statement here.
    let input = write_master(&dir, 96_000, 0.25, 1.0);
    let report = {
        let config = config(&dir, &sacd_config(&dir)).unwrap();
        let resolved = config.resolve_target("sacd").unwrap();
        pipeline::process(&input, &resolved, Some(SEED)).expect("process")
    };
    let dsd = report.dsd.as_ref().expect("a DSD report");

    println!(
        "\n  delivered {:.2} dBTP -> peak modulation {:.2} % (SACD 0 dB = 50 %, \
         format max = 71.43 %), overload {}",
        report.delivered.true_peak_dbtp, dsd.max_modulation_percent, dsd.overload_samples
    );
    assert!(
        dsd.max_modulation_percent < 71.43,
        "past the 20/28 rule: {:.2} %",
        dsd.max_modulation_percent
    );
    // The ceiling stage really did have to work: a full-scale source would be 50 %, and
    // `ceiling_dbtp = -1` puts it at 44.6 %. Both bounds matter — the upper one says the
    // ceiling was enforced, the lower one says the source was actually hot and the test is
    // not passing on a quiet signal.
    //
    // Measured 45.57 %, a shade above the 44.6 % the ceiling implies, and that is correct
    // rather than slack: the index is sampled *after* interpolation, so unlike a PCM sample
    // peak it sees the reconstructed inter-sample peaks. It is a true-peak figure, and the
    // window has to be wide enough to hold one.
    assert!(
        (43.0..47.0).contains(&dsd.max_modulation_percent),
        "expected ~45 % (-1 dBTP), got {:.2} %",
        dsd.max_modulation_percent
    );
    assert_eq!(dsd.overload_samples, 0, "the loop should never have clamped");
    assert_eq!(dsd.resets, 0);
    assert!(
        !report.warnings.iter().any(|w| w.contains("modulation")),
        "{:?}",
        report.warnings
    );
}

#[test]
fn the_same_seed_gives_the_same_file() {
    // mint runs its batch under rayon and the modulator is stateful, so "same seed, same
    // bytes" is a property that has to be asserted, not assumed. The seed is derived per
    // channel exactly as the PCM multi-channel dither derives its streams.
    let (a, b) = (tmp("e2e-det-a"), tmp("e2e-det-b"));
    let (_, first) = render(&a, &sacd_config(&a), "sacd", 96_000);
    let (_, second) = render(&b, &sacd_config(&b), "sacd", 96_000);
    assert_eq!(first.len(), second.len());
    assert!(first == second, "two runs with seed {SEED:#x} differ");
}

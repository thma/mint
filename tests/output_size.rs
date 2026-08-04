//! What `--dry-run` promises about output size, checked against files on disk.
//!
//! The DSD side of this lives in `tests/dsd_pipeline.rs`
//! (`the_dry_run_predicts_the_dsd_file_size_exactly`), because it needs the whole pipeline.
//! Here it is the two halves that can be checked without one: the writers' own size
//! functions, and the header probe that feeds them a frame count.
//!
//! The point of the split between `SizeEstimate::Exact`, `About` and `AtMost` is that the
//! qualifier is a claim, so each one is tested as the claim it makes:
//!
//! | codec | claim | how it is checked |
//! |---|---|---|
//! | WAV (s16/s24/f32/f64, ±bext) | exact | byte-equal to the written file |
//! | DSF | exact | byte-equal (in `dsd_pipeline`) |
//! | FLAC | upper bound | written file is smaller, and the bound is not absurd |
//! | MP3 | approximate | within 5 % (needs `--features mp3`) |
//!
//! The probe half exists for one measured quirk: symphonia derives the frame count from the
//! sample-chunk length, and for AIFF that length still counts the 8 bytes of
//! `offset`/`blockSize` that the SSND reader already consumed. So it reads high by
//! `floor(8 / block_align)` — 2 frames for 16-bit stereo, 4 for 16-bit mono — while WAV is
//! exact. `io_read::probe_source` corrects that, and
//! [`the_probe_reports_the_true_frame_count_for_both_container_families`] is what would
//! notice if symphonia ever fixed it upstream and turned the correction into a new error.

use std::path::{Path, PathBuf};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::{io_read, io_write};

fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("output-size-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(name)
}

const RATE: u32 = 48_000;
const FRAMES: usize = 24_000; // 0.5 s

fn buffer(channels: usize, format: OutputSampleFormat) -> AudioBuffer {
    let tone = |phase: f64| -> Vec<f64> {
        (0..FRAMES)
            .map(|i| {
                0.5 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / f64::from(RATE) + phase)
                    .sin()
            })
            .collect()
    };
    AudioBuffer {
        channels: (0..channels).map(|c| tone(c as f64 * 0.3)).collect(),
        sample_rate: RATE,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels,
            sample_rate: RATE,
            codec: "test".to_string(),
        },
        out_format: format,
    }
}

#[test]
fn the_predicted_wav_size_is_the_written_size() {
    // Each row picks a different header shape: hound's 44-byte PCMWAVEFORMAT (<=2 ch,
    // <=16 bit), its 68-byte WAVEFORMATEXTENSIBLE (everything else), and the hand-written
    // 56-byte RIFF/fmt/fact/data of `encode_wav_f64`.
    let cases = [
        (2, OutputSampleFormat::S16, "s16-stereo"),
        (1, OutputSampleFormat::S16, "s16-mono"),
        (2, OutputSampleFormat::S24, "s24-stereo"),
        (2, OutputSampleFormat::F32, "f32-stereo"),
        (2, OutputSampleFormat::F64, "f64-stereo"),
        (3, OutputSampleFormat::S16, "s16-three"),
    ];

    for (channels, format, name) in cases {
        let buf = buffer(channels, format);
        let path = tmp(&format!("size-{name}.wav"));
        io_write::write_wav(&path, &buf, None).expect("write wav");

        let actual = std::fs::metadata(&path).unwrap().len();
        let predicted = io_write::wav_file_bytes(FRAMES as u64, RATE, channels, format, false);
        assert_eq!(predicted, actual, "{name}");
    }
}

#[test]
fn the_predicted_wav_size_includes_the_bext_chunk() {
    let buf = buffer(2, OutputSampleFormat::S24);
    let path = tmp("size-bext.wav");
    let bext = io_write::BextMetadata {
        description: "test".to_string(),
        integrated_lufs: -23.0,
        loudness_range_lu: 6.0,
        max_true_peak_dbtp: -1.0,
        max_momentary_lufs: -12.0,
        max_short_term_lufs: -13.0,
        sample_rate: RATE,
        channels: 2,
        bits: 24,
    };
    io_write::write_wav(&path, &buf, Some(&bext)).expect("write bwf");

    let actual = std::fs::metadata(&path).unwrap().len();
    let predicted =
        io_write::wav_file_bytes(FRAMES as u64, RATE, 2, OutputSampleFormat::S24, true);
    assert_eq!(predicted, actual);

    // And the chunk really is what the difference is made of, rather than two errors
    // cancelling: the plain file plus the bext chunk is the whole story.
    let plain = io_write::wav_file_bytes(FRAMES as u64, RATE, 2, OutputSampleFormat::S24, false);
    assert!(
        (610..=720).contains(&(predicted - plain)),
        "bext overhead {} is outside the 602-byte structure plus a coding-history line",
        predicted - plain
    );
}

/// FLAC's real size depends on the material, so the promise is only an upper bound. Both
/// halves of that are worth holding: it must not be exceeded, and it must not be so loose
/// that it stops meaning anything.
///
/// Two minutes of white noise, not half a second of a sine: the sine compresses to ~40 %
/// and can never approach the bound, which is how a fixed 8 kB overhead allowance survived
/// review. Incompressible material encodes verbatim, so it sits a hair under the payload
/// and exposes any overhead term that does not scale with duration — the earlier constant
/// ran out after ~64 s and the "bound" was exceeded from there on.
#[test]
fn the_flac_bound_holds_for_incompressible_material_of_real_length() {
    // Comfortably past the ~64 s where the old fixed allowance ran out, without paying for
    // a two-minute encode in a debug build on every `cargo test`.
    const LONG: u64 = 48_000 * 75;
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut noise = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    };
    let buf = AudioBuffer {
        channels: vec![
            (0..LONG).map(|_| noise()).collect(),
            (0..LONG).map(|_| noise()).collect(),
        ],
        sample_rate: 48_000,
        src_info: SourceInfo {
            path: PathBuf::from("noise.wav"),
            channels: 2,
            sample_rate: 48_000,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    let path = tmp("size-long.flac");
    io_write::write_flac(&path, &buf, 24).expect("write flac");

    let actual = std::fs::metadata(&path).unwrap().len();
    let bound = io_write::flac_max_file_bytes(LONG, 2, 24);
    assert!(actual <= bound, "flac {actual} exceeded its bound {bound}");

    // ...and the bound must stay close enough to the payload to mean something.
    let raw = LONG * 2 * 3;
    assert!(
        bound < raw + raw / 100,
        "bound {bound} is more than 1 % above the {raw}-byte payload"
    );
    std::fs::remove_file(&path).ok();
}

/// The short, compressible case: the bound must also not be absurd for ordinary material.
#[test]
fn the_flac_bound_is_an_upper_bound_and_still_useful() {
    let buf = buffer(2, OutputSampleFormat::S24);
    let path = tmp("size.flac");
    io_write::write_flac(&path, &buf, 24).expect("write flac");

    let actual = std::fs::metadata(&path).unwrap().len();
    let bound = io_write::flac_max_file_bytes(FRAMES as u64, 2, 24);
    assert!(actual <= bound, "flac {actual} exceeded its bound {bound}");

    let raw = FRAMES as u64 * 2 * 3;
    assert!(bound < raw + 16_384, "the bound drifted away from the PCM payload");
}

/// LAME picks the MPEG version from the sample rate and then clamps the bitrate into that
/// version's table without saying so. A 22.05 kHz target left at the 320 kbps default gets
/// 160, and an estimate that trusts the request over-predicts by exactly 2x.
#[test]
fn the_mp3_estimate_follows_lames_bitrate_clamp() {
    assert_eq!(io_write::effective_mp3_bitrate(48_000, 320), 320);
    assert_eq!(io_write::effective_mp3_bitrate(44_100, 128), 128);
    assert_eq!(io_write::effective_mp3_bitrate(24_000, 320), 160, "MPEG-2 tops out at 160");
    assert_eq!(io_write::effective_mp3_bitrate(22_050, 320), 160);
    assert_eq!(io_write::effective_mp3_bitrate(22_050, 96), 96, "below the cap, untouched");
    assert_eq!(io_write::effective_mp3_bitrate(12_000, 320), 64, "MPEG-2.5 tops out at 64");

    // One second at the clamped 160 kbps is 20 000 bytes, not the 40 000 the request implies.
    let one_second = io_write::mp3_file_bytes(22_050, 22_050, 320);
    assert!(
        (20_000..=22_000).contains(&one_second),
        "1 s at the clamped 160 kbps should be ~21 kB, got {one_second}"
    );
}

#[cfg(feature = "mp3")]
#[test]
fn the_predicted_mp3_size_is_within_a_few_percent() {
    let buf = buffer(2, OutputSampleFormat::F32);
    let path = tmp("size.mp3");
    io_write::write_mp3(&path, &buf, 320).expect("write mp3");

    let actual = std::fs::metadata(&path).unwrap().len() as f64;
    let predicted = io_write::mp3_file_bytes(FRAMES as u64, RATE, 320) as f64;
    let error = (predicted - actual).abs() / actual;
    assert!(
        error < 0.05,
        "predicted {predicted} vs actual {actual} ({:.1} % off)",
        error * 100.0
    );
}

// ═══════════════════════ the frame count that feeds all of it ════════════════

/// A sample rate as AIFF stores it: an 80-bit IEEE 754 extended float, big-endian.
fn extended80(rate: u32) -> [u8; 10] {
    let mut out = [0u8; 10];
    if rate == 0 {
        return out;
    }
    let exponent = 31 - rate.leading_zeros(); // floor(log2(rate))
    let biased = (16_383 + exponent) as u16;
    let mantissa = u64::from(rate) << (63 - exponent);
    out[0..2].copy_from_slice(&biased.to_be_bytes());
    out[2..10].copy_from_slice(&mantissa.to_be_bytes());
    out
}

/// Hand-build a 16-bit big-endian AIFF. There are no committed binary fixtures, and the
/// point of the test is a byte-level quirk of the container, so the container is written
/// byte by byte rather than through a library that might paper over it.
fn write_aiff(path: &Path, rate: u32, channels: u16, frames: u32) {
    let data_len = frames as usize * channels as usize * 2;
    let ssnd_len = 8 + data_len; // offset + blockSize + samples

    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&((4 + 8 + 18 + 8 + ssnd_len) as u32).to_be_bytes());
    out.extend_from_slice(b"AIFF");

    out.extend_from_slice(b"COMM");
    out.extend_from_slice(&18u32.to_be_bytes());
    out.extend_from_slice(&channels.to_be_bytes());
    out.extend_from_slice(&frames.to_be_bytes());
    out.extend_from_slice(&16u16.to_be_bytes()); // sample size
    out.extend_from_slice(&extended80(rate));

    out.extend_from_slice(b"SSND");
    out.extend_from_slice(&(ssnd_len as u32).to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // offset
    out.extend_from_slice(&0u32.to_be_bytes()); // blockSize
    for i in 0..frames {
        let v = ((16_000.0
            * (2.0 * std::f64::consts::PI * 997.0 * f64::from(i) / f64::from(rate)).sin())
            as i16)
            .to_be_bytes();
        for _ in 0..channels {
            out.extend_from_slice(&v);
        }
    }

    std::fs::write(path, out).expect("write aiff");
}

#[test]
fn the_probe_reports_the_true_frame_count_for_both_container_families() {
    // WAV: symphonia is exact here, so the probe must not "correct" anything.
    let wav = tmp("probe.wav");
    io_write::write_wav(&wav, &buffer(2, OutputSampleFormat::S24), None).expect("write wav");
    let probe = io_read::probe_source(&wav).expect("probe wav");
    assert_eq!(probe.sample_rate, RATE);
    assert_eq!(probe.channels, Some(2));
    assert_eq!(probe.frames, Some(FRAMES as u64));

    // AIFF: symphonia reads floor(8 / block_align) frames high — 2 for 16-bit stereo, 4
    // for 16-bit mono. If this ever fails by +2/+4, the upstream bug was fixed and the
    // correction in `probe_source` has to go; failing by -2/-4 means it was applied twice.
    for (channels, name) in [(2u16, "probe-stereo.aiff"), (1, "probe-mono.aiff")] {
        let aiff = tmp(name);
        write_aiff(&aiff, RATE, channels, FRAMES as u32);
        let probe = io_read::probe_source(&aiff).expect("probe aiff");
        assert_eq!(probe.sample_rate, RATE, "{name}");
        assert_eq!(probe.channels, Some(channels as usize), "{name}");
        assert_eq!(probe.frames, Some(FRAMES as u64), "{name}");
    }

    // And the corrected count is the one the decoder actually delivers.
    let decoded = io_read::read_audio(&tmp("probe-stereo.aiff")).expect("decode aiff");
    assert_eq!(decoded.channels[0].len(), FRAMES);
}

/// A WAV streamed to a pipe carries a data-chunk length of 0xFFFFFFFF. Left alone that
/// would turn a half-second file into a multi-terabyte "estimate", so the probe clamps the
/// frame count to what the file on disk can actually hold.
///
/// Every sample format, because the clamp needs a frame width and symphonia does not
/// report one for 64-bit float — that case escaped an earlier version of this test, which
/// only covered s16, and produced 268 435 455 frames from a 1.5 MB file.
#[test]
fn an_unbounded_data_chunk_is_clamped_to_the_real_file() {
    for (format, name) in [
        (OutputSampleFormat::S16, "s16"),
        (OutputSampleFormat::S24, "s24"),
        (OutputSampleFormat::F32, "f32"),
        (OutputSampleFormat::F64, "f64"),
    ] {
        let path = tmp(&format!("probe-streamed-{name}.wav"));
        io_write::write_wav(&path, &buffer(2, format), None).expect("write wav");

        let mut bytes = std::fs::read(&path).unwrap();
        let payload = FRAMES * 2 * (format.bits_per_sample() as usize / 8);
        let data = bytes.len() - payload - 8; // offset of the data chunk header
        assert_eq!(&bytes[data..data + 4], b"data", "{name}: fixture layout changed");
        // Both sizes: a chunk longer than its parent is simply malformed, and symphonia
        // says so. What a pipe produces is the pair of them at 0xFFFFFFFF, which it accepts.
        bytes[data + 4..data + 8].copy_from_slice(&u32::MAX.to_le_bytes());
        bytes[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();

        let frames = io_read::probe_source(&path)
            .expect("probe streamed wav")
            .frames
            .expect("a frame count");
        assert!(
            frames <= bytes.len() as u64 / 2,
            "{name}: {frames} frames cannot fit in a {}-byte stereo file",
            bytes.len()
        );
    }
}

/// `cli.rs` accepts .wav and .aiff for either content, so the AIFF frame correction has to
/// key off what is actually in the file. Keying off the extension applied it to WAVs named
/// `.aiff` (2 frames short) and skipped it for AIFFs named `.wav` (2 frames long) — both
/// then printed as an *exact* size.
#[test]
fn the_container_family_is_decided_by_the_magic_not_the_extension() {
    let wav = tmp("named-right.wav");
    io_write::write_wav(&wav, &buffer(2, OutputSampleFormat::S24), None).expect("write wav");
    let wav_as_aiff = tmp("wav-content.aiff");
    std::fs::copy(&wav, &wav_as_aiff).unwrap();

    let aiff = tmp("named-right.aiff");
    write_aiff(&aiff, RATE, 2, FRAMES as u32);
    let aiff_as_wav = tmp("aiff-content.wav");
    std::fs::copy(&aiff, &aiff_as_wav).unwrap();

    for path in [&wav, &wav_as_aiff, &aiff, &aiff_as_wav] {
        let probe = io_read::probe_source(path).expect("probe");
        assert_eq!(
            probe.frames,
            Some(FRAMES as u64),
            "{}",
            path.file_name().unwrap().to_string_lossy()
        );
    }
}

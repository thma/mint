use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write::{self, BextMetadata};

/// 0.5 s stereo 997 Hz tone at -6 dBFS, values in [-1, 1].
fn buffer(format: OutputSampleFormat) -> AudioBuffer {
    let rate = 48_000;
    let n = rate as usize / 2;
    let tone = |phase: f64| -> Vec<f64> {
        (0..n)
            .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / rate as f64 + phase).sin())
            .collect()
    };
    AudioBuffer {
        channels: vec![tone(0.0), tone(0.3)],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels: 2,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: format,
    }
}

fn tmp(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("render_test_{name}"))
}

/// Locate a RIFF chunk by id, returning the offset of its 8-byte header.
fn find_chunk(wav: &[u8], id: &[u8; 4]) -> Option<usize> {
    let mut p = 12; // skip "RIFF"<size>"WAVE"
    while p + 8 <= wav.len() {
        let size = u32::from_le_bytes([wav[p + 4], wav[p + 5], wav[p + 6], wav[p + 7]]) as usize;
        if &wav[p..p + 4] == id {
            return Some(p);
        }
        p += 8 + size + (size & 1); // chunks are word-aligned
    }
    None
}

#[test]
fn wav_bext_chunk_carries_r128_loudness_and_stays_decodable() {
    let buf = buffer(OutputSampleFormat::S24);
    let path = tmp("bext.wav");
    let bext = BextMetadata {
        description: "test".to_string(),
        integrated_lufs: -23.0,
        loudness_range_lu: 6.0,
        max_true_peak_dbtp: -1.0,
        max_momentary_lufs: -12.0,
        max_short_term_lufs: -13.0,
        sample_rate: 48_000,
        channels: 2,
        bits: 24,
    };
    io_write::write_wav(&path, &buf, Some(&bext)).expect("write bext wav");
    let bytes = std::fs::read(&path).expect("read back");

    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");

    let pos = find_chunk(&bytes, b"bext").expect("bext chunk present");
    let data = &bytes[pos + 8..];
    // Fixed bext v2 offsets.
    assert_eq!(u16::from_le_bytes([data[346], data[347]]), 2, "bext version 2");
    assert_eq!(i16::from_le_bytes([data[412], data[413]]), -2300, "LoudnessValue ×100");
    assert_eq!(i16::from_le_bytes([data[414], data[415]]), 600, "LoudnessRange ×100");
    assert_eq!(i16::from_le_bytes([data[416], data[417]]), -100, "MaxTruePeak ×100");

    // The spliced file must still decode as valid audio.
    let decoded = mint::audio::io_read::read_audio(&path).expect("decode bext wav");
    assert_eq!(decoded.channels_count(), 2);
    assert!(decoded.frame_len() > 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wav_without_bext_has_no_bext_chunk() {
    let buf = buffer(OutputSampleFormat::S16);
    let path = tmp("plain.wav");
    io_write::write_wav(&path, &buf, None).expect("write plain wav");
    let bytes = std::fs::read(&path).expect("read back");
    assert!(find_chunk(&bytes, b"bext").is_none(), "no bext when not requested");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn flac_output_has_flac_marker() {
    let buf = buffer(OutputSampleFormat::S24);
    let path = tmp("out.flac");
    io_write::write_flac(&path, &buf, 24).expect("write flac");
    let bytes = std::fs::read(&path).expect("read back");
    assert_eq!(&bytes[0..4], b"fLaC", "FLAC stream marker");
    assert!(bytes.len() > 100, "non-trivial flac output");
    let _ = std::fs::remove_file(&path);
}

#[cfg(feature = "mp3")]
#[test]
fn mp3_output_looks_like_mp3() {
    let buf = buffer(OutputSampleFormat::F32);
    let path = tmp("out.mp3");
    io_write::write_mp3(&path, &buf, 192).expect("write mp3");
    let bytes = std::fs::read(&path).expect("read back");
    let _ = std::fs::remove_file(&path);
    assert!(bytes.len() > 500, "mp3 output should be non-trivial");
    // ID3 tag or an MPEG audio frame sync (11 set bits).
    let mp3_sync = bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0;
    assert!(bytes.starts_with(b"ID3") || mp3_sync, "should look like MP3");
}

#[cfg(not(feature = "mp3"))]
#[test]
fn mp3_without_feature_errors_clearly() {
    let buf = buffer(OutputSampleFormat::F32);
    let path = tmp("nope.mp3");
    let err = io_write::write_mp3(&path, &buf, 320).expect_err("should error without feature");
    assert!(err.to_string().contains("--features mp3"));
}

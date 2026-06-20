use std::fs::File;
use std::io::Write;
use std::path::Path;

use mint::audio::io_read;

/// Hand-build a mono 64-bit IEEE-float WAV. `hound` can't *write* f64, and there
/// are no committed binary fixtures, so we synthesize the bytes directly:
/// a minimal RIFF/WAVE header with a 16-byte `fmt ` chunk (format tag 3 =
/// WAVE_FORMAT_IEEE_FLOAT, 64 bits/sample) followed by little-endian f64 samples.
fn write_f64_wav(path: &Path, sample_rate: u32, samples: &[f64]) {
    let bytes_per_sample = 8u32;
    let data_len = samples.len() as u32 * bytes_per_sample;
    let byte_rate = sample_rate * bytes_per_sample; // mono

    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&(36 + data_len).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    buf.extend_from_slice(&1u16.to_le_bytes()); // channels = mono
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&8u16.to_le_bytes()); // block align (1ch * 8 bytes)
    buf.extend_from_slice(&64u16.to_le_bytes()); // bits per sample

    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }

    File::create(path).unwrap().write_all(&buf).unwrap();
}

#[test]
fn reads_64bit_float_wav_losslessly() {
    // Values deliberately chosen to be UNREPRESENTABLE in f32: each differs from
    // its own f32 round-trip, so bit-exact recovery proves the input path keeps
    // full f64 precision rather than funneling through f32.
    let samples = vec![
        0.1_f64,
        -0.3_f64,
        1.0_f64 / 3.0_f64,
        0.123_456_789_012_345_67_f64,
        -0.987_654_321_098_765_4_f64,
    ];
    // Sanity-check the premise: these would change if passed through f32.
    assert!(
        samples.iter().any(|&s| (s as f32) as f64 != s),
        "test values must not be f32-representable, else the test proves nothing"
    );

    let dir = std::env::temp_dir().join("render-f64-input");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tone_f64.wav");
    write_f64_wav(&path, 48_000, &samples);

    let buffer = io_read::read_audio(&path).expect("should decode 64-bit float WAV");

    assert_eq!(buffer.sample_rate, 48_000);
    assert_eq!(buffer.channels.len(), 1);
    assert_eq!(
        buffer.channels[0], samples,
        "f64 samples must be bit-exact, not truncated to f32 at the input boundary"
    );
}

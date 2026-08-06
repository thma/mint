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
fn wav_f64_roundtrip_is_bit_exact() {
    let buf = buffer(OutputSampleFormat::F64);
    // The premise: the tone's f64 values must not survive an f32 round trip, else
    // bit-exact recovery would not prove the writer keeps full precision.
    assert!(
        buf.channels.iter().flatten().any(|&s| (s as f32) as f64 != s),
        "test signal must exercise f64 precision"
    );

    let path = tmp("out_f64.wav");
    io_write::write_wav(&path, &buf, None).expect("write f64 wav");
    let bytes = std::fs::read(&path).expect("read back");

    // fmt: tag 3 (IEEE float) at data offset 0, bits/sample at data offset 14.
    let fmt = find_chunk(&bytes, b"fmt ").expect("fmt chunk");
    assert_eq!(u16::from_le_bytes([bytes[fmt + 8], bytes[fmt + 9]]), 3, "WAVE_FORMAT_IEEE_FLOAT");
    assert_eq!(u16::from_le_bytes([bytes[fmt + 22], bytes[fmt + 23]]), 64, "64 bits/sample");
    // Non-PCM WAV requires a fact chunk carrying the per-channel frame count.
    let fact = find_chunk(&bytes, b"fact").expect("fact chunk");
    let n = u32::from_le_bytes([bytes[fact + 8], bytes[fact + 9], bytes[fact + 10], bytes[fact + 11]]);
    assert_eq!(n as usize, buf.frame_len(), "fact dwSampleLength");

    let decoded = mint::audio::io_read::read_audio(&path).expect("decode f64 wav");
    assert_eq!(decoded.sample_rate, buf.sample_rate);
    assert_eq!(decoded.channels, buf.channels, "f64 output must round-trip bit-exact");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn wav_f64_with_bext_stays_bit_exact() {
    let buf = buffer(OutputSampleFormat::F64);
    let path = tmp("bext_f64.wav");
    let bext = BextMetadata {
        description: "test".to_string(),
        integrated_lufs: -23.0,
        loudness_range_lu: 6.0,
        max_true_peak_dbtp: -1.0,
        max_momentary_lufs: -12.0,
        max_short_term_lufs: -13.0,
        sample_rate: 48_000,
        channels: 2,
        bits: 64,
    };
    io_write::write_wav(&path, &buf, Some(&bext)).expect("write f64 bext wav");
    let bytes = std::fs::read(&path).expect("read back");
    assert!(find_chunk(&bytes, b"bext").is_some(), "bext chunk present");

    // The splice must not disturb the audio payload.
    let decoded = mint::audio::io_read::read_audio(&path).expect("decode f64 bext wav");
    assert_eq!(decoded.channels, buf.channels, "bit-exact after bext splice");
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

/// Read the STREAMINFO metadata block, which the spec pins as the first block after the
/// 4-byte `fLaC` marker: a 4-byte block header (`is_last`|type|24-bit length) then 34
/// bytes of payload opening with `min_blocksize` and `max_blocksize`, both big-endian u16.
fn flac_streaminfo_block_sizes(bytes: &[u8]) -> (u16, u16) {
    assert_eq!(&bytes[0..4], b"fLaC", "FLAC stream marker");
    assert_eq!(bytes[4] & 0x7f, 0, "first metadata block must be STREAMINFO");
    let len = u32::from_be_bytes([0, bytes[5], bytes[6], bytes[7]]);
    assert_eq!(len, 34, "STREAMINFO is a fixed 34 bytes");
    (
        u16::from_be_bytes([bytes[8], bytes[9]]),
        u16::from_be_bytes([bytes[10], bytes[11]]),
    )
}

/// Block size of the stream's **first** audio frame, decoded straight from the frame
/// header — an independent reading of what the frames actually use, so STREAMINFO can be
/// checked against the bitstream rather than against itself.
///
/// Walks the metadata blocks to the one flagged `is_last`, then parses the frame header:
/// 14-bit sync `0b11111111111110`, 1 reserved bit, 1 blocking-strategy bit (0 = fixed),
/// then a 4-bit block-size code in the high nibble of the next byte. Codes 6 and 7 — the
/// ones flacenc actually emits — carry the real size (minus one) *after* the header's
/// UTF-8-coded frame number, so that has to be stepped over first. Returns
/// `(block_size, is_fixed_blocking)`.
fn flac_first_frame_block_size(bytes: &[u8]) -> (u32, bool) {
    // Skip the `fLaC` marker, then each metadata block, until one is flagged last.
    let mut p = 4;
    loop {
        let is_last = bytes[p] & 0x80 != 0;
        let len = u32::from_be_bytes([0, bytes[p + 1], bytes[p + 2], bytes[p + 3]]) as usize;
        p += 4 + len;
        if is_last {
            break;
        }
    }

    assert_eq!(bytes[p], 0xff, "frame sync byte 0");
    assert_eq!(bytes[p + 1] & 0xfc, 0xf8, "frame sync byte 1");
    let is_fixed = bytes[p + 1] & 0x01 == 0;
    let code = bytes[p + 2] >> 4;

    // Frame/sample number: UTF-8-style, its length encoded in the leading ones of byte 0.
    let lead = bytes[p + 4];
    let coded_number_len = if lead & 0x80 == 0 { 1 } else { lead.leading_ones() as usize };
    let after_number = p + 4 + coded_number_len;

    let block_size = match code {
        1 => 192,
        2..=5 => 576u32 << (code - 2),
        6 => u32::from(bytes[after_number]) + 1,
        7 => u32::from(u16::from_be_bytes([bytes[after_number], bytes[after_number + 1]])) + 1,
        8..=15 => 256u32 << (code - 8),
        _ => panic!("reserved block-size code {code}"),
    };
    (block_size, is_fixed)
}

/// A tone of exactly `frames` frames, so the encoder's final block can be made partial.
fn flac_buffer(frames: usize, format: OutputSampleFormat) -> AudioBuffer {
    let rate = 48_000;
    let tone: Vec<f64> = (0..frames)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * 997.0 * i as f64 / rate as f64).sin())
        .collect();
    AudioBuffer {
        channels: vec![tone.clone(), tone],
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

/// Regression: mint's FLAC files would not play on any Apple platform.
///
/// `encode_with_fixed_block_size` emits fixed-blocksize frames (frame-number headers),
/// but flacenc folds the short *final* frame into STREAMINFO's running `min_blocksize`,
/// so a file whose length is not a multiple of the block size declared e.g. min=512 /
/// max=4096. RFC 9639 §8.2 excludes the last block from that minimum precisely because
/// `min != max` means "variable blocksize" — and CoreAudio rejects that against
/// fixed-blocksize frames, failing `ExtAudioFileRead` with −50. The bitstream itself was
/// always valid and lossless, which is why nothing else caught it.
///
/// Checking `min == max` alone would be too weak: collapsing the other way, to
/// `(min, min)`, satisfies it just as well and still yields an Apple-unplayable file (it
/// declares the *final* block's length for every block). So the declared size is pinned
/// against the first frame header, read independently out of the bitstream.
#[test]
fn flac_streaminfo_declares_a_fixed_blocksize() {
    // 4096 is flacenc's default block size; +1000 guarantees a partial final block.
    for frames in [4096 * 3 + 1000, 4096 * 2, 500] {
        for bits in [16usize, 24] {
            let buf = flac_buffer(frames, if bits == 16 {
                OutputSampleFormat::S16
            } else {
                OutputSampleFormat::S24
            });
            let path = tmp(&format!("blocksize_{frames}_{bits}.flac"));
            io_write::write_flac(&path, &buf, bits).expect("write flac");
            let bytes = std::fs::read(&path).expect("read back");
            let _ = std::fs::remove_file(&path);

            let case = format!("{frames} frames, {bits}-bit");
            let (min, max) = flac_streaminfo_block_sizes(&bytes);
            assert_eq!(
                min, max,
                "STREAMINFO must declare a fixed blocksize ({case}); \
                 min={min} max={max} reads as a variable-blocksize stream",
            );

            let (frame_bs, is_fixed) = flac_first_frame_block_size(&bytes);
            assert!(is_fixed, "frames must use fixed-blocksize headers ({case})");
            if frames > usize::from(max) {
                // More than one block, so the first frame is a full, non-final one and the
                // declaration must match it exactly. This is what fails if the fixup ever
                // collapses to `(min, min)`: it would declare the final block's length.
                assert_eq!(
                    u32::from(max),
                    frame_bs,
                    "STREAMINFO blocksize {max} must match the {frame_bs} the frames use ({case})",
                );
            } else {
                // A stream shorter than one block has only a *last* block, which RFC 9639
                // §8.2 excludes from the minimum — so the encoder is free to declare the
                // configured block size instead, which is what the FLAC reference encoder
                // does (and flacenc deliberately mirrors). Over-declaring is safe; it only
                // makes a decoder allocate more than it needs. Under-declaring is not.
                assert!(
                    u32::from(max) >= frame_bs,
                    "STREAMINFO blocksize {max} under-declares the {frame_bs} frame ({case})",
                );
            }
        }
    }
}

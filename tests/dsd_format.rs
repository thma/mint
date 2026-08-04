//! DSF container: byte layout, bit order, blocking and padding.
//!
//! A container writer is the one place in mint where "it runs" and "it is correct" are
//! completely unrelated. A file with the data chunk size off by 12, or the bits packed from
//! the wrong end, still writes without complaint and still decodes into *something* — so
//! everything load-bearing here is asserted against a value derived outside the writer:
//! a literal golden header, hand-computed sizes, and a foreign parser (the `dsf` crate) that
//! has never seen our code.
//!
//! Run with `cargo test --test dsd_format -- --nocapture` to see the measured tables and
//! the path of the playable file the end-to-end test leaves behind.
//!
//! # The one deliberate deviation from the specification
//!
//! Sony's Annotation 3 says to fill the final block with `0x00` and leave `sampleCount` at
//! the true sample count. mint fills with `0x69` and counts the padding as audio. The
//! reason is measured in [`zero_padding_would_be_a_full_scale_dc_step`], which is the whole
//! argument in one test:
//!
//! | final-block fill | decimated level in the padding |
//! |---|---|
//! | `0x00` (per spec) | -1.0 — full-scale negative DC |
//! | `0x69` (mint) | 0.0 — true digital silence |
//!
//! A DSD bit is +-1, so a zero byte is not "no signal", it is the most negative signal the
//! format can express. See `src/audio/io_write_dsd.rs` for what the readers do about it.
//!
//! # Measured baseline (2026-08-03, Apple Silicon, seed 0xD5F, ~4.5 s in a debug build)
//!
//! | property | measured | gate |
//! |---|---|---|
//! | in-band SNR of the decoded file, DSD64 at 44.1 kHz | 104.4 dB | > 95 dB |
//! | the same bits decoded at 88.2 kHz | 39.7 dB | not asserted — see below |
//! | `0x00` padding, decimated | -1.000000 | < -0.99 |
//! | `0x69` padding, decimated level / peak | 0.0 / 0.0 | < 1e-6 / < 1e-3 |
//!
//! The 39.7 dB is not a defect and is the reason the end-to-end gate decodes at 44.1 kHz:
//! DSD moves its quantization noise above the audio band, so a wideband time-domain
//! comparison measures that ultrasonic noise instead of the signal. Same bits, same
//! alignment (best-fit gain 1.0000002, zero lag) — only the analysis bandwidth differs.

use std::path::PathBuf;

use mint::audio::io_write_dsd::{BLOCK_BYTES, BLOCK_SAMPLES, DsfWriter, HEADER_BYTES};
use mint::config::ResampleQuality;
use mint::ops::dsd::DsdRate;
use mint::ops::dsd::decimate::Decimator;
use mint::ops::dsd::interpolate::SoxrStream;
use mint::ops::dsd::modulator::Modulator;
use mint::ops::dsd::pack::{BitOrder, BitPacker, DSD_SILENCE};

const SEED: u64 = 0xD5F;

/// The complete 92-byte header of a stereo DSD64 file holding exactly one block per
/// channel. Written out by hand from the specification's field table rather than generated,
/// so it fails if the writer's *construction* changes, not just its output.
///
/// Sizes for this file: payload `2 x 4096 = 8192`, whole file `92 + 8192 = 8284` (0x205C),
/// data chunk `12 + 8192 = 8204` (0x200C), `sampleCount` `32 768` (0x8000) per channel.
#[rustfmt::skip]
const GOLDEN_HEADER: [u8; 92] = [
    // ── "DSD " chunk, 28 bytes ───────────────────────────────────────────────
    b'D', b'S', b'D', b' ',                     // note the trailing space, not a NUL
    28, 0, 0, 0, 0, 0, 0, 0,                    // chunk size, including this header
    0x5C, 0x20, 0, 0, 0, 0, 0, 0,               // total file size, 8284
    0, 0, 0, 0, 0, 0, 0, 0,                     // metadata pointer: no ID3v2
    // ── "fmt " chunk, 52 bytes ───────────────────────────────────────────────
    b'f', b'm', b't', b' ',
    52, 0, 0, 0, 0, 0, 0, 0,                    // chunk size, including this header
    1, 0, 0, 0,                                 // format version
    0, 0, 0, 0,                                 // format id: 0 = DSD raw
    2, 0, 0, 0,                                 // channel type: 2 = stereo (FL, FR)
    2, 0, 0, 0,                                 // channel count
    0x00, 0x11, 0x2B, 0x00,                     // sampling frequency, 2 822 400 Hz
    1, 0, 0, 0,                                 // bits per sample: 1 means LSB first
    0x00, 0x80, 0, 0, 0, 0, 0, 0,               // sample count per channel, 32 768
    0x00, 0x10, 0, 0,                           // block size per channel, 4096
    0, 0, 0, 0,                                 // reserved: readers reject non-zero
    // ── "data" chunk header, 12 bytes ────────────────────────────────────────
    b'd', b'a', b't', b'a',
    0x0C, 0x20, 0, 0, 0, 0, 0, 0,               // chunk size = 12 + payload
];

// ────────────────────────────────── helpers ──────────────────────────────────

/// Scratch path, unique to this process.
///
/// Deliberately *not* the `std::env::temp_dir()` idiom the older tests use. Every test here
/// writes a file and reads it straight back, so a fixed machine-global path means two
/// overlapping runs of this binary — a second checkout, a re-run started before the first
/// finished, two CI jobs on one runner — truncate each other's files mid-read. Measured:
/// three concurrent runs sharing one temp dir fail 1, 3 and 4 tests respectively; the same
/// three with private dirs pass every test.
///
/// What makes that worth a comment rather than a shrug is the *shape* of the failure. The
/// end-to-end test reports it as `the chain lost the signal: -15.2 dB`, which reads exactly
/// like a modulator regression and sends the reader into `modulator.rs`. The chain itself is
/// bit-deterministic: 22 isolated runs produced one identical SHA-256 and 104.4 dB every
/// time.
///
/// `CARGO_TARGET_TMPDIR` is what Cargo hands integration tests for this; the pid prefix
/// covers two runs against the same target directory. It goes in the file name rather than
/// in a directory so that repeated runs leave nothing behind but the one file
/// [`a_modulated_tone_survives_the_whole_chain`] deliberately keeps.
fn tmp(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir.join(format!("dsd-{}-{name}", std::process::id()))
}

/// Where the playable artefact ends up. Fixed, unlike [`tmp`], so a player can stay pointed
/// at it across runs — safe because it is only ever *copied* to, after every assertion has
/// passed, and nothing reads it back.
fn artefact(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

/// Write one file from a planar bit stream, in `chunk`-sized pieces, and read it back.
fn write_dsf(name: &str, rate: DsdRate, bits: &[Vec<bool>], chunk: usize) -> (PathBuf, Vec<u8>) {
    let path = tmp(name);
    let mut w = DsfWriter::create(&path, bits.len(), rate).expect("create dsf");
    let total = bits[0].len();
    let mut pos = 0;
    while pos < total {
        let n = chunk.min(total - pos);
        let block: Vec<Vec<bool>> = bits.iter().map(|c| c[pos..pos + n].to_vec()).collect();
        w.write(&block).expect("write block");
        pos += n;
    }
    w.finish().expect("finish dsf");
    let bytes = std::fs::read(&path).expect("read back");
    (path, bytes)
}

/// The `n`th 4096-byte block of channel `ch`, straight out of the payload.
fn block(payload: &[u8], channels: usize, ch: usize, n: usize) -> &[u8] {
    let off = (n * channels + ch) * BLOCK_BYTES;
    &payload[off..off + BLOCK_BYTES]
}

/// Recover one channel's bit stream from the payload — the reader half of the format,
/// written independently of `pack.rs` so the two have to agree on LSB-first for real.
fn unpack(payload: &[u8], channels: usize, ch: usize) -> Vec<bool> {
    let blocks = payload.len() / (BLOCK_BYTES * channels);
    let mut bits = Vec::with_capacity(blocks * BLOCK_SAMPLES as usize);
    for b in 0..blocks {
        for &byte in block(payload, channels, ch, b) {
            for i in 0..8 {
                bits.push((byte >> i) & 1 == 1);
            }
        }
    }
    bits
}

fn u32_at(bytes: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap())
}

fn tone(rate: u32, frames: usize, hz: f64, amp: f64) -> Vec<f64> {
    (0..frames)
        .map(|n| amp * (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin())
        .collect()
}

fn rms(x: &[f64]) -> f64 {
    (x.iter().map(|v| v * v).sum::<f64>() / x.len() as f64).sqrt()
}

// ═══════════════════════════ 1. the header, byte for byte ════════════════════

#[test]
fn dsf_header_is_byte_exact() {
    let bits = vec![vec![false; BLOCK_SAMPLES as usize]; 2];
    let (path, bytes) = write_dsf("golden.dsf", DsdRate::Dsd64, &bits, 4096);

    assert_eq!(
        &bytes[..92],
        &GOLDEN_HEADER[..],
        "header does not match the specification layout"
    );
    assert_eq!(bytes.len(), 8284, "92-byte header + one block per channel");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_three_patched_fields_agree_with_the_file_on_disk() {
    // 2.5 blocks of audio, so the padding path runs and the sizes are not round numbers.
    let n = (BLOCK_SAMPLES * 5 / 2) as usize;
    let bits = vec![vec![true; n]; 2];
    let (path, bytes) = write_dsf("sizes.dsf", DsdRate::Dsd128, &bits, 7777);

    let payload = bytes.len() as u64 - HEADER_BYTES;
    assert_eq!(
        payload,
        3 * BLOCK_BYTES as u64 * 2,
        "2.5 blocks round up to 3"
    );
    assert_eq!(
        u64_at(&bytes, 12),
        bytes.len() as u64,
        "totalFileSize must equal the real file size — WavPack rejects the file otherwise"
    );
    assert_eq!(
        u64_at(&bytes, 84),
        12 + payload,
        "the data chunk size includes its own 12-byte header"
    );
    assert_eq!(
        u64_at(&bytes, 64),
        3 * BLOCK_SAMPLES,
        "sampleCount counts the padded blocks, not just the audio"
    );
    assert_eq!(
        u32_at(&bytes, 56),
        DsdRate::Dsd128.hz(),
        "sampling frequency"
    );
    assert_eq!(u32_at(&bytes, 76), 0, "reserved must be zero");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn channel_type_follows_the_channel_count() {
    // (channels, expected channel type id). Id 5 is unreachable from a bare count — see
    // the table in `io_write_dsd::channel_type`.
    for (channels, want) in [(1, 1), (2, 2), (3, 3), (4, 4), (5, 6), (6, 7)] {
        let bits = vec![vec![false; BLOCK_SAMPLES as usize]; channels];
        let (path, bytes) = write_dsf(&format!("ct{channels}.dsf"), DsdRate::Dsd64, &bits, 8192);
        assert_eq!(
            u32_at(&bytes, 48),
            want,
            "channel type for {channels} channels"
        );
        assert_eq!(u32_at(&bytes, 52), channels as u32, "channel count");
        let _ = std::fs::remove_file(&path);
    }
}

// ═══════════════════════ 2. bit order, blocking, padding ═════════════════════

#[test]
fn samples_are_packed_lsb_first() {
    // Two high samples then six low: LSB first that is 0b0000_0011, MSB first it would be
    // 0b1100_0000. The distinction is the whole reason bit order is a parameter.
    let mut first = vec![false; BLOCK_SAMPLES as usize];
    first[0] = true;
    first[1] = true;
    let (path, bytes) = write_dsf("lsb.dsf", DsdRate::Dsd64, &[first], 4096);

    assert_eq!(bytes[92], 0x03, "earliest sample belongs in bit 0");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn blocks_are_planar_and_round_robin_across_channels() {
    // Two blocks per channel, each channel a constant, so round-robin gives FF 00 FF 00 and
    // channel-major (all of ch0, then all of ch1) would give FF FF 00 00.
    //
    // The chunk sizes matter here and the small one alone is not enough: fed in 999-sample
    // pieces, every flush has at most one block ready per channel, and the two loop nestings
    // in `flush_full_blocks` are then indistinguishable. Only the single-call case, where two
    // block rounds are emitted at once, actually pins the ordering.
    let n = 2 * BLOCK_SAMPLES as usize;
    let bits = vec![vec![true; n], vec![false; n]];

    for (label, chunk) in [("one call, two block rounds", n), ("dribbled in", 999)] {
        let (path, bytes) = write_dsf("planar.dsf", DsdRate::Dsd64, &bits, chunk);
        let payload = &bytes[92..];

        assert_eq!(payload.len(), 4 * BLOCK_BYTES, "2 blocks x 2 channels");
        for b in 0..2 {
            assert!(
                block(payload, 2, 0, b).iter().all(|&x| x == 0xFF),
                "{label}: ch0 block {b}"
            );
            assert!(
                block(payload, 2, 1, b).iter().all(|&x| x == 0x00),
                "{label}: ch1 block {b}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn the_final_block_is_filled_out_with_dsd_silence() {
    // One single low sample: the first byte is part audio, part pad, and everything after
    // it is pad. Filling with 0x69 has to continue *in phase* across that seam, which is
    // why the first byte is 0x68 (0x69 with bit 0 cleared by the audio sample) and not
    // 0x69 or 0xD2.
    let (path, bytes) = write_dsf("pad.dsf", DsdRate::Dsd64, &[vec![false]], 1);
    let payload = &bytes[92..];

    assert_eq!(
        payload.len(),
        BLOCK_BYTES,
        "one sample still costs a whole block"
    );
    assert_eq!(
        payload[0], 0x68,
        "the part-filled byte keeps the pad pattern's phase"
    );
    assert!(
        payload[1..].iter().all(|&b| b == DSD_SILENCE),
        "the rest of the block must be the DSD idle pattern"
    );
    assert_eq!(
        u64_at(&bytes, 64),
        BLOCK_SAMPLES,
        "the padding counts as audio"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_pad_byte_is_a_parameter_so_a_spec_conformant_file_is_still_reachable() {
    // Annotation 3's `0x00` fill, which mint does not use by default (see the module docs
    // and `zero_padding_would_be_a_full_scale_dc_step` for the measurement that decided it).
    // The writer can still produce it, and this is the test that proves the override works
    // rather than being an untested escape hatch.
    let path = tmp("pad_zero.dsf");
    let mut w = DsfWriter::create(&path, 1, DsdRate::Dsd64)
        .expect("create")
        .with_pad_byte(0x00);
    w.write(&[vec![true; 9]]).expect("write");
    assert_eq!(w.audio_samples(), 9);
    let summary = w.finish().expect("finish");

    let bytes = std::fs::read(&path).expect("read back");
    let payload = &bytes[92..];
    assert_eq!(summary.padding_samples, BLOCK_SAMPLES - 9);
    assert_eq!(payload[0], 0xFF, "eight high samples");
    assert_eq!(
        payload[1], 0x01,
        "the ninth, then zero fill in the same byte"
    );
    assert!(
        payload[2..].iter().all(|&b| b == 0x00),
        "the override has to reach the whole-byte fill too, not just the seam"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_whole_number_of_blocks_needs_no_padding() {
    let path = tmp("exact.dsf");
    let n = 3 * BLOCK_SAMPLES as usize;
    let mut w = DsfWriter::create(&path, 2, DsdRate::Dsd256).expect("create");
    assert_eq!(w.samples_to_block_boundary(), 0, "nothing written yet");
    w.write(&vec![vec![true; n]; 2]).expect("write");
    assert_eq!(
        w.samples_to_block_boundary(),
        0,
        "landed on a block boundary"
    );

    let s = w.finish().expect("finish");
    assert_eq!(s.padding_samples, 0);
    assert_eq!(s.blocks, 3);
    assert_eq!(s.samples_per_channel, s.audio_samples_per_channel);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn samples_to_block_boundary_reports_what_is_missing() {
    let path = tmp("boundary.dsf");
    let mut w = DsfWriter::create(&path, 1, DsdRate::Dsd64).expect("create");
    w.write(&[vec![false; 1000]]).expect("write");
    assert_eq!(w.samples_to_block_boundary(), BLOCK_SAMPLES - 1000);

    // Topping up by exactly that much is how a caller avoids the pad splice entirely.
    let top_up = w.samples_to_block_boundary() as usize;
    w.write(&[vec![false; top_up]]).expect("top up");
    assert_eq!(w.samples_to_block_boundary(), 0);
    assert_eq!(w.finish().expect("finish").padding_samples, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn chunking_does_not_change_the_file() {
    // The writer's block accounting is the only stateful part of it; feeding the same
    // stream in different-sized pieces is what would expose a bookkeeping slip.
    let n = 2 * BLOCK_SAMPLES as usize + 12_345;
    let bits: Vec<Vec<bool>> = (0..2)
        .map(|c| (0..n).map(|i| (i * 7 + c * 3) % 11 < 5).collect())
        .collect();

    let (p0, reference) = write_dsf("chunk_ref.dsf", DsdRate::Dsd64, &bits, n);
    for chunk in [1, 3, 4095, 32_768, 100_000] {
        let (p, bytes) = write_dsf(&format!("chunk_{chunk}.dsf"), DsdRate::Dsd64, &bits, chunk);
        assert_eq!(
            bytes, reference,
            "chunk size {chunk} produced a different file"
        );
        let _ = std::fs::remove_file(&p);
    }
    let _ = std::fs::remove_file(&p0);
}

// ══════════════════════════ 3. the packer on its own ═════════════════════════

#[test]
fn msb_first_is_the_mirror_image_of_lsb_first() {
    let bits: Vec<bool> = (0..32).map(|i| i % 3 == 0).collect();

    let mut lsb = BitPacker::new(BitOrder::LsbFirst);
    let mut msb = BitPacker::new(BitOrder::MsbFirst);
    lsb.push(&bits);
    msb.push(&bits);

    let mirrored: Vec<u8> = lsb.bytes().iter().map(|b| b.reverse_bits()).collect();
    assert_eq!(
        msb.bytes(),
        &mirrored[..],
        "the two orders must be bit reversals"
    );
    assert_eq!(lsb.byte_len(), 4);
    assert_eq!(lsb.pending_bits(), 0);
}

#[test]
fn pad_partial_takes_the_fills_bits_from_the_same_positions() {
    // Three real samples (low, high, low), five positions to fill from 0x69 = 0b0110_1001.
    //
    // LSB first: samples occupy bits 0..2 -> 0b0000_0010, fill contributes bits 3..7 of
    //            0x69 -> 0b0110_1000, giving 0x6A.
    // MSB first: samples occupy bits 7..5 -> 0b0100_0000, fill contributes bits 4..0 of
    //            0x69 -> 0b0000_1001, giving 0x49.
    //
    // The point is that the fill bits come from the *same* positions in both cases, so the
    // pad pattern continues in phase across the seam instead of restarting at its low end.
    for (order, want) in [(BitOrder::LsbFirst, 0x6Au8), (BitOrder::MsbFirst, 0x49u8)] {
        let mut p = BitPacker::new(order);
        p.push(&[false, true, false]);
        assert_eq!(p.pending_bits(), 3);
        assert!(
            p.pad_partial(DSD_SILENCE),
            "there was a partial byte to close"
        );
        assert!(!p.pad_partial(DSD_SILENCE), "and now there is not");
        assert_eq!(p.bytes(), &[want], "{order:?}");
    }
}

#[test]
fn ragged_and_oversized_inputs_are_rejected() {
    let path = tmp("reject.dsf");
    let err = DsfWriter::create(&path, 7, DsdRate::Dsd64).unwrap_err();
    assert!(
        err.to_string().contains("1 to 6 channels"),
        "unhelpful error: {err}"
    );

    let mut w = DsfWriter::create(&path, 2, DsdRate::Dsd64).expect("create");
    let err = w.write(&[vec![false; 8], vec![false; 9]]).unwrap_err();
    assert!(err.to_string().contains("ragged"), "unhelpful error: {err}");
    let err = w.write(&[vec![false; 8]]).unwrap_err();
    assert!(
        err.to_string().contains("2 channels"),
        "unhelpful error: {err}"
    );
    let _ = std::fs::remove_file(&path);
}

// ═════════════════ 4. why the padding is not 0x00, measured ══════════════════

#[test]
fn zero_padding_would_be_a_full_scale_dc_step() {
    // The justification for deviating from Annotation 3. Decimate a block of each candidate
    // fill pattern and look at what actually comes out of a decoder.
    let dec = Decimator::for_rates(DsdRate::Dsd64.hz(), 88_200).expect("decimator");
    let mut table = Vec::new();

    for (name, byte) in [("0x00 (per spec)", 0x00u8), ("0x69 (mint)", DSD_SILENCE)] {
        let bits: Vec<bool> = std::iter::repeat_n(byte, BLOCK_BYTES)
            .flat_map(|b| (0..8).map(move |i| (b >> i) & 1 == 1))
            .collect();
        let pcm = dec.process_bits(&bits);
        // Middle half only: the ends are the decimator's own transient.
        let mid = &pcm[pcm.len() / 4..pcm.len() * 3 / 4];
        let level = mid.iter().sum::<f64>() / mid.len() as f64;
        let peak = mid.iter().fold(0.0f64, |a, &v| a.max(v.abs()));
        table.push((name, level, peak));
    }

    println!("\n  final-block fill      decimated level    peak");
    for (name, level, peak) in &table {
        println!("  {name:<20}  {level:>12.6}    {peak:>8.6}");
    }

    let (_, zero_level, _) = table[0];
    let (_, silence_level, silence_peak) = table[1];
    assert!(
        zero_level < -0.99,
        "0x00 must show up as full-scale negative DC, got {zero_level}"
    );
    assert!(
        silence_level.abs() < 1e-6 && silence_peak < 1e-3,
        "0x69 must decode to silence, got level {silence_level} peak {silence_peak}"
    );
}

// ═══════════════════ 5. cross-checks against foreign code ════════════════════

#[test]
fn the_dsf_crate_accepts_what_we_write() {
    // A second opinion from a parser that has never seen our writer. It validates the
    // magic, version, format id, channel type, block size and the reserved field, but
    // *not* the three length fields — so those are checked here by hand. That is exactly
    // where an off-by-12 on the data chunk would hide.
    use dsf::{ChannelType, DsfFile};

    let n = 2 * BLOCK_SAMPLES as usize;
    let bits: Vec<Vec<bool>> = (0..2)
        .map(|c| (0..n).map(|i| (i / (c + 1)) % 5 == 0).collect())
        .collect();
    let (path, bytes) = write_dsf("oracle.dsf", DsdRate::Dsd64, &bits, 30_000);

    let f = DsfFile::open(&path).expect("the dsf crate rejected our file");
    let fmt = f.fmt_chunk();
    assert_eq!(*fmt.channel_type(), ChannelType::Stereo);
    assert_eq!(fmt.channel_num(), 2);
    assert_eq!(fmt.sampling_frequency(), DsdRate::Dsd64.hz());
    assert_eq!(fmt.bits_per_sample(), 1);
    assert_eq!(fmt.block_size_per_channel(), BLOCK_BYTES as u32);
    assert_eq!(fmt.sample_count(), 2 * BLOCK_SAMPLES);
    assert!(f.id3_tag().is_none(), "phase 3 writes no metadata chunk");

    assert_eq!(
        f.dsd_chunk().file_size(),
        bytes.len() as u64,
        "the crate parses totalFileSize but never checks it — we do"
    );
    assert_eq!(
        f.data_chunk().chunk_size(),
        12 + (bytes.len() as u64 - HEADER_BYTES),
        "the crate parses the data chunk size but never checks it — we do"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn the_container_round_trips_the_bitstream_exactly() {
    // Container correctness with the DSP taken out of the argument: the bits read back out
    // of the file must be the bits the modulator produced, per channel, in order.
    let pcm_rate = 88_200;
    let rate = DsdRate::Dsd64;
    let frames = 4000;
    let source = vec![
        tone(pcm_rate, frames, 997.0, 0.8),
        tone(pcm_rate, frames, 440.0, 0.5),
    ];

    let (path, expected) = modulate_to_file("roundtrip.dsf", &source, pcm_rate, rate, 512);
    let bytes = std::fs::read(&path).expect("read back");
    let payload = &bytes[HEADER_BYTES as usize..];

    for (c, want) in expected.iter().enumerate() {
        let got = unpack(payload, 2, c);
        assert_eq!(got.len(), u64_at(&bytes, 64) as usize, "channel {c} length");
        assert_eq!(&got, want, "channel {c} bitstream");
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_modulated_tone_survives_the_whole_chain() {
    // The first playable file. PCM in, .dsf on disk, decoded back out of the file's own
    // bytes — nothing in this measurement comes from the writer's in-memory state.
    //
    // The decode target is 44.1 kHz, not the 88.2 kHz the source is at, and that is not
    // incidental: DSD's whole trick is that the quantization noise is *outside* the audio
    // band, so a wideband comparison measures the ultrasonic noise rather than the signal.
    // Decoding this chain at 88.2 kHz gives 39.7 dB and decoding it at 44.1 kHz gives
    // 104.6 dB — from the same bits. Only the second number says anything about what a
    // listener hears. (The modulator's own 121 dB is measured properly, in band, in
    // `tests/dsd_verification.rs`; the difference is the 17.6-22 kHz transition band that
    // the decimator still passes.)
    const OUT_RATE: u32 = 44_100;
    let pcm_rate = 88_200;
    let rate = DsdRate::Dsd64;
    let frames = pcm_rate as usize / 2; // 0.5 s
    // Different tones per channel, so the file is audibly stereo rather than a mono beep.
    let source = vec![
        tone(pcm_rate, frames, 997.0, 0.5),
        tone(pcm_rate, frames, 1245.0, 0.5),
    ];

    let (path, _) = modulate_to_file("tone.dsf", &source, pcm_rate, rate, 4096);
    let bytes = std::fs::read(&path).expect("read back");
    let bits = unpack(&bytes[HEADER_BYTES as usize..], 2, 0);

    let dec = Decimator::for_rates(rate.hz(), OUT_RATE).expect("decimator");
    // Out of modulation units: the modulator scales PCM 0 dBFS to 50 % modulation.
    let pcm: Vec<f64> = dec
        .process_bits(&bits)
        .into_iter()
        .map(|v| v * 2.0)
        .collect();
    let reference = tone(OUT_RATE, pcm.len(), 997.0, 0.5);

    // Middle half only: the ends are the decimator's transient and the trailing silence
    // that filled out the last block.
    let lo = pcm.len() / 4;
    let hi = pcm.len() * 3 / 4;
    let error: Vec<f64> = (lo..hi).map(|i| pcm[i] - reference[i]).collect();
    let snr = 20.0 * (rms(&reference[lo..hi]) / rms(&error)).log10();

    println!("\n  in-band SNR of the decoded file: {snr:.1} dB");
    assert!(snr > 95.0, "the chain lost the signal: {snr:.1} dB");

    // `docs/dsd-design.md` phase 3 asks for an audible artefact, so this one file survives —
    // at a fixed path, copied only once everything above has passed.
    let keep = artefact("mint-dsd64-tone.dsf");
    std::fs::copy(&path, &keep).expect("keep the artefact");
    let _ = std::fs::remove_file(&path);
    println!("  playable file: {}", keep.display());
}

/// PCM -> streaming interpolation -> per-channel modulator -> DSF, topping the stream up
/// with modulated silence so the final block needs no pad splice. This is the shape the
/// phase-4 block loop will have.
fn modulate_to_file(
    name: &str,
    source: &[Vec<f64>],
    pcm_rate: u32,
    rate: DsdRate,
    block: usize,
) -> (PathBuf, Vec<Vec<bool>>) {
    let channels = source.len();
    let path = tmp(name);
    let mut src = SoxrStream::new(pcm_rate, rate.hz(), channels, Some(ResampleQuality::Vhq))
        .expect("soxr stream");
    let mut mods: Vec<Modulator> = (0..channels)
        .map(|c| Modulator::for_channel(rate, Some(SEED), c))
        .collect();
    let mut w = DsfWriter::create(&path, channels, rate).expect("create dsf");

    let mut all = vec![Vec::<bool>::new(); channels];
    let push = |w: &mut DsfWriter,
                mods: &mut Vec<Modulator>,
                up: &[Vec<f64>],
                all: &mut Vec<Vec<bool>>| {
        let bits: Vec<Vec<bool>> = up
            .iter()
            .enumerate()
            .map(|(c, pcm)| {
                let mut b = Vec::with_capacity(pcm.len());
                mods[c].modulate_into(pcm, &mut b);
                b
            })
            .collect();
        for (c, b) in bits.iter().enumerate() {
            all[c].extend_from_slice(b);
        }
        w.write(&bits).expect("write dsf block");
    };

    let frames = source[0].len();
    let mut pos = 0;
    while pos < frames {
        let n = block.min(frames - pos);
        let chunk: Vec<Vec<f64>> = source.iter().map(|c| c[pos..pos + n].to_vec()).collect();
        let up = src.process(&chunk).expect("interpolate").to_vec();
        push(&mut w, &mut mods, &up, &mut all);
        pos += n;
    }
    let tail = src.finish().expect("flush").to_vec();
    push(&mut w, &mut mods, &tail, &mut all);

    // Genuine modulated silence rather than a spliced pattern: no discontinuity at all,
    // and the loop state runs continuously through the tail.
    let short = w.samples_to_block_boundary() as usize;
    if short > 0 {
        let silence = vec![vec![0.0; short]; channels];
        push(&mut w, &mut mods, &silence, &mut all);
    }

    let summary = w.finish().expect("finish dsf");
    assert_eq!(
        summary.padding_samples, 0,
        "topping up should remove all padding"
    );
    (path, all)
}

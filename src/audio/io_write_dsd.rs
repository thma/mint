//! DSF (DSD Stream File, Sony) container writer — streaming, header patched at the end.
//!
//! The odd one out among mint's writers. Every other one in [`crate::audio::io_write`] is
//! handed a finished `AudioBuffer` and serializes it; this one cannot be, because the thing
//! it serializes never exists in memory (`docs/dsd-design.md` section 3.2: five minutes of
//! DSD64 stereo would be ~13.5 GB as f64). So it is fed in blocks, writes each one straight
//! through, and seeks back at the end to fill in the three header fields that depend on the
//! total length.
//!
//! Peak resident state is one block per channel plus change: 4 kB x channels.
//!
//! # Layout
//!
//! Little-endian throughout. The header is exactly 92 bytes and the payload starts there;
//! the three fields marked `<-` are the ones [`DsfWriter::finish`] patches.
//!
//! Read the left column top to bottom, then the right one: it is a single run of ascending
//! offsets, wrapped once for width. Worth being fussy about, because this block is the map
//! anyone adding the metadata chunk or a DFF writer will trust — an earlier version that
//! interleaved the offsets across three columns had a wrong one in it within a day.
//!
//! ```text
//!  0  "DSD "                     44  formatID u32 = 0
//!  4  chunkSize u64 = 28         48  channelType u32
//! 12  totalFileSize u64      <-  52  channelNum u32
//! 20  metadataPointer u64 = 0    56  samplingFrequency u32
//! 28  "fmt "                     60  bitsPerSample u32 = 1
//! 32  chunkSize u64 = 52         64  sampleCount u64, per channel   <-
//! 40  formatVersion u32 = 1      72  blockSizePerChannel u32 = 4096
//!                                76  reserved u32 = 0
//!                                80  "data"
//!                                84  chunkSize u64 = 12 + n         <-
//!                                92  payload, n bytes
//! ```
//!
//! Both `chunkSize` fields count the chunk's own header, which is why the data chunk is
//! `12 + n` and not `n` — the single most common way to get this format subtly wrong.
//!
//! The payload is **planar in 4096-byte blocks, round-robin across channels**: ch0's first
//! block, ch1's first block, ch0's second block, and so on. Not interleaved samples.
//!
//! # Why the three fields are patched rather than computed
//!
//! Only `totalFileSize` strictly has to be: it is the one field a reader can check against
//! the filesystem, and WavPack's DSF reader *rejects the file outright* when it disagrees
//! with the on-disk size. Writing it after the fact is the only way to be sure. The other
//! two follow the same route because they come from the same counter, and because a header
//! written with zeros is honestly wrong if the process dies mid-file rather than plausibly
//! wrong (WavPack treats a `totalFileSize` of 0 as "unknown" and skips the check, so a
//! truncated file degrades into an unverified one instead of a corrupt one).
//!
//! `reserved` must be zero: WavPack and the Go reference reader both reject a non-zero
//! value. ffmpeg's leniency here is the outlier, not the rule.
//!
//! # The final block, and why `sampleCount` includes its padding
//!
//! Blocks are a fixed 4096 bytes per channel, so unless the stream happens to be a multiple
//! of 32 768 samples the last one has to be filled out. The spec says to fill it with
//! `0x00` (Annotation 3) and leave `sampleCount` at the true, unpadded value. mint does
//! neither, deliberately, and both deviations have the same cause: `0x00` is not silence.
//!
//! A DSD bit is +-1, so a zero byte is a constant -1 — full-scale negative DC. Measured
//! through ffmpeg's own decimation filter, splicing `0x00` onto a real tail peaks at 1.045
//! (i.e. it clips) and settles at -1.0; the same splice with [`DSD_SILENCE`] peaks at
//! 0.0100 and settles at exactly 0.0. That is ~74 dB, and it is not theoretical: the
//! sacd-ripper source carries the comment *"This does zero fill to make the last block size
//! 4096 bytes which leads to pop noise in some players"*, and a whole padding-less mode was
//! added to work around it.
//!
//! Whether the pop is *audible* then depends on `sampleCount`, and the decoders disagree
//! about that too:
//!
//! | reader | with an unpadded `sampleCount` |
//! |---|---|
//! | ffmpeg | trims byte-exactly; errors out if the last block is entirely padding |
//! | MPD | floors to whole blocks — **drops up to 11.6 ms of real audio** (DSD64) |
//! | WavPack | rounds *up* to a whole byte and warns |
//!
//! So mint pads with [`DSD_SILENCE`] and reports the **padded** count. Then there is no
//! "unused sample data area" for anyone to disagree about: every reader plays the whole
//! payload, byte-trimming and block-flooring produce the same answer, nothing is truncated,
//! and the tail is true digital silence rather than a DC step. The cost is that the file
//! runs up to 11.6 ms (DSD64) / 5.8 (DSD128) / 2.9 (DSD256) / 1.5 (DSD512) longer than the
//! source. For a distribution deliverable that is nothing; for gapless track sets it would
//! matter, which is the one reason to revisit this.
//!
//! Better still is to not need the pad at all: a caller that tops the stream up with
//! *modulated* silence gets the same whole-block payload with no splice discontinuity
//! whatsoever. [`DsfWriter::samples_to_block_boundary`] exists for exactly that, and the
//! DSD block loop is expected to use it.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::ops::dsd::DsdRate;
use crate::ops::dsd::pack::{BitOrder, BitPacker, DSD_SILENCE};

/// Bytes per channel in one DSF block. Fixed by the format; readers hard-check it.
pub const BLOCK_BYTES: usize = 4096;
/// One-bit samples per channel in one DSF block.
pub const BLOCK_SAMPLES: u64 = BLOCK_BYTES as u64 * 8;
/// `DSD ` (28) + `fmt ` (52) + the `data` chunk header (12).
pub const HEADER_BYTES: u64 = 92;
/// The channel-type table tops out at 5.1.
pub const MAX_CHANNELS: usize = 6;

// Absolute offsets of the fields `finish` patches.
const OFF_TOTAL_FILE_SIZE: u64 = 12;
const OFF_SAMPLE_COUNT: u64 = 64;
const OFF_DATA_CHUNK_SIZE: u64 = 84;

/// What a finished file actually contains — the numbers the processing report quotes and
/// the format tests assert against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DsfSummary {
    /// `sampleCount` as written: samples per channel including the final block's padding.
    pub samples_per_channel: u64,
    /// Samples per channel the caller actually supplied.
    pub audio_samples_per_channel: u64,
    /// Difference between the two — see the module docs on why it is counted as audio.
    pub padding_samples: u64,
    pub blocks: u64,
    /// Sample payload, i.e. `blocks * 4096 * channels`.
    pub payload_bytes: u64,
    /// Whole file, which is `92 + payload_bytes`.
    pub file_bytes: u64,
}

/// A DSF file being written. Feed it planar bit blocks with [`write`](Self::write), then
/// call [`finish`](Self::finish) exactly once.
pub struct DsfWriter {
    file: BufWriter<File>,
    path: PathBuf,
    channels: usize,
    rate: DsdRate,
    /// One packer per channel: the payload is planar, so the channels never share a byte.
    packers: Vec<BitPacker>,
    pad: u8,
    audio_samples: u64,
    blocks: u64,
}

impl std::fmt::Debug for DsfWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DsfWriter")
            .field("path", &self.path)
            .field("channels", &self.channels)
            .field("rate", &self.rate.tag())
            .field("audio_samples", &self.audio_samples)
            .field("blocks", &self.blocks)
            .finish_non_exhaustive()
    }
}

impl DsfWriter {
    /// Create the file and write a header with the three length-dependent fields zeroed.
    pub fn create(path: &Path, channels: usize, rate: DsdRate) -> Result<Self> {
        if channels == 0 || channels > MAX_CHANNELS {
            bail!("DSF supports 1 to {MAX_CHANNELS} channels, got {channels}");
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create output dsf: {}", path.display()))?;

        let mut writer = Self {
            file: BufWriter::new(file),
            path: path.to_path_buf(),
            channels,
            rate,
            // LSB first is not a choice here: it is what `bitsPerSample = 1` means.
            packers: vec![BitPacker::new(BitOrder::LsbFirst); channels],
            pad: DSD_SILENCE,
            audio_samples: 0,
            blocks: 0,
        };
        writer.write_header()?;
        Ok(writer)
    }

    /// Override the byte the final block is filled out with.
    ///
    /// Exists so a spec-conformant `0x00` file stays reachable — mint's default deviates
    /// from Annotation 3 for the measured reason in the module docs, and an escape hatch is
    /// cheaper than a rewrite if some player ever turns out to need the letter of the spec.
    /// Exercised by `the_pad_byte_is_a_parameter_so_a_spec_conformant_file_is_still_reachable`
    /// in `tests/dsd_format.rs`; production has no reason to call it.
    pub fn with_pad_byte(mut self, byte: u8) -> Self {
        self.pad = byte;
        self
    }

    /// Append one planar block: `channels[c]` is channel `c`'s next run of samples, `true`
    /// for the high level. All channels must advance by the same number of samples — the
    /// container has no way to express a ragged stream.
    ///
    /// Block sizes are free and need not line up with the 4096-byte container blocks; this
    /// is what lets the caller pass whatever [`crate::ops::dsd::interpolate::SoxrStream`]
    /// happened to have ready.
    pub fn write(&mut self, channels: &[Vec<bool>]) -> Result<()> {
        if channels.len() != self.channels {
            bail!(
                "DsfWriter was opened for {} channels, got {}",
                self.channels,
                channels.len()
            );
        }
        let samples = channels[0].len();
        if channels.iter().any(|c| c.len() != samples) {
            bail!("DsfWriter got a ragged block (channels differ in length)");
        }
        if samples == 0 {
            return Ok(());
        }

        for (packer, bits) in self.packers.iter_mut().zip(channels) {
            packer.push(bits);
        }
        self.audio_samples += samples as u64;
        self.flush_full_blocks()
    }

    /// Samples per channel still needed to land on a block boundary.
    ///
    /// A caller that keeps modulating silence until this reaches zero gets a file with no
    /// spliced pad pattern at all — the tail is genuine modulator output and the loop state
    /// runs through it continuously. That is strictly better than any fixed byte pattern
    /// and it is what the DSD block loop should do.
    pub fn samples_to_block_boundary(&self) -> u64 {
        match self.audio_samples % BLOCK_SAMPLES {
            0 => 0,
            r => BLOCK_SAMPLES - r,
        }
    }

    /// Samples per channel supplied so far.
    pub fn audio_samples(&self) -> u64 {
        self.audio_samples
    }

    /// Pad out the final block, then patch the header. Consumes the writer.
    pub fn finish(mut self) -> Result<DsfSummary> {
        // Complete a part-filled byte first, so the whole pad bytes that follow continue
        // the same pattern instead of restarting it mid-byte.
        for packer in &mut self.packers {
            packer.pad_partial(self.pad);
        }
        let short = self.packers[0].byte_len() % BLOCK_BYTES;
        if short != 0 {
            for packer in &mut self.packers {
                packer.push_pad_bytes(self.pad, BLOCK_BYTES - short);
            }
        }
        self.flush_full_blocks()?;
        debug_assert!(self.packers.iter().all(|p| p.byte_len() == 0));

        let samples_per_channel = self.blocks * BLOCK_SAMPLES;
        let payload_bytes = self.blocks * BLOCK_BYTES as u64 * self.channels as u64;
        let summary = DsfSummary {
            samples_per_channel,
            audio_samples_per_channel: self.audio_samples,
            padding_samples: samples_per_channel - self.audio_samples,
            blocks: self.blocks,
            payload_bytes,
            file_bytes: HEADER_BYTES + payload_bytes,
        };

        self.patch(OFF_TOTAL_FILE_SIZE, summary.file_bytes)?;
        self.patch(OFF_SAMPLE_COUNT, summary.samples_per_channel)?;
        self.patch(OFF_DATA_CHUNK_SIZE, 12 + summary.payload_bytes)?;
        self.file
            .flush()
            .with_context(|| format!("failed to flush dsf: {}", self.path.display()))?;

        Ok(summary)
    }

    /// Write out every block round both channels have complete, oldest first.
    fn flush_full_blocks(&mut self) -> Result<()> {
        let ready = self
            .packers
            .iter()
            .map(|p| p.byte_len() / BLOCK_BYTES)
            .min()
            .unwrap_or(0);
        if ready == 0 {
            return Ok(());
        }

        for b in 0..ready {
            let range = b * BLOCK_BYTES..(b + 1) * BLOCK_BYTES;
            for packer in &self.packers {
                self.file
                    .write_all(&packer.bytes()[range.clone()])
                    .with_context(|| {
                        format!("failed to write dsf block: {}", self.path.display())
                    })?;
            }
        }
        // One drain per channel rather than one per block: draining the front of a Vec is
        // linear, so per-block draining would make a long file quadratic.
        for packer in &mut self.packers {
            packer.consume(ready * BLOCK_BYTES);
        }
        self.blocks += ready as u64;
        Ok(())
    }

    fn patch(&mut self, offset: u64, value: u64) -> Result<()> {
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.write_all(&value.to_le_bytes()))
            .with_context(|| {
                format!(
                    "failed to patch dsf header at offset {offset}: {}",
                    self.path.display()
                )
            })
    }

    fn write_header(&mut self) -> Result<()> {
        let mut h = Vec::with_capacity(HEADER_BYTES as usize);

        // ── DSD chunk (28 bytes) ─────────────────────────────────────────────
        h.extend_from_slice(b"DSD "); // note the trailing space, not a NUL
        h.extend_from_slice(&28u64.to_le_bytes());
        h.extend_from_slice(&0u64.to_le_bytes()); // totalFileSize, patched
        h.extend_from_slice(&0u64.to_le_bytes()); // metadataPointer: no ID3v2

        // ── fmt chunk (52 bytes) ─────────────────────────────────────────────
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&52u64.to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes()); // formatVersion
        h.extend_from_slice(&0u32.to_le_bytes()); // formatID: 0 = DSD raw
        h.extend_from_slice(&channel_type(self.channels).to_le_bytes());
        h.extend_from_slice(&(self.channels as u32).to_le_bytes());
        // Only DSD64 and DSD128 are named in the spec; DSD256/512 are de-facto extensions
        // that every current reader accepts (ffmpeg does not even look at the value). The
        // "limited player support" caveat belongs in the dry-run, not in the writer.
        h.extend_from_slice(&self.rate.hz().to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes()); // bitsPerSample: 1 == LSB first
        h.extend_from_slice(&0u64.to_le_bytes()); // sampleCount, patched
        h.extend_from_slice(&(BLOCK_BYTES as u32).to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes()); // reserved: readers reject non-zero

        // ── data chunk header (12 bytes) ─────────────────────────────────────
        h.extend_from_slice(b"data");
        h.extend_from_slice(&0u64.to_le_bytes()); // chunkSize = 12 + payload, patched

        debug_assert_eq!(h.len() as u64, HEADER_BYTES);
        self.file
            .write_all(&h)
            .with_context(|| format!("failed to write dsf header: {}", self.path.display()))
    }
}

/// DSF channel-type id for a bare channel count.
///
/// The id fixes both the count *and* the channel order inside each block group, and mint
/// only ever knows the count — the input file's layout is not carried through the pipeline.
/// So this picks the spec's canonical layout for each count:
///
/// | channels | id | order |
/// |---|---|---|
/// | 1 | 1 | mono |
/// | 2 | 2 | FL, FR |
/// | 3 | 3 | FL, FR, C |
/// | 4 | 4 | FL, FR, BL, BR (quad) |
/// | 5 | 6 | FL, FR, C, BL, BR |
/// | 6 | 7 | FL, FR, C, LFE, BL, BR (5.1) |
///
/// Id 5 ("4 channels" = FL, FR, C, LFE) is therefore unreachable: at four channels quad is
/// the same choice WAV's default speaker mask makes, so a four-channel WAV round-trips with
/// its layout intact. A `channels` field on the target would be the way to reach id 5.
const fn channel_type(channels: usize) -> u32 {
    match channels {
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 4,
        5 => 6,
        _ => 7,
    }
}

use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};

use crate::audio::buffer::{AudioBuffer, OutputSampleFormat};

/// Write the buffer as a WAV file. When `bext` is `Some`, a Broadcast WAV extension
/// chunk (EBU Tech 3285) is spliced into the RIFF; otherwise a plain WAV is written.
///
/// The PCM is encoded by `hound` (battle-tested 16/24/32-bit little-endian packing);
/// the `bext` path just writes hound's output to memory and inserts one extra chunk
/// before handing the bytes to disk, so sample encoding is never reimplemented.
/// The one exception is f64: hound's Float support is 32-bit only, so that stream
/// is serialized by `encode_wav_f64` (f64 has no packing subtleties to delegate).
pub fn write_wav(path: &Path, buffer: &AudioBuffer, bext: Option<&BextMetadata>) -> Result<()> {
    if buffer.out_format == OutputSampleFormat::F64 {
        let wav = encode_wav_f64(buffer)?;
        let bytes = match bext {
            None => wav,
            Some(meta) => splice_chunk_after_wave(&wav, b"bext", &meta.to_bytes())
                .context("failed to splice bext chunk")?,
        };
        return std::fs::write(path, bytes)
            .with_context(|| format!("failed to write output wav: {}", path.display()));
    }

    let spec = wav_spec(buffer);

    match bext {
        // Fast path: stream straight to the file.
        None => {
            let mut writer = WavWriter::create(path, spec)
                .with_context(|| format!("failed to create output wav: {}", path.display()))?;
            write_samples(&mut writer, buffer)?;
            writer.finalize().context("failed to finalize wav writer")?;
        }
        // Splice path: render to memory, insert the bext chunk, then write.
        Some(meta) => {
            let mut cursor = Cursor::new(Vec::<u8>::new());
            {
                let mut writer = WavWriter::new(&mut cursor, spec)
                    .context("failed to create in-memory wav writer")?;
                write_samples(&mut writer, buffer)?;
                writer.finalize().context("failed to finalize wav writer")?;
            }
            let wav = cursor.into_inner();
            let spliced = splice_chunk_after_wave(&wav, b"bext", &meta.to_bytes())
                .context("failed to splice bext chunk")?;
            std::fs::write(path, spliced)
                .with_context(|| format!("failed to write output wav: {}", path.display()))?;
        }
    }
    Ok(())
}

fn wav_spec(buffer: &AudioBuffer) -> WavSpec {
    let format = buffer.out_format;
    WavSpec {
        channels: buffer.channels_count() as u16,
        sample_rate: buffer.sample_rate,
        bits_per_sample: format.bits_per_sample(),
        sample_format: match format {
            OutputSampleFormat::F32 => SampleFormat::Float,
            OutputSampleFormat::S16 | OutputSampleFormat::S24 => SampleFormat::Int,
            OutputSampleFormat::F64 => unreachable!("f64 is serialized by encode_wav_f64"),
        },
    }
}

/// Serialize a complete RIFF/WAVE stream of 64-bit IEEE float: a 16-byte `fmt `
/// chunk with format tag 3 (WAVE_FORMAT_IEEE_FLOAT), the `fact` chunk the WAV spec
/// requires for non-PCM formats (per-channel frame count), then interleaved
/// little-endian f64 data. No pad bytes ever needed: 8-byte samples keep every
/// chunk word-aligned.
fn encode_wav_f64(buffer: &AudioBuffer) -> Result<Vec<u8>> {
    let channels = buffer.channels_count() as u32;
    let frames = buffer.frame_len();
    let block_align = channels * 8;
    let data_len = frames as u64 * block_align as u64;
    // "WAVE" + fmt + fact + data headers and payload; RIFF sizes are u32 and f64
    // doubles the payload vs f32, so guard the ~4 GB container limit explicitly.
    let riff_size = 4 + (8 + 16) + (8 + 4) + (8 + data_len);
    if riff_size > u32::MAX as u64 {
        anyhow::bail!("f64 WAV would exceed the 4 GB RIFF size limit");
    }

    let mut out = Vec::with_capacity(8 + riff_size as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");

    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&buffer.sample_rate.to_le_bytes());
    out.extend_from_slice(&(buffer.sample_rate * block_align).to_le_bytes()); // byte rate
    out.extend_from_slice(&(block_align as u16).to_le_bytes());
    out.extend_from_slice(&64u16.to_le_bytes()); // bits per sample

    out.extend_from_slice(b"fact");
    out.extend_from_slice(&4u32.to_le_bytes());
    out.extend_from_slice(&(frames as u32).to_le_bytes()); // dwSampleLength

    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for i in 0..frames {
        for channel in &buffer.channels {
            out.extend_from_slice(&channel[i].to_le_bytes());
        }
    }
    Ok(out)
}

fn write_samples<W: std::io::Write + std::io::Seek>(
    writer: &mut WavWriter<W>,
    buffer: &AudioBuffer,
) -> Result<()> {
    let format = buffer.out_format;
    // Integer targets were already quantized in `ops::bitdepth`; here we only
    // materialize those grid points as PCM integers for the container writer.
    let int_grid = format.int_grid();
    let frames = buffer.frame_len();
    for i in 0..frames {
        for channel in &buffer.channels {
            let sample = channel[i];
            match format {
                OutputSampleFormat::F64 => unreachable!("f64 is serialized by encode_wav_f64"),
                OutputSampleFormat::F32 => writer.write_sample(sample as f32)?,
                OutputSampleFormat::S16 => {
                    let Some((max, min_i, max_i)) = int_grid else {
                        unreachable!("S16 must have an integer grid")
                    };
                    let scaled = (sample * max).round() as i32;
                    writer.write_sample(scaled.clamp(min_i, max_i) as i16)?;
                }
                OutputSampleFormat::S24 => {
                    let Some((max, min_i, max_i)) = int_grid else {
                        unreachable!("S24 must have an integer grid")
                    };
                    let scaled = (sample * max).round() as i32;
                    writer.write_sample(scaled.clamp(min_i, max_i))?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Broadcast WAV (bext) chunk — EBU Tech 3285 v2 with EBU R128 loudness metadata.
// ---------------------------------------------------------------------------

/// Broadcast WAV extension metadata. The loudness fields come straight from the
/// delivered-signal `analyze()` pass, so the `bext` chunk we write is a faithful EBU R128
/// loudness report — the part most tools make you fill in by hand.
pub struct BextMetadata {
    pub description: String,
    pub integrated_lufs: f64,
    pub loudness_range_lu: f64,
    pub max_true_peak_dbtp: f64,
    pub max_momentary_lufs: f64,
    pub max_short_term_lufs: f64,
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: u16,
}

impl BextMetadata {
    /// Serialize the bext chunk *data* (without the 8-byte `bext`/size header). Layout is
    /// the fixed 602-byte v2 structure followed by an ASCII coding-history string.
    fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(602);
        push_fixed_str(&mut v, &self.description, 256); // Description
        push_fixed_str(&mut v, "mint", 32); // Originator
        push_fixed_str(&mut v, "", 32); // OriginatorReference
        push_fixed_str(&mut v, "", 10); // OriginationDate (blank: no wall clock dep)
        push_fixed_str(&mut v, "", 8); // OriginationTime
        v.extend_from_slice(&0u32.to_le_bytes()); // TimeReference low
        v.extend_from_slice(&0u32.to_le_bytes()); // TimeReference high
        v.extend_from_slice(&2u16.to_le_bytes()); // Version 2 (enables loudness fields)
        v.extend_from_slice(&[0u8; 64]); // UMID
        v.extend_from_slice(&r128_field(self.integrated_lufs)); // LoudnessValue
        v.extend_from_slice(&r128_field(self.loudness_range_lu)); // LoudnessRange
        v.extend_from_slice(&r128_field(self.max_true_peak_dbtp)); // MaxTruePeakLevel
        v.extend_from_slice(&r128_field(self.max_momentary_lufs)); // MaxMomentaryLoudness
        v.extend_from_slice(&r128_field(self.max_short_term_lufs)); // MaxShortTermLoudness
        v.extend_from_slice(&[0u8; 180]); // Reserved
        debug_assert_eq!(v.len(), 602);

        v.extend_from_slice(coding_history(self.sample_rate, self.bits, self.channels).as_bytes());
        v
    }
}

/// CodingHistory (free ASCII; one line describing the PCM source). Split out so the size
/// prediction below can measure the real string instead of guessing at it.
fn coding_history(sample_rate: u32, bits: u16, channels: usize) -> String {
    format!(
        "A=PCM,F={sample_rate},W={bits},M={},T=mint\r\n",
        if channels >= 2 { "stereo" } else { "mono" },
    )
}

/// Encode one EBU R128 loudness field: value×100 as a signed 16-bit LE integer, or the
/// `0x7FFF` "not used" sentinel when the value is non-finite (e.g. silence).
fn r128_field(value: f64) -> [u8; 2] {
    let raw: i16 = if value.is_finite() {
        (value * 100.0).round().clamp(i16::MIN as f64, i16::MAX as f64) as i16
    } else {
        0x7FFF
    };
    raw.to_le_bytes()
}

/// Copy `s` (ASCII) into a fixed `len`-byte field, truncating or null-padding as needed.
fn push_fixed_str(out: &mut Vec<u8>, s: &str, len: usize) {
    let bytes = s.as_bytes();
    let n = bytes.len().min(len);
    out.extend_from_slice(&bytes[..n]);
    out.extend(std::iter::repeat_n(0u8, len - n));
}

/// Insert a chunk (`id` + size + `data`, word-aligned) right after the 12-byte
/// `RIFF<size>WAVE` header and fix up the top-level RIFF size. Chunk order in WAVE is
/// free, so a leading bext is valid and readers that don't know it just skip it.
fn splice_chunk_after_wave(wav: &[u8], id: &[u8; 4], data: &[u8]) -> Result<Vec<u8>> {
    if wav.len() < 12 || &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        anyhow::bail!("not a RIFF/WAVE stream");
    }

    let mut chunk = Vec::with_capacity(8 + data.len() + 1);
    chunk.extend_from_slice(id);
    chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
    chunk.extend_from_slice(data);
    if data.len() % 2 == 1 {
        chunk.push(0); // RIFF chunks are word-aligned (pad byte not counted in the size).
    }

    let old_riff_size = u32::from_le_bytes([wav[4], wav[5], wav[6], wav[7]]);
    let new_riff_size = old_riff_size
        .checked_add(chunk.len() as u32)
        .context("RIFF size overflow")?;

    let mut out = Vec::with_capacity(wav.len() + chunk.len());
    out.extend_from_slice(&wav[0..4]); // "RIFF"
    out.extend_from_slice(&new_riff_size.to_le_bytes());
    out.extend_from_slice(&wav[8..12]); // "WAVE"
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&wav[12..]);
    Ok(out)
}

// ---------------------------------------------------------------------------
// FLAC (pure-Rust flacenc) — lossless, integer (s16/s24) only.
// ---------------------------------------------------------------------------

/// Encode the (already quantized) buffer as FLAC at `bits_per_sample` (16 or 24).
pub fn write_flac(path: &Path, buffer: &AudioBuffer, bits_per_sample: usize) -> Result<()> {
    use anyhow::anyhow;
    use flacenc::bitsink::ByteSink;
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;
    use flacenc::source::MemSource;

    let channels = buffer.channels_count();
    let frames = buffer.frame_len();
    // The buffer holds quantized values (k / max); recover the integer grid points.
    let max = ((1i64 << (bits_per_sample - 1)) - 1) as f64;
    let mut interleaved = Vec::with_capacity(frames * channels);
    for i in 0..frames {
        for ch in &buffer.channels {
            interleaved.push((ch[i] * max).round() as i32);
        }
    }

    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| anyhow!("invalid flac encoder config: {e:?}"))?;
    let source =
        MemSource::from_samples(&interleaved, channels, bits_per_sample, buffer.sample_rate as usize);
    let mut stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| anyhow!("flac encode failed: {e:?}"))?;

    normalize_fixed_blocksize_streaminfo(&mut stream)?;

    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|_| anyhow!("flac bitstream serialization failed"))?;
    std::fs::write(path, sink.as_slice())
        .with_context(|| format!("failed to write flac: {}", path.display()))?;
    Ok(())
}

/// Make STREAMINFO declare the fixed-blocksize stream that `encode_with_fixed_block_size`
/// actually produced, by restoring `min_blocksize == max_blocksize`.
///
/// This works around a flacenc 0.5 bug rather than overriding the library: its
/// `encode_with_fixed_block_size` deliberately sets `set_block_sizes(block_size,
/// block_size)` up front — its comment says it does so "to follow the FLAC reference
/// encoder's behavior" — and then every `add_frame` runs `update_frame_info`, whose
/// running `min_block_size = min(frame, min_block_size)` quietly undoes it as soon as the
/// short *final* frame arrives. The declared minimum ends up at `total_samples %
/// block_size`: 1542 against a 4096 max for a 136 710-frame file. RFC 9639 §8.2 excludes
/// the last block from that minimum precisely so this cannot happen.
///
/// The mismatch is not cosmetic. `min != max` declares a *variable*-blocksize stream while
/// every frame carries a fixed-blocksize (frame-number) header, and Apple CoreAudio
/// rejects that combination outright — `ExtAudioFileRead` fails with −50, so the file will
/// not play on any Apple platform even though the bitstream decodes losslessly elsewhere.
///
/// The direction matters: it must be `min` that moves up to `max`, never `max` down to `min`.
/// `set_block_sizes(min, min)` would equally satisfy "min == max" and equally pass a naive
/// test, but it would declare 1000 where the frames are really 4096 — still unplayable on
/// Apple, and a four-fold under-allocation for any decoder that sizes its buffer from
/// `max_block_size`. `max` is the honest figure: `update_frame_info` only ever *raises* it
/// and no frame can exceed the fixed block size, so it stays exactly `config.block_size`
/// at any file length. A stream with no frames at all never had `min` lowered, so the
/// early-out takes it.
///
/// (`set_block_sizes` itself will not catch a mistake here — flacenc's `verify_block_size!`
/// bounds the value only from above, at `MAX_BLOCK_SIZE`.)
fn normalize_fixed_blocksize_streaminfo(stream: &mut flacenc::component::Stream) -> Result<()> {
    use anyhow::anyhow;

    let info = stream.stream_info();
    let (min, max) = (info.min_block_size(), info.max_block_size());
    if min == max {
        return Ok(());
    }
    stream
        .stream_info_mut()
        .set_block_sizes(max, max)
        .map_err(|e| anyhow!("flac streaminfo blocksize fixup failed: {e:?}"))
}

// ---------------------------------------------------------------------------
// MP3 (libmp3lame via mp3lame-encoder) — lossy; opt-in `mp3` feature.
// ---------------------------------------------------------------------------

/// Encode the full-precision buffer as CBR MP3 at `bitrate_kbps`. libmp3lame quantizes
/// internally, so we feed the f64 samples directly — no PCM dither/quantization first.
#[cfg(feature = "mp3")]
pub fn write_mp3(path: &Path, buffer: &AudioBuffer, bitrate_kbps: u32) -> Result<()> {
    use anyhow::{anyhow, bail};
    use mp3lame_encoder::{Builder, DualPcm, FlushNoGap, MonoPcm};

    let channels = buffer.channels_count();
    if channels == 0 || channels > 2 {
        bail!("MP3 supports mono or stereo only (got {channels} channels)");
    }

    let bitrate = map_bitrate(bitrate_kbps)?;
    let mut builder = Builder::new().ok_or_else(|| anyhow!("failed to create LAME builder"))?;
    builder.set_num_channels(channels as u8).map_err(|e| anyhow!("lame channels: {e}"))?;
    builder.set_sample_rate(buffer.sample_rate).map_err(|e| anyhow!("lame sample rate: {e}"))?;
    builder.set_brate(bitrate).map_err(|e| anyhow!("lame bitrate: {e}"))?;
    builder
        .set_quality(mp3lame_encoder::Quality::Best)
        .map_err(|e| anyhow!("lame quality: {e}"))?;
    let mut encoder = builder.build().map_err(|e| anyhow!("lame init: {e}"))?;

    let frames = buffer.frame_len();
    let mut out: Vec<u8> = Vec::with_capacity(mp3lame_encoder::max_required_buffer_size(frames) + 7200);
    out.reserve(mp3lame_encoder::max_required_buffer_size(frames));
    if channels == 1 {
        encoder
            .encode_to_vec(MonoPcm(&buffer.channels[0]), &mut out)
            .map_err(|e| anyhow!("mp3 encode: {e}"))?;
    } else {
        let input = DualPcm { left: &buffer.channels[0], right: &buffer.channels[1] };
        encoder.encode_to_vec(input, &mut out).map_err(|e| anyhow!("mp3 encode: {e}"))?;
    }
    out.reserve(7200);
    encoder
        .flush_to_vec::<FlushNoGap>(&mut out)
        .map_err(|e| anyhow!("mp3 flush: {e}"))?;

    std::fs::write(path, out).with_context(|| format!("failed to write mp3: {}", path.display()))?;
    Ok(())
}

/// Map a kbps value onto libmp3lame's bitrate enum (validated upstream in config).
#[cfg(feature = "mp3")]
fn map_bitrate(kbps: u32) -> Result<mp3lame_encoder::Bitrate> {
    use mp3lame_encoder::Bitrate::*;
    Ok(match kbps {
        8 => Kbps8,
        16 => Kbps16,
        24 => Kbps24,
        32 => Kbps32,
        40 => Kbps40,
        48 => Kbps48,
        64 => Kbps64,
        80 => Kbps80,
        96 => Kbps96,
        112 => Kbps112,
        128 => Kbps128,
        160 => Kbps160,
        192 => Kbps192,
        224 => Kbps224,
        256 => Kbps256,
        320 => Kbps320,
        other => anyhow::bail!("unsupported MP3 bitrate: {other} kbps"),
    })
}

/// Stub when built without the `mp3` feature: fail clearly instead of silently.
#[cfg(not(feature = "mp3"))]
pub fn write_mp3(_path: &Path, _buffer: &AudioBuffer, _bitrate_kbps: u32) -> Result<()> {
    anyhow::bail!("MP3 output requires building with `--features mp3`")
}

// ---------------------------------------------------------------------------
// Planned output size — what `--dry-run` prints before anything is written.
// ---------------------------------------------------------------------------
//
// Each function mirrors one writer above and lives next to it for the same reason
// `dsf_file_bytes` lives next to the DSF writer: a container change that does not pass
// through here turns the dry-run into a confident lie. The DSD counterpart is
// `io_write_dsd::dsf_file_bytes`; it is exact, these are not (see each doc comment).

/// Bytes `write_wav` will produce. Exact.
///
/// The header is the only subtle part, and it is fixed by what the writers do: hound picks
/// `PCMWAVEFORMAT` (44 bytes of header) for up to 2 channels at up to 16 bits and
/// `WAVEFORMATEXTENSIBLE` (68) otherwise, while `encode_wav_f64` hand-writes
/// `RIFF`+`fmt `+`fact`+`data` = 56.
pub fn wav_file_bytes(
    frames: u64,
    sample_rate: u32,
    channels: usize,
    format: OutputSampleFormat,
    bwf: bool,
) -> u64 {
    let bytes_per_sample = u64::from(format.bits_per_sample()) / 8;
    let header = match format {
        OutputSampleFormat::F64 => 56,
        _ if channels > 2 || format.bits_per_sample() > 16 => 68,
        _ => 44,
    };
    let bext = if bwf {
        // 8-byte chunk header + the 602-byte v2 structure + CodingHistory, word-aligned.
        // The history is measured, not guessed: its length follows the rate's digit count.
        let data = 602 + coding_history(sample_rate, format.bits_per_sample(), channels).len() as u64;
        8 + data + data % 2
    } else {
        0
    };
    header + bext + frames * channels as u64 * bytes_per_sample
}

/// Upper bound on what `write_flac` will produce — a lossless codec's real size depends on
/// the material, so this is the only honest number available before encoding.
///
/// FLAC stores a frame verbatim when nothing compresses, so a well-behaved encoder never
/// meaningfully exceeds the PCM payload; typical masters land at 50–70 % of it. On top of
/// the payload the bound allows 8 kB for STREAMINFO and the metadata blocks, plus 16 bytes
/// per 1024 frames for frame headers, subframe headers and CRCs. That second term is the
/// point: it is *linear*, and a fixed allowance was measured to run out after about a
/// minute of incompressible material (~11 bytes per 4096-frame block, i.e. ~128 B/s at
/// 48 kHz stereo), after which the "bound" was quietly no longer one.
///
/// Known exception, and it is a defect rather than a modelling gap: `flacenc` expands
/// *limited* full-scale high-entropy 24-bit material by ~14x instead of falling back to
/// verbatim frames (`docs/dsd-design.md` § 3.7). Ordinary program material and unlimited
/// noise both stay inside the bound.
pub fn flac_max_file_bytes(frames: u64, channels: usize, bits_per_sample: u16) -> u64 {
    let payload = frames * channels as u64 * (u64::from(bits_per_sample) / 8);
    8_192 + frames.div_ceil(1_024) * 16 + payload
}

/// Bytes `write_mp3` will produce, approximately: CBR makes this duration × bitrate, but
/// LAME emits a Xing/LAME info frame and the last frame is padded, so the real file lands a
/// few kB above.
pub fn mp3_file_bytes(frames: u64, sample_rate: u32, bitrate_kbps: u32) -> u64 {
    if sample_rate == 0 {
        return 0;
    }
    let seconds = frames as f64 / f64::from(sample_rate);
    let bitrate = effective_mp3_bitrate(sample_rate, bitrate_kbps);
    1_024 + (seconds * f64::from(bitrate) * 1000.0 / 8.0) as u64
}

/// The bitrate LAME will actually use, which is not always the one that was asked for.
///
/// `write_mp3` sets only the input rate, so LAME picks the MPEG version from it — and then
/// silently clamps the bitrate into that version's table. Below 32 kHz that is MPEG-2,
/// which stops at 160 kbps, and below 16 kHz MPEG-2.5, which stops at 64. A 22.05 kHz
/// target left at the 320 kbps default therefore delivers exactly half of what the
/// requested bitrate implies, with no error and no warning.
///
/// Note the per-file report still quotes the *requested* bitrate in its format tag; that
/// is a separate, older inaccuracy, and this function is what a fix for it would use.
pub fn effective_mp3_bitrate(sample_rate: u32, requested_kbps: u32) -> u32 {
    // LAME rounds the output rate up to the nearest one MPEG supports, so the version
    // boundaries sit at the top of each family: 12 kHz for MPEG-2.5, 24 kHz for MPEG-2.
    let ceiling = match sample_rate {
        0..=12_000 => 64,
        12_001..=24_000 => 160,
        _ => 320,
    };
    requested_kbps.min(ceiling)
}

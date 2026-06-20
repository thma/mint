use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};

use crate::audio::buffer::{AudioBuffer, OutputSampleFormat};

/// Write the buffer as a WAV file. When `bext` is `Some`, a Broadcast WAV extension
/// chunk (EBU Tech 3285) is spliced into the RIFF; otherwise a plain WAV is written.
///
/// The PCM is always encoded by `hound` (battle-tested 16/24/32-bit little-endian
/// packing); the `bext` path just writes hound's output to memory and inserts one extra
/// chunk before handing the bytes to disk, so sample encoding is never reimplemented.
pub fn write_wav(path: &Path, buffer: &AudioBuffer, bext: Option<&BextMetadata>) -> Result<()> {
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
        },
    }
}

fn write_samples<W: std::io::Write + std::io::Seek>(
    writer: &mut WavWriter<W>,
    buffer: &AudioBuffer,
) -> Result<()> {
    let format = buffer.out_format;
    let frames = buffer.frame_len();
    for i in 0..frames {
        for channel in &buffer.channels {
            let sample = channel[i];
            match format {
                OutputSampleFormat::F32 => writer.write_sample(sample as f32)?,
                OutputSampleFormat::S16 => {
                    let scaled = (sample.clamp(-1.0, 1.0) * i16::MAX as f64).round() as i32;
                    writer.write_sample(scaled.clamp(i16::MIN as i32, i16::MAX as i32) as i16)?;
                }
                OutputSampleFormat::S24 => {
                    let max = 8_388_607_f64;
                    let scaled = (sample.clamp(-1.0, 1.0) * max).round() as i32;
                    writer.write_sample(scaled.clamp(-8_388_608, max as i32))?;
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

        // CodingHistory (free ASCII; one line describing the PCM source).
        let history = format!(
            "A=PCM,F={},W={},M={},T=mint\r\n",
            self.sample_rate,
            self.bits,
            if self.channels >= 2 { "stereo" } else { "mono" },
        );
        v.extend_from_slice(history.as_bytes());
        v
    }
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
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| anyhow!("flac encode failed: {e:?}"))?;

    let mut sink = ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|_| anyhow!("flac bitstream serialization failed"))?;
    std::fs::write(path, sink.as_slice())
        .with_context(|| format!("failed to write flac: {}", path.display()))?;
    Ok(())
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

use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

use anyhow::{Context, Result, bail};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{
    CODEC_TYPE_NULL, CODEC_TYPE_PCM_F64BE, CODEC_TYPE_PCM_F64LE, DecoderOptions,
};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

use crate::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};

/// What the container header says, without decoding a single sample.
#[derive(Debug, Clone, Copy)]
pub struct SourceProbe {
    pub sample_rate: u32,
    /// Frames per channel, `None` when the container does not declare a usable length.
    pub frames: Option<u64>,
    pub channels: Option<usize>,
}

/// Probe the source header without decoding the audio.
///
/// Used by `--dry-run`, which needs the rate to show whether a resample step is needed and
/// the frame/channel counts to predict the output size. Only the rate is worth failing
/// over: a file whose length the header does not declare should still get its planned
/// chain printed, just without a size.
pub fn probe_source(path: &Path) -> Result<SourceProbe> {
    let mut file = File::open(path)
        .with_context(|| format!("failed to open input file: {}", path.display()))?;
    let file_bytes = file.metadata().ok().map(|m| m.len());

    // Which container this is gets decided by the magic, not by the extension: `cli.rs`
    // accepts .wav and .aiff for either content, and the SSND correction below has to
    // follow what is actually in the file. Four bytes, then rewind for the real probe.
    let mut magic = [0u8; 4];
    let is_aiff = file.read_exact(&mut magic).is_ok() && &magic == b"FORM";
    file.rewind()
        .with_context(|| format!("failed to rewind input file: {}", path.display()))?;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|x| x.to_str()) {
        hint.with_extension(ext);
    }

    let probed = get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("failed to probe audio format: {}", path.display()))?;

    let format = probed.format;
    let params = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .map(|t| &t.codec_params)
        .with_context(|| format!("no decodable audio track found: {}", path.display()))?;

    let sample_rate = params
        .sample_rate
        .with_context(|| format!("missing sample rate in stream metadata: {}", path.display()))?;
    let channels = params.channels.map(|c| c.count());
    // symphonia fills in `bits_per_sample` for every PCM format it decodes except 64-bit
    // float, where the codec type is the only place the width is stated. mint reads f64 WAV
    // as a first-class input (bit-exact passthrough), so that one gap is worth closing
    // rather than degrading around.
    let bits = params.bits_per_sample.or(match params.codec {
        CODEC_TYPE_PCM_F64LE | CODEC_TYPE_PCM_F64BE => Some(64),
        _ => None,
    });
    let block_align = match (channels, bits) {
        (Some(c), Some(bits)) if c > 0 && bits >= 8 => Some(c as u64 * u64::from(bits) / 8),
        _ => None,
    };

    let frames = params.n_frames.and_then(|declared| {
        // symphonia derives n_frames from the sample-chunk length. For AIFF that length
        // still includes the 8 bytes of `offset`/`blockSize` the SSND reader already
        // consumed, so the count comes out floor(8 / block_align) frames high — 2 frames
        // for 16-bit stereo, 4 for 16-bit mono. WAV has no such preamble and is exact.
        //
        // Without a frame width the correction cannot be made, and an uncorrected count
        // would be printed as an *exact* size. Report nothing instead.
        let corrected = if is_aiff {
            declared.checked_sub(8 / block_align?)?
        } else {
            declared
        };
        // A WAV streamed to a pipe carries a data-chunk length of 0xFFFFFFFF, which
        // symphonia passes through: that alone would turn a normal file into a
        // multi-terabyte "estimate". The file on disk is the ceiling. When the frame width
        // is unknown, one byte per channel is still a valid lower bound on a frame — a
        // loose cap, but the point is only to keep an absurd number off the screen.
        let frame_floor = block_align
            .or(channels.map(|c| c as u64))
            .unwrap_or(1)
            .max(1);
        Some(match file_bytes {
            Some(bytes) => corrected.min(bytes / frame_floor),
            None => corrected,
        })
    });

    Ok(SourceProbe {
        sample_rate,
        frames,
        channels,
    })
}

pub fn read_audio(path: &Path) -> Result<AudioBuffer> {
    let file = File::open(path)
        .with_context(|| format!("failed to open input file: {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|x| x.to_str()) {
        hint.with_extension(ext);
    }

    let probed = get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .with_context(|| format!("failed to probe audio format: {}", path.display()))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .context("no decodable audio track found")?;

    let track_id = track.id;
    let mut decoder = get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("failed to create decoder")?;

    let sample_rate = track
        .codec_params
        .sample_rate
        .context("missing sample rate in stream metadata")?;
    // Total frames if the container declares it; used to pre-size the per-channel
    // buffers so a long decode doesn't repeatedly reallocate (the f64 buffer doubles
    // the per-regrow memcpy cost vs the old f32 path). Unknown ⇒ 0 ⇒ no pre-alloc.
    let frame_capacity = track.codec_params.n_frames.unwrap_or(0) as usize;

    let mut channels_data: Vec<Vec<f64>> = Vec::new();
    let mut channels_count = 0usize;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(err))
                if err.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(err) => return Err(err).context("failed to read packet"),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder.decode(&packet).context("failed to decode packet")?;
        let (buffer, spec_channels) = to_f64_interleaved(decoded)?;

        if channels_count == 0 {
            channels_count = spec_channels;
            channels_data =
                (0..channels_count).map(|_| Vec::with_capacity(frame_capacity)).collect();
        }

        for frame in buffer.chunks(channels_count) {
            for (ch, sample) in frame.iter().enumerate() {
                channels_data[ch].push(*sample);
            }
        }
    }

    if channels_data.is_empty() {
        bail!("decoded stream has no audio samples");
    }

    Ok(AudioBuffer {
        channels: channels_data,
        sample_rate,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: channels_count,
            sample_rate,
            codec: "decoded".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    })
}

/// Decode one packet's worth of samples to interleaved f64. Keeping the canonical
/// buffer in f64 from the input boundary means full-precision sources (64-/32-bit
/// float, 32-bit int) pass through losslessly instead of being truncated to f32.
fn to_f64_interleaved(decoded: AudioBufferRef<'_>) -> Result<(Vec<f64>, usize)> {
    let spec = *decoded.spec();
    let channels_count = spec.channels.count();

    let mut sample_buffer = SampleBuffer::<f64>::new(decoded.capacity() as u64, spec);
    sample_buffer.copy_interleaved_ref(decoded);

    Ok((sample_buffer.samples().to_vec(), channels_count))
}

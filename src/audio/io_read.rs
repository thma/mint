use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer};
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};

use crate::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};

/// Probe just the source sample rate without decoding the audio.
///
/// Used by `--dry-run` so the planned chain can show whether a resample step is
/// actually needed for a given input, without paying for a full decode.
pub fn probe_source_rate(path: &Path) -> Result<u32> {
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

    probed
        .format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .and_then(|t| t.codec_params.sample_rate)
        .with_context(|| format!("missing sample rate in stream metadata: {}", path.display()))
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

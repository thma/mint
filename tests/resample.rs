use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::ResampleQuality;
use mint::ops::resample;

fn sine_buffer(rate: u32, frames: usize, hz: f64) -> AudioBuffer {
    let mut ch = Vec::with_capacity(frames);
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        ch.push((2.0 * std::f64::consts::PI * hz * t).sin() * 0.5);
    }

    AudioBuffer {
        channels: vec![ch],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("tone.wav"),
            channels: 1,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

#[test]
fn output_length_matches_theoretical_ratio() {
    let mut buffer = sine_buffer(48_000, 48_000, 997.0);
    resample::apply(&mut buffer, 44_100, Some(ResampleQuality::Hq)).expect("resample should succeed");

    let expected = (48_000_f64 * (44_100_f64 / 48_000_f64)).round() as usize;
    assert_eq!(buffer.frame_len(), expected);
    assert_eq!(buffer.sample_rate, 44_100);
}

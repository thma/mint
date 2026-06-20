use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::DitherMode;
use mint::ops::bitdepth;

fn mk_buffer(samples: &[f64]) -> AudioBuffer {
    AudioBuffer {
        channels: vec![samples.to_vec()],
        sample_rate: 48_000,
        src_info: SourceInfo {
            path: PathBuf::from("test.wav"),
            channels: 1,
            sample_rate: 48_000,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

#[test]
fn reduction_without_dither_is_rejected() {
    let mut buffer = mk_buffer(&[0.0, 0.2, -0.2]);
    let mut current = OutputSampleFormat::F32;

    let err = bitdepth::apply(
        &mut buffer,
        &mut current,
        OutputSampleFormat::S16,
        Some(DitherMode::None),
        Some(1),
    )
    .expect_err("reduction with dither none must fail");

    assert!(err.to_string().contains("not allowed"));
}

#[test]
fn seeded_tpdf_is_reproducible() {
    let mut a = mk_buffer(&[0.1, -0.1, 0.1234, -0.9876]);
    let mut b = mk_buffer(&[0.1, -0.1, 0.1234, -0.9876]);

    let mut current_a = OutputSampleFormat::F32;
    let mut current_b = OutputSampleFormat::F32;

    bitdepth::apply(
        &mut a,
        &mut current_a,
        OutputSampleFormat::S16,
        Some(DitherMode::Tpdf),
        Some(42),
    )
    .expect("apply should succeed");

    bitdepth::apply(
        &mut b,
        &mut current_b,
        OutputSampleFormat::S16,
        Some(DitherMode::Tpdf),
        Some(42),
    )
    .expect("apply should succeed");

    assert_eq!(a.channels, b.channels);
}

#[test]
fn quantized_samples_snap_to_integer_grid() {
    let mut buffer = mk_buffer(&[0.5, -0.5, 0.999999, -0.999999]);
    let mut current = OutputSampleFormat::F32;

    bitdepth::apply(
        &mut buffer,
        &mut current,
        OutputSampleFormat::S16,
        Some(DitherMode::Tpdf),
        Some(7),
    )
    .expect("apply should succeed");

    let max = 32_767.0;
    for sample in &buffer.channels[0] {
        let scaled = sample * max;
        let rounded = scaled.round();
        assert!((scaled - rounded).abs() < 1e-9, "sample not on integer grid");
        assert!(*sample <= 1.0 && *sample >= -1.0);
    }
}

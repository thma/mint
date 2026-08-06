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
fn reduction_without_dither_is_allowed_for_s16() {
    let mut buffer = mk_buffer(&[0.0, 0.2, -0.2]);
    let mut current = OutputSampleFormat::F32;

    let result = bitdepth::apply(
        &mut buffer,
        &mut current,
        OutputSampleFormat::S16,
        Some(DitherMode::None),
        None,
        None,
        Some(1),
    )
    .expect("reduction with dither none should succeed");

    assert!(result.reduced);
    assert!(!result.dithered);
    assert!(!result.shaped);
    assert_eq!(current, OutputSampleFormat::S16);
}

#[test]
fn reduction_without_dither_is_allowed_for_s24() {
    let mut buffer = mk_buffer(&[0.0, 0.2, -0.2]);
    let mut current = OutputSampleFormat::F32;

    let result = bitdepth::apply(
        &mut buffer,
        &mut current,
        OutputSampleFormat::S24,
        Some(DitherMode::None),
        None,
        None,
        Some(1),
    )
    .expect("reduction to s24 with dither none should succeed");

    assert!(result.reduced);
    assert!(!result.dithered);
    assert!(!result.shaped);
    assert_eq!(current, OutputSampleFormat::S24);
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
        None,
        None,
        Some(42),
    )
    .expect("apply should succeed");

    bitdepth::apply(
        &mut b,
        &mut current_b,
        OutputSampleFormat::S16,
        Some(DitherMode::Tpdf),
        None,
        None,
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
        None,
        None,
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

/// A tone long enough for several STFT analysis frames, at an arbitrary rate.
fn tone_at(rate: u32, freq_hz: f64, frames: usize) -> AudioBuffer {
    let samples: Vec<f64> = (0..frames)
        .map(|i| 0.5 * (2.0 * std::f64::consts::PI * freq_hz * i as f64 / rate as f64).sin())
        .collect();
    AudioBuffer {
        channels: vec![samples],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("test.wav"),
            channels: 1,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

/// Regression: `dither = "adaptive"` wrote a digitally silent file at 176.4/192 kHz.
///
/// Terhardt's `1e-3·f⁴` ATH term overflowed to `+inf` above ~50 kHz, so every masking
/// threshold went non-finite; `design_minimal_phase_fir` then computed `inf − inf` for
/// each bin and produced `NaN` taps, which propagated through the error-feedback loop
/// and quantized to zero. No error was raised and the report still claimed adaptive
/// shaping had run — the failure was silent in both senses.
/// `config.rs` only requires `rate > 0`, so the guard has to hold above 192 kHz too.
#[test]
fn adaptive_dither_is_not_silent_at_ultrasonic_rates() {
    for rate in [44_100, 96_000, 176_400, 192_000, 352_800, 384_000] {
        let mut buffer = tone_at(rate, 997.0, 8192);
        let mut current = OutputSampleFormat::F32;

        let result = bitdepth::apply(
            &mut buffer,
            &mut current,
            OutputSampleFormat::S16,
            Some(DitherMode::Adaptive),
            None,
            None,
            Some(1),
        )
        .expect("adaptive quantization should succeed");
        assert_eq!(result.shaping_tag, Some("adaptive"), "adaptive path at {rate} Hz");

        let out = &buffer.channels[0];
        assert!(
            out.iter().all(|v| v.is_finite()),
            "NaN/inf in adaptive output at {rate} Hz",
        );
        let peak = out.iter().fold(0.0f64, |m, v| m.max(v.abs()));
        // The input peaks at 0.5; anything near that means real audio survived. The bug
        // produced an exact 0.0, so the bound only has to exclude digital silence.
        assert!(
            peak > 0.4,
            "adaptive dither at {rate} Hz produced near-silence (peak {peak:.6})",
        );
    }
}

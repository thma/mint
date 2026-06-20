use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::ops::limiter;

fn make_buffer(channels: Vec<Vec<f64>>, rate: u32) -> AudioBuffer {
    let ch_count = channels.len();
    AudioBuffer {
        channels,
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("test.wav"),
            channels: ch_count,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

fn sine(rate: u32, seconds: f64, hz: f64, amp: f64) -> Vec<f64> {
    let frames = (rate as f64 * seconds) as usize;
    (0..frames)
        .map(|n| {
            let t = n as f64 / rate as f64;
            (2.0 * std::f64::consts::PI * hz * t).sin() * amp
        })
        .collect()
}

#[test]
fn sample_peaks_stay_at_or_below_ceiling() {
    // Full-scale sine boosted 6 dB: sample peaks ≈ 2.0, well above -1 dBTP ceiling.
    let mut buffer = make_buffer(vec![sine(48_000, 2.0, 997.0, 2.0)], 48_000);

    let ceiling_dbtp = -1.0;
    limiter::apply_true_peak_limit(&mut buffer, ceiling_dbtp).expect("limiter should not fail");

    // Linear ceiling: allow a tiny floating-point margin.
    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let max_sample = buffer.channels[0]
        .iter()
        .map(|s| s.abs())
        .fold(0.0_f64, f64::max);

    assert!(
        max_sample <= ceiling_lin + 1e-6,
        "max sample {max_sample:.6} exceeds ceiling {ceiling_lin:.6}"
    );
}

#[test]
fn silence_is_unaffected_by_limiter() {
    let mut buffer = make_buffer(vec![vec![0.0; 4_800]], 48_000);
    limiter::apply_true_peak_limit(&mut buffer, -1.0).expect("limiter on silence should succeed");

    for &s in &buffer.channels[0] {
        assert_eq!(s, 0.0, "silence must remain silence after limiting");
    }
}

#[test]
fn signal_below_ceiling_is_unchanged() {
    // -3 dBTP amplitude — must pass through the limiter without any gain change.
    let amp = 10_f64.powf(-3.0 / 20.0);
    let samples = sine(48_000, 1.0, 997.0, amp);
    let original = samples.clone();
    let mut buffer = make_buffer(vec![samples], 48_000);

    limiter::apply_true_peak_limit(&mut buffer, -1.0).expect("limiter should not fail");

    for (orig, processed) in original.iter().zip(buffer.channels[0].iter()) {
        // Gain stays exactly 1.0 → multiplication is exact in IEEE 754.
        assert!(
            (orig - processed).abs() < 1e-12,
            "below-ceiling sample was modified: orig={orig}, processed={processed}"
        );
    }
}

#[test]
fn stereo_channels_receive_the_same_gain_reduction() {
    // Left channel at full blast, right channel silent.
    // Both must be reduced by the same envelope to preserve stereo balance.
    let loud = sine(48_000, 1.0, 997.0, 2.0);
    let silent = vec![0.0_f64; loud.len()];
    let mut buffer = make_buffer(vec![loud, silent], 48_000);

    let ceiling_dbtp = -1.0;
    limiter::apply_true_peak_limit(&mut buffer, ceiling_dbtp).expect("limiter should not fail");

    // Loud channel: peaks must be at or below ceiling.
    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let max_loud = buffer.channels[0]
        .iter()
        .map(|s| s.abs())
        .fold(0.0_f64, f64::max);
    assert!(max_loud <= ceiling_lin + 1e-6);

    // Silent channel: must remain silent (same gain applied, 0 * anything = 0).
    for &s in &buffer.channels[1] {
        assert_eq!(s, 0.0, "silent channel must stay silent");
    }
}

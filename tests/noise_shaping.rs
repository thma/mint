use std::f64::consts::PI;
use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::DitherMode;
use mint::ops::bitdepth;

const SR: u32 = 44_100;

fn buffer_from(channels: Vec<Vec<f64>>) -> AudioBuffer {
    let channel_count = channels.len();
    AudioBuffer {
        channels,
        sample_rate: SR,
        src_info: SourceInfo {
            path: PathBuf::from("test.wav"),
            channels: channel_count,
            sample_rate: SR,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

fn mk_buffer(samples: Vec<f64>) -> AudioBuffer {
    buffer_from(vec![samples])
}

/// A 997 Hz sine (the BS.1770 reference tone) — smooth and fractional, so it
/// exercises the quantizer at every phase while staying well clear of full scale.
fn test_signal(n: usize, amp: f64) -> Vec<f64> {
    (0..n)
        .map(|i| amp * (2.0 * PI * 997.0 * i as f64 / SR as f64).sin())
        .collect()
}

fn quantize(signal: &[f64], mode: DitherMode, target: OutputSampleFormat, seed: u64) -> Vec<f64> {
    let mut buf = mk_buffer(signal.to_vec());
    let mut current = OutputSampleFormat::F32;
    bitdepth::apply(&mut buf, &mut current, target, Some(mode), Some(seed)).expect("apply");
    buf.channels.into_iter().next().unwrap()
}

/// The added noise (quantized − original) in LSB units. For both paths this equals
/// the output deviation from the desired code value; for the shaper specifically it
/// is `NTF(z) * e`, i.e. the shaped error, which is exactly what we want to inspect.
fn error_lsb(original: &[f64], quantized: &[f64], max: f64) -> Vec<f64> {
    original
        .iter()
        .zip(quantized)
        .map(|(o, q)| (*q - *o) * max)
        .collect()
}

/// Normalized lag-1 autocorrelation. White noise → ~0; a high-pass-shaped sequence
/// → strongly negative (successive samples anti-correlated).
fn lag1_autocorr(x: &[f64]) -> f64 {
    let energy: f64 = x.iter().map(|v| v * v).sum();
    let cross: f64 = x.windows(2).map(|w| w[0] * w[1]).sum();
    cross / energy
}

/// Energy left after a length-`l` moving-average low-pass — a no-FFT proxy for the
/// low-frequency (ear's most-sensitive band) content of `x`.
fn lowpass_energy(x: &[f64], l: usize) -> f64 {
    let mut sum = 0.0;
    let mut energy = 0.0;
    for i in 0..x.len() {
        sum += x[i];
        if i >= l {
            sum -= x[i - l];
        }
        if i >= l - 1 {
            let avg = sum / l as f64;
            energy += avg * avg;
        }
    }
    energy
}

#[test]
fn shaped_flag_tracks_target_format() {
    let probe = || mk_buffer(vec![0.1, -0.1, 0.2, -0.3]);

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S16, Some(DitherMode::Shaped), Some(1)).unwrap();
    assert!(r.shaped && r.dithered, "shaping must engage at s16");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S24, Some(DitherMode::Shaped), Some(1)).unwrap();
    assert!(!r.shaped && r.dithered, "shaped must degrade to flat tpdf at s24");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::F32, Some(DitherMode::Shaped), Some(1)).unwrap();
    assert!(!r.shaped && !r.dithered, "f32 has no quantization to dither or shape");
}

#[test]
fn shaped_is_reproducible_with_seed() {
    let sig = test_signal(4096, 0.5);
    let a = quantize(&sig, DitherMode::Shaped, OutputSampleFormat::S16, 42);
    let b = quantize(&sig, DitherMode::Shaped, OutputSampleFormat::S16, 42);
    assert_eq!(a, b, "seeded shaped output must be deterministic");
}

#[test]
fn shaped_degrades_to_flat_tpdf_at_s24() {
    // shaping is gated off above s16, so both modes must run the identical flat path.
    let sig = test_signal(4096, 0.5);
    let shaped = quantize(&sig, DitherMode::Shaped, OutputSampleFormat::S24, 99);
    let tpdf = quantize(&sig, DitherMode::Tpdf, OutputSampleFormat::S24, 99);
    assert_eq!(shaped, tpdf, "shaped must be byte-identical to flat tpdf at s24");
}

#[test]
fn shaped_stereo_is_reproducible_and_per_channel() {
    let sig = test_signal(4096, 0.5);

    let run = |seed: u64| {
        let mut buf = buffer_from(vec![sig.clone(), sig.clone()]);
        let mut current = OutputSampleFormat::F32;
        bitdepth::apply(&mut buf, &mut current, OutputSampleFormat::S16, Some(DitherMode::Shaped), Some(seed)).unwrap();
        buf.channels
    };

    // Reproducible: same seed -> identical stereo output.
    assert_eq!(run(7), run(7), "seeded stereo shaping must be deterministic");

    // Per-channel: identical input in both channels still yields decorrelated noise,
    // because each channel runs its own shaper with a distinct sub-seed.
    let ch = run(7);
    assert_ne!(ch[0], ch[1], "L/R must get independent (decorrelated) shaped noise");

    // Channel 0's sub-seed is (seed ^ 0) == seed, so it must match the mono path.
    let mono = quantize(&sig, DitherMode::Shaped, OutputSampleFormat::S16, 7);
    assert_eq!(ch[0], mono, "channel 0 must match the mono shaper for the same seed");
}

#[test]
fn shaping_tilts_noise_to_high_frequencies() {
    let n = 1 << 16;
    let sig = test_signal(n, 0.5);
    let max = 32_767.0;

    let e_tpdf = error_lsb(&sig, &quantize(&sig, DitherMode::Tpdf, OutputSampleFormat::S16, 12345), max);
    let e_shaped = error_lsb(&sig, &quantize(&sig, DitherMode::Shaped, OutputSampleFormat::S16, 12345), max);

    // 1. Spectral character. Flat TPDF noise is white (lag-1 ≈ 0); the (1 − z⁻¹)²
    //    shaper produces high-pass noise that is strongly anti-correlated
    //    sample-to-sample (theory: r1 ≈ −2/3).
    let r1_tpdf = lag1_autocorr(&e_tpdf);
    let r1_shaped = lag1_autocorr(&e_shaped);
    assert!(r1_tpdf.abs() < 0.1, "flat TPDF noise should be ~white, got r1={r1_tpdf:.3}");
    assert!(r1_shaped < -0.4, "shaped noise should be high-pass (r1 ≪ 0), got r1={r1_shaped:.3}");

    // 2. The actual win: substantially less noise energy in the low/sensitive band.
    let lp_tpdf = lowpass_energy(&e_tpdf, 32);
    let lp_shaped = lowpass_energy(&e_shaped, 32);
    assert!(
        lp_shaped < 0.5 * lp_tpdf,
        "shaping should cut in-band noise: shaped={lp_shaped:.1}, tpdf={lp_tpdf:.1}"
    );

    // 3. The trade-off: total noise power rises (it is moved, not removed).
    let p_tpdf: f64 = e_tpdf.iter().map(|v| v * v).sum();
    let p_shaped: f64 = e_shaped.iter().map(|v| v * v).sum();
    assert!(p_shaped > p_tpdf, "shaped total noise power should exceed flat TPDF");
}

#[test]
fn psychoacoustic_flag_tracks_target_format() {
    let probe = || mk_buffer(vec![0.1, -0.1, 0.2, -0.3]);

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S16, Some(DitherMode::Psychoacoustic), Some(1)).unwrap();
    assert!(r.shaped && r.dithered, "psychoacoustic shaping must engage at s16");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S24, Some(DitherMode::Psychoacoustic), Some(1)).unwrap();
    assert!(!r.shaped && r.dithered, "psychoacoustic must degrade to flat tpdf at s24");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::F32, Some(DitherMode::Psychoacoustic), Some(1)).unwrap();
    assert!(!r.shaped && !r.dithered, "f32 has no quantization to dither or shape");
}

#[test]
fn psychoacoustic_is_reproducible_with_seed() {
    let sig = test_signal(4096, 0.5);
    let a = quantize(&sig, DitherMode::Psychoacoustic, OutputSampleFormat::S16, 42);
    let b = quantize(&sig, DitherMode::Psychoacoustic, OutputSampleFormat::S16, 42);
    assert_eq!(a, b, "seeded psychoacoustic output must be deterministic");
}

#[test]
fn psychoacoustic_degrades_to_flat_tpdf_at_s24() {
    let sig = test_signal(4096, 0.5);
    let psy = quantize(&sig, DitherMode::Psychoacoustic, OutputSampleFormat::S24, 99);
    let tpdf = quantize(&sig, DitherMode::Tpdf, OutputSampleFormat::S24, 99);
    assert_eq!(psy, tpdf, "psychoacoustic must be byte-identical to flat tpdf at s24");
}

/// The whole point of the new curve: it's a *stronger*, ear-weighted shaper than the
/// gentle `(1 - z⁻¹)²`. These are no-FFT proxies (the deep ~4 kHz notch needs an FFT
/// to see directly), but they pin the structural differences predicted by theory.
#[test]
fn psychoacoustic_is_stronger_than_gentle() {
    let n = 1 << 16;
    let sig = test_signal(n, 0.5);
    let max = 32_767.0;

    let e_tpdf = error_lsb(&sig, &quantize(&sig, DitherMode::Tpdf, OutputSampleFormat::S16, 2024), max);
    let e_gentle = error_lsb(&sig, &quantize(&sig, DitherMode::Shaped, OutputSampleFormat::S16, 2024), max);
    let e_psy = error_lsb(&sig, &quantize(&sig, DitherMode::Psychoacoustic, OutputSampleFormat::S16, 2024), max);

    // 1. More strongly high-pass than gentle (theory: lag-1 ≈ −0.89 vs −0.67).
    let r_gentle = lag1_autocorr(&e_gentle);
    let r_psy = lag1_autocorr(&e_psy);
    assert!(r_psy < r_gentle, "psychoacoustic should be more high-pass than gentle: psy={r_psy:.3}, gentle={r_gentle:.3}");
    assert!(r_psy < -0.5, "psychoacoustic lag-1 should be strongly negative, got {r_psy:.3}");

    // 2. Still cuts the ear-sensitive low band hard vs flat TPDF (NTF ≈ −16 dB at DC).
    assert!(
        lowpass_energy(&e_psy, 32) < 0.5 * lowpass_energy(&e_tpdf, 32),
        "psychoacoustic should cut in-band noise vs flat TPDF"
    );

    // 3. The cost: it dumps MORE total noise than gentle (theory: +12.2 vs +7.8 dB),
    //    just placed where the ear can't hear it.
    let p_gentle: f64 = e_gentle.iter().map(|v| v * v).sum();
    let p_psy: f64 = e_psy.iter().map(|v| v * v).sum();
    assert!(p_psy > p_gentle, "psychoacoustic moves more total noise than gentle: psy={p_psy:.0}, gentle={p_gentle:.0}");
}

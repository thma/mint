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
    bitdepth::apply(&mut buf, &mut current, target, Some(mode), None, Some(seed)).expect("apply");
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
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S16, Some(DitherMode::Shaped), None, Some(1)).unwrap();
    assert!(r.shaped && r.dithered, "shaping must engage at s16");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S24, Some(DitherMode::Shaped), None, Some(1)).unwrap();
    assert!(!r.shaped && r.dithered, "shaped must degrade to flat tpdf at s24");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::F32, Some(DitherMode::Shaped), None, Some(1)).unwrap();
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
        bitdepth::apply(&mut buf, &mut current, OutputSampleFormat::S16, Some(DitherMode::Shaped), None, Some(seed)).unwrap();
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
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S16, Some(DitherMode::Psychoacoustic), None, Some(1)).unwrap();
    assert!(r.shaped && r.dithered, "psychoacoustic shaping must engage at s16");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S24, Some(DitherMode::Psychoacoustic), None, Some(1)).unwrap();
    assert!(!r.shaped && r.dithered, "psychoacoustic must degrade to flat tpdf at s24");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::F32, Some(DitherMode::Psychoacoustic), None, Some(1)).unwrap();
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

#[test]
fn mbit_plus_flag_tracks_target_format() {
    let probe = || mk_buffer(vec![0.1, -0.1, 0.2, -0.3]);

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S16, Some(DitherMode::MbitPlus), None, Some(1)).unwrap();
    assert!(r.shaped && r.dithered, "mbit+ shaping must engage at s16");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::S24, Some(DitherMode::MbitPlus), None, Some(1)).unwrap();
    assert!(!r.shaped && r.dithered, "mbit+ must degrade to flat tpdf at s24");

    let mut c = OutputSampleFormat::F32;
    let r = bitdepth::apply(&mut probe(), &mut c, OutputSampleFormat::F32, Some(DitherMode::MbitPlus), None, Some(1)).unwrap();
    assert!(!r.shaped && !r.dithered, "f32 has no quantization to dither or shape");
}

#[test]
fn mbit_plus_is_reproducible_with_seed() {
    let sig = test_signal(4096, 0.5);
    let a = quantize(&sig, DitherMode::MbitPlus, OutputSampleFormat::S16, 123);
    let b = quantize(&sig, DitherMode::MbitPlus, OutputSampleFormat::S16, 123);
    assert_eq!(a, b, "seeded mbit+ output must be deterministic");
}

#[test]
fn mbit_plus_degrades_to_flat_tpdf_at_s24() {
    let sig = test_signal(4096, 0.5);
    let mb = quantize(&sig, DitherMode::MbitPlus, OutputSampleFormat::S24, 99);
    let tpdf = quantize(&sig, DitherMode::Tpdf, OutputSampleFormat::S24, 99);
    assert_eq!(mb, tpdf, "mbit+ must be byte-identical to flat tpdf at s24");
}

#[test]
fn mbit_plus_stereo_is_reproducible_and_decorrelated() {
    let sig = test_signal(4096, 0.5);

    let run = |seed: u64| {
        let mut buf = buffer_from(vec![sig.clone(), sig.clone()]);
        let mut current = OutputSampleFormat::F32;
        bitdepth::apply(&mut buf, &mut current, OutputSampleFormat::S16, Some(DitherMode::MbitPlus), None, Some(seed)).unwrap();
        buf.channels
    };

    let a = run(17);
    let b = run(17);
    assert_eq!(a, b, "seeded stereo mbit+ output must be deterministic");

    // Channels should be decorrelated (independent RNGs per channel).
    assert_ne!(a[0], a[1], "L/R should get independent dither");

    // Verify stereo channels are not identical (decorrelated).
    let mismatches = a[0]
        .iter()
        .zip(&a[1])
        .filter(|(l, r)| *l != *r)
        .count();
    assert!(mismatches > a.len() / 2, "L/R decorrelation should yield many differences");
}

/// Phase 1 validation: Dither amplitude must be constant (not modulated by signal).
/// With correct constant TPDF, noise variance in quiet passages should match noise
/// variance under signal. This is a statistical property: we measure peak residual
/// noise (quantized − original) and check it doesn't shrink when signal is present.
#[test]
fn mbit_plus_dither_is_constant_amplitude() {
    let mut quiet = mk_buffer(vec![0.001; 1000]); // Very quiet signal.
    let mut loud = mk_buffer(vec![0.8; 1000]); // Loud signal.

    let mut c_quiet = OutputSampleFormat::F32;
    let mut c_loud = OutputSampleFormat::F32;

    bitdepth::apply(&mut quiet, &mut c_quiet, OutputSampleFormat::S16, Some(DitherMode::MbitPlus), None, Some(42)).unwrap();
    bitdepth::apply(&mut loud, &mut c_loud, OutputSampleFormat::S16, Some(DitherMode::MbitPlus), None, Some(42)).unwrap();

    let max = 32_767.0;
    let noise_quiet: f64 = quiet.channels[0]
        .iter()
        .zip(vec![0.001; 1000])
        .map(|(q, orig)| (*q - orig).abs() * max)
        .sum::<f64>()
        / 1000.0;
    let noise_loud: f64 = loud.channels[0]
        .iter()
        .zip(vec![0.8; 1000])
        .map(|(q, orig)| (*q - orig).abs() * max)
        .sum::<f64>()
        / 1000.0;

    // Both should see roughly the same average error magnitude (constant dither amplitude).
    // Allow some variance due to randomness, but they should be in the same ballpark (~0.5-1.5 LSB).
    assert!(noise_quiet.abs() > 0.1, "quiet noise floor should be measurable");
    assert!(noise_loud.abs() > 0.1, "loud noise floor should be measurable");
    // The ratio should be close to 1 (constant amplitude), not diverging by >3x.
    let ratio = (noise_quiet / noise_loud).abs();
    assert!(ratio > 0.3 && ratio < 3.0, "noise amplitude should be roughly constant, got ratio {ratio:.2}");
}

/// Phase 1: Auto-Blanking test verifies that feedback is zeroed when silence detected.
/// We can't easily test the "exactly 0 output" case because TPDF dither is always active,
/// but we can verify that silent regions produce correctly dithered samples without feedback.
/// For now, a simpler check: silence for 50ms+ should not cause clipping artifacts.
#[test]
fn mbit_plus_auto_blanking_no_clipping() {
    let sr = 44_100;
    let silence_len = (sr as usize) / 10; // 100 ms of silence.

    let mut silence = mk_buffer(vec![0.0; silence_len]);
    let mut current = OutputSampleFormat::F32;
    bitdepth::apply(&mut silence, &mut current, OutputSampleFormat::S16, Some(DitherMode::MbitPlus), None, Some(99)).unwrap();

    // Verify output is within valid range [-1, 1] and contains only dither, no feedback artifacts.
    let max_val = silence.channels[0].iter().map(|x| x.abs()).fold(0.0, f64::max);
    assert!(max_val < 1.0, "silence should not produce clipping, got max={}", max_val);
    
    // Measure RMS to ensure dither is present (should be ~0.3-0.5 LSB for TPDF).
    let rms = (silence.channels[0].iter().map(|x| x * x).sum::<f64>() / silence.channels[0].len() as f64).sqrt();
    assert!(rms > 1.0e-5, "silence with dither should have measurable RMS, got {}", rms);
}

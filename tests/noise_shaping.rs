use std::f64::consts::PI;
use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::DitherMode;
use mint::dither::mbit_plus::MbitPlusStrength;
use mint::dither::psychoacoustic::{PsychoacousticAnalysis, FRAME_SIZE, HOP_SIZE};
use mint::dither::adaptive_mbit::AdaptiveShaper;
use mint::ops::bitdepth;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

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

fn ntf_magnitude_response(strength: MbitPlusStrength, sample_rate: u32, fft_len: usize) -> Vec<f64> {
    let coeffs = strength.coeffs_for_rate(sample_rate);
    let mut response = vec![Complex::new(0.0, 0.0); fft_len];
    response[0] = Complex::new(1.0, 0.0);

    for (index, coeff) in coeffs.iter().enumerate() {
        response[index + 1] = Complex::new(-coeff, 0.0);
    }

    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_len);
    fft.process(&mut response);

    response.into_iter().map(|c| c.norm()).collect()
}

fn band_power(magnitudes: &[f64], sample_rate: u32, low_hz: f64, high_hz: f64) -> f64 {
    let bin_hz = sample_rate as f64 / magnitudes.len() as f64;
    let start = ((low_hz / bin_hz).ceil() as usize).max(1);
    let end = ((high_hz / bin_hz).floor() as usize).min(magnitudes.len() / 2 - 1);

    assert!(start <= end, "band {low_hz}-{high_hz} Hz is too narrow for this FFT size");

    let mut power = 0.0;
    let mut count = 0usize;
    for magnitude in &magnitudes[start..=end] {
        power += magnitude * magnitude;
        count += 1;
    }

    power / count as f64
}

fn band_minimum_frequency(magnitudes: &[f64], sample_rate: u32, low_hz: f64, high_hz: f64) -> (f64, f64) {
    let bin_hz = sample_rate as f64 / magnitudes.len() as f64;
    let start = ((low_hz / bin_hz).ceil() as usize).max(1);
    let end = ((high_hz / bin_hz).floor() as usize).min(magnitudes.len() / 2 - 1);

    assert!(start <= end, "band {low_hz}-{high_hz} Hz is too narrow for this FFT size");

    let mut best_frequency = 0.0;
    let mut best_magnitude = f64::INFINITY;
    for (offset, magnitude) in magnitudes[start..=end].iter().enumerate() {
        if *magnitude < best_magnitude {
            best_magnitude = *magnitude;
            best_frequency = (start + offset) as f64 * bin_hz;
        }
    }

    (best_frequency, best_magnitude)
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

    // Exact silence should stay exact silence once blanking is active.
    let max_val = silence.channels[0].iter().map(|x| x.abs()).fold(0.0, f64::max);
    assert!(max_val < 1.0, "silence should not produce clipping, got max={}", max_val);
    assert!(silence.channels[0].iter().all(|&x| x == 0.0), "silence should be rendered as exact zero after blanking");
}

/// Phase 3: FFT validation of the static NTF tables. The filter should suppress the
/// ear-sensitive low band much more than the high band, and stronger modes should
/// suppress it more than weaker ones.
#[test]
fn mbit_plus_fft_ntf_stronger_modes_suppress_low_band_more() {
    let fft_len = 16_384;
    let sample_rate = 44_100;

    let low = ntf_magnitude_response(MbitPlusStrength::Low, sample_rate, fft_len);
    let normal = ntf_magnitude_response(MbitPlusStrength::Normal, sample_rate, fft_len);
    let high = ntf_magnitude_response(MbitPlusStrength::High, sample_rate, fft_len);

    let low_ratio = band_power(&low, sample_rate, 20.0, 2_000.0) / band_power(&low, sample_rate, 8_000.0, 18_000.0);
    let normal_ratio = band_power(&normal, sample_rate, 20.0, 2_000.0) / band_power(&normal, sample_rate, 8_000.0, 18_000.0);
    let high_ratio = band_power(&high, sample_rate, 20.0, 2_000.0) / band_power(&high, sample_rate, 8_000.0, 18_000.0);

    assert!(low_ratio > normal_ratio, "normal should suppress the low band more than low");
    assert!(normal_ratio > high_ratio, "high should suppress the low band more than normal");
    assert!(high_ratio < 0.30, "high-strength NTF should strongly favor the high band, got ratio={high_ratio:.3}");
}

/// Phase 3: rate-aware validation. The notch should move with the sample-rate table
/// instead of staying stuck at one absolute frequency.
#[test]
fn mbit_plus_fft_ntf_rate_tables_move_the_notch() {
    let fft_len = 16_384;
    let (f441, _) = band_minimum_frequency(&ntf_magnitude_response(MbitPlusStrength::Normal, 44_100, fft_len), 44_100, 3_000.0, 5_000.0);
    let (f48, _) = band_minimum_frequency(&ntf_magnitude_response(MbitPlusStrength::Normal, 48_000, fft_len), 48_000, 3_000.0, 5_000.0);
    let (f882, _) = band_minimum_frequency(&ntf_magnitude_response(MbitPlusStrength::Normal, 88_200, fft_len), 88_200, 6_000.0, 8_500.0);
    let (f96, _) = band_minimum_frequency(&ntf_magnitude_response(MbitPlusStrength::Normal, 96_000, fft_len), 96_000, 6_500.0, 9_000.0);

    assert!((f441 - 3_500.0).abs() < 1_500.0, "44.1k notch should sit in the critical band, got {f441:.0} Hz");
    assert!((f48 - 3_600.0).abs() < 1_500.0, "48k notch should stay in the same perceptual zone, got {f48:.0} Hz");
    assert!((f882 - 7_000.0).abs() < 1_500.0, "88.2k notch should move upward with the rate table, got {f882:.0} Hz");
    assert!((f96 - 7_200.0).abs() < 1_500.0, "96k notch should move upward with the rate table, got {f96:.0} Hz");
}

// ─── Phase 4.1: Psychoacoustic Analysis integration tests ────────────────────

/// A sine at a given frequency (normalized –1..1).
fn sine_signal(freq_hz: f64, n: usize, sample_rate: u32) -> Vec<f64> {
    (0..n)
        .map(|i| (2.0 * PI * freq_hz * i as f64 / sample_rate as f64).sin())
        .collect()
}

/// Phase 4.1: Analysis of a 1-second buffer returns a sensible frame count.
#[test]
fn psychoacoustic_analysis_frame_count_matches_hop_size() {
    let sr = 44_100u32;
    let sig = sine_signal(997.0, sr as usize, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);

    // Expected frames: ceil((n - FRAME_SIZE) / HOP_SIZE) + 1 ≈ 1723 for 1 s @ 44.1 kHz.
    let expected = (sig.len() - FRAME_SIZE) / HOP_SIZE + 1;
    assert!(
        (analysis.num_frames() as isize - expected as isize).abs() <= 2,
        "frame count should be ~{expected}, got {}",
        analysis.num_frames()
    );
    assert_eq!(
        analysis.num_bins, FRAME_SIZE / 2 + 1,
        "num_bins should equal FRAME_SIZE / 2 + 1"
    );
}

/// Phase 4.1: A loud tone raises the masking threshold in its own frequency region.
#[test]
fn psychoacoustic_analysis_loud_tone_raises_threshold() {
    let sr = 44_100u32;
    let loud = sine_signal(1_000.0, sr as usize, sr); // 0 dBFS 1 kHz
    let silent = vec![0.0f64; sr as usize];

    let a_loud = PsychoacousticAnalysis::analyze(&loud, sr);
    let a_silent = PsychoacousticAnalysis::analyze(&silent, sr);

    // Sample in the middle of the signal to avoid transient onset effects.
    let t_loud = a_loud.threshold_for_sample(sr as usize / 2);
    let t_silent = a_silent.threshold_for_sample(sr as usize / 2);

    let bin_1k = (1_000.0 * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize;
    assert!(
        t_loud[bin_1k] > t_silent[bin_1k],
        "loud 1 kHz tone should raise the masking threshold at 1 kHz"
    );
}

/// Phase 4.1: Spreading — a 1 kHz tone elevates the threshold in neighboring
/// Bark bands, not just in its own bin.
#[test]
fn psychoacoustic_analysis_spreading_elevates_adjacent_bark_bands() {
    let sr = 44_100u32;
    let sig = sine_signal(1_000.0, sr as usize, sr);
    let silent = vec![0.0f64; sr as usize];

    let a_sig = PsychoacousticAnalysis::analyze(&sig, sr);
    let a_sil = PsychoacousticAnalysis::analyze(&silent, sr);

    let t_sig = a_sig.threshold_for_sample(sr as usize / 2);
    let t_sil = a_sil.threshold_for_sample(sr as usize / 2);

    // Bins within one Bark band of 1 kHz (900–1200 Hz) should see elevated thresholds.
    let bin = |f: f64| -> usize {
        (f * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize
    };
    assert!(
        t_sig[bin(900.0)] > t_sil[bin(900.0)],
        "spreading should elevate threshold near 900 Hz"
    );
    assert!(
        t_sig[bin(1_200.0)] > t_sil[bin(1_200.0)],
        "spreading should elevate threshold near 1.2 kHz"
    );
}

/// Phase 4.1: Masking is weaker far from the masker than near it (spreading
/// function rolls off with Bark-distance).
#[test]
fn psychoacoustic_analysis_masking_rolls_off_with_bark_distance() {
    let sr = 44_100u32;
    let sig = sine_signal(1_000.0, sr as usize, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);

    let t = analysis.threshold_for_sample(sr as usize / 2);

    let bin = |f: f64| -> usize {
        (f * (FRAME_SIZE / 2) as f64 / (sr as f64 / 2.0)).round() as usize
    };

    // Threshold near the masker (1 kHz) should be much higher than far away (8 kHz).
    assert!(
        t[bin(1_000.0)] > t[bin(8_000.0)] * 2.0,
        "masking should roll off with Bark distance from the 1 kHz masker"
    );
}

/// Phase 4.1: All thresholds must be positive (≥ ATH > 0 everywhere).
#[test]
fn psychoacoustic_analysis_all_thresholds_positive() {
    let sr = 44_100u32;
    let sig = sine_signal(440.0, sr as usize / 4, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);

    for (fi, frame) in analysis.thresholds.iter().enumerate() {
        for (bi, &t) in frame.iter().enumerate() {
            assert!(
                t > 0.0,
                "threshold must be > 0 everywhere (frame {fi}, bin {bi})"
            );
        }
    }
}

/// Phase 4.1: `threshold_for_sample` must not panic on out-of-range indices.
#[test]
fn psychoacoustic_analysis_threshold_clamps_gracefully() {
    let sig = vec![0.1f64; 2048];
    let a = PsychoacousticAnalysis::analyze(&sig, 44_100);
    let t = a.threshold_for_sample(usize::MAX);
    assert_eq!(t.len(), a.num_bins);
}

// ─── Phase 4.2: Adaptive noise shaping with minimal-phase design ────────────

/// Phase 4.2: A shaper designed from a psychoacoustic analysis has the right
/// number of frames and order.
#[test]
fn adaptive_shaper_from_analysis_has_correct_structure() {
    let sr = 44_100u32;
    let sig = sine_signal(1_000.0, sr as usize / 2, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);
    let shaper = AdaptiveShaper::from_analysis(&analysis, 7);

    assert_eq!(
        shaper.num_frames(),
        analysis.num_frames(),
        "shaper should have one entry per analysis frame"
    );
    assert_eq!(shaper.order(), 7, "FIR order should match the request");
}

/// Phase 4.2: Coefficients are linearly interpolated between frames, producing
/// smooth transitions without zipper artifacts.
#[test]
fn adaptive_shaper_coefficients_interpolate_smoothly() {
    let sr = 44_100u32;
    let sig = sine_signal(1_000.0, sr as usize / 2, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);
    let shaper = AdaptiveShaper::from_analysis(&analysis, 5);

    // Collect coefficients over one hop period. They should vary smoothly,
    // not jump discontinuously.
    let mut prev_coeffs = shaper.coeffs_for_sample(0);
    let mut max_delta = 0.0f64;

    for sample in (0..HOP_SIZE).step_by(HOP_SIZE / 100) {
        let curr_coeffs = shaper.coeffs_for_sample(sample);
        for (i, &curr) in curr_coeffs.iter().enumerate() {
            let delta = (curr - prev_coeffs[i]).abs();
            max_delta = max_delta.max(delta);
        }
        prev_coeffs = curr_coeffs;
    }

    // The maximum change over the hop should be bounded (smooth transition).
    assert!(
        max_delta < 0.5,
        "coefficient changes should be smooth, got max delta {max_delta:.3}"
    );
}

/// Phase 4.2: At frame boundaries, coefficients should match the frame's design
/// (frame 0 at sample 0, frame 1 at sample HOP_SIZE, etc.).
#[test]
fn adaptive_shaper_frame_boundaries_exact() {
    let sr = 44_100u32;
    let sig = sine_signal(1_000.0, sr as usize, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);
    let shaper = AdaptiveShaper::from_analysis(&analysis, 5);

    // At the start of each frame (sample = k * HOP_SIZE), the shaper should
    // return exactly the frame's coefficients.
    for frame in 0..shaper.num_frames().min(3) {
        let sample = frame * HOP_SIZE;
        let c_shaper = shaper.coeffs_for_sample(sample);
        let c_frame = &shaper.frame_coeffs()[frame];
        for (i, &cs) in c_shaper.iter().enumerate() {
            assert!(
                (cs - c_frame[i]).abs() < 1e-10,
                "shaper({}) should match frame {} coeff {}",
                sample,
                frame,
                i
            );
        }
    }
}

/// Phase 4.2: All FIR coefficients should be finite and of reasonable magnitude
/// (no NaN, inf, or wildly large values that would destabilize the quantizer).
#[test]
fn adaptive_shaper_coefficients_are_stable() {
    let sr = 44_100u32;
    let sig = sine_signal(1_000.0, sr as usize / 2, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);
    let shaper = AdaptiveShaper::from_analysis(&analysis, 9);

    for (frame_idx, frame_coeffs_vec) in shaper.frame_coeffs().iter().enumerate() {
        for (tap_idx, &coeff) in frame_coeffs_vec.iter().enumerate() {
            assert!(
                coeff.is_finite(),
                "frame {} tap {} must be finite, got {}",
                frame_idx,
                tap_idx,
                coeff
            );
            assert!(
                coeff.abs() < 100.0,
                "frame {} tap {} magnitude should stay bounded, got {}",
                frame_idx,
                tap_idx,
                coeff
            );
        }
    }
}

/// Phase 4.2: Interpolation should handle boundary cases gracefully.
#[test]
fn adaptive_shaper_bounds_checking() {
    let sr = 44_100u32;
    let sig = sine_signal(440.0, sr as usize / 4, sr);
    let analysis = PsychoacousticAnalysis::analyze(&sig, sr);
    let shaper = AdaptiveShaper::from_analysis(&analysis, 7);

    // Very large sample indices should not panic or return garbage.
    let c_large = shaper.coeffs_for_sample(1_000_000_000);
    assert_eq!(c_large.len(), 7, "should return a full coefficient vector");
    assert!(c_large.iter().all(|c| c.is_finite()), "all coefficients must be finite");
}

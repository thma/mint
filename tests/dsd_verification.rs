//! DSD sigma-delta modulator **verification harness**.
//!
//! A modulator is a claim until it is measured. `docs/dsd-design.md` makes the dB table
//! below the acceptance criterion for phase 2 — not "it compiles and plays". Same stance
//! as `tests/dither_verification.rs` and `tests/limiter_verification.rs`, and the same
//! convention: the numbers here are *measured*, the bounds are placed between the passing
//! and the failing regime, and if a legitimate change moves them, this header moves too.
//!
//! Run with `cargo test --test dsd_verification -- --nocapture` to see the tables.
//!
//! # Two independent methods, cross-checked
//!
//! 1. **Analytic** — `|NTF(e^jw)| = |N(z)/D(z)|`, with the polynomials rebuilt from the
//!    CIFB table by Faddeev-LeVerrier. Independent of the modulator code: it never runs a
//!    sample.
//! 2. **Empirical** — run the real modulator, take the Kaiser-windowed PSD of the
//!    bitstream, and for the end-to-end tests bring it back to PCM through
//!    `ops::dsd::decimate`.
//!
//! `noise_shape_follows_the_designed_ntf` holds the two against each other, which is what
//! makes the rest trustworthy: it proves the implemented recurrence realizes the
//! transfer function the table was designed for.
//!
//! # Measured baseline (order 7, H_inf 1.30, 2026-08-03, Apple Silicon, seed 0xD5D)
//!
//! Level convention throughout: **0 dBFS == 50 % modulation index**, the Scarlet Book
//! reference. A 1 kHz tone at 0 dBFS is therefore amplitude 0.5 in the +-1 bitstream, and
//! "SNR" is measured there.
//!
//! | rate | analytic sup\|NTF\| | analytic in-band max | SNR 20 Hz-20 kHz | THD+N |
//! |---|---|---|---|---|
//! | DSD64  | 1.300 | -94.6 dB  | **121.0 dB** | 120.0 dB |
//! | DSD128 | 1.300 | -135.1 dB | 163.8 dB | 162.1 dB |
//! | DSD256 | 1.300 | -151.1 dB | 173.7 dB | 171.1 dB |
//! | DSD512 | 1.300 | -151.8 dB | 175.1 dB | 171.3 dB |
//!
//! The phase-2 target in `docs/dsd-design.md` was >= 110 dB at DSD64; the gate is set
//! there and the measurement clears it by 11 dB.
//!
//! The last two analytic in-band figures are **not** the design values (-176.9 and
//! -219.0 dB). They are where f64 cancellation bottoms out: past DSD128 the notch is
//! deeper than double precision can evaluate, let alone realize. That costs nothing —
//! anything past DSD128 is already 40 dB beyond relevance — but it is why the gates in
//! here are floors and ratios rather than equalities.
//!
//! Everything else the harness pins down:
//!
//! | property | measured | gate |
//! |---|---|---|
//! | shaping vs a 2nd-order modulator, in band | 48.2 dB better | > 40 dB |
//! | measured spectrum vs the table's loop filter | 0.32 dB rms, k = 2.42 | < 1.5 dB |
//! | passband, 19 Hz - 19.8 kHz, whole chain | 0.0001 dB | < 0.1 dB |
//! | DC at digital silence | exactly 0 | < 1e-4 |
//! | idle tone, 1e-3 DC offset, dithered | 16.1 dB (34.5 undithered) | < 25 dB |
//! | clamping at or below 60 % modulation | none | none |
//! | SNR after a 90 % overdrive | 120.7 dB | > 110 dB |
//! | decimator's own error (the instrument) | -185.7 dB | < -160 dB |
//!
//! `k` is the comparator's effective gain, which is *not* 1 for a 1-bit quantizer — see
//! [`ntf_mag_with_gain`]. It is fitted, not assumed, and the residual is the real check.

use std::f64::consts::PI;

use mint::config::ResampleQuality;
use mint::ops::dsd::DsdRate;
use mint::ops::dsd::decimate::Decimator;
use mint::ops::dsd::interpolate::SoxrStream;
use mint::ops::dsd::modulator::{
    INPUT_SCALE, MAX_MODULATION, Modulator, Ntf, ORDER, RESONATORS, level, ntf_for,
};
use rustfft::FftPlanner;
use rustfft::num_complex::Complex;

const SEED: u64 = 0xD5D;
/// Analysis length in DSD samples. At DSD64 that is 0.19 s and 5.4 Hz per bin; at DSD512
/// it is 0.023 s and 43 Hz. Both give a few hundred in-band bins, which is plenty for a
/// noise floor, and keeps the whole harness inside a minute in a debug build.
const N: usize = 1 << 19;
const BAND_LO: f64 = 20.0;
const BAND_HI: f64 = 20_000.0;
/// Bins either side of a tone treated as part of it. The Kaiser window concentrates a
/// tone into far fewer than this; the margin is for the modulator's own low-frequency
/// wander around the fundamental.
const TONE_BINS: usize = 16;

// ────────────────────────────── spectral tools ───────────────────────────────

/// Modified Bessel I0 — the Kaiser kernel. All terms positive, so no cancellation.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let (mut term, mut sum) = (1.0, 1.0);
    for k in 1..128 {
        let f = half / k as f64;
        term *= f * f;
        sum += term;
        if term < 1e-18 * sum {
            break;
        }
    }
    sum
}

/// Kaiser window, beta 20: sidelobes below -200 dB. Anything less and a full-scale tone
/// smears into the -120 dB noise floor we are trying to read, and the harness measures
/// its own window instead of the modulator.
fn kaiser(n: usize, beta: f64) -> Vec<f64> {
    let denom = bessel_i0(beta);
    let half = (n - 1) as f64 / 2.0;
    (0..n)
        .map(|i| {
            let r = (i as f64 - half) / half;
            bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / denom
        })
        .collect()
}

/// One-sided power spectrum of a Kaiser-windowed real signal.
fn spectrum(x: &[f64]) -> Vec<f64> {
    let w = kaiser(x.len(), 20.0);
    let mut buf: Vec<Complex<f64>> = x
        .iter()
        .zip(&w)
        .map(|(v, wi)| Complex::new(v * wi, 0.0))
        .collect();
    FftPlanner::new().plan_fft_forward(buf.len()).process(&mut buf);
    buf[..buf.len() / 2 + 1].iter().map(|c| c.norm_sqr()).collect()
}

/// Tone with an integer number of cycles, so it sits on one bin. Returns the samples and
/// that bin index.
fn tone(n: usize, fs: f64, hz: f64, amp: f64) -> (Vec<f64>, usize) {
    let cycles = (hz * n as f64 / fs).round().max(1.0);
    let x = (0..n)
        .map(|i| (2.0 * PI * cycles * i as f64 / n as f64).sin() * amp)
        .collect();
    (x, cycles as usize)
}

#[derive(Debug, Clone, Copy)]
struct Reading {
    /// Signal power over in-band noise, harmonics excluded.
    snr_db: f64,
    /// Same, harmonics counted as noise.
    thdn_db: f64,
    /// In-band noise relative to a full-scale tone.
    noise_dbfs: f64,
}

/// In-band noise/distortion around a tone. Deliberately calibration-free: everything is a
/// ratio inside one spectrum, so the window's coherent gain cancels and there is no
/// absolute scale factor to get wrong.
fn analyse(psd: &[f64], fs: f64, n: usize, bin: usize, amp: f64, full_scale: f64) -> Reading {
    let hz = |k: usize| k as f64 * fs / n as f64;
    let near = |k: usize, centre: usize| k.abs_diff(centre) <= TONE_BINS;

    let (mut sig, mut noise, mut noise_with_harmonics) = (0.0, 0.0, 0.0);
    for (k, &p) in psd.iter().enumerate() {
        let f = hz(k);
        if !(BAND_LO..=BAND_HI).contains(&f) {
            continue;
        }
        if near(k, bin) {
            sig += p;
            continue;
        }
        noise_with_harmonics += p;
        let harmonic = (2..=40).any(|h| near(k, bin * h));
        if !harmonic {
            noise += p;
        }
    }
    let snr_db = 10.0 * (sig / noise).log10();
    Reading {
        snr_db,
        thdn_db: 10.0 * (sig / noise_with_harmonics).log10(),
        // Referred to full scale: shift the ratio by however far the probe sits below it.
        noise_dbfs: -snr_db + 20.0 * (amp / full_scale).log10(),
    }
}

/// Mean noise power per bin over a frequency span, in dB.
///
/// The scale is arbitrary but consistent, so only *differences* between two calls mean
/// anything — which is all this harness asks of it. `skip` masks the probe tone: for the
/// in-band span the fundamental is 100+ dB above the noise and would otherwise be reported
/// as the noise density.
fn band_level_db(psd: &[f64], fs: f64, n: usize, lo: f64, hi: f64, skip: Option<usize>) -> f64 {
    let mut sum = 0.0;
    let mut count = 0usize;
    for (k, &p) in psd.iter().enumerate() {
        let f = k as f64 * fs / n as f64;
        if (lo..=hi).contains(&f) && skip.is_none_or(|s| k.abs_diff(s) > TONE_BINS) {
            sum += p;
            count += 1;
        }
    }
    10.0 * (sum / count.max(1) as f64).log10()
}

// ─────────────────────── analytic NTF, straight from the table ───────────────

/// The state matrix the modulator's sequential update is equivalent to. Rebuilt here from
/// the resonator gains alone, so this path shares no code with `modulator.rs`.
fn state_matrix(g: &[f64; RESONATORS]) -> Vec<Vec<f64>> {
    let mut a = vec![vec![0.0; ORDER]; ORDER];
    let mut carry = vec![0.0; ORDER];
    for s in 0..ORDER {
        let mut row = carry.clone();
        row[s] += 1.0;
        if s % 2 == 0 && s + 1 < ORDER {
            row[s + 1] -= g[s / 2];
        }
        a[s] = row.clone();
        carry = row;
    }
    a
}

/// `(numerator, denominator)` of the NTF, highest power first.
///
/// `N(z) = det(zI - A)` and `D(z) = N(z) + C adj(zI - A) B`, with `adj` from
/// Faddeev-LeVerrier. `B` is the running sum of `a` because each stage adds its
/// predecessor's already-updated value.
fn ntf_polynomials(ntf: &Ntf) -> (Vec<f64>, Vec<f64>) {
    let a_mat = state_matrix(&ntf.g);
    let b: Vec<f64> = ntf
        .a
        .iter()
        .scan(0.0, |acc, v| {
            *acc += v;
            Some(*acc)
        })
        .collect();

    let mut m = vec![vec![0.0; ORDER]; ORDER];
    (0..ORDER).for_each(|i| m[i][i] = 1.0);
    let mut num = vec![1.0];
    let mut correction = Vec::with_capacity(ORDER);

    for k in 1..=ORDER {
        // correction coefficient for z^(ORDER-k) uses M_{k-1}, i.e. the current `m`
        correction.push((0..ORDER).map(|j| m[ORDER - 1][j] * b[j]).sum::<f64>());

        let am: Vec<Vec<f64>> = (0..ORDER)
            .map(|i| {
                (0..ORDER)
                    .map(|j| (0..ORDER).map(|l| a_mat[i][l] * m[l][j]).sum())
                    .collect()
            })
            .collect();
        let ck = -(0..ORDER).map(|i| am[i][i]).sum::<f64>() / k as f64;
        num.push(ck);
        m = am;
        (0..ORDER).for_each(|i| m[i][i] += ck);
    }

    let mut den = num.clone();
    for (i, c) in correction.iter().enumerate() {
        den[i + 1] += c;
    }
    (num, den)
}

fn eval_poly(coeffs: &[f64], z: Complex<f64>) -> Complex<f64> {
    coeffs
        .iter()
        .fold(Complex::new(0.0, 0.0), |acc, &c| acc * z + Complex::new(c, 0.0))
}

fn ntf_mag(ntf: &Ntf, w: f64) -> f64 {
    let (num, den) = ntf_polynomials(ntf);
    ntf_mag_with_gain(&num, &den, w, 1.0)
}

/// `|NTF|` when the comparator is modelled as gain `k` plus additive error, rather than as
/// unity gain.
///
/// A 1-bit quantizer has no intrinsic gain — only a sign — so the loop's *realized* noise
/// transfer is not the designed one; it is the designed one re-solved for whatever
/// effective gain the comparator happens to present. From `v = k*y + e`,
///
///   NTF_k(z) = N(z) / (N(z) + k*(D(z) - N(z))),
///
/// which is the designed NTF at `k = 1`. Fitting the single scalar `k` to the measured
/// spectrum is what lets `noise_shape_follows_the_designed_ntf` compare the two honestly.
fn ntf_mag_with_gain(num: &[f64], den: &[f64], w: f64, k: f64) -> f64 {
    let z = Complex::from_polar(1.0, w);
    let n = eval_poly(num, z);
    let d = eval_poly(den, z);
    (n / (n + (d - n) * k)).norm()
}

// ────────────────────────────── chain helpers ────────────────────────────────

/// Modulate a signal synthesized directly at the DSD rate. Isolates the modulator: no
/// interpolation filter in the measurement.
fn modulate(rate: DsdRate, pcm: &[f64], seed: u64) -> (Vec<f64>, Modulator) {
    let mut m = Modulator::new(rate, Some(seed));
    let bits = m.modulate(pcm);
    (bits.into_iter().map(level).collect(), m)
}

/// The real chain: PCM -> streaming interpolation -> modulator -> decimation back to PCM.
/// Returns the recovered PCM, rescaled out of modulation units so it is directly
/// comparable with the input.
fn round_trip(pcm: &[f64], pcm_rate: u32, rate: DsdRate, block: usize) -> Vec<f64> {
    let mut src = SoxrStream::new(pcm_rate, rate.hz(), 1, Some(ResampleQuality::Vhq)).unwrap();
    let mut m = Modulator::new(rate, Some(SEED));
    let mut bits = Vec::new();

    for chunk in pcm.chunks(block) {
        let up = src.process(&[chunk.to_vec()]).unwrap();
        m.modulate_into(&up[0], &mut bits);
    }
    let tail = src.finish().unwrap()[0].clone();
    m.modulate_into(&tail, &mut bits);

    let dec = Decimator::for_rates(rate.hz(), pcm_rate).unwrap();
    dec.process_bits(&bits)
        .into_iter()
        .map(|v| v / INPUT_SCALE)
        .collect()
}

/// Textbook second-order modulator, `NTF = (1 - z^-1)^2`, as the reference the shaping
/// gain is quoted against. Error feedback rather than CIFB — it needs no design step, so
/// it cannot silently inherit a bug from the thing it is benchmarking.
fn second_order_reference(pcm: &[f64]) -> Vec<f64> {
    let (mut e1, mut e2) = (0.0, 0.0);
    pcm.iter()
        .map(|&u| {
            let w = u * INPUT_SCALE - 2.0 * e1 + e2;
            let v = if w >= 0.0 { 1.0 } else { -1.0 };
            e2 = e1;
            e1 = v - w;
            v
        })
        .collect()
}

// ═════════════════════════════════ 1. SNR ════════════════════════════════════

#[test]
fn in_band_snr_clears_the_target_at_every_rate() {
    println!("\n  rate     SNR      THD+N    noise      max|x|/limit  overload");
    let mut worst = f64::INFINITY;

    for rate in DsdRate::ALL {
        let fs = rate.hz() as f64;
        // Amplitude 1.0 in PCM terms == 0 dBFS == 50 % modulation after INPUT_SCALE.
        let (pcm, bin) = tone(N, fs, 1000.0, 1.0);
        let (bits, m) = modulate(rate, &pcm, SEED);
        let r = analyse(&spectrum(&bits), fs, N, bin, 0.5, 0.5);

        let ntf = ntf_for(rate);
        let headroom = m
            .states()
            .iter()
            .zip(&ntf.state_limit)
            .map(|(x, l)| x.abs() / l)
            .fold(0.0f64, f64::max);
        let stats = m.stats();

        println!(
            "  {:<7} {:7.1} dB {:7.1} dB {:8.1} dBFS {:9.1e}    {}",
            rate.tag(),
            r.snr_db,
            r.thdn_db,
            r.noise_dbfs,
            headroom,
            stats.overload_samples
        );

        assert_eq!(stats.overload_samples, 0, "{} clamped at 0 dBFS", rate.tag());
        assert!(
            r.snr_db > 110.0,
            "{}: in-band SNR {:.1} dB is under the 110 dB target",
            rate.tag(),
            r.snr_db
        );
        // Harmonics must not be what is left: THD+N within a few dB of pure SNR.
        assert!(
            r.snr_db - r.thdn_db < 6.0,
            "{}: distortion dominates ({:.1} dB SNR vs {:.1} dB THD+N)",
            rate.tag(),
            r.snr_db,
            r.thdn_db
        );
        worst = worst.min(r.snr_db);
    }

    // Doubling the rate must actually buy something, or the table is not per-rate at all.
    let fs = DsdRate::Dsd128.hz() as f64;
    let (pcm, bin) = tone(N, fs, 1000.0, 1.0);
    let (bits, _) = modulate(DsdRate::Dsd128, &pcm, SEED);
    let hi = analyse(&spectrum(&bits), fs, N, bin, 0.5, 0.5);
    assert!(
        hi.snr_db > worst + 15.0,
        "DSD128 ({:.1} dB) should be well clear of the worst rate ({worst:.1} dB)",
        hi.snr_db
    );
}

// ═══════════════════ 2. shaping: analytic vs. what came out ══════════════════

#[test]
fn noise_shape_follows_the_designed_ntf() {
    let rate = DsdRate::Dsd64;
    let fs = rate.hz() as f64;
    let (pcm, bin) = tone(N, fs, 1000.0, 1.0);
    let (bits, _) = modulate(rate, &pcm, SEED);
    let psd = spectrum(&bits);
    let ntf = ntf_for(rate);

    // Out of band the linearized model holds, so the measured PSD must follow |NTF_k|^2 —
    // with `k` the comparator's effective gain, which for a 1-bit quantizer is not 1 and
    // is not knowable in advance. So: fit the one scalar, then judge the residual. A good
    // fit proves the implemented recurrence realizes the *loop filter* in the table (the
    // whole k-family is generated by it); a bad one means the recurrence is wrong.
    let (num, den) = ntf_polynomials(ntf);
    let probe = [50e3, 100e3, 200e3, 400e3, 700e3, 1_200e3];
    let measured: Vec<f64> = probe
        .iter()
        .map(|f| band_level_db(&psd, fs, N, f * 0.9, f * 1.1, None))
        .collect();
    let predict = |k: f64| -> Vec<f64> {
        probe
            .iter()
            .map(|f| 20.0 * ntf_mag_with_gain(&num, &den, 2.0 * PI * f / fs, k).log10())
            .collect()
    };
    // Normalize both curves at 100 kHz (probe index 1) so only the *shape* is compared;
    // the absolute level also carries the quantizer's error variance, which is not the
    // claim under test.
    let residual = |k: f64| -> f64 {
        let p = predict(k);
        probe
            .iter()
            .enumerate()
            .map(|(i, _)| ((measured[i] - measured[1]) - (p[i] - p[1])).powi(2))
            .sum::<f64>()
            .sqrt()
            / (probe.len() as f64).sqrt()
    };
    let (best_k, best) = (30..=1200)
        .map(|i| i as f64 / 100.0)
        .map(|k| (k, residual(k)))
        .fold((0.0, f64::INFINITY), |acc, c| if c.1 < acc.1 { c } else { acc });

    println!(
        "\n  effective comparator gain k = {best_k:.2} (rms shape error {best:.2} dB)\n\
           \n  freq      measured   NTF_k     designed NTF   (normalized at 100 kHz)"
    );
    let fitted = predict(best_k);
    for (i, f) in probe.iter().enumerate() {
        println!(
            "  {f:>8.0}  {:8.2} dB {:8.2} dB {:11.2} dB",
            measured[i] - measured[1],
            fitted[i] - fitted[1],
            20.0 * ntf_mag(ntf, 2.0 * PI * f / fs).log10()
                - 20.0 * ntf_mag(ntf, 2.0 * PI * 100e3 / fs).log10()
        );
    }
    assert!(
        best < 1.5,
        "the bitstream spectrum does not follow the table's loop filter for any comparator \
         gain (best k = {best_k:.2}, rms shape error {best:.2} dB) — the implemented \
         recurrence is not realizing the NTF it was designed for"
    );
    // The expected value is not 1. For a comparator, k = E[|y|]/E[y^2] = sqrt(2/pi)/sigma_y
    // for roughly Gaussian y, and the quantizer input runs at sigma ~ 0.3, which predicts
    // k ~ 2.6. (A 1-bit quantizer has no intrinsic gain — only sign — so the loop settles
    // wherever its own state scale puts it. That is why the designed and realized curves
    // differ out of band even though the recurrence is correct.)
    assert!(
        (1.5..=4.5).contains(&best_k),
        "effective comparator gain {best_k:.2} is far from the ~2.6 that sigma_y ~ 0.3 \
         predicts; the loop is not operating where the design assumes"
    );

    // Shape: quiet in band, steeply rising above it.
    let in_band = band_level_db(&psd, fs, N, BAND_LO, BAND_HI, Some(bin));
    let steps = [
        ("20 Hz-20 kHz", in_band),
        ("25-40 kHz", band_level_db(&psd, fs, N, 25e3, 40e3, None)),
        ("40-100 kHz", band_level_db(&psd, fs, N, 40e3, 100e3, None)),
        ("100-400 kHz", band_level_db(&psd, fs, N, 100e3, 400e3, None)),
        ("400 kHz-Nyq", band_level_db(&psd, fs, N, 400e3, fs / 2.0, None)),
    ];
    println!("\n  per-bin noise density, relative to in-band:");
    for (label, db) in steps {
        println!("  {label:>14}  {:+8.1} dB", db - in_band);
    }
    for pair in steps.windows(2) {
        assert!(
            pair[1].1 > pair[0].1,
            "noise density is not rising: {} ({:.1}) then {} ({:.1})",
            pair[0].0,
            pair[0].1,
            pair[1].0,
            pair[1].1
        );
    }

    // Against the second-order reference, in band — the honest way to quote shaping gain.
    let reference_bits = second_order_reference(&pcm);
    let ref_in_band =
        band_level_db(&spectrum(&reference_bits), fs, N, BAND_LO, BAND_HI, Some(bin));
    let gain = ref_in_band - in_band;
    println!("\n  in-band vs 2nd-order reference: {gain:.1} dB better\n");
    assert!(
        gain > 40.0,
        "order 7 should bury a 2nd-order modulator in band; got only {gain:.1} dB"
    );
}

// ═════════════════════ 3. passband through the real chain ════════════════════

#[test]
fn passband_is_flat_through_the_whole_chain() {
    // 88.2 kHz -> DSD64 is x32, the ratio the rate rule picks for a 96 kHz master.
    const PCM_RATE: u32 = 88_200;
    const FRAMES: usize = 1 << 15;

    // Odd-prime bins: no harmonic and no second-order intermodulation product of any two
    // partials can land on another partial (odd + odd is even, k*prime is composite), so
    // a magnitude reading cannot be contaminated by the modulator's own nonlinearity.
    const BINS: [usize; 17] = [
        7, 11, 19, 29, 47, 73, 113, 179, 283, 449, 709, 1123, 1777, 2803, 4441, 6961, 7369,
    ];

    let mut pcm = vec![0.0; FRAMES];
    for (i, &b) in BINS.iter().enumerate() {
        let phase = i as f64 * 0.618_033_988_75 * 2.0 * PI;
        for (n, v) in pcm.iter_mut().enumerate() {
            *v += (2.0 * PI * b as f64 * n as f64 / FRAMES as f64 + phase).sin();
        }
    }
    // Normalize the peak, so every partial keeps a known *relative* amplitude and the
    // interpolator's inter-sample overshoot still cannot reach the format maximum.
    let peak = pcm.iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let scale = 0.8 / peak;
    pcm.iter_mut().for_each(|v| *v *= scale);

    let out = round_trip(&pcm, PCM_RATE, DsdRate::Dsd64, 2048);
    assert_eq!(out.len(), FRAMES, "round trip changed the length");

    let (before, after) = (spectrum(&pcm), spectrum(&out));
    println!("\n     freq     deviation");
    let mut worst = 0.0f64;
    for &b in &BINS {
        // Sum the tone's few leakage bins on both sides, so the reading does not depend on
        // the window's exact main-lobe shape.
        let energy = |psd: &[f64]| -> f64 {
            (b.saturating_sub(4)..=b + 4).map(|k| psd[k]).sum()
        };
        let dev = 10.0 * (energy(&after) / energy(&before)).log10();
        let hz = b as f64 * PCM_RATE as f64 / FRAMES as f64;
        println!("  {hz:8.0} Hz  {dev:+.4} dB");
        worst = worst.max(dev.abs());
    }
    println!("  worst deviation {worst:.4} dB\n");
    assert!(
        worst < 0.1,
        "passband deviates by {worst:.3} dB, the budget is 0.1 dB"
    );
}

// ════════════════════════════ 4. DC at silence ═══════════════════════════════

#[test]
fn digital_silence_carries_no_dc_offset() {
    for rate in DsdRate::ALL {
        let (bits, m) = modulate(rate, &vec![0.0; N], SEED);
        let mean = bits.iter().sum::<f64>() / bits.len() as f64;
        // The NTF has a zero at DC, so the residual is far below the 1/sqrt(N) a white
        // bitstream would leave — this is the test that the DC zero is really there.
        println!("  {:<7} bitstream mean {:+.3e}", rate.tag(), mean);
        assert!(
            mean.abs() < 1e-4,
            "{}: silence has a DC offset of {mean:+.3e} (density {:.4} %)",
            rate.tag(),
            50.0 * (1.0 + mean)
        );
        assert_eq!(m.stats().overload_samples, 0);
        assert_eq!(m.stats().max_modulation, 0.0);
    }
}

// ═════════════════════════════ 5. idle tones ═════════════════════════════════

#[test]
fn quantizer_dither_suppresses_idle_tones() {
    let rate = DsdRate::Dsd64;
    let fs = rate.hz() as f64;

    // Peak-to-median across in-band bins: a discrete idle tone is a spike over an
    // otherwise smooth floor, which is exactly what that ratio detects.
    //
    // Two things must be masked or they masquerade as idle tones. The probe tone itself,
    // obviously — and **DC**. A DC input puts a real DC component in the bitstream, and
    // even a Kaiser window's main lobe reaches the first few bins, so an unmasked bin 4
    // reads the signal rather than the noise. (It looks damning, too: it scales as DC^2
    // and hits 68 dB at a 1e-3 offset. It is not an idle tone. The real one is at
    // 1410 Hz.)
    let peakiness = |bits: &[f64], skip: Option<usize>| -> (f64, f64) {
        let psd = spectrum(bits);
        let mut band = Vec::new();
        let (mut peak, mut peak_hz) = (0.0f64, 0.0);
        for (k, &p) in psd.iter().enumerate() {
            let f = k as f64 * fs / N as f64;
            if !(BAND_LO..=BAND_HI).contains(&f)
                || k <= TONE_BINS
                || skip.is_some_and(|s| k.abs_diff(s) <= TONE_BINS)
            {
                continue;
            }
            if p > peak {
                peak = p;
                peak_hz = f;
            }
            band.push(p);
        }
        band.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (10.0 * (peak / band[band.len() / 2]).log10(), peak_hz)
    };

    let quiet = tone(N, fs, 1000.0, 10f64.powf(-90.0 / 20.0));
    let quieter = tone(N, fs, 997.0, 10f64.powf(-120.0 / 20.0));
    println!("\n  signal             dithered            undithered          delta");
    for (label, input, skip, needs_dither) in [
        ("digital silence", vec![0.0; N], None, false),
        ("-90 dBFS 1 kHz", quiet.0, Some(quiet.1), false),
        ("-120 dBFS 997 Hz", quieter.0, Some(quieter.1), false),
        ("DC 1e-4", vec![1e-4; N], None, false),
        ("DC 1e-3", vec![1e-3; N], None, true),
        ("DC 1e-2", vec![1e-2; N], None, true),
    ] {
        let (dithered, _) = modulate(rate, &input, SEED);
        let mut plain = Modulator::with_quantizer_dither(rate, Some(SEED), 0.0);
        let undithered: Vec<f64> = plain.modulate(&input).into_iter().map(level).collect();

        let (d, d_hz) = peakiness(&dithered, skip);
        let (u, u_hz) = peakiness(&undithered, skip);
        println!("  {label:<18} {d:5.1} dB @{d_hz:7.0} Hz  {u:5.1} dB @{u_hz:7.0} Hz  {:+5.1} dB", d - u);

        // The production gate: the shipped configuration shows no discrete spike over its
        // own noise floor. ~14 dB is what a tone-free periodogram gives on its own.
        assert!(
            d < 25.0,
            "{label}: a {d:.1} dB spike at {d_hz:.0} Hz over the in-band median is an idle tone"
        );
        // And on the probes that *do* provoke a limit cycle, the dither has to be why it
        // is gone — otherwise the gate above is passing for some unrelated reason and
        // QUANTIZER_DITHER could be silently wrong.
        if needs_dither {
            assert!(
                u - d > 8.0,
                "{label}: undithered reads {u:.1} dB and dithered {d:.1} dB — the \
                 comparator dither is not what is suppressing the idle tone"
            );
        }
    }
    println!();
}

// ══════════════════════════════ 6. stability ═════════════════════════════════

#[test]
fn legal_signals_never_clamp_and_overdrive_recovers() {
    let rate = DsdRate::Dsd64;
    let fs = rate.hz() as f64;
    let ntf = ntf_for(rate);
    let n = 1 << 17;

    // Everything the pipeline can actually produce. PCM is bounded at +-1 and the ceiling
    // stage caps the true peak at 0 dBFS, so 50 % modulation is the hard operating limit —
    // and nothing at or below it may clamp. The 60 % probe is the margin check: the
    // measured MSA is 0.64-0.69 depending on signal shape, so +1.6 dB over the operating
    // point must still be clean.
    let hostile: [(&str, Vec<f64>); 4] = [
        ("0 dBFS 1 kHz", tone(n, fs, 1000.0, 1.0).0),
        ("0 dBFS square", (0..n).map(|i| if (i / 1411) % 2 == 0 { 1.0 } else { -1.0 }).collect()),
        ("0 dBFS DC", vec![1.0; n]),
        ("60 % modulation", tone(n, fs, 100.0, 0.6 / INPUT_SCALE).0),
    ];
    println!("\n  signal                 max|x|/limit  overload  resets");
    for (label, pcm) in hostile {
        let (bits, m) = modulate(rate, &pcm, SEED);
        let s = m.stats();
        let headroom = m
            .states()
            .iter()
            .zip(&ntf.state_limit)
            .map(|(x, l)| x.abs() / l)
            .fold(0.0f64, f64::max);
        println!(
            "  {label:<22} {headroom:9.1e}     {:<9} {}",
            s.overload_samples, s.resets
        );
        assert!(bits.iter().all(|v| v.is_finite()));
        assert_eq!(
            s.overload_samples,
            0,
            "{label} is inside the format and must not clamp"
        );
    }

    // Past the maximum stable amplitude — which includes the format's own 71.43 % ceiling,
    // reachable only from PCM hotter than 0 dBFS. It *will* overload; what matters is that
    // it stays finite, stays inside the clamps, and comes back when the input is sane.
    let mut m = Modulator::new(rate, Some(SEED));
    let at_format_max = m.modulate(&tone(n, fs, 100.0, MAX_MODULATION / INPUT_SCALE).0);
    assert!(at_format_max.len() == n);
    for (x, limit) in m.states().iter().zip(&ntf.state_limit) {
        assert!(x.is_finite() && x.abs() <= limit * 1.000_001);
    }
    println!(
        "  {:<22} overload {} / {n} (expected: past MSA)",
        "71.4 % format max",
        m.stats().overload_samples
    );

    let mut m = Modulator::new(rate, Some(SEED));
    let overdrive = tone(n, fs, 100.0, 1.8).0; // 90 % modulation, far past MSA (~0.69)
    let bits = m.modulate(&overdrive);
    assert!(bits.len() == n);
    let during = m.stats();
    assert!(
        during.overload_samples > 0,
        "90 % modulation should have tripped the clamp"
    );
    for (x, limit) in m.states().iter().zip(&ntf.state_limit) {
        assert!(
            x.is_finite() && x.abs() <= limit * 1.000_001,
            "state {x:e} escaped its clamp {limit:e}"
        );
    }
    println!(
        "  {:<22} overload {} / {n}, resets {}",
        "90 % overdrive", during.overload_samples, during.resets
    );

    // Recovery: same instance, now fed a normal tone. It has to measure like a fresh one.
    let (pcm, bin) = tone(N, fs, 1000.0, 1.0);
    let recovered: Vec<f64> = m.modulate(&pcm).into_iter().map(level).collect();
    let r = analyse(&spectrum(&recovered), fs, N, bin, 0.5, 0.5);
    println!("  after overdrive: SNR back to {:.1} dB\n", r.snr_db);
    assert!(
        r.snr_db > 110.0,
        "modulator did not recover from overdrive (SNR {:.1} dB)",
        r.snr_db
    );
}

// ═══════════════════════════ 7. modulation index ═════════════════════════════

#[test]
fn modulation_index_tracks_the_ceiling() {
    let rate = DsdRate::Dsd64;
    let fs = rate.hz() as f64;

    for (ceiling_dbtp, expect_percent) in [(-1.0, 44.6), (-0.3, 48.3), (0.0, 50.0)] {
        let amp = 10f64.powf(ceiling_dbtp / 20.0);
        let (_, m) = modulate(rate, &tone(1 << 14, fs, 997.0, amp).0, SEED);
        let got = m.stats().max_modulation_percent();
        println!("  ceiling {ceiling_dbtp:+.1} dBTP -> {got:.1} % modulation");
        assert!(
            (got - expect_percent).abs() < 0.2,
            "ceiling {ceiling_dbtp} dBTP gave {got:.2} %, expected ~{expect_percent} %"
        );
        assert!(
            !m.stats().exceeds_format_maximum(),
            "a legal ceiling must stay inside the 20/28 rule"
        );
    }

    // And the flag has to actually fire when it should.
    let hot = tone(1 << 14, fs, 997.0, 1.5).0;
    let (_, m) = modulate(rate, &hot, SEED);
    assert!(
        m.stats().exceeds_format_maximum(),
        "75 % modulation must be reported as over the format maximum"
    );
    println!(
        "  +3.5 dBFS input -> {:.1} % modulation, flagged\n",
        m.stats().max_modulation_percent()
    );
}

// ════════════════════════ 8. the table's own claims ══════════════════════════

#[test]
fn ntf_table_matches_its_design_targets() {
    println!("\n  rate     sup|NTF|  in-band max   DC residue  decay");
    for rate in DsdRate::ALL {
        let ntf = ntf_for(rate);
        let fs = rate.hz() as f64;

        let mut sup = 0.0f64;
        let mut in_band = 0.0f64;
        for k in 1..=20_000 {
            let w = PI * k as f64 / 20_000.0;
            let mag = ntf_mag(ntf, w);
            sup = sup.max(mag);
            if w * fs / (2.0 * PI) <= BAND_HI {
                in_band = in_band.max(mag);
            }
        }
        let (num, den) = ntf_polynomials(ntf);

        // The DC zero, checked structurally rather than by evaluating |NTF(1)|. Both N(1)
        // and D(1) are vanishingly small for a high-pass NTF whose poles cluster near
        // z = 1, so their *ratio* at DC is pure cancellation noise and says nothing. That
        // `(z - 1)` divides the numerator — which is the actual claim — is exactly the
        // statement that its coefficients sum to zero.
        let dc = num.iter().sum::<f64>().abs() / num.iter().fold(0.0f64, |m, c| m.max(c.abs()));

        // Poles inside the unit circle <=> the impulse response dies. Read it off the
        // denominator by long division rather than trusting the design script.
        let mut h = vec![0.0; 4096];
        for n in 0..h.len() {
            let mut v = if n < num.len() { num[n] } else { 0.0 };
            for (k, d) in den.iter().enumerate().skip(1) {
                if n >= k {
                    v -= d * h[n - k];
                }
            }
            h[n] = v;
        }
        let decay = h[h.len() - 64..].iter().fold(0.0f64, |m, v| m.max(v.abs()));

        println!(
            "  {:<7} {sup:8.4}  {:9.1} dB  {:9.1e}  {decay:9.1e}",
            rate.tag(),
            20.0 * in_band.log10(),
            dc
        );

        // Lee criterion, and the value the table was designed to.
        assert!(sup < 2.0, "{}: sup|NTF| {sup:.3} violates Lee", rate.tag());
        assert!(
            (sup - 1.30).abs() < 0.02,
            "{}: sup|NTF| is {sup:.4}, the table claims 1.30",
            rate.tag()
        );
        assert!(
            20.0 * in_band.log10() < -80.0,
            "{}: in-band NTF only reaches {:.1} dB",
            rate.tag(),
            20.0 * in_band.log10()
        );
        assert!(
            dc < 1e-12,
            "{}: (z - 1) does not divide the NTF numerator (residue {dc:.2e}) — the zero \
             at DC is missing, and DC accuracy goes with it",
            rate.tag()
        );
        assert!(
            decay < 1e-9,
            "{}: NTF impulse response has not decayed ({decay:.2e}) — a pole is on or \
             outside the unit circle",
            rate.tag()
        );
    }
    println!();
}

// ══════════════════════════════ 9. determinism ═══════════════════════════════

#[test]
fn same_seed_gives_the_same_bitstream() {
    let rate = DsdRate::Dsd64;
    let (pcm, _) = tone(1 << 16, rate.hz() as f64, 997.0, 0.9);

    let a = Modulator::new(rate, Some(SEED)).modulate(&pcm);
    let b = Modulator::new(rate, Some(SEED)).modulate(&pcm);
    assert_eq!(a, b, "the modulator is not reproducible from its seed");

    let c = Modulator::new(rate, Some(SEED + 1)).modulate(&pcm);
    assert_ne!(a, c, "the seed does not reach the comparator dither");

    // Per-channel derivation: reproducible, but independent between channels.
    let l = Modulator::for_channel(rate, Some(SEED), 0).modulate(&pcm);
    let r = Modulator::for_channel(rate, Some(SEED), 1).modulate(&pcm);
    assert_eq!(l, Modulator::for_channel(rate, Some(SEED), 0).modulate(&pcm));
    assert_ne!(l, r, "both channels drew the same dither stream");

    // Blocking must not change a single bit — the DSD stage feeds the modulator in DSF
    // sized chunks, and the loop state has to carry across them.
    let mut m = Modulator::new(rate, Some(SEED));
    let mut chunked = Vec::new();
    for chunk in pcm.chunks(1000) {
        m.modulate_into(chunk, &mut chunked);
    }
    assert_eq!(a, chunked, "block boundaries changed the bitstream");
}

// ═══════════════════ 10. is the instrument good enough? ══════════════════════

#[test]
fn decimator_is_transparent_enough_to_measure_the_modulator() {
    let rate = DsdRate::Dsd64;
    let fs = rate.hz() as f64;
    let dec = Decimator::for_rates(fs as u32, 88_200).unwrap();

    // Stopband: this is the number that decides whether the ultrasonic noise a DSD stream
    // is mostly made of folds back into the reading.
    let worst_stop = (0..=1000)
        .map(|i| 0.30 + 0.20 * i as f64 / 1000.0)
        .map(|f| dec.stage_response(f))
        .fold(0.0f64, f64::max);
    let passband_ripple = (0..=1000)
        .map(|i| 0.20 * i as f64 / 1000.0)
        .map(|f| (dec.stage_response(f) - 1.0).abs())
        .fold(0.0f64, f64::max);
    println!(
        "\n  half-band: {} taps, stopband {:.1} dB, passband ripple {:.1} dB",
        dec.taps_per_stage(),
        20.0 * worst_stop.log10(),
        20.0 * passband_ripple.log10()
    );
    assert!(
        20.0 * worst_stop.log10() < -150.0,
        "stage stopband is only {:.1} dB; folded DSD noise would show up as modulator noise",
        20.0 * worst_stop.log10()
    );

    // End to end on a signal with no quantization in it at all, measured *differentially*
    // against the tone the cascade should have reproduced. Not through an FFT: a Kaiser
    // window bottoms out around -170 dB once its sidelobes are summed across the band, so
    // a spectral reading here would be measuring the window. Subtracting the expected
    // waveform has no such floor.
    let (pure, cycles) = tone(N, fs, 1000.0, 0.5);
    let out = dec.process(&pure);
    // Decimation is zero-delay, so out[m] should be the input at m*factor, i.e. the same
    // tone with the same cycle count over the shorter buffer.
    let expected = |m: usize| (2.0 * PI * cycles as f64 * m as f64 / out.len() as f64).sin() * 0.5;

    // Skip the zero-extended edges: the cascade's impulse response spans roughly
    // taps * (2^stages - 1) input samples, which is a couple of hundred output samples.
    let guard = 512;
    let err = out[guard..out.len() - guard]
        .iter()
        .enumerate()
        .map(|(i, v)| (v - expected(i + guard)).powi(2))
        .sum::<f64>()
        / (out.len() - 2 * guard) as f64;
    let transparency = 10.0 * ((0.5f64.powi(2) / 2.0) / err).log10();
    println!("  end-to-end on an undithered f64 tone: {transparency:.1} dB below signal\n");
    assert!(
        transparency > 160.0,
        "the decimator's own error is only {transparency:.1} dB down — too close to the \
         ~123 dB in-band noise it has to measure"
    );
}

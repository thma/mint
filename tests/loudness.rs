use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::LimiterCharacter;
use mint::config::OnClipPolicy;
use mint::ops::{limiter, loudness};

fn limiter_opts() -> limiter::LimiterOptions {
    limiter::LimiterOptions {
        character: LimiterCharacter::Balanced,
        soft_clip: false,
        link_channels: true,
    }
}

fn sine_buffer(rate: u32, seconds: f64, hz: f64, amp: f64) -> AudioBuffer {
    let frames = (rate as f64 * seconds) as usize;
    let mut ch = Vec::with_capacity(frames);
    for n in 0..frames {
        let t = n as f64 / rate as f64;
        ch.push((2.0 * std::f64::consts::PI * hz * t).sin() * amp);
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

fn sparse_peak_buffer(rate: u32, seconds: f64, peak: f64) -> AudioBuffer {
    let frames = (rate as f64 * seconds) as usize;
    let mut ch = vec![0.0; frames];
    if frames > 100 {
        ch[10] = peak;
        ch[60] = -peak;
    }

    AudioBuffer {
        channels: vec![ch],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("sparse.wav"),
            channels: 1,
            sample_rate: rate,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

/// A steady carrier with short loud clicks: the loudness lives in the carrier, the
/// true peak in the clicks — exactly the shape where limiting eats integrated loudness
/// and single-shot normalization undershoots a hot target. One click per second, so the
/// limiter's release completes between clicks and makeup gain genuinely buys loudness
/// back (denser clicks saturate: the gain never recovers and added gain buys ~nothing,
/// which is the capped case, not the convergent one).
fn carrier_with_clicks(rate: u32, seconds: f64, carrier_amp: f64, click: f64) -> AudioBuffer {
    let mut buffer = sine_buffer(rate, seconds, 997.0, carrier_amp);
    let frames = buffer.channels[0].len();
    let period = rate as usize; // 1 click per second
    for i in (period..frames).step_by(period) {
        for j in i..(i + 3).min(frames) {
            buffer.channels[0][j] = click;
        }
    }
    buffer
}

#[test]
fn maximize_converges_to_target_where_limit_undershoots() {
    let ceiling = -1.0;
    // Pick a target 6 dB above the source: the clicks then need ~6 dB of limiting,
    // which eats enough loudness that single-shot normalization visibly undershoots.
    let source = carrier_with_clicks(48_000, 6.0, 0.15, 0.95);
    let in_lufs = loudness::analyze(&source).expect("analyze").integrated_lufs;
    let target = in_lufs + 6.0;

    let mut limit_buffer = carrier_with_clicks(48_000, 6.0, 0.15, 0.95);
    let limit_result = loudness::apply(
        &mut limit_buffer,
        target,
        Some(ceiling),
        Some(OnClipPolicy::Limit),
        limiter_opts(),
        8.0,
    )
    .expect("limit apply should succeed");
    assert!(
        target - limit_result.out_lufs > 0.1,
        "test premise: plain limit must undershoot (delivered {:.2}, target {target:.2})",
        limit_result.out_lufs
    );

    let mut max_buffer = carrier_with_clicks(48_000, 6.0, 0.15, 0.95);
    // Generous cap: this test is about convergence, not the cap (covered below).
    let max_result = loudness::apply(
        &mut max_buffer,
        target,
        Some(ceiling),
        Some(OnClipPolicy::Maximize),
        limiter_opts(),
        12.0,
    )
    .expect("maximize apply should succeed");

    assert!(
        (max_result.out_lufs - target).abs() <= 0.1,
        "maximize must hit the target ±0.1 LU (delivered {:.2}, target {target:.2})",
        max_result.out_lufs
    );
    assert!(
        max_result.true_peak_out_dbtp <= ceiling + 0.1,
        "ceiling must hold under maximization (got {:.3})",
        max_result.true_peak_out_dbtp
    );
    assert!(
        max_result.limiter_gain_reduction_db > limit_result.limiter_gain_reduction_db,
        "makeup gain must have bought extra limiting"
    );
    // Honesty stats: some but not all of the signal was limited, at a mean depth
    // that cannot exceed the peak depth.
    assert!(max_result.limiter_active_ratio > 0.0);
    assert!(max_result.limiter_active_ratio <= 1.0);
    assert!(max_result.limiter_avg_gain_reduction_db > 0.0);
    assert!(
        max_result.limiter_avg_gain_reduction_db
            <= max_result.limiter_gain_reduction_db + 1e-9
    );
}

#[test]
fn maximize_respects_max_limiting_db_cap_and_warns() {
    let ceiling = -1.0;
    let cap = 2.0;
    let mut buffer = carrier_with_clicks(48_000, 6.0, 0.15, 0.95);
    let in_lufs = loudness::analyze(&buffer).expect("analyze").integrated_lufs;
    let target = in_lufs + 12.0; // absurd: would need ~12 dB of peak reduction

    let result = loudness::apply(
        &mut buffer,
        target,
        Some(ceiling),
        Some(OnClipPolicy::Maximize),
        limiter_opts(),
        cap,
    )
    .expect("maximize apply should succeed");

    // The gain was clamped so peak GR lands exactly on the cap.
    assert!(
        (result.limiter_gain_reduction_db - cap).abs() < 1e-6,
        "peak GR {:.3} should sit exactly at the cap {cap:.1}",
        result.limiter_gain_reduction_db
    );
    assert!(
        target - result.out_lufs > 0.1,
        "capped maximization must fall short of the target"
    );
    assert!(
        result.warnings.iter().any(|w| w.contains("maximization capped")),
        "cap warning expected, got: {:?}",
        result.warnings
    );
    assert!(result.true_peak_out_dbtp <= ceiling + 0.1);
}

#[test]
fn maximize_without_ceiling_breach_hits_target_in_one_pass() {
    // Quiet source, quiet target: no limiting → plain normalization, exact landing.
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.1);
    let result = loudness::apply(
        &mut buffer,
        -20.0,
        Some(-1.0),
        Some(OnClipPolicy::Maximize),
        limiter_opts(),
        8.0,
    )
    .expect("maximize apply should succeed");

    assert_eq!(result.limiter_gain_reduction_db, 0.0);
    assert_eq!(result.limiter_active_ratio, 0.0);
    assert!(
        (result.out_lufs - (-20.0)).abs() <= 0.05,
        "no-limiting maximize is plain normalization (got {:.3})",
        result.out_lufs
    );
    assert!(result.warnings.is_empty(), "unexpected warnings: {:?}", result.warnings);
}

#[test]
fn limit_reports_avg_gain_reduction_and_duty_cycle() {
    // A steady sine pushed above the ceiling is limited essentially everywhere:
    // duty cycle near 1, mean depth positive but never above the peak depth.
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.9);
    let result = loudness::apply(
        &mut buffer,
        -1.0,
        Some(-1.0),
        Some(OnClipPolicy::Limit),
        limiter_opts(),
        8.0,
    )
    .expect("loudness apply should succeed");

    assert!(result.limiter_gain_reduction_db > 0.0, "premise: limiting must run");
    assert!(
        result.limiter_active_ratio > 0.9,
        "steady over-ceiling sine should be limited nearly everywhere (got {:.3})",
        result.limiter_active_ratio
    );
    assert!(result.limiter_avg_gain_reduction_db > 0.0);
    assert!(
        result.limiter_avg_gain_reduction_db <= result.limiter_gain_reduction_db + 1e-9,
        "mean GR {:.3} cannot exceed peak GR {:.3}",
        result.limiter_avg_gain_reduction_db,
        result.limiter_gain_reduction_db
    );
}

#[test]
fn loudness_moves_towards_target() {
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.1);
    let target = -16.0;

    let result = loudness::apply(
        &mut buffer,
        target,
        Some(-1.0),
        Some(OnClipPolicy::Warn),
        limiter_opts(),
        8.0,
    )
    .expect("loudness apply should succeed");

    let before = (result.in_lufs - target).abs();
    let after = (result.out_lufs - target).abs();
    assert!(after < before, "loudness should move closer to target");
}

#[test]
fn analyze_reports_sane_dynamics_metrics() {
    let buffer = sine_buffer(48_000, 4.0, 997.0, 0.5);
    let a = loudness::analyze(&buffer).expect("analyze should succeed");

    assert!(a.integrated_lufs.is_finite(), "integrated should be finite");
    assert!(a.true_peak_dbtp.is_finite(), "true peak should be finite");
    assert!(a.max_momentary_lufs.is_finite(), "momentary max should be finite");
    assert!(
        a.max_short_term_lufs.is_finite(),
        "short-term max should be finite for a >3 s tone"
    );
    // A steady tone has essentially no loudness range.
    assert!(
        (0.0..1.0).contains(&a.loudness_range_lu),
        "steady-tone LRA {} should be ~0",
        a.loudness_range_lu
    );
    // Crest figures are exact by definition.
    assert!((a.plr() - (a.true_peak_dbtp - a.integrated_lufs)).abs() < 1e-9);
    assert!((a.psr() - (a.true_peak_dbtp - a.max_short_term_lufs)).abs() < 1e-9);
    // For a steady sine the loudest short-term window sits near the integrated value.
    assert!((a.max_short_term_lufs - a.integrated_lufs).abs() < 1.0);
}

#[test]
fn analyze_loudness_range_grows_with_dynamics() {
    // Steady tone: essentially no loudness range.
    let steady = sine_buffer(48_000, 12.0, 997.0, 0.5);
    let steady_lra = loudness::analyze(&steady).unwrap().loudness_range_lu;

    // A quiet half followed by a loud half spans a wide loudness range.
    let mut dynamic = sine_buffer(48_000, 6.0, 997.0, 0.1);
    let loud = sine_buffer(48_000, 6.0, 997.0, 0.7);
    dynamic.channels[0].extend_from_slice(&loud.channels[0]);
    let dynamic_lra = loudness::analyze(&dynamic).unwrap().loudness_range_lu;

    assert!(
        dynamic_lra > steady_lra + 3.0,
        "dynamic LRA {dynamic_lra} should exceed steady LRA {steady_lra} by several LU"
    );
    assert!(dynamic_lra > 5.0, "expected a sizable loudness range, got {dynamic_lra}");
}

#[test]
fn limit_policy_reports_gain_reduction_when_squashing_to_target() {
    // A loud source pushed louder must be limited; the reported gain reduction is
    // the headline "how hard was it squashed" number.
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.9);
    let ceiling = -1.0;

    let result = loudness::apply(
        &mut buffer,
        -1.0,
        Some(ceiling),
        Some(OnClipPolicy::Limit),
        limiter_opts(),
        8.0,
    )
    .expect("loudness apply should succeed");

    assert!(
        result.limiter_gain_reduction_db > 0.0,
        "limiting should have been applied (got {:.3} dB GR)",
        result.limiter_gain_reduction_db
    );
    // GR is exactly how far the post-gain peak sat above the ceiling.
    let expected = (result.true_peak_in_dbtp + result.gain_db) - ceiling;
    assert!(
        (result.limiter_gain_reduction_db - expected).abs() < 1e-9,
        "GR {:.6} should equal predicted_tp - ceiling {:.6}",
        result.limiter_gain_reduction_db,
        expected
    );
    // ...and the limiter actually held the ceiling (within its convergence tolerance;
    // precise ceiling-holding is covered by the dedicated limiter tests).
    assert!(
        result.true_peak_out_dbtp <= ceiling + 0.1,
        "true peak {:.3} should be at or near ceiling {ceiling:.3}",
        result.true_peak_out_dbtp
    );
}

#[test]
fn limit_policy_reports_zero_gain_reduction_when_within_ceiling() {
    // Quiet source raised by a few dB stays well under the ceiling: no limiting.
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.1);

    let result = loudness::apply(
        &mut buffer,
        -20.0,
        Some(-1.0),
        Some(OnClipPolicy::Limit),
        limiter_opts(),
        8.0,
    )
    .expect("loudness apply should succeed");

    assert_eq!(
        result.limiter_gain_reduction_db, 0.0,
        "no limiting expected when the post-gain peak stays under the ceiling"
    );
}

#[test]
fn reduce_gain_policy_caps_predicted_peak() {
    let mut buffer = sparse_peak_buffer(48_000, 3.0, 0.95);
    let target = -16.0;

    let result = loudness::apply(
        &mut buffer,
        target,
        Some(-1.0),
        Some(OnClipPolicy::ReduceGain),
        limiter_opts(),
        8.0,
    )
    .expect("loudness apply should succeed");

    assert!(
        result.true_peak_out_dbtp <= -0.8,
        "true peak should remain close to configured ceiling"
    );
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("reduce_gain applied")),
        "reduce_gain warning should be emitted"
    );
}

#[test]
fn limiter_holds_ceiling_against_bs1770_meter_for_inter_sample_peaks() {
    // A high-frequency, near-full-scale sine has true (inter-sample) peaks well above
    // its sample peaks — the case linear-interp detection misses. After enforcement,
    // the BS.1770 meter itself must agree the ceiling is held.
    let ceiling = -1.0;
    let mut buffer = sine_buffer(48_000, 1.0, 11_000.0, 0.97);

    let before = loudness::measure_true_peak_dbtp(&buffer).expect("measure");
    assert!(before > ceiling, "test needs a source above the ceiling (got {before:.2})");

    let warnings = loudness::enforce_ceiling(
        &mut buffer,
        ceiling,
        OnClipPolicy::Limit,
        limiter_opts(),
    )
    .expect("enforce should succeed");
    assert!(!warnings.is_empty(), "limiter ran; expected a warning");

    let after = loudness::measure_true_peak_dbtp(&buffer).expect("measure");
    // 0.05 dB tolerance matches the verifier; allow a hair more for measurement noise.
    assert!(
        after <= ceiling + 0.1,
        "true peak {after:.3} dBTP must be held at/under ceiling {ceiling:.3} (was {before:.3})"
    );
}

#[test]
fn recheck_applies_limiter_when_ceiling_exceeded() {
    let ceiling_dbtp = -1.0;
    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);

    // Build a buffer with one sample pushed 5% above the ceiling.
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.1);
    buffer.channels[0][100] = ceiling_lin * 1.05;

    let warn = loudness::recheck_and_limit_if_needed(&mut buffer, ceiling_dbtp, limiter_opts())
        .expect("recheck should succeed");

    assert!(!warn.is_empty(), "limiter was applied; must produce a warning");
    let max_sample = buffer.channels[0].iter().map(|s| s.abs()).fold(0.0_f64, f64::max);
    assert!(
        max_sample <= ceiling_lin + 1e-6,
        "sample {max_sample:.6} must be at or below ceiling {ceiling_lin:.6} after re-check"
    );
}

#[test]
fn enforce_ceiling_limits_transcode_without_loudness() {
    // No loudness normalization happened, but a sample sits above the ceiling.
    let ceiling_dbtp = -1.0;
    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let mut buffer = sine_buffer(48_000, 1.0, 997.0, 0.1);
    buffer.channels[0][100] = ceiling_lin * 1.10;

    let warnings = loudness::enforce_ceiling(
        &mut buffer,
        ceiling_dbtp,
        OnClipPolicy::Limit,
        limiter_opts(),
    )
    .expect("enforce should succeed");

    assert!(!warnings.is_empty(), "limiter ran; expected a warning");
    let max_sample = buffer.channels[0].iter().map(|s| s.abs()).fold(0.0, f64::max);
    assert!(
        max_sample <= ceiling_lin + 1e-6,
        "peak {max_sample:.6} must be at or below ceiling {ceiling_lin:.6}"
    );
}

#[test]
fn enforce_ceiling_warn_policy_leaves_samples_untouched() {
    let ceiling_dbtp = -1.0;
    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let mut buffer = sine_buffer(48_000, 1.0, 997.0, 0.1);
    buffer.channels[0][100] = ceiling_lin * 1.10;
    let before = buffer.channels[0][100];

    let warnings = loudness::enforce_ceiling(
        &mut buffer,
        ceiling_dbtp,
        OnClipPolicy::Warn,
        limiter_opts(),
    )
    .expect("enforce should succeed");

    assert!(warnings.iter().any(|w| w.contains("on_clip=warn")));
    assert_eq!(buffer.channels[0][100], before, "warn must not alter samples");
}

#[test]
fn recheck_leaves_buffer_unchanged_when_within_ceiling() {
    let mut buffer = sine_buffer(48_000, 3.0, 997.0, 0.5);
    let sum_before: f64 = buffer.channels[0].iter().sum();

    let warn = loudness::recheck_and_limit_if_needed(&mut buffer, -1.0, limiter_opts())
        .expect("recheck should succeed");

    assert!(warn.is_empty(), "no warning expected when signal is within ceiling");
    let sum_after: f64 = buffer.channels[0].iter().sum();
    assert!(
        (sum_before - sum_after).abs() < 1e-10,
        "buffer must be untouched when already within ceiling"
    );
}

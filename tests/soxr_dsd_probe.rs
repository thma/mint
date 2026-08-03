//! Phase-1 probe for the DSD interpolation stage: can libsoxr carry the whole
//! PCM -> DSD-rate range on its own, or does the ratio have to be staged?
//!
//! DSD rates are 44 100 x {64, 128, 256, 512}. Feeding the modulator means interpolating
//! from a 44.1-family PCM rate up to the DSD rate, so the ratio spans 32 (352.8k ->
//! DSD256) to 512 (44.1k -> DSD512) — see `docs/dsd-design.md` section 2.1. libsoxr
//! builds its polyphase/DFT decomposition from the rate pair, and nothing in mint has
//! ever asked it for a ratio above 4, let alone 512.
//!
//! This is a measurement, not an assertion harness: it prints a table and only fails on
//! outright breakage (error, wrong length, mangled signal). Run it explicitly:
//!
//!     cargo test --release --test soxr_dsd_probe -- --ignored --nocapture
//!
//! Numbers are recorded in `docs/dsd-design.md`; re-run and update them there if the
//! libsoxr version or the quality mapping changes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::ResampleQuality;
use mint::ops::dsd::interpolate::SoxrStream;
use mint::ops::resample;

/// Output block the DSD loop works in: one DSF block is 4096 bytes per channel = 32 768
/// 1-bit samples. Sizing the loop in *DSD* samples rather than PCM frames keeps the
/// scratch constant across ratios — the PCM block shrinks as the ratio grows.
const DSD_BLOCK_FRAMES: usize = 32_768;

/// Every (PCM rate -> DSD rate) pair the derivation rule in section 2.1 can produce.
const CASES: &[(u32, u32)] = &[
    // -> DSD64 (2 822 400)
    (44_100, 2_822_400),
    (88_200, 2_822_400),
    // -> DSD128 (5 644 800)
    (44_100, 5_644_800),
    (88_200, 5_644_800),
    (176_400, 5_644_800),
    // -> DSD256 (11 289 600)
    (88_200, 11_289_600),
    (176_400, 11_289_600),
    (352_800, 11_289_600),
    // -> DSD512 (22 579 200)
    (44_100, 22_579_200), // ratio 512: legal, silly, must not fall over
    (176_400, 22_579_200),
    (352_800, 22_579_200),
];

/// Output frames per channel to process per case. Sets both the runtime and the peak
/// memory of the probe (`apply` is one-shot: ~16 bytes per output frame per channel for
/// the interleaved scratch plus the planar result).
const TARGET_OUT_FRAMES: usize = 4_000_000;

const CHANNELS: usize = 2;
const AMPLITUDE: f64 = 0.5;

/// A tone near `approx_hz` that fits `frames` with an *integer* number of cycles.
///
/// Without this the buffer ends mid-cycle, and the implicit step to silence makes the
/// interpolation filter ring: at 88.2k -> DSD256 the tone happened to be cut off at its
/// crest and the output overshot by 1.1 dB. That is an artefact of the stimulus, not of
/// libsoxr — an exactly periodic tone removes it.
fn periodic_hz(rate: u32, frames: usize, approx_hz: f64) -> f64 {
    let cycles = (frames as f64 * approx_hz / rate as f64).round().max(1.0);
    cycles * rate as f64 / frames as f64
}

fn stereo_buffer(rate: u32, frames: usize, hz: f64) -> AudioBuffer {
    let channel: Vec<f64> = (0..frames)
        .map(|n| (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin() * AMPLITUDE)
        .collect();
    AudioBuffer {
        channels: vec![channel; CHANNELS],
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("probe.wav"),
            channels: CHANNELS,
            sample_rate: rate,
            codec: "test".into(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

fn time_apply(pcm_rate: u32, dsd_rate: u32, in_frames: usize) -> (Duration, AudioBuffer) {
    let hz = periodic_hz(pcm_rate, in_frames, 997.0);
    let mut buffer = stereo_buffer(pcm_rate, in_frames, hz);
    let start = Instant::now();
    resample::apply(&mut buffer, dsd_rate, Some(ResampleQuality::Vhq))
        .unwrap_or_else(|e| panic!("{pcm_rate} -> {dsd_rate} failed: {e}"));
    (start.elapsed(), buffer)
}

/// The same conversion the way the DSD stage will actually run it: one stream over the
/// whole file, fed in blocks, output consumed and dropped rather than accumulated.
/// Returns the elapsed time, the frames produced, and a checksum so nothing is optimized
/// away.
fn time_stream(
    pcm_rate: u32,
    dsd_rate: u32,
    in_frames: usize,
    pcm_block: usize,
) -> (Duration, u64, f64) {
    let hz = periodic_hz(pcm_rate, in_frames, 997.0);
    let source = stereo_buffer(pcm_rate, in_frames, hz);

    let start = Instant::now();
    let mut stream = SoxrStream::new(pcm_rate, dsd_rate, CHANNELS, Some(ResampleQuality::Vhq))
        .unwrap_or_else(|e| panic!("{pcm_rate} -> {dsd_rate} stream: {e}"));
    let mut checksum = 0.0f64;
    let mut pos = 0;
    while pos < in_frames {
        let n = pcm_block.min(in_frames - pos);
        let chunk: Vec<Vec<f64>> = source
            .channels
            .iter()
            .map(|c| c[pos..pos + n].to_vec())
            .collect();
        for got in stream.process(&chunk).expect("stream process") {
            checksum += got.first().copied().unwrap_or(0.0);
        }
        pos += n;
    }
    for got in stream.finish().expect("stream finish") {
        checksum += got.first().copied().unwrap_or(0.0);
    }
    (start.elapsed(), stream.out_frames_total(), checksum)
}

#[test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
fn soxr_handles_the_full_dsd_ratio_range() {
    println!();
    println!(
        "{:>9} {:>9} {:>6} {:>8} {:>9} {:>9} {:>8} {:>10} {:>10} {:>7}",
        "PCM kHz", "DSD MHz", "ratio", "PCM blk", "setup ms", "1shot ms", "strm ms", "strm xRT",
        "resident", "peak"
    );
    println!("{}", "-".repeat(97));

    // Warm up allocator and libsoxr's process-wide tables so the first row measures the
    // same thing as the rest.
    time_apply(44_100, 2_822_400, 8192);

    for &(pcm_rate, dsd_rate) in CASES {
        let ratio = dsd_rate / pcm_rate;
        assert!(
            ratio.is_power_of_two() && dsd_rate % pcm_rate == 0,
            "{pcm_rate} -> {dsd_rate} is not an integer power-of-two ratio"
        );

        let in_frames = (TARGET_OUT_FRAMES / ratio as usize).max(1024);

        // Best of N: these runs are short enough that a single scheduling hiccup shows
        // up as a 40% outlier. The minimum is the honest figure for a throughput claim.
        const RUNS: usize = 5;

        // A near-empty run isolates the fixed cost: libsoxr builds its filter bank in
        // `Soxr::create`, which is what a per-file streaming wrapper pays exactly once.
        let setup = (0..RUNS)
            .map(|_| time_apply(pcm_rate, dsd_rate, 1024).0)
            .min()
            .expect("RUNS > 0");
        let (mut total, mut out) = time_apply(pcm_rate, dsd_rate, in_frames);
        for _ in 1..RUNS {
            let (t, o) = time_apply(pcm_rate, dsd_rate, in_frames);
            if t < total {
                total = t;
                out = o;
            }
        }

        let expected = (in_frames as f64 * ratio as f64).round() as usize;
        assert_eq!(out.frame_len(), expected, "{pcm_rate} -> {dsd_rate}: length contract broken");
        assert_eq!(out.sample_rate, dsd_rate);

        // Interpolation must not touch the signal: the tone keeps its amplitude.
        // (Guards against a silently mis-decomposed ratio producing noise or silence.)
        let (peak_at, peak) = out.channels[0]
            .iter()
            .enumerate()
            .fold((0usize, 0.0f64), |(i, m), (j, v)| {
                if v.abs() > m { (j, v.abs()) } else { (i, m) }
            });
        assert!(
            (peak - AMPLITUDE).abs() < 0.02,
            "{pcm_rate} -> {dsd_rate}: peak {peak:.4} != {AMPLITUDE} at frame {peak_at} \
             of {expected} — signal mangled"
        );

        // The streaming path: what the DSD stage will actually run. Blocks are sized in
        // DSD samples, so the PCM block shrinks as the ratio grows.
        let pcm_block = (DSD_BLOCK_FRAMES / ratio as usize).max(1);
        let mut stream = time_stream(pcm_rate, dsd_rate, in_frames, pcm_block);
        for _ in 1..RUNS {
            let candidate = time_stream(pcm_rate, dsd_rate, in_frames, pcm_block);
            if candidate.0 < stream.0 {
                stream = candidate;
            }
        }
        let (stream_time, stream_frames, _checksum) = stream;
        assert_eq!(
            stream_frames, expected as u64,
            "{pcm_rate} -> {dsd_rate}: streaming length contract broken"
        );

        let one_shot = total.saturating_sub(setup).as_secs_f64().max(1e-9);
        let streamed = stream_time.as_secs_f64().max(1e-9);
        // Seconds of DSD produced per second of wall clock.
        let realtime = (expected as f64 / dsd_rate as f64) / streamed;
        // What the streaming stage actually holds: one output block, planar f64. The
        // one-shot path would hold the whole file instead — the reason this exists.
        let resident_kb = (DSD_BLOCK_FRAMES * CHANNELS * 8) as f64 / 1024.0;

        println!(
            "{:>9.1} {:>9.4} {:>6} {:>8} {:>9.1} {:>9.1} {:>8.1} {:>10.1} {:>10} {:>7.4}",
            pcm_rate as f64 / 1000.0,
            dsd_rate as f64 / 1_000_000.0,
            format!("x{ratio}"),
            pcm_block,
            setup.as_secs_f64() * 1000.0,
            one_shot * 1000.0,
            streamed * 1000.0,
            realtime,
            format!("{resident_kb:.0} kB"),
            peak,
        );
    }
    println!();
    println!(
        "Stereo, one core, vhq, {TARGET_OUT_FRAMES} output frames per case, best of 5.\n\
         strm xRT = seconds of DSD produced per second of wall clock; a 5-minute stereo\n\
         track needs 300 / xRT seconds in this stage. `resident` is the streaming stage's\n\
         working set — the one-shot column would need {:.0} MB for the same case, and\n\
         13.5 GB for a real five-minute DSD64 file.",
        (TARGET_OUT_FRAMES * CHANNELS * 8 * 2) as f64 / 1024.0 / 1024.0
    );
}

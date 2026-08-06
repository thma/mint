//! `SoxrStream` — the streaming interpolation stage feeding the DSD modulator.
//!
//! The load-bearing property is that chunking must not change the result: whatever block
//! size the DSD loop happens to use, the samples handed to the modulator have to be the
//! ones the (memory-impossible) one-shot path would have produced. Everything else here
//! guards the bookkeeping that makes that true — priming discard, the length contract,
//! and the shared libsoxr init lock.

use std::path::PathBuf;

use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::config::ResampleQuality;
use mint::ops::dsd::interpolate::SoxrStream;
use mint::ops::resample;

const DSD64: u32 = 2_822_400;
const DSD512: u32 = 22_579_200;

/// A tone with an integer number of cycles in `frames`, so truncation at the buffer end
/// introduces no step for the interpolation filter to ring on.
fn periodic_tone(rate: u32, frames: usize, approx_hz: f64, amp: f64) -> Vec<f64> {
    let cycles = (frames as f64 * approx_hz / rate as f64).round().max(1.0);
    let hz = cycles * rate as f64 / frames as f64;
    (0..frames)
        .map(|n| (2.0 * std::f64::consts::PI * hz * n as f64 / rate as f64).sin() * amp)
        .collect()
}

fn buffer_from(rate: u32, channels: Vec<Vec<f64>>) -> AudioBuffer {
    let count = channels.len();
    AudioBuffer {
        channels,
        sample_rate: rate,
        src_info: SourceInfo {
            path: PathBuf::from("t.wav"),
            channels: count,
            sample_rate: rate,
            codec: "test".into(),
        },
        out_format: OutputSampleFormat::F32,
    }
}

/// Drive a stream to completion in `block`-sized chunks and concatenate everything it
/// hands back — i.e. exactly what the DSD block loop will do, minus the modulator.
fn run_stream(input: &[Vec<f64>], in_rate: u32, out_rate: u32, block: usize) -> Vec<Vec<f64>> {
    let channels = input.len();
    let frames = input[0].len();
    let mut stream = SoxrStream::new(in_rate, out_rate, channels, Some(ResampleQuality::Vhq))
        .expect("stream construction");
    let mut out = vec![Vec::<f64>::new(); channels];

    let mut pos = 0;
    while pos < frames {
        let n = block.min(frames - pos);
        let chunk: Vec<Vec<f64>> = input.iter().map(|c| c[pos..pos + n].to_vec()).collect();
        let ready = stream.process(&chunk).expect("process");
        for (c, got) in ready.iter().enumerate() {
            out[c].extend_from_slice(got);
        }
        pos += n;
    }

    let tail = stream.finish().expect("finish");
    for (c, got) in tail.iter().enumerate() {
        out[c].extend_from_slice(got);
    }

    let expected = (frames as f64 * (out_rate as f64 / in_rate as f64)).round() as usize;
    assert_eq!(out[0].len(), expected, "length contract broken at block={block}");
    assert_eq!(
        stream.out_frames_total(),
        expected as u64,
        "internal counter disagrees with what was handed out"
    );
    out
}

fn one_shot(input: &[Vec<f64>], in_rate: u32, out_rate: u32) -> Vec<Vec<f64>> {
    let mut buffer = buffer_from(in_rate, input.to_vec());
    resample::apply(&mut buffer, out_rate, Some(ResampleQuality::Vhq)).expect("one-shot resample");
    buffer.channels
}

fn max_abs_diff(a: &[Vec<f64>], b: &[Vec<f64>]) -> f64 {
    assert_eq!(a.len(), b.len(), "channel count differs");
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            assert_eq!(x.len(), y.len(), "frame count differs");
            x.iter().zip(y).fold(0.0f64, |m, (p, q)| m.max((p - q).abs()))
        })
        .fold(0.0f64, f64::max)
}

#[test]
fn streaming_matches_the_one_shot_path() {
    // x64, stereo, fed in 512-frame blocks — the shape the DSD loop will actually use.
    let left = periodic_tone(44_100, 4096, 997.0, 0.5);
    let right = periodic_tone(44_100, 4096, 1_493.0, 0.35);
    let input = vec![left, right];

    let streamed = run_stream(&input, 44_100, DSD64, 512);
    let oneshot = one_shot(&input, 44_100, DSD64);

    let diff = max_abs_diff(&streamed, &oneshot);
    println!("streaming vs one-shot: max |diff| = {diff:.3e}");
    assert!(
        diff < 1e-12,
        "streaming diverges from the one-shot path by {diff:.3e}"
    );
}

#[test]
fn block_size_does_not_change_the_result() {
    let input = vec![periodic_tone(88_200, 8192, 997.0, 0.5)];

    // Deliberately including a size that divides neither the input nor the ratio.
    let reference = run_stream(&input, 88_200, DSD64, 8192);
    for block in [1, 127, 512, 1000, 4096] {
        let got = run_stream(&input, 88_200, DSD64, block);
        let diff = max_abs_diff(&got, &reference);
        assert!(diff < 1e-12, "block={block} diverges by {diff:.3e}");
    }
}

#[test]
fn length_contract_holds_across_rates_and_block_sizes() {
    // (in_rate, out_rate, in_frames) — includes x512, the extreme the rate rule allows.
    let cases = [
        (44_100u32, DSD64, 3000usize),
        (88_200, DSD64, 2048),
        (176_400, 5_644_800, 1500),
        (352_800, 11_289_600, 900),
        (44_100, DSD512, 512),
    ];

    for (in_rate, out_rate, frames) in cases {
        let input = vec![periodic_tone(in_rate, frames, 997.0, 0.4)];
        for block in [64, 333, frames] {
            // run_stream asserts the contract itself; this exercises the combinations.
            run_stream(&input, in_rate, out_rate, block);
        }
    }
}

#[test]
fn impulse_stays_sample_aligned() {
    // Priming discard is the only thing standing between us and a DSD file that is
    // offset against its source by the filter's group delay.
    let ratio = (DSD64 / 44_100) as usize;
    let mut ch = vec![0.0f64; 2048];
    ch[1000] = 1.0;
    let input = vec![ch];

    let out = run_stream(&input, 44_100, DSD64, 256);
    let (peak_at, _) = out[0]
        .iter()
        .enumerate()
        .fold((0usize, 0.0f64), |(i, m), (j, v)| {
            if v.abs() > m { (j, v.abs()) } else { (i, m) }
        });

    let expected = 1000 * ratio;
    let off = peak_at.abs_diff(expected);
    assert!(
        off <= ratio,
        "impulse landed at {peak_at}, expected ~{expected} (off by {off} frames)"
    );
}

#[test]
fn concurrent_streams_are_not_corrupted() {
    // `Soxr::create` builds shared tables lazily and races when several threads do it at
    // once. Streams and the one-shot path must serialize against the *same* lock, so this
    // deliberately runs both kinds side by side.
    let input = vec![periodic_tone(88_200, 4096, 997.0, 0.5)];
    let reference = run_stream(&input, 88_200, DSD64, 512);

    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let input = &input;
                let reference = &reference;
                scope.spawn(move || {
                    if i % 2 == 0 {
                        let got = run_stream(input, 88_200, DSD64, 256 + i * 37);
                        assert!(max_abs_diff(&got, reference) < 1e-12, "thread {i} corrupted");
                    } else {
                        // A concurrent one-shot resample at an unrelated rate pair.
                        let out = one_shot(input, 88_200, 44_100);
                        assert_eq!(out[0].len(), 2048);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker panicked");
        }
    });
}

#[test]
fn empty_blocks_are_harmless() {
    let input = [periodic_tone(44_100, 1024, 997.0, 0.5)];
    let mut stream = SoxrStream::new(44_100, DSD64, 1, Some(ResampleQuality::Vhq)).unwrap();

    let mut out = Vec::new();
    for chunk in input[0].chunks(256) {
        assert!(stream.process(&[Vec::new()]).unwrap()[0].is_empty());
        out.extend_from_slice(&stream.process(&[chunk.to_vec()]).unwrap()[0]);
    }
    out.extend_from_slice(&stream.finish().unwrap()[0]);

    assert_eq!(out.len(), 1024 * (DSD64 / 44_100) as usize);
}

#[test]
fn non_power_of_two_ratio_is_rejected() {
    // 48k -> DSD64 is 58.8:1. The rate derivation is supposed to prevent this; if it ever
    // lets one through, failing loudly beats writing a subtly wrong-length file.
    //
    // The guard is on `new_power_of_two`, not `new`: the PCM resampler drives the same
    // stream and legitimately runs irrational ratios, so the DSD-specific constraint has
    // to sit on the DSD-specific constructor. `encode_dsf` is the only caller of it.
    let err = SoxrStream::new_power_of_two(48_000, DSD64, 2, Some(ResampleQuality::Vhq))
        .expect_err("48k -> DSD64 must be rejected");
    assert!(
        err.to_string().contains("power-of-two"),
        "unhelpful error: {err}"
    );
    SoxrStream::new(48_000, DSD64, 2, Some(ResampleQuality::Vhq))
        .expect("the unconstrained constructor must still accept it");
}

#[test]
fn ragged_and_mismatched_blocks_are_rejected() {
    let mut stream = SoxrStream::new(44_100, DSD64, 2, Some(ResampleQuality::Vhq)).unwrap();

    let err = stream.process(&[vec![0.0; 64]]).expect_err("channel count");
    assert!(err.to_string().contains("channels"), "unhelpful error: {err}");

    let ragged = vec![vec![0.0; 64], vec![0.0; 63]];
    let err = stream.process(&ragged).expect_err("ragged block");
    assert!(err.to_string().contains("ragged"), "unhelpful error: {err}");
}

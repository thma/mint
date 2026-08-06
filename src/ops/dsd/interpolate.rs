//! Streaming interpolation from a PCM rate up to a DSD rate.
//!
//! `ops::resample::apply` cannot be used here: it materializes the entire output as
//! planar f64, which at DSD rates is tens of gigabytes (see the module docs). Instead one
//! libsoxr instance is kept alive for the whole file and fed block by block, so only a
//! few hundred kB of scratch is ever resident.
//!
//! Ratios run from x32 to x512 (`docs/dsd-design.md` section 2.1). libsoxr carries that
//! whole range on one instance — measured in `tests/soxr_dsd_probe.rs`, where the cost
//! turned out to depend only on the number of *output* samples, not on the ratio.
//!
//! Three things the one-shot path gets for free and this one has to do by hand:
//!
//! * **Priming latency.** libsoxr's first `delay()` output frames are filter priming, not
//!   audio. The one-shot path drains everything and then drops them; here they have to be
//!   discarded from the head of the stream as they emerge.
//! * **The length contract.** `out_total == round(in_total * ratio)` holds over the whole
//!   file, not per block — libsoxr hands back whatever it happens to have ready per call.
//!   Output is therefore capped at the running theoretical budget, any surplus parked in
//!   `pending`, and the tail padded (or dropped) in [`SoxrStream::finish`].
//! * **Construction races.** Instances come from `resample::create_soxr`, which holds the
//!   process-wide init lock — the *same* lock the one-shot path uses, so a rayon batch
//!   mixing PCM and DSD targets stays safe.

use anyhow::{Result, anyhow, bail};
use libsoxr::Soxr;

use crate::config::ResampleQuality;
use crate::ops::resample;

/// Minimum interleaved scratch, in frames. Also the chunk size `finish` drains with, so
/// it must be non-zero even if `process` was never called.
const MIN_SCRATCH_FRAMES: usize = 8192;

/// A libsoxr instance driven incrementally: feed planar PCM blocks with
/// [`process`](Self::process), then call [`finish`](Self::finish) exactly once.
///
/// Blocks handed back are borrowed from an internal buffer and are only valid until the
/// next call, which is what keeps the allocation count flat over a whole file. Their
/// length varies — libsoxr returns what it has ready — so the consumer must not assume a
/// fixed size. Downstream (modulator, bit packer) is sample-oriented anyway.
pub struct SoxrStream {
    soxr: Soxr,
    channels: usize,
    ratio: f64,
    /// Priming frames still to be dropped from the head of the output.
    priming_left: usize,
    in_total: u64,
    out_total: u64,
    /// Interleaved scratch either side of the FFI boundary, reused across calls.
    in_scratch: Vec<f64>,
    out_scratch: Vec<f64>,
    /// Produced but held back to keep `out_total` from running past the budget.
    pending: Vec<Vec<f64>>,
    /// The planar block handed to the caller; reused.
    block: Vec<Vec<f64>>,
}

impl std::fmt::Debug for SoxrStream {
    /// `Soxr` is an opaque FFI handle with no `Debug` of its own; the bookkeeping is the
    /// interesting state anyway (and the part that goes wrong).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoxrStream")
            .field("channels", &self.channels)
            .field("ratio", &self.ratio)
            .field("priming_left", &self.priming_left)
            .field("in_total", &self.in_total)
            .field("out_total", &self.out_total)
            .field("pending_frames", &self.pending.first().map_or(0, Vec::len))
            .finish_non_exhaustive()
    }
}

impl SoxrStream {
    /// A stream at any ratio libsoxr accepts. `ops::resample` drives its one-shot
    /// conversions through this too, so the interleaved scratch there is one block rather
    /// than the whole file.
    pub fn new(
        in_rate: u32,
        out_rate: u32,
        channels: usize,
        quality: Option<ResampleQuality>,
    ) -> Result<Self> {
        if channels == 0 {
            bail!("SoxrStream needs at least one channel");
        }
        if in_rate == 0 || out_rate == 0 {
            bail!("SoxrStream needs non-zero rates (got {in_rate} -> {out_rate})");
        }

        let soxr = resample::create_soxr(in_rate as f64, out_rate as f64, channels, quality)?;
        // Query before any processing: this is the steady-state latency, and after the
        // first `process` call libsoxr reports the *current* fill instead.
        let priming_left = soxr.delay().round() as usize;

        Ok(Self {
            soxr,
            channels,
            ratio: out_rate as f64 / in_rate as f64,
            priming_left,
            in_total: 0,
            out_total: 0,
            in_scratch: Vec::new(),
            out_scratch: vec![0.0; MIN_SCRATCH_FRAMES * channels],
            pending: vec![Vec::new(); channels],
            block: vec![Vec::new(); channels],
        })
    }

    /// Interpolate one planar block. Returns the frames that are ready *now*, which is
    /// generally not `input.len() * ratio` — the stream runs behind by the priming delay
    /// and by whatever libsoxr is still holding.
    pub fn process(&mut self, input: &[Vec<f64>]) -> Result<&[Vec<f64>]> {
        if input.len() != self.channels {
            bail!(
                "SoxrStream was built for {} channels, got {}",
                self.channels,
                input.len()
            );
        }
        let in_frames = input[0].len();
        if input.iter().any(|c| c.len() != in_frames) {
            bail!("SoxrStream got a ragged block (channels differ in length)");
        }

        if in_frames > 0 {
            self.interleave(input, in_frames);
            self.reserve_out_scratch(in_frames);
            self.pump(in_frames)?;
            self.in_total += in_frames as u64;
        }

        self.emit();
        Ok(&self.block)
    }

    /// Drain libsoxr and emit the tail, bringing the stream to exactly
    /// `round(in_total * ratio)` output frames. Call once; the stream is spent afterwards.
    pub fn finish(&mut self) -> Result<&[Vec<f64>]> {
        loop {
            let (_, odone) = self
                .soxr
                .process::<f64, f64>(None, &mut self.out_scratch)
                .map_err(|e| anyhow!("libsoxr drain failed: {e}"))?;
            if odone == 0 {
                break;
            }
            self.collect(odone);
        }

        // Everything still owed against the contract, padded with silence if libsoxr came
        // up short (it can, by a frame or two, at awkward ratios). Surplus is dropped: the
        // theoretical length is what the container's sample count will claim.
        let owed = (self.expected_total().saturating_sub(self.out_total)) as usize;
        for c in 0..self.channels {
            let take = owed.min(self.pending[c].len());
            self.block[c].clear();
            self.block[c].extend(self.pending[c].drain(..take));
            self.block[c].resize(owed, 0.0);
            self.pending[c].clear();
        }
        self.out_total += owed as u64;

        Ok(&self.block)
    }

    /// Output frames emitted so far. After `finish` this is `round(in_total * ratio)`.
    pub fn out_frames_total(&self) -> u64 {
        self.out_total
    }

    /// Theoretical output length for all input consumed so far.
    fn expected_total(&self) -> u64 {
        (self.in_total as f64 * self.ratio).round() as u64
    }

    fn interleave(&mut self, input: &[Vec<f64>], in_frames: usize) {
        self.in_scratch.clear();
        self.in_scratch.reserve(in_frames * self.channels);
        for f in 0..in_frames {
            for channel in input {
                self.in_scratch.push(channel[f]);
            }
        }
    }

    /// Size the output scratch so a block's worth of output fits in one call. Not a
    /// correctness requirement — `pump` loops — but it keeps the common case to one FFI
    /// crossing per block.
    fn reserve_out_scratch(&mut self, in_frames: usize) {
        let want = (in_frames as f64 * self.ratio).ceil() as usize + self.priming_left + 64;
        let want = want.max(MIN_SCRATCH_FRAMES) * self.channels;
        if self.out_scratch.len() < want {
            self.out_scratch.resize(want, 0.0);
        }
    }

    /// Push the interleaved scratch through libsoxr until all of it is consumed.
    fn pump(&mut self, in_frames: usize) -> Result<()> {
        let mut consumed = 0usize;
        while consumed < in_frames {
            let from = consumed * self.channels;
            let to = in_frames * self.channels;
            let (idone, odone) = self
                .soxr
                .process(Some(&self.in_scratch[from..to]), &mut self.out_scratch)
                .map_err(|e| anyhow!("libsoxr process failed: {e}"))?;

            // Collect before looping: the next iteration overwrites the scratch.
            self.collect(odone);
            consumed += idone;

            if idone == 0 && odone == 0 {
                bail!(
                    "libsoxr stalled with {} of {in_frames} input frames consumed",
                    consumed
                );
            }
        }
        Ok(())
    }

    /// Deinterleave `frames` of output scratch into `pending`, swallowing whatever is
    /// left of the priming delay.
    fn collect(&mut self, frames: usize) {
        let skip = self.priming_left.min(frames);
        self.priming_left -= skip;

        for f in skip..frames {
            let base = f * self.channels;
            for (c, pending) in self.pending.iter_mut().enumerate() {
                pending.push(self.out_scratch[base + c]);
            }
        }
    }

    /// Move as much of `pending` into the caller-visible block as the running budget
    /// allows. The cap is what makes the length contract hold without ever having to take
    /// back frames the caller has already written to disk.
    fn emit(&mut self) {
        let allowed = self.expected_total().saturating_sub(self.out_total) as usize;
        let ready = self.pending[0].len();
        let n = allowed.min(ready);

        for c in 0..self.channels {
            self.block[c].clear();
            self.block[c].extend(self.pending[c].drain(..n));
        }
        self.out_total += n as u64;
    }
}

impl SoxrStream {
    /// [`SoxrStream::new`] plus the one thing that is DSD-specific: the ratio must be an
    /// exact power of two. Everything upstream (the intermediate-rate derivation)
    /// guarantees it, so a violation is a programming error, not user input — but it is
    /// worth catching here, because the failure mode otherwise is a subtly wrong-length
    /// file rather than an error. It lives on its own constructor rather than in `new`
    /// because the PCM resampler shares the stream and legitimately runs 96k -> 44.1k.
    pub fn new_power_of_two(
        in_rate: u32,
        out_rate: u32,
        channels: usize,
        quality: Option<ResampleQuality>,
    ) -> Result<Self> {
        if in_rate == 0 || !out_rate.is_multiple_of(in_rate) || !(out_rate / in_rate).is_power_of_two()
        {
            bail!("DSD interpolation needs a power-of-two ratio, got {in_rate} -> {out_rate}");
        }
        Self::new(in_rate, out_rate, channels, quality)
    }
}

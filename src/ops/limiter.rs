//! Oversampled true-peak brickwall limiter.
//!
//! ## Why this streams
//!
//! The DSP is unchanged — 4x band-limited detection, look-ahead sliding minimum, adaptive
//! release, raised-cosine attack ramp — but none of it is allowed to materialize a
//! full-length 4x array any more. The whole-file version built five to six of them
//! (envelope, `g_inst`, the sliding minimum's two scratch reversals, the release curve,
//! the smoothed curve), which is 160-192 bytes per *input* frame: 4.6 GB for a five-minute
//! 96 kHz stereo track whose audio is 461 MB, and ~127 GB for a 74-minute album master.
//! That was the hard ceiling on what mint could process, and it failed by dying rather
//! than by warning.
//!
//! So the gain curve is produced by a state machine ([`GainStage`]) that keeps only what
//! the algorithm actually needs to look at: a monotonic deque and a ring buffer, both
//! bounded by the look-ahead window (a few hundred oversampled samples), plus one block of
//! scratch. Working set is constant in file length.
//!
//! This is the same shape as [`crate::ops::dsd::encode_dsf`]'s block loop, and for the same
//! reason — there a DSD-rate buffer is impossible, here a 4x-rate one merely bankrupts you.
//! Nothing is *shared* with the DSD stage (its interpolator is libsoxr at power-of-two
//! ratios, this one is a fixed 12-tap-per-phase polyphase FIR that has to stay bit-for-bit
//! what the BS.1770-adjacent detector always was), but the conventions are: fixed block
//! step, scratch buffers reused for the whole file, per-stream state carried across blocks,
//! explicit tail flush, and a test that pins the result as independent of the block size.
//!
//! ## What the block loop must not do
//!
//! Gains are applied *in place*, into the same buffer the detector is still reading ahead
//! of. The detector reads `FIR_TAPS_PER_PHASE/2 - 1` input frames *behind* its own
//! position, so a block's gains cannot be applied until the next block has been detected.
//! That is why `limit_channels` always writes one block behind — see the safety argument
//! there.
//!
//! ## Gain-reduction reporting
//!
//! [`GainReductionTally`] exists so the loudness stage can report exact per-sample gain
//! reduction without cloning the whole buffer to diff against afterwards (that clone was
//! the second-largest allocation in the pipeline). The limiter knows every gain it
//! applies; it just has to be asked.

use std::collections::VecDeque;

use anyhow::Result;

use crate::audio::buffer::AudioBuffer;
use crate::config::LimiterCharacter;

/// 4× oversampling for inter-sample peak detection.
const OVERSAMPLE: usize = 4;

/// Taps per polyphase branch of the band-limited 4× interpolator. A 12-tap-per-phase
/// windowed-sinc reconstructs the waveform *between* samples far more faithfully than
/// linear interpolation, so estimated inter-sample peaks track the true continuous peak
/// (and the BS.1770 meter) closely — even near Nyquist, exactly where linear
/// interpolation under-reads worst and lets real peaks slip past the ceiling.
const FIR_TAPS_PER_PHASE: usize = 12;

/// Input frames the detector reaches *backward* from the sample it is reconstructing
/// (tap `j` reads offset `j - (FIR_TAPS_PER_PHASE/2 - 1)`). The block loop's write-behind
/// lag exists to stay clear of exactly this many already-processed frames.
const FIR_LOOKBEHIND: usize = FIR_TAPS_PER_PHASE / 2 - 1;

/// Soft-clip knee starts at this fraction of the linear ceiling.
const SOFT_CLIP_KNEE_RATIO: f64 = 0.97;

/// Input frames per pass of the block loop. 4096 frames is 16 384 oversampled envelope
/// samples (128 kB of scratch, L2-resident) and comfortably more than `FIR_LOOKBEHIND`,
/// which is what makes the write-behind lag exactly one block.
const BLOCK_FRAMES: usize = 4096;

/// Below this magnitude a sample's output/input ratio carries no information, so it is
/// left out of the gain-reduction statistics entirely rather than counted as untouched.
const GR_MIN_REFERENCE: f64 = 1e-9;

/// Gains at or above `1 - GR_ACTIVE_EPS` count as untouched. The unity fast path emits a
/// literal `1.0`, so this only has to absorb float noise, not a real threshold decision.
const GR_ACTIVE_EPS: f64 = 1e-9;

#[derive(Debug, Clone, Copy)]
pub struct LimiterOptions {
    /// Transient-focused (faster release) vs. balanced (smoother release).
    pub character: LimiterCharacter,
    /// Apply a gentle post-limiter saturating curve near the ceiling.
    pub soft_clip: bool,
    /// When true, one shared envelope is applied across all channels.
    pub link_channels: bool,
}

impl Default for LimiterOptions {
    fn default() -> Self {
        Self {
            character: LimiterCharacter::Balanced,
            soft_clip: false,
            link_channels: true,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ReleaseProfile {
    /// Slow end of the adaptive release (heavy gain reduction).
    base_db_per_second: f64,
    /// Fast end of the adaptive release (light gain reduction).
    fast_db_per_second: f64,
    /// Look-ahead in milliseconds.
    look_ahead_ms: f64,
}

fn release_profile(character: LimiterCharacter) -> ReleaseProfile {
    match character {
        // Smoother envelope movement, slightly longer look-ahead.
        LimiterCharacter::Balanced => ReleaseProfile {
            base_db_per_second: 18.0,
            fast_db_per_second: 42.0,
            look_ahead_ms: 1.2,
        },
        // Faster recovery and shorter look-ahead for a snappier feel.
        LimiterCharacter::Transient => ReleaseProfile {
            base_db_per_second: 30.0,
            fast_db_per_second: 75.0,
            look_ahead_ms: 0.6,
        },
        // Mastering-style profile for *sustained* gain reduction (loudness
        // maximization): a much slower release rides between peaks instead of
        // sawtooth-pumping, and the doubled look-ahead doubles the raised-cosine
        // attack ramp, spreading attack energy further. Balanced/transient are
        // tuned for occasional overshoots, where fast recovery matters more.
        LimiterCharacter::Dense => ReleaseProfile {
            base_db_per_second: 7.0,  // 3 dB recovery ≈ 430 ms under heavy GR
            fast_db_per_second: 28.0, // still breathes near unity gain
            look_ahead_ms: 2.0,
        },
    }
}

/// Exact per-sample gain-reduction bookkeeping, filled in as the limiter applies its
/// curve.
///
/// The figures are the ones `ProcessingReport` quotes: mean gain reduction over the
/// samples that were actually attenuated, and what fraction of the assessable samples
/// those were. Peak gain reduction alone cannot tell one shaved needle from wall-to-wall
/// limiting; these can.
///
/// Accumulating rather than diffing matters for two reasons. It removes a full clone of
/// the audio buffer from the loudness stage, and it composes correctly across the
/// verification loop's several passes: gain reductions in dB *add* where gains multiply,
/// so the running sum is exactly what a single before/after comparison would have found.
/// The one thing a running sum cannot do is de-duplicate — a sample two passes both
/// attenuate must count once — hence the bitsets. At two bits per sample they cost 1/64th
/// of the copy they replace.
#[derive(Debug)]
pub struct GainReductionTally {
    channels: usize,
    frames: usize,
    /// Samples whose pre-limiter magnitude makes their ratio meaningful. Decided by the
    /// audio the *first* pass saw, which is the reference the report is against.
    assessable: BitSet,
    assessable_count: usize,
    /// Samples attenuated by at least one pass.
    attenuated: BitSet,
    attenuated_count: usize,
    /// Σ gain reduction (dB) over every attenuated sample, across all passes.
    gr_db_sum: f64,
    /// Set once the first pass has seen every sample; later passes must not re-decide
    /// which samples are measurable.
    reference_fixed: bool,
}

impl GainReductionTally {
    pub fn new(channels: usize, frames: usize) -> Self {
        let bits = channels * frames;
        Self {
            channels,
            frames,
            assessable: BitSet::new(bits),
            assessable_count: 0,
            attenuated: BitSet::new(bits),
            attenuated_count: 0,
            gr_db_sum: 0.0,
            reference_fixed: false,
        }
    }

    /// Forget everything, including which samples are assessable. `maximize` restarts each
    /// makeup pass from the pristine source, so only the final pass's limiting is in the
    /// delivered audio and only it may be reported.
    pub fn reset(&mut self) {
        self.assessable.clear();
        self.attenuated.clear();
        self.assessable_count = 0;
        self.attenuated_count = 0;
        self.gr_db_sum = 0.0;
        self.reference_fixed = false;
    }

    /// Mean gain reduction (dB) over the attenuated samples, and the fraction of
    /// assessable samples that were attenuated. `(0.0, 0.0)` when nothing was.
    pub fn result(&self) -> (f64, f64) {
        if self.attenuated_count == 0 || self.assessable_count == 0 {
            return (0.0, 0.0);
        }
        (
            self.gr_db_sum / self.attenuated_count as f64,
            self.attenuated_count as f64 / self.assessable_count as f64,
        )
    }

    /// Record what the gain curve did to one sample. `pre` is its value going in.
    #[inline]
    fn note(&mut self, channel: usize, frame: usize, pre: f64, gain: f64) {
        let Some(slot) = self.slot(channel, frame) else {
            return;
        };
        if !self.reference_fixed && pre.abs() > GR_MIN_REFERENCE && self.assessable.set(slot) {
            self.assessable_count += 1;
        }
        self.fold(slot, gain);
    }

    /// Fold in a later multiplicative stage (the soft clipper) without letting it revise
    /// which samples are assessable.
    #[inline]
    fn note_gain(&mut self, channel: usize, frame: usize, gain: f64) {
        if let Some(slot) = self.slot(channel, frame) {
            self.fold(slot, gain);
        }
    }

    /// Every sample has now been seen once; freeze the assessable set.
    fn seal_reference(&mut self) {
        self.reference_fixed = true;
    }

    #[inline]
    fn slot(&self, channel: usize, frame: usize) -> Option<usize> {
        (channel < self.channels && frame < self.frames).then(|| channel * self.frames + frame)
    }

    #[inline]
    fn fold(&mut self, slot: usize, gain: f64) {
        if gain >= 1.0 - GR_ACTIVE_EPS || !self.assessable.get(slot) {
            return;
        }
        if self.attenuated.set(slot) {
            self.attenuated_count += 1;
        }
        self.gr_db_sum += -20.0 * gain.max(1e-12).log10();
    }
}

/// One bit per sample. Only ever used for set-membership over the whole file, where the
/// alternative (a `Vec<bool>`, let alone a copy of the audio) is 8x/64x the memory.
#[derive(Debug)]
struct BitSet {
    words: Vec<u64>,
}

impl BitSet {
    fn new(bits: usize) -> Self {
        Self {
            words: vec![0; bits.div_ceil(64)],
        }
    }

    /// Set the bit; `true` if it was not already set.
    #[inline]
    fn set(&mut self, i: usize) -> bool {
        let bit = 1_u64 << (i % 64);
        let word = &mut self.words[i / 64];
        let fresh = *word & bit == 0;
        *word |= bit;
        fresh
    }

    #[inline]
    fn get(&self, i: usize) -> bool {
        self.words[i / 64] & (1_u64 << (i % 64)) != 0
    }

    fn clear(&mut self) {
        self.words.fill(0);
    }
}

/// Apply an oversampled true-peak brickwall limiter in place.
///
/// Strategy:
/// 1. Band-limited 4× FIR upsample to estimate inter-sample peaks.
/// 2. Compute per-oversampled-frame gain reduction required to stay below `ceiling_dbtp`.
/// 3. Sliding-minimum look-ahead: propagate gain reduction backward so attenuation
///    starts BEFORE the peak arrives (eliminates brickwall overshoot).
/// 4. Forward pass with instant attack and controlled release.
/// 5. Raised-cosine smoothing over the look-ahead window: the attack becomes a smooth
///    ramp reaching full attenuation exactly when the peak arrives, instead of a step
///    (removes gain-slew modulation distortion; provably never lifts a sample above
///    the ceiling — see [`GainStage::smooth`]).
/// 6. Sample the gain curve at phase 0; apply uniformly across all channels to preserve
///    inter-channel balance.
///
/// Steps 1–5 run as one streaming stage over blocks of input frames, so nothing at 4× rate
/// is ever resident (module docs).
pub fn apply_true_peak_limit(buffer: &mut AudioBuffer, ceiling_dbtp: f64) -> Result<()> {
    apply_true_peak_limit_with_options(buffer, ceiling_dbtp, LimiterOptions::default())
}

/// Apply an oversampled true-peak brickwall limiter with configurable character,
/// optional soft clipping, and channel linking mode.
pub fn apply_true_peak_limit_with_options(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    options: LimiterOptions,
) -> Result<()> {
    limit(buffer, ceiling_dbtp, options, None)
}

/// As [`apply_true_peak_limit_with_options`], recording exactly what the gain curve did
/// into `tally`. Call repeatedly with the same tally to accumulate over a verification
/// loop's passes.
pub fn apply_true_peak_limit_tallied(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    options: LimiterOptions,
    tally: &mut GainReductionTally,
) -> Result<()> {
    limit(buffer, ceiling_dbtp, options, Some(tally))
}

fn limit(
    buffer: &mut AudioBuffer,
    ceiling_dbtp: f64,
    options: LimiterOptions,
    mut tally: Option<&mut GainReductionTally>,
) -> Result<()> {
    if buffer.frame_len() == 0 {
        return Ok(());
    }

    let ceiling_lin = 10_f64.powf(ceiling_dbtp / 20.0);
    let over_rate = buffer.sample_rate as f64 * OVERSAMPLE as f64;
    let profile = release_profile(options.character);
    // Oversampled-sample distance for the look-ahead.
    let look_ahead = ((over_rate * profile.look_ahead_ms / 1000.0).round() as usize).max(1);
    let shape = CurveShape {
        ceiling_lin,
        look_ahead,
        over_rate,
        profile,
    };

    if options.link_channels {
        // One combined envelope preserves inter-channel imaging.
        limit_channels(
            &mut buffer.channels,
            0,
            shape,
            BLOCK_FRAMES,
            tally.as_deref_mut(),
        );
    } else {
        // Independent envelopes can preserve more channel-specific transients at the
        // cost of possible stereo image movement under heavy limiting.
        for c in 0..buffer.channels.len() {
            limit_channels(
                std::slice::from_mut(&mut buffer.channels[c]),
                c,
                shape,
                BLOCK_FRAMES,
                tally.as_deref_mut(),
            );
        }
    }

    // Every sample has been through the gain curve exactly once by now, so the soft
    // clipper below must not be allowed to add to the assessable set.
    if let Some(t) = tally.as_deref_mut() {
        t.seal_reference();
    }

    if options.soft_clip {
        apply_soft_clip(buffer, ceiling_lin, tally);
    }

    Ok(())
}

/// Everything about the gain curve that does not change over a file.
#[derive(Debug, Clone, Copy)]
struct CurveShape {
    ceiling_lin: f64,
    look_ahead: usize,
    over_rate: f64,
    profile: ReleaseProfile,
}

/// Detect over `channels` (their joint envelope) and apply the resulting curve to all of
/// them. `channel_offset` is where this group starts in the buffer, so the tally's slots
/// line up whether we were handed every channel or one at a time.
///
/// `block` is a parameter only so tests can prove the result does not depend on it.
fn limit_channels(
    channels: &mut [Vec<f64>],
    channel_offset: usize,
    shape: CurveShape,
    block: usize,
    mut tally: Option<&mut GainReductionTally>,
) {
    let frames = channels[0].len();
    if frames == 0 {
        return;
    }
    // Write-behind safety, part one. After detecting a block of `m` frames starting at
    // `start`, the detector sits at oversampled index `OVERSAMPLE*(start+m) + look_ahead`
    // and will never read below input frame
    // `start + m + look_ahead/OVERSAMPLE - FIR_LOOKBEHIND`; the block written just after
    // ends at `start - 1`. That is clear whenever `m + look_ahead/OVERSAMPLE >=
    // FIR_LOOKBEHIND`, which any realistic look-ahead satisfies on its own — but a
    // degenerate one (a handful of oversampled samples) needs the block widened instead.
    let block = block
        .max(FIR_LOOKBEHIND.saturating_sub(shape.look_ahead / OVERSAMPLE))
        .max(1);
    let total = frames * OVERSAMPLE;
    let kernels = polyphase_kernels();
    let mut stage = GainStage::new(shape, total);

    // Gains the stage has emitted but the loop has not consumed yet, and the oversampled
    // index `ready[0]` corresponds to. Never longer than one block plus the look-ahead.
    let mut ready: Vec<f64> = Vec::with_capacity(block * OVERSAMPLE + shape.look_ahead);
    let mut ready_base = 0usize;

    // Phase-0 gains for the block being detected, and for the one behind it that is now
    // safe to write. Both reused for the whole file.
    let mut current: Vec<f64> = Vec::with_capacity(block);
    let mut previous: Vec<f64> = Vec::with_capacity(block);
    let mut previous_start = 0usize;

    let mut fed = 0usize;
    let mut start = 0usize;
    while start < frames {
        let m = block.min(frames - start);

        // The sliding minimum cannot finalize the gain at oversampled index `e` until it
        // has seen `e + look_ahead`, so run the detector that far past the block.
        let target = (OVERSAMPLE * (start + m) + shape.look_ahead).min(total);
        while fed < target {
            stage.push(envelope_at(channels, &kernels, fed), &mut ready);
            fed += 1;
        }
        if fed == total {
            stage.flush(&mut ready);
        }

        current.clear();
        for f in start..start + m {
            current.push(ready[OVERSAMPLE * f - ready_base]);
        }
        // Retire what no later block can ask for.
        let keep_from = OVERSAMPLE * (start + m);
        ready.drain(..keep_from - ready_base);
        ready_base = keep_from;

        // Write-behind safety, part two: this is where the one-block lag is spent. The
        // block just detected stays untouched until the *next* one has been detected,
        // which is what keeps the gain application clear of the reads above (and, when
        // the detector has run to the end of the file, there are no reads left at all).
        apply_gains(
            channels,
            channel_offset,
            previous_start,
            &previous,
            tally.as_deref_mut(),
        );
        std::mem::swap(&mut previous, &mut current);
        previous_start = start;
        start += m;
    }

    apply_gains(channels, channel_offset, previous_start, &previous, tally);
}

fn apply_gains(
    channels: &mut [Vec<f64>],
    channel_offset: usize,
    start: usize,
    gains: &[f64],
    mut tally: Option<&mut GainReductionTally>,
) {
    for (c, channel) in channels.iter_mut().enumerate() {
        // Hoisted out of the sample loop: the tallied path costs a few ops per sample and
        // only the loudness stage asks for it.
        match tally.as_deref_mut() {
            Some(t) => {
                for (k, &g) in gains.iter().enumerate() {
                    let sample = &mut channel[start + k];
                    t.note(channel_offset + c, start + k, *sample, g);
                    *sample *= g;
                }
            }
            None => {
                for (k, &g) in gains.iter().enumerate() {
                    channel[start + k] *= g;
                }
            }
        }
    }
}

/// The gain curve as a streaming state machine: detector envelope in, final
/// per-oversampled-sample gain out, in order, with bounded state.
///
/// Four stages, unchanged from the whole-file version and in the same order:
///
/// 1. instantaneous requirement `g_inst = min(1, ceiling/peak)`;
/// 2. look-ahead `g_la[i] = min(g_inst[i .. min(i+L, N)])`, as a monotonic deque running
///    forward — it can only finalize `g_la[i]` once it has seen `i + L - 1`, which is the
///    stage's entire latency;
/// 3. instant-attack / adaptive-release forward pass (one scalar of state);
/// 4. causal raised-cosine smoothing over the trailing look-ahead window (a ring buffer
///    of `L + 1` gains).
///
/// `total` is the only thing it needs to know about the future: the look-ahead window is
/// truncated at the end of the stream, exactly as the whole-file version's was.
///
/// Non-finite envelope values cannot leak past stage 1: `peak > ceiling` is false for NaN,
/// so `g_inst` is always a finite value in `(0, 1]`.
struct GainStage {
    shape: CurveShape,
    total: usize,

    /// Indices and values of the look-ahead sliding minimum, values increasing.
    deque: VecDeque<(usize, f64)>,
    /// Envelope samples pushed so far.
    fed: usize,
    /// Sliding-minimum outputs produced so far.
    la_emitted: usize,

    /// Previous release-pass output, `None` before the first sample.
    release_prev: Option<f64>,

    /// Raised-cosine taps; empty when the window degenerates to a passthrough.
    taps: Vec<f64>,
    /// The last `look_ahead + 1` release-pass outputs. Rounded up to a power of two and
    /// indexed with `ring_mask`: the smoothing dot product is the hot loop of the whole
    /// limiter (one multiply-add per tap per oversampled sample, and the look-ahead is
    /// hundreds of taps), so a real `%` here costs more than the rest of the stage.
    ring: Vec<f64>,
    ring_mask: usize,
    /// The release pass's first output, used to pad reads before index 0.
    head: f64,
    /// Non-unity gains in the trailing window; while zero the smoother is a no-op.
    active_in_window: usize,
    /// Final gains emitted so far.
    emitted: usize,
}

impl GainStage {
    fn new(shape: CurveShape, total: usize) -> Self {
        let window = shape.look_ahead;
        // `smooth_gain_curve` degenerated to a passthrough for a window of 1; keep that,
        // so a degenerate look-ahead stays bit-exact rather than merely equivalent.
        let smoothing = window > 1;
        Self {
            shape,
            total,
            deque: VecDeque::with_capacity(window.min(1024)),
            fed: 0,
            la_emitted: 0,
            release_prev: None,
            taps: if smoothing {
                hann_smoothing_taps(window)
            } else {
                Vec::new()
            },
            ring: if smoothing {
                vec![0.0; (window + 1).next_power_of_two()]
            } else {
                Vec::new()
            },
            ring_mask: (window + 1).next_power_of_two() - 1,
            head: 1.0,
            active_in_window: 0,
            emitted: 0,
        }
    }

    /// Feed one envelope sample, appending every gain sample it makes final.
    fn push(&mut self, envelope: f64, out: &mut Vec<f64>) {
        let ceiling = self.shape.ceiling_lin;
        let g_inst = if envelope > ceiling {
            ceiling / envelope
        } else {
            1.0
        };

        // Drop back entries that can never be the minimum again while this one is in the
        // window — the standard monotonic deque, run forward instead of over a reversal.
        while self.deque.back().is_some_and(|&(_, v)| v >= g_inst) {
            self.deque.pop_back();
        }
        self.deque.push_back((self.fed, g_inst));
        self.fed += 1;

        if self.fed >= self.shape.look_ahead {
            let i = self.fed - self.shape.look_ahead;
            self.emit(i, out);
        }
    }

    /// No more envelope. Emit the tail, whose look-ahead windows are cut short by the end
    /// of the stream.
    fn flush(&mut self, out: &mut Vec<f64>) {
        while self.la_emitted < self.total {
            let i = self.la_emitted;
            self.emit(i, out);
        }
    }

    fn emit(&mut self, i: usize, out: &mut Vec<f64>) {
        while self.deque.front().is_some_and(|&(idx, _)| idx < i) {
            self.deque.pop_front();
        }
        let g_la = self
            .deque
            .front()
            .expect("sliding-minimum window is never empty before the stream ends")
            .1;
        self.la_emitted = i + 1;
        let released = self.release(g_la);
        self.smooth(released, out);
    }

    /// Instant attack, program-dependent release: deep into gain reduction recover
    /// slowly, near unity gain recover fast.
    fn release(&mut self, target: f64) -> f64 {
        let Some(prev) = self.release_prev else {
            self.release_prev = Some(target);
            return target;
        };
        let g = if target <= prev {
            target
        } else {
            let profile = self.shape.profile;
            let gr_db = (-20.0 * prev.max(1e-12).log10()).max(0.0);
            let heavy = (gr_db / 12.0).clamp(0.0, 1.0);
            let rel_db_s = profile.fast_db_per_second
                - (profile.fast_db_per_second - profile.base_db_per_second) * heavy;
            let rel_mul = 10_f64.powf(rel_db_s / (20.0 * self.shape.over_rate));
            // Clamp to `target`, not 1.0: gain ≤ g_la everywhere is what lets the
            // raised-cosine smoother below guarantee the ceiling (target ≤ 1.0 always).
            (prev * rel_mul).min(target)
        };
        self.release_prev = Some(g);
        g
    }

    /// Causally convolve the release curve with normalized raised-cosine (Hann) taps
    /// spanning the look-ahead window, so the sliding-min attack step becomes a smooth
    /// ramp that reaches full attenuation exactly when the peak arrives.
    ///
    /// No-overshoot argument: `g_la[i] = min(g_inst[i .. i+L))` implies
    /// `g_la[n−k] ≤ g_inst[n]` for every `k ∈ [0, L)`, and the release pass keeps
    /// `g[i] ≤ g_la[i]` (its release branch clamps to target). Each output
    /// `Σ w[k]·g[n−k]` is therefore a convex combination of values `≤ g_inst[n]` — the
    /// ramp can never lift a sample above its own ceiling requirement. Reads before index
    /// 0 use `g[0]` (= the minimum over the first window), which keeps the bound valid for
    /// peaks inside the file's first look-ahead window; padding with 1.0 would not.
    ///
    /// While the trailing window holds no gain reduction the output is a literal `1.0` and
    /// the dot product is skipped — that is what keeps below-ceiling material bit-exact
    /// (no reliance on the taps summing to exactly 1.0) and the limiter ~O(n) for the
    /// common occasional-overshoot case.
    fn smooth(&mut self, g: f64, out: &mut Vec<f64>) {
        let i = self.emitted;
        self.emitted += 1;

        if self.taps.is_empty() {
            out.push(g);
            return;
        }

        let window = self.shape.look_ahead;
        let mask = self.ring_mask;
        if i == 0 {
            self.head = g;
        }
        // Written before the eviction read below: the ring is longer than `window`, so
        // slots `i` and `i - window` never collide.
        self.ring[i & mask] = g;

        if g != 1.0 {
            self.active_in_window += 1;
        }
        if i >= window && self.ring[(i - window) & mask] != 1.0 {
            self.active_in_window -= 1;
        }
        if self.active_in_window == 0 {
            out.push(1.0);
            return;
        }

        let mut acc = 0.0;
        for (k, &w) in self.taps.iter().enumerate() {
            let v = if i >= k { self.ring[(i - k) & mask] } else { self.head };
            acc += w * v;
        }
        out.push(acc);
    }
}

/// Normalized raised-cosine (Hann) taps. The `(k+1)/(len+1)` phase avoids the zero
/// endpoints of a textbook Hann window (which would waste support), and normalizing to
/// unity sum makes the smoother a convex combination — level-preserving on constant
/// input, never exceeding the window's maximum.
fn hann_smoothing_taps(len: usize) -> Vec<f64> {
    use std::f64::consts::PI;
    let mut taps: Vec<f64> = (0..len)
        .map(|k| 0.5 * (1.0 - (2.0 * PI * (k + 1) as f64 / (len + 1) as f64).cos()))
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

fn apply_soft_clip(
    buffer: &mut AudioBuffer,
    ceiling_lin: f64,
    mut tally: Option<&mut GainReductionTally>,
) {
    let knee = ceiling_lin * SOFT_CLIP_KNEE_RATIO;
    if knee <= 0.0 || knee >= ceiling_lin {
        return;
    }
    let tanh_norm = 1.0_f64.tanh();

    for (c, channel) in buffer.channels.iter_mut().enumerate() {
        for (f, sample) in channel.iter_mut().enumerate() {
            let sign = sample.signum();
            let a = sample.abs();
            if a <= knee {
                continue;
            }
            let t = ((a - knee) / (ceiling_lin - knee)).clamp(0.0, 1.0);
            let shaped = knee + (ceiling_lin - knee) * (t.tanh() / tanh_norm);
            let clipped = sign * shaped.min(ceiling_lin);
            // Folded in as a plain multiplicative factor, so the reported gain reduction
            // still describes the limiter stage as a whole — what diffing against a full
            // pre-limiter copy of the buffer used to measure.
            if let Some(t) = tally.as_deref_mut() {
                t.note_gain(c, f, clipped / *sample);
            }
            *sample = clipped;
        }
    }
}

/// Build the `OVERSAMPLE` polyphase kernels of a windowed-sinc fractional-delay
/// interpolator. Phase `p` reconstructs the signal at offset `p/OVERSAMPLE` between
/// input samples. Generated from first principles (ideal sinc fractional delay ×
/// Blackman window), then normalized per phase to unity DC gain so levels are
/// preserved. Phase 0 reduces to an exact unit impulse, so original-sample positions
/// pass through untouched.
fn polyphase_kernels() -> [[f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE] {
    use std::f64::consts::PI;

    // Support spans `FIR_TAPS_PER_PHASE` input samples centered on the interpolation
    // point; tap `j` reads input offset `k = j - (half - 1)`.
    let half = (FIR_TAPS_PER_PHASE / 2) as isize;
    let m = (FIR_TAPS_PER_PHASE - 1) as f64;

    let mut kernels = [[0.0_f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE];
    for (p, kernel) in kernels.iter_mut().enumerate() {
        let frac = p as f64 / OVERSAMPLE as f64; // fractional delay in [0, 1)
        let mut sum = 0.0;
        for (j, coeff) in kernel.iter_mut().enumerate() {
            let k = j as isize - (half - 1);
            let x = frac - k as f64;
            // Ideal fractional-delay tap: sinc(frac - k), with the removable hole at 0.
            let sinc = if x.abs() < 1e-12 { 1.0 } else { (PI * x).sin() / (PI * x) };
            // Blackman window over the tap support keeps side lobes (and thus passband
            // ripple / stopband leakage) low for a clean reconstruction.
            let n = j as f64;
            let w = 0.42 - 0.5 * (2.0 * PI * n / m).cos() + 0.08 * (4.0 * PI * n / m).cos();
            *coeff = sinc * w;
            sum += *coeff;
        }
        for coeff in kernel.iter_mut() {
            *coeff /= sum; // unity DC gain ⇒ no level shift introduced by detection
        }
    }
    kernels
}

/// One 4× upsampled sample: the signal reconstructed at input offset
/// `i + phase/OVERSAMPLE`, `kernel` being that phase's branch. Samples beyond the edges
/// are treated as zero — negligible for peak detection.
///
/// Computed on demand rather than from a materialized 4× array; the tap loop is the
/// hot path of the whole limiter, so it stays a plain indexed accumulation in tap order.
#[inline]
fn oversampled(signal: &[f64], i: usize, kernel: &[f64; FIR_TAPS_PER_PHASE]) -> f64 {
    // Interior: the whole tap support is in range, so the per-tap range test becomes one
    // slice bound and the window is a contiguous read. Taps are still accumulated in
    // ascending order — the edge branch below has to produce the same bits.
    if i >= FIR_LOOKBEHIND && i + FIR_TAPS_PER_PHASE - FIR_LOOKBEHIND <= signal.len() {
        let window = &signal[i - FIR_LOOKBEHIND..][..FIR_TAPS_PER_PHASE];
        let mut acc = 0.0;
        for (&s, &c) in window.iter().zip(kernel.iter()) {
            acc += s * c;
        }
        return acc;
    }

    let n = signal.len();
    let mut acc = 0.0;
    for (j, &c) in kernel.iter().enumerate() {
        let idx = i as isize + j as isize - FIR_LOOKBEHIND as isize;
        if idx >= 0 && (idx as usize) < n {
            acc += signal[idx as usize] * c;
        }
    }
    acc
}

/// Joint detector envelope at oversampled index `e`: the loudest channel's reconstructed
/// magnitude. One channel in `channels` gives the unlinked case for free.
#[inline]
fn envelope_at(
    channels: &[Vec<f64>],
    kernels: &[[f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE],
    e: usize,
) -> f64 {
    let i = e / OVERSAMPLE;
    let kernel = &kernels[e % OVERSAMPLE];
    let mut envelope = 0.0_f64;
    for channel in channels {
        envelope = envelope.max(oversampled(channel, i, kernel).abs());
    }
    envelope
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::buffer::{OutputSampleFormat, SourceInfo};
    use std::f64::consts::PI;
    use std::path::PathBuf;

    /// The whole-file implementations the streaming stage replaced, kept as the thing it
    /// is measured against. They are the readable statement of the algorithm; the
    /// equivalence tests below are what stop the fast version from drifting away from it.
    mod reference {
        use super::super::*;

        pub fn upsample_fir_4x(
            signal: &[f64],
            kernels: &[[f64; FIR_TAPS_PER_PHASE]; OVERSAMPLE],
        ) -> Vec<f64> {
            let mut out = Vec::with_capacity(signal.len() * OVERSAMPLE);
            for i in 0..signal.len() {
                for kernel in kernels.iter() {
                    out.push(oversampled(signal, i, kernel));
                }
            }
            out
        }

        pub fn build_gain_curve(envelope: &[f64], shape: CurveShape) -> Vec<f64> {
            let g_inst: Vec<f64> = envelope
                .iter()
                .map(|&p| {
                    if p > shape.ceiling_lin {
                        shape.ceiling_lin / p
                    } else {
                        1.0
                    }
                })
                .collect();

            let g_la = sliding_min_forward(&g_inst, shape.look_ahead);

            let mut g_smooth = vec![1.0_f64; envelope.len()];
            if g_smooth.is_empty() {
                return g_smooth;
            }
            g_smooth[0] = g_la[0];
            for i in 1..g_smooth.len() {
                let prev = g_smooth[i - 1];
                let target = g_la[i];
                if target <= prev {
                    g_smooth[i] = target;
                } else {
                    let gr_db = (-20.0 * prev.max(1e-12).log10()).max(0.0);
                    let heavy = (gr_db / 12.0).clamp(0.0, 1.0);
                    let rel_db_s = shape.profile.fast_db_per_second
                        - (shape.profile.fast_db_per_second - shape.profile.base_db_per_second)
                            * heavy;
                    let rel_mul = 10_f64.powf(rel_db_s / (20.0 * shape.over_rate));
                    g_smooth[i] = (prev * rel_mul).min(target);
                }
            }

            smooth_gain_curve(&g_smooth, shape.look_ahead)
        }

        pub fn smooth_gain_curve(gains: &[f64], window: usize) -> Vec<f64> {
            if window <= 1 || gains.is_empty() {
                return gains.to_vec();
            }
            let taps = hann_smoothing_taps(window);
            let n = gains.len();
            let mut out = Vec::with_capacity(n);

            let mut active_in_window = 0usize;
            for i in 0..n {
                if gains[i] != 1.0 {
                    active_in_window += 1;
                }
                if i >= window && gains[i - window] != 1.0 {
                    active_in_window -= 1;
                }
                if active_in_window == 0 {
                    out.push(1.0);
                    continue;
                }
                let mut acc = 0.0;
                for (k, &w) in taps.iter().enumerate() {
                    let g = if i >= k { gains[i - k] } else { gains[0] };
                    acc += w * g;
                }
                out.push(acc);
            }
            out
        }

        /// `result[i] = min(values[i .. min(i + window, n)])`, via a reversal and a
        /// backward monotonic deque.
        pub fn sliding_min_forward(values: &[f64], window: usize) -> Vec<f64> {
            if values.is_empty() {
                return Vec::new();
            }
            let rev: Vec<f64> = values.iter().cloned().rev().collect();
            let bwd = sliding_min_backward(&rev, window);
            bwd.into_iter().rev().collect()
        }

        fn sliding_min_backward(values: &[f64], window: usize) -> Vec<f64> {
            use std::collections::VecDeque;
            let n = values.len();
            let mut result = Vec::with_capacity(n);
            let mut deque: VecDeque<usize> = VecDeque::new();

            for i in 0..n {
                while deque.front().is_some_and(|&front| i - front >= window) {
                    deque.pop_front();
                }
                while deque.back().is_some_and(|&back| values[back] >= values[i]) {
                    deque.pop_back();
                }
                deque.push_back(i);
                result.push(values[*deque.front().unwrap()]);
            }
            result
        }

        /// The whole-file limiter, linked mode — five to six full-length 4× arrays, which
        /// is precisely why it is test-only now.
        pub fn limit_linked(channels: &mut [Vec<f64>], shape: CurveShape) {
            let n = channels[0].len();
            if n == 0 {
                return;
            }
            let kernels = polyphase_kernels();
            let mut envelope = vec![0.0_f64; n * OVERSAMPLE];
            for channel in channels.iter() {
                let upsampled = upsample_fir_4x(channel, &kernels);
                for (env, s) in envelope.iter_mut().zip(upsampled.iter()) {
                    *env = env.max(s.abs());
                }
            }
            let gains = build_gain_curve(&envelope, shape);
            for channel in channels.iter_mut() {
                for (i, sample) in channel.iter_mut().enumerate() {
                    *sample *= gains[i * OVERSAMPLE];
                }
            }
        }
    }

    /// Drive the production stage over a whole envelope — what the tests that used to call
    /// `build_gain_curve` need, without reintroducing it as production code.
    fn stage_gain_curve(envelope: &[f64], shape: CurveShape) -> Vec<f64> {
        let mut stage = GainStage::new(shape, envelope.len());
        let mut out = Vec::with_capacity(envelope.len());
        for &e in envelope {
            stage.push(e, &mut out);
        }
        stage.flush(&mut out);
        out
    }

    /// Must go through the same conversion `limit` does, or comparisons against it fail
    /// by one ULP.
    fn ceiling_lin(dbtp: f64) -> f64 {
        10_f64.powf(dbtp / 20.0)
    }

    fn shape_for(character: LimiterCharacter, ceiling_lin: f64, over_rate: f64) -> CurveShape {
        let profile = release_profile(character);
        CurveShape {
            ceiling_lin,
            look_ahead: ((over_rate * profile.look_ahead_ms / 1000.0).round() as usize).max(1),
            over_rate,
            profile,
        }
    }

    /// Deterministic LCG in `[0, 1)` — no dev-dependency on a RNG, and reproducible.
    fn lcg(state: &mut u64) -> f64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 11) as f64 / (1_u64 << 53) as f64
    }

    fn make_buffer(channels: Vec<Vec<f64>>, rate: u32) -> AudioBuffer {
        let count = channels.len();
        AudioBuffer {
            channels,
            sample_rate: rate,
            src_info: SourceInfo {
                path: PathBuf::from("test.wav"),
                channels: count,
                sample_rate: rate,
                codec: "test".to_string(),
            },
            out_format: OutputSampleFormat::F32,
        }
    }

    /// Program-like material: a carrier that mostly sits below the ceiling, bursts that
    /// sit well above it, and a silent lead-in — so attack, release, the unity fast path
    /// and the assessable-sample filter all get exercised.
    fn program(frames: usize, seed: u64, rate: f64) -> Vec<f64> {
        let mut state = seed;
        (0..frames)
            .map(|i| {
                let t = i as f64 / rate;
                let carrier = (2.0 * PI * 997.0 * t).sin() * 0.35;
                if i < 500 {
                    0.0
                } else if i % 3313 == 0 {
                    1.6 + 2.0 * lcg(&mut state)
                } else {
                    carrier + 0.05 * (lcg(&mut state) - 0.5)
                }
            })
            .collect()
    }

    #[test]
    fn kernels_are_level_preserving() {
        // Each phase must sum to 1.0 (unity DC gain) or the detector would shift levels.
        for kernel in polyphase_kernels() {
            let sum: f64 = kernel.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "phase DC gain {sum} != 1.0");
        }
    }

    #[test]
    fn phase_zero_is_a_unit_impulse() {
        // Phase 0 must reproduce the original sample exactly (so sub-ceiling signals
        // pass through untouched and the gain-application indexing stays valid).
        let kernels = polyphase_kernels();
        let half = (FIR_TAPS_PER_PHASE / 2) as isize;
        for (j, &c) in kernels[0].iter().enumerate() {
            let k = j as isize - (half - 1);
            let expected = if k == 0 { 1.0 } else { 0.0 };
            assert!((c - expected).abs() < 1e-12, "phase-0 tap {k} = {c}, want {expected}");
        }
    }

    #[test]
    fn smoothing_taps_are_normalized_and_nonnegative() {
        for len in [1_usize, 2, 3, 57, 230] {
            let taps = hann_smoothing_taps(len);
            assert_eq!(taps.len(), len);
            assert!(taps.iter().all(|&t| t > 0.0), "len {len}: zero/negative tap");
            let sum: f64 = taps.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "len {len}: tap sum {sum} != 1.0");
        }
    }

    #[test]
    fn streaming_gain_curve_is_bit_identical_to_the_whole_file_one() {
        // The load-bearing claim of the rewrite. Several look-aheads (including the
        // degenerate 1 and one longer than the envelope) and several envelope shapes.
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        for &look_ahead in &[1_usize, 2, 7, 64, 211, 4096] {
            for &len in &[0_usize, 1, 5, 300, 5000] {
                let envelope: Vec<f64> = (0..len)
                    .map(|i| {
                        let r = lcg(&mut state);
                        if i % 97 == 0 { 0.5 + 3.0 * r } else { 0.45 * r }
                    })
                    .collect();
                let shape = CurveShape {
                    ceiling_lin: ceiling_lin(-1.0),
                    look_ahead,
                    over_rate: 192_000.0,
                    profile: release_profile(LimiterCharacter::Balanced),
                };

                let streamed = stage_gain_curve(&envelope, shape);
                let expected = reference::build_gain_curve(&envelope, shape);
                assert_eq!(streamed.len(), expected.len(), "L={look_ahead} n={len}");
                for (i, (&a, &b)) in streamed.iter().zip(&expected).enumerate() {
                    assert_eq!(
                        a.to_bits(),
                        b.to_bits(),
                        "L={look_ahead} n={len}: gain[{i}] {a} != {b}"
                    );
                }
            }
        }
    }

    #[test]
    fn block_size_does_not_change_the_limited_audio() {
        // The DSD interpolator's contract, restated for this stage: whatever the loop
        // chops the file into, the samples written have to be the ones the whole-file
        // path would have produced.
        let frames = 20_000;
        let source = vec![program(frames, 0x1234_5678, 48_000.0), program(frames, 0xfeed_beef, 48_000.0)];
        let shape = shape_for(LimiterCharacter::Balanced, ceiling_lin(-1.0), 192_000.0);

        let mut expected = source.clone();
        reference::limit_linked(&mut expected, shape);

        // Sizes that divide neither the file nor the oversampling ratio, plus ones far
        // below and far above the look-ahead (211 at this rate).
        for block in [1, 7, 13, 211, 512, 4096, frames, frames * 2] {
            let mut got = source.clone();
            limit_channels(&mut got, 0, shape, block, None);
            for (c, (a, b)) in got.iter().zip(&expected).enumerate() {
                for (i, (&x, &y)) in a.iter().zip(b).enumerate() {
                    assert_eq!(x.to_bits(), y.to_bits(), "block={block} ch{c}[{i}]: {x} != {y}");
                }
            }
        }
    }

    #[test]
    fn unlinked_mode_matches_per_channel_whole_file_limiting() {
        let frames = 9_000;
        let source = vec![program(frames, 0xaaaa, 48_000.0), program(frames, 0xbbbb, 48_000.0)];
        let shape = shape_for(LimiterCharacter::Transient, ceiling_lin(-1.0), 192_000.0);

        let mut expected = source.clone();
        for channel in &mut expected {
            reference::limit_linked(std::slice::from_mut(channel), shape);
        }

        let mut buffer = make_buffer(source, 48_000);
        limit(
            &mut buffer,
            -1.0,
            LimiterOptions {
                character: LimiterCharacter::Transient,
                soft_clip: false,
                link_channels: false,
            },
            None,
        )
        .unwrap();

        for (c, (a, b)) in buffer.channels.iter().zip(&expected).enumerate() {
            for (i, (&x, &y)) in a.iter().zip(b).enumerate() {
                assert_eq!(x.to_bits(), y.to_bits(), "ch{c}[{i}]: {x} != {y}");
            }
        }
    }

    #[test]
    fn tally_matches_diffing_against_a_full_copy() {
        // The tally replaced a clone-and-diff; it has to agree with it, including across
        // several passes with different ceilings and with the soft clipper engaged.
        let frames = 12_000;
        let source = vec![program(frames, 0xc0ffee, 48_000.0), program(frames, 0xdecafb, 48_000.0)];

        for soft_clip in [false, true] {
            let options = LimiterOptions {
                character: LimiterCharacter::Balanced,
                soft_clip,
                link_channels: true,
            };
            let mut buffer = make_buffer(source.clone(), 48_000);
            let pre = buffer.channels.clone();
            let mut tally = GainReductionTally::new(2, frames);

            // Two passes at descending ceilings, the shape `limit_and_verify` produces.
            for ceiling in [-1.0, -1.35] {
                apply_true_peak_limit_tallied(&mut buffer, ceiling, options, &mut tally).unwrap();
            }

            let (avg_db, ratio) = tally.result();
            let (want_avg, want_ratio) = diff_stats(&pre, &buffer.channels);
            // The tally sums each stage's dB where the oracle takes one log of the
            // composite ratio, so they can disagree in the last bits. The only systematic
            // part is the soft clipper's knee, which *raises* samples by up to +0.028 dB:
            // the oracle folds that into the ratio, the tally drops it (a gain ≥ 1 is not
            // gain reduction). Measured residual is ~1.3e-6 dB on this signal — five
            // orders of magnitude below the 0.1 dB the report prints.
            let tolerance = if soft_clip { 1e-4 } else { 1e-9 };
            assert!(
                (avg_db - want_avg).abs() < tolerance,
                "soft_clip={soft_clip}: avg GR {avg_db} vs {want_avg}"
            );
            assert!(
                (ratio - want_ratio).abs() < 1e-9,
                "soft_clip={soft_clip}: active ratio {ratio} vs {want_ratio}"
            );
            assert!(ratio > 0.0, "the test signal must actually force limiting");
        }
    }

    /// The old `loudness::gain_reduction_stats`, kept here as the tally's oracle.
    fn diff_stats(pre: &[Vec<f64>], post: &[Vec<f64>]) -> (f64, f64) {
        let mut assessable = 0_usize;
        let mut active = 0_usize;
        let mut gr_db_sum = 0.0;
        for (pre_ch, post_ch) in pre.iter().zip(post) {
            for (&p, &q) in pre_ch.iter().zip(post_ch) {
                if p.abs() <= 1e-9 {
                    continue;
                }
                assessable += 1;
                let ratio = q / p;
                if ratio < 1.0 - 1e-9 {
                    active += 1;
                    gr_db_sum += -20.0 * ratio.max(1e-12).log10();
                }
            }
        }
        if active == 0 || assessable == 0 {
            return (0.0, 0.0);
        }
        (gr_db_sum / active as f64, active as f64 / assessable as f64)
    }

    #[test]
    fn tally_reset_forgets_the_previous_pass() {
        // `maximize` restarts from pristine audio every makeup pass, so only the last
        // pass's limiting may be reported.
        let frames = 4_000;
        let source = vec![program(frames, 0x5eed, 48_000.0)];
        let options = LimiterOptions::default();
        let mut tally = GainReductionTally::new(1, frames);

        let mut hard = make_buffer(source.clone(), 48_000);
        apply_true_peak_limit_tallied(&mut hard, -6.0, options, &mut tally).unwrap();
        let (hard_db, _) = tally.result();

        tally.reset();
        let mut soft = make_buffer(source, 48_000);
        apply_true_peak_limit_tallied(&mut soft, -1.0, options, &mut tally).unwrap();
        let (soft_db, _) = tally.result();

        assert!(hard_db > soft_db, "-6 dBTP ({hard_db}) should bite harder than -1 ({soft_db})");
        let mut fresh = GainReductionTally::new(1, frames);
        let mut again = make_buffer(
            vec![program(frames, 0x5eed, 48_000.0)],
            48_000,
        );
        apply_true_peak_limit_tallied(&mut again, -1.0, options, &mut fresh).unwrap();
        assert_eq!(fresh.result().0.to_bits(), soft_db.to_bits(), "reset left residue");
    }

    #[test]
    fn smoothed_gain_never_exceeds_instantaneous_requirement() {
        // Encodes the no-overshoot proof of `GainStage::smooth`: every sample's final
        // (ramped) gain must still satisfy that sample's own instantaneous requirement.
        // Includes a peak inside the first look-ahead window (left-edge padding case).
        let ceiling = 0.5;
        let shape = CurveShape {
            ceiling_lin: ceiling,
            look_ahead: 37,
            over_rate: 192_000.0,
            profile: release_profile(LimiterCharacter::Balanced),
        };

        // Deterministic LCG envelope: mostly sub-ceiling, occasional loud spikes, so
        // attack, release, and unity fast path all get exercised.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let envelope: Vec<f64> = (0..4096)
            .map(|i| {
                let r = lcg(&mut state);
                if i == 5 || r > 0.97 { 0.5 + 2.0 * r } else { 0.4 * r }
            })
            .collect();

        let gains = stage_gain_curve(&envelope, shape);
        for (i, (&g, &p)) in gains.iter().zip(envelope.iter()).enumerate() {
            let required = if p > ceiling { ceiling / p } else { 1.0 };
            assert!(g <= required + 1e-12, "gain {g} at {i} exceeds requirement {required}");
        }
    }

    #[test]
    fn attack_is_a_raised_cosine_ramp_not_a_step() {
        let look_ahead = 48;
        let shape = CurveShape {
            ceiling_lin: 1.0,
            look_ahead,
            over_rate: 192_000.0,
            profile: release_profile(LimiterCharacter::Balanced),
        };
        let peak_pos = 500;
        let peak_val = 2.0;
        let mut envelope = vec![0.1_f64; 1000];
        envelope[peak_pos] = peak_val;

        let gains = stage_gain_curve(&envelope, shape);
        let required = shape.ceiling_lin / peak_val;

        // Full required attenuation exactly when the peak arrives.
        assert!(gains[peak_pos] <= required + 1e-12, "peak gain {} too high", gains[peak_pos]);

        // Attenuation starts exactly one look-ahead early — and not a sample sooner:
        // everything before is untouched unity via the fast path (bit-exact passthrough).
        let ramp_start = peak_pos - look_ahead + 1;
        for (i, &g) in gains[..ramp_start].iter().enumerate() {
            assert_eq!(g, 1.0, "gain at {i} not exactly 1.0 before the ramp");
        }
        assert!(gains[ramp_start] < 1.0, "ramp did not start at peak - look_ahead + 1");

        // The largest per-sample drop of a smoothed step is the step height times the
        // largest tap (≈ 2/L) — a fraction of the old one-sample step. The small extra
        // margin absorbs release movement overlapping the ramp.
        let step = 1.0 - required;
        let max_tap = hann_smoothing_taps(look_ahead).into_iter().fold(0.0_f64, f64::max);
        let max_slew = gains.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0_f64, f64::max);
        assert!(
            max_slew <= step * max_tap + 1e-5,
            "slew {max_slew} exceeds raised-cosine bound {}",
            step * max_tap
        );
    }

    #[test]
    fn smoothing_unity_regions_stay_exactly_one() {
        // The fast path must emit literal 1.0 wherever the trailing window holds no
        // gain reduction — this is what keeps below-ceiling material bit-exact.
        let shape = CurveShape {
            ceiling_lin: 1.0,
            look_ahead: 37,
            over_rate: 192_000.0,
            profile: release_profile(LimiterCharacter::Balanced),
        };
        let gains = stage_gain_curve(&vec![0.2_f64; 500], shape);
        assert!(gains.iter().all(|&g| g == 1.0), "sub-ceiling input must stay literal 1.0");
    }

    #[test]
    fn dense_release_recovers_slower_than_balanced() {
        // Dense exists for sustained gain reduction: it must ride between peaks instead
        // of pumping, i.e. recover clearly slower than balanced, and look further ahead
        // (longer raised-cosine attack ramp).
        let over_rate = 48_000.0;
        let dense = shape_for(LimiterCharacter::Dense, 1.0, over_rate);
        let balanced = shape_for(LimiterCharacter::Balanced, 1.0, over_rate);
        assert!(dense.profile.look_ahead_ms > balanced.profile.look_ahead_ms);

        // Same isolated over-ceiling impulse through both profiles; count samples until
        // the gain climbs back to 0.99 after the peak. The impulse is deep (18 dB of
        // required reduction) so recovery spends real time in the heavy-GR region where
        // the base release rates differ most; the adaptive blend toward the fast rate
        // near unity dilutes the ratio at shallow reductions.
        let mut envelope = vec![0.1_f64; 120_000];
        let peak_pos = 2_000;
        envelope[peak_pos] = 8.0; // forces gain 0.125 at the peak

        let recovery_samples = |shape: CurveShape| {
            stage_gain_curve(&envelope, shape)[peak_pos..]
                .iter()
                .position(|&g| g > 0.99)
                .expect("gain should recover within the test buffer")
        };

        let dense_recovery = recovery_samples(dense);
        let balanced_recovery = recovery_samples(balanced);
        // Continuous-time integration of the two rate curves gives ≈ 2.2×.
        assert!(
            dense_recovery as f64 >= 1.8 * balanced_recovery as f64,
            "dense recovery ({dense_recovery} samples) should be ≈ 2× balanced ({balanced_recovery})"
        );
    }

    #[test]
    fn fir_recovers_inter_sample_peak_better_than_linear() {
        // Textbook worst case for inter-sample peaks: an fs/4 sine at 45° phase. Every
        // sample sits at exactly ±0.707 while the true continuous peak is 1.0, with the
        // crest landing exactly between samples. Linear interpolation can't see it; the
        // band-limited FIR should recover it.
        let n = 256;
        let sig: Vec<f64> = (0..n)
            .map(|i| (PI * i as f64 / 2.0 + PI / 4.0).sin()) // fs/4, +45°
            .collect();

        let sample_peak = sig.iter().fold(0.0_f64, |m, &s| m.max(s.abs()));
        assert!(sample_peak < 0.8, "samples should sit at ~0.707, not on the crest");

        let kernels = polyphase_kernels();
        let fir_peak = reference::upsample_fir_4x(&sig, &kernels)
            .iter()
            .fold(0.0_f64, |m, &s| m.max(s.abs()));

        // Linear interpolation, for reference, never exceeds the (too-low) sample peak.
        let lin_peak = sig
            .windows(2)
            .flat_map(|w| [w[0], 0.5 * (w[0] + w[1])])
            .fold(0.0_f64, |m, s| m.max(s.abs()));

        // True peak is 1.0; FIR should land close, clearly beating linear (~0.707).
        assert!(fir_peak > 0.93, "FIR peak {fir_peak} too low");
        assert!(
            fir_peak > lin_peak + 0.05,
            "FIR ({fir_peak}) should clearly beat linear ({lin_peak})"
        );
    }

    #[test]
    fn on_demand_envelope_matches_the_materialized_one() {
        // `envelope_at` is the streaming replacement for "upsample every channel, then
        // fold" — the detector is the one part of the chain that must not move at all.
        let frames = 777;
        let channels = vec![program(frames, 0x11, 48_000.0), program(frames, 0x22, 48_000.0)];
        let kernels = polyphase_kernels();

        let mut expected = vec![0.0_f64; frames * OVERSAMPLE];
        for channel in &channels {
            for (env, s) in expected
                .iter_mut()
                .zip(reference::upsample_fir_4x(channel, &kernels))
            {
                *env = env.max(s.abs());
            }
        }

        for (e, &want) in expected.iter().enumerate() {
            let got = envelope_at(&channels, &kernels, e);
            assert_eq!(got.to_bits(), want.to_bits(), "envelope[{e}]: {got} != {want}");
        }
    }
}

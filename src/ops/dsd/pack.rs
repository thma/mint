//! Packing 1-bit samples into bytes.
//!
//! The arithmetic is trivial. The one thing that is not trivial — and that is expensive to
//! retrofit once a container writer has been built around the wrong assumption — is
//! **which end of the byte the earliest sample goes to**. DSF and DFF disagree: DSF with
//! `bitsPerSample = 1` is LSB first, DFF is MSB first. Making that a parameter here is the
//! whole reason this is its own module, and it is what keeps a later DFF writer an
//! afternoon rather than a rewrite (`docs/dsd-design.md` section 3.4).
//!
//! Nothing here knows about containers, blocks or channels: bits in, bytes out. Block
//! accounting and the planar round-robin live in [`crate::audio::io_write_dsd`], and DFF's
//! sample interleaving would live in its own writer.

/// Where in a byte the *earliest* of its eight samples sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitOrder {
    /// Earliest sample in bit 0 (mask 0x01). DSF with `bitsPerSample = 1`, which is what
    /// mint writes.
    LsbFirst,
    /// Earliest sample in bit 7 (mask 0x80). DFF, and DSF with `bitsPerSample = 8`.
    MsbFirst,
}

/// The conventional DSD idle pattern, `0b01101001`.
///
/// **Not 0x00.** A DSD bit maps to +-1 ([`crate::ops::dsd::modulator::level`]), so a run of
/// zero bytes is not silence but a constant -1: full-scale negative DC. Fed to a real
/// decimator the step into it overshoots past -1.0 and clips. 0x69 decodes to exactly 0.0
/// instead, which is why it is the silence constant in ffmpeg (`DSD_SILENCE`), in MPD, and
/// in the Linux kernel's ALSA format table for every `SNDRV_PCM_FORMAT_DSD_*`.
///
/// Several bytes have zero mean (0x0F, 0x33, 0x55, ...); what singles out 0x69 is *where*
/// its residual ultrasonic energy sits — mostly at `fs/8 * 3`, only -11.4 dBFS at `fs/8`.
/// The most obviously "alternating" byte, 0x55, is the worst possible choice: a full-scale
/// square wave at `fs/2`.
///
/// Bit-order agnostic on purpose: `0x69.reverse_bits() == 0x96`, and both decode to 0.0, so
/// a writer never has to pick the pattern to match its [`BitOrder`].
pub const DSD_SILENCE: u8 = 0x69;

/// Accumulates a bit stream into bytes, oldest sample first.
///
/// Completed bytes queue up in an internal buffer that the caller drains with
/// [`consume`](Self::consume) after writing them, so a whole-file packer stays flat in
/// memory instead of growing to the size of the stream.
#[derive(Debug, Clone)]
pub struct BitPacker {
    order: BitOrder,
    bytes: Vec<u8>,
    /// The byte under construction. Positions past `filled` are still zero and carry no
    /// meaning until [`pad_partial`](Self::pad_partial) fills them.
    partial: u8,
    /// How many of the eight positions of `partial` are occupied; always 0..=7, because
    /// the eighth push flushes.
    filled: u32,
}

impl BitPacker {
    pub fn new(order: BitOrder) -> Self {
        Self {
            order,
            bytes: Vec::new(),
            partial: 0,
            filled: 0,
        }
    }

    pub fn order(&self) -> BitOrder {
        self.order
    }

    /// Append one sample. `true` is the high level (+1) — the polarity the modulator emits
    /// and the one DSF stores directly, so nothing on this path ever inverts.
    #[inline]
    pub fn push_bit(&mut self, bit: bool) {
        if bit {
            self.partial |= match self.order {
                BitOrder::LsbFirst => 1u8 << self.filled,
                BitOrder::MsbFirst => 0x80u8 >> self.filled,
            };
        }
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.partial);
            self.partial = 0;
            self.filled = 0;
        }
    }

    pub fn push(&mut self, bits: &[bool]) {
        self.bytes.reserve(bits.len() / 8);
        for &bit in bits {
            self.push_bit(bit);
        }
    }

    /// Completed bytes waiting to be written.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Samples sitting in the part-filled byte, 0..=7.
    pub fn pending_bits(&self) -> u32 {
        self.filled
    }

    /// Drop the first `n` completed bytes, which the caller has just written out.
    pub fn consume(&mut self, n: usize) {
        self.bytes.drain(..n);
    }

    /// Complete a part-filled byte by taking its remaining positions from `fill`. Returns
    /// whether there was anything to complete.
    ///
    /// The bits are taken from the *same positions* in `fill`, not from its low end, so
    /// finishing a byte with [`DSD_SILENCE`] and then appending whole `DSD_SILENCE` bytes
    /// produces one unbroken idle pattern rather than a phase jump at the seam.
    ///
    /// In practice this never fires from the DSD stage: interpolation ratios are powers of
    /// two of at least 32, so a channel's sample count is always a multiple of 32. It is
    /// here so the packer is correct for callers that are not the DSD stage.
    pub fn pad_partial(&mut self, fill: u8) -> bool {
        if self.filled == 0 {
            return false;
        }
        let occupied = match self.order {
            BitOrder::LsbFirst => (1u8 << self.filled) - 1,
            BitOrder::MsbFirst => !((1u8 << (8 - self.filled)) - 1),
        };
        self.partial |= fill & !occupied;
        self.bytes.push(self.partial);
        self.partial = 0;
        self.filled = 0;
        true
    }

    /// Append `count` whole bytes of `byte`. Only meaningful on a byte boundary — call
    /// [`pad_partial`](Self::pad_partial) first.
    pub fn push_pad_bytes(&mut self, byte: u8, count: usize) {
        debug_assert_eq!(self.filled, 0, "pad bytes must start on a byte boundary");
        self.bytes.extend(std::iter::repeat_n(byte, count));
    }
}

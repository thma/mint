//! DSD (1-bit / Direct Stream Digital) output stage.
//!
//! Unlike every other output path in mint, this one **streams**. The canonical
//! `AudioBuffer` holds the whole file as planar f64, which is the right call at PCM rates
//! but impossible at DSD rates: five minutes of DSD64 stereo would be ~13.5 GB as f64
//! (~54 GB at DSD256). So the DSD stage never materializes a full-rate buffer — it walks
//! the PCM buffer in blocks, and each block is interpolated, modulated, packed and
//! written before the next one is touched.
//!
//! Design and the derivation of the intermediate PCM rate: `docs/dsd-design.md`.

pub mod decimate;
pub mod interpolate;
pub mod modulator;
pub mod pack;

/// The four DSD rates mint can write. All are multiples of 44 100 Hz — the reason the
/// intermediate PCM rate is pulled into the 44.1 kHz family before interpolation, so the
/// ratio is an exact power of two (`docs/dsd-design.md` section 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DsdRate {
    Dsd64,
    Dsd128,
    Dsd256,
    Dsd512,
}

impl DsdRate {
    /// Sample rate of the 1-bit stream, in Hz.
    pub const fn hz(self) -> u32 {
        44_100 * self.multiple()
    }

    /// The "64" in DSD64: how many times 44 100 Hz the stream runs at.
    pub const fn multiple(self) -> u32 {
        match self {
            DsdRate::Dsd64 => 64,
            DsdRate::Dsd128 => 128,
            DsdRate::Dsd256 => 256,
            DsdRate::Dsd512 => 512,
        }
    }

    /// Config / dry-run label.
    pub const fn tag(self) -> &'static str {
        match self {
            DsdRate::Dsd64 => "dsd64",
            DsdRate::Dsd128 => "dsd128",
            DsdRate::Dsd256 => "dsd256",
            DsdRate::Dsd512 => "dsd512",
        }
    }

    pub const ALL: [DsdRate; 4] = [
        DsdRate::Dsd64,
        DsdRate::Dsd128,
        DsdRate::Dsd256,
        DsdRate::Dsd512,
    ];
}

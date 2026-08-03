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

pub mod interpolate;

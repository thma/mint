//! The libsoxr C API, bound by hand — the only `extern "C"` block in mint.
//!
//! This replaces the `libsoxr` (0.2.9) and `libsoxr-sys` (0.1.4) crates, for two reasons
//! that happen to point the same way.
//!
//! **Licensing.** Both crates are LGPL-2.0-or-later *Rust* code, and Rust code is statically
//! linked. Linking the C library dynamically — which mint does, deliberately, see
//! `THIRD-PARTY-LICENSES.md` — does nothing about that: the bindings were still copyleft
//! object code inside every released binary, with LGPL §6's relink obligation attached. The
//! C library's own licence is unaffected by any of this and is discharged by the dynamic
//! link; what this file removes is the *second*, avoidable copy of the problem.
//!
//! **Maintenance.** `libsoxr-rs` and `libsoxr-sys` were last touched in April 2023
//! (`docs/gui-proposal.md` §10 lists their abandonment as a standing risk). mint used six
//! items from them across two files, all of it one construction call and one process call,
//! so the dependency bought roughly a hundred lines and cost an unmaintained FFI surface in
//! the one place where a subtle error is inaudible until it is on a master.
//!
//! **What is deliberately *not* here.** The crates were general: eight sample formats,
//! interleaved and split, variable-rate resampling, input callbacks, runtime thread specs.
//! mint has exactly one I/O contract — interleaved f64 in, interleaved f64 out, chosen so
//! libsoxr's internal TPDF dither can never engage — so this binds that contract and nothing
//! else. The split-buffer pointer indirection the wrapper carried is gone with it, along
//! with the branch that decided per call which of the two shapes to use.
//!
//! **One real bug is fixed on the way.** `libsoxr::Soxr::create` passed a *null*
//! `soxr_error_t*` to `soxr_create` and then tested `if error.is_null()` — the pointer it had
//! just declared as null, never the value libsoxr writes. Creation errors were therefore
//! never detected, and a failed create returned an `Ok(Soxr)` wrapping a null handle for the
//! next `soxr_process` to dereference. [`Soxr::create`] checks the returned handle and reads
//! the error string libsoxr actually set.
//!
//! Every constant below is checked against `soxr.h`, and `tests/soxr.rs` pins the two that
//! would be silent if wrong: the recipe a quality tier maps to, and the engine libsoxr then
//! selects.

use std::ffi::CStr;
use std::os::raw::{c_char, c_uint, c_ulong, c_void};

use anyhow::{Result, bail};

// ─────────────────────────── the raw ABI ───────────────────────────

/// `soxr_t` — opaque.
type Handle = *mut c_void;
/// `soxr_error_t`: a borrowed C string, NULL meaning success.
type ErrorPtr = *const c_char;
/// `soxr_datatype_t` is a C enum, i.e. `unsigned int`.
type Datatype = c_uint;

/// `SOXR_FLOAT64_I` — 64-bit float, interleaved. The only datatype mint uses, and the
/// reason libsoxr's internal dither never runs (it applies to INT16 output only).
const FLOAT64_I: Datatype = 1;

/// `soxr_io_spec_t`. Field order and types are the ABI; do not reorder.
#[repr(C)]
#[derive(Clone, Copy)]
struct IoSpec {
    itype: Datatype,
    otype: Datatype,
    scale: f64,
    e: *mut c_void,
    flags: c_ulong,
}

/// `soxr_quality_spec_t`. Built by `soxr_quality_spec`, never by hand: the four figures are
/// *derived* from the recipe by libsoxr, and inventing them here would be a second,
/// divergent definition of what "vhq" means.
#[repr(C)]
#[derive(Clone, Copy)]
struct QualitySpec {
    precision: f64,
    phase_response: f64,
    passband_end: f64,
    stopband_begin: f64,
    e: *mut c_void,
    flags: c_ulong,
}

unsafe extern "C" {
    fn soxr_create(
        input_rate: f64,
        output_rate: f64,
        num_channels: c_uint,
        error: *mut ErrorPtr,
        io_spec: *const IoSpec,
        quality_spec: *const QualitySpec,
        runtime_spec: *const c_void,
    ) -> Handle;

    fn soxr_process(
        resampler: Handle,
        buf_in: *const c_void,
        ilen: usize,
        idone: *mut usize,
        buf_out: *mut c_void,
        olen: usize,
        odone: *mut usize,
    ) -> ErrorPtr;

    fn soxr_delay(resampler: Handle) -> f64;
    fn soxr_delete(resampler: Handle);
    fn soxr_engine(resampler: Handle) -> *const c_char;

    fn soxr_io_spec(itype: Datatype, otype: Datatype) -> IoSpec;
    fn soxr_quality_spec(recipe: c_ulong, flags: c_ulong) -> QualitySpec;
}

// ─────────────────────────── quality vocabulary ───────────────────────────

/// A libsoxr quality recipe. Values from `soxr.h`; `SOXR_HQ` and `SOXR_VHQ` are aliases for
/// `SOXR_20_BITQ` and `SOXR_28_BITQ`, which is why they are 4 and 6 rather than 3 and 4 —
/// the off-by-one a hand-written binding would most plausibly ship, and inaudible until
/// someone measured the stopband.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recipe {
    Low,
    Medium,
    High,
    VeryHigh,
}

impl Recipe {
    fn value(self) -> c_ulong {
        match self {
            Self::Low => 1,       // SOXR_LQ
            Self::Medium => 2,    // SOXR_MQ
            Self::High => 4,      // SOXR_HQ == SOXR_20_BITQ
            Self::VeryHigh => 6,  // SOXR_VHQ == SOXR_28_BITQ
        }
    }
}

/// `SOXR_ROLLOFF_SMALL` — <= 0.01 dB passband ripple. Zero, so it is the absence of the
/// other rolloff flags rather than a bit; named anyway, because `0` at a call site says
/// nothing.
pub const ROLLOFF_SMALL: c_ulong = 0;
/// `SOXR_HI_PREC_CLOCK` — higher accuracy for irrational ratios (the 44.1 ↔ 48 kHz family).
pub const HI_PREC_CLOCK: c_ulong = 8;

// ─────────────────────────── the safe wrapper ───────────────────────────

/// One libsoxr resampler instance: interleaved f64 in, interleaved f64 out.
///
/// Not `Send` or `Sync`, and that is not an oversight — a handle is created and driven on
/// one thread for the life of one job. `cli.rs` parallelizes across *jobs*, so each worker
/// builds its own; see `resample::create_soxr` for the construction lock that makes that
/// safe against libsoxr's lazy shared tables.
pub struct Soxr {
    handle: Handle,
    channels: usize,
}

impl Soxr {
    /// Build a resampler. **Not the entry point** — go through
    /// `ops::resample::create_soxr`, which holds the process-wide construction lock.
    pub(crate) fn create(
        input_rate: f64,
        output_rate: f64,
        channels: usize,
        recipe: Recipe,
        flags: c_ulong,
    ) -> Result<Self> {
        // Both specs come from libsoxr's own constructors. `soxr_quality_spec` fills in
        // precision, phase response and the band edges from the recipe; those are the
        // definition of the tier and mint does not get a vote.
        let io = unsafe { soxr_io_spec(FLOAT64_I, FLOAT64_I) };
        let quality = unsafe { soxr_quality_spec(recipe.value(), flags) };

        let mut error: ErrorPtr = std::ptr::null();
        // Runtime spec left NULL: libsoxr's default is one thread, which is what mint wants
        // — parallelism is per job, under rayon, one level up.
        let handle = unsafe {
            soxr_create(
                input_rate,
                output_rate,
                channels as c_uint,
                &mut error,
                &io,
                &quality,
                std::ptr::null(),
            )
        };

        // The *handle* is the success test. The binding this replaced tested a pointer it
        // had itself set to null a line earlier, so it never saw a failure at all.
        if handle.is_null() {
            bail!(
                "failed to create libsoxr resampler ({input_rate} -> {output_rate} Hz, \
                 {channels} ch): {}",
                unsafe { message(error) }
            );
        }
        Ok(Self { handle, channels })
    }

    /// Steady-state latency in output frames. Query it *before* the first
    /// [`process`](Self::process): afterwards libsoxr reports the current fill instead.
    pub fn delay(&self) -> f64 {
        unsafe { soxr_delay(self.handle) }
    }

    /// The resampling engine libsoxr chose — `cr32`, `cr64`, `cr64s`, … Purely diagnostic,
    /// and the one figure that reveals `vhq` is not bit-reproducible across platforms
    /// (`docs/gui-proposal.md` §11 Q6).
    pub fn engine(&self) -> String {
        unsafe { message(soxr_engine(self.handle)) }
    }

    /// Feed a block and collect what is ready.
    ///
    /// `input` is `None` to signal end-of-input and drain; call it repeatedly until the
    /// returned output count is zero. Both slices are **interleaved**, and both lengths are
    /// divided by the channel count before being handed to libsoxr — the returned pair is
    /// therefore `(input frames consumed, output frames produced)`, per channel, which is
    /// the unit the callers count in.
    pub fn process(&self, input: Option<&[f64]>, output: &mut [f64]) -> Result<(usize, usize)> {
        let mut idone: usize = 0;
        let mut odone: usize = 0;
        let olen = output.len() / self.channels;
        let out_ptr = output.as_mut_ptr().cast::<c_void>();

        let error = match input {
            Some(input) => unsafe {
                soxr_process(
                    self.handle,
                    input.as_ptr().cast::<c_void>(),
                    input.len() / self.channels,
                    &mut idone,
                    out_ptr,
                    olen,
                    &mut odone,
                )
            },
            None => unsafe {
                soxr_process(
                    self.handle,
                    std::ptr::null(),
                    0,
                    &mut idone,
                    out_ptr,
                    olen,
                    &mut odone,
                )
            },
        };

        if !error.is_null() {
            bail!("libsoxr process failed: {}", unsafe { message(error) });
        }
        Ok((idone, odone))
    }
}

impl Drop for Soxr {
    fn drop(&mut self) {
        unsafe { soxr_delete(self.handle) };
    }
}

/// A libsoxr string, or a stand-in when it handed back nothing.
///
/// # Safety
/// `ptr` must be NULL or a valid NUL-terminated C string owned by libsoxr.
unsafe fn message(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return "libsoxr gave no detail".to_string();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

pub trait Operation {
    fn name(&self) -> &'static str;
}

pub mod bitdepth;
pub mod dsd;
pub mod limiter;
pub mod loudness;
pub mod resample;
/// The hand-written libsoxr binding — the only `extern "C"` block in the crate.
pub mod soxr;

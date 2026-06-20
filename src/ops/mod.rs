pub trait Operation {
    fn name(&self) -> &'static str;
}

pub mod bitdepth;
pub mod limiter;
pub mod loudness;
pub mod resample;

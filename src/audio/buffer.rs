use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputSampleFormat {
    S16,
    S24,
    F32,
}

impl OutputSampleFormat {
    pub fn bits_per_sample(self) -> u16 {
        match self {
            Self::S16 => 16,
            Self::S24 => 24,
            Self::F32 => 32,
        }
    }

    /// Integer quantization grid `(max, min_i, max_i)` for this format, or `None`
    /// for f32 (no quantization). The grid is symmetric (±(2^(bits−1) − 1)) so the
    /// mapping is an odd function and introduces no DC bias at full scale. Single
    /// source of truth shared by the flat and noise-shaped quantizers.
    pub fn int_grid(self) -> Option<(f64, i32, i32)> {
        match self {
            Self::S16 => Some((32_767.0, -32_767, 32_767)),
            Self::S24 => Some((8_388_607.0, -8_388_607, 8_388_607)),
            Self::F32 => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SourceInfo {
    pub path: PathBuf,
    pub channels: usize,
    pub sample_rate: u32,
    pub codec: String,
}

#[derive(Debug, Clone)]
pub struct AudioBuffer {
    pub channels: Vec<Vec<f64>>,
    pub sample_rate: u32,
    pub src_info: SourceInfo,
    pub out_format: OutputSampleFormat,
}

impl AudioBuffer {
    pub fn frame_len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn channels_count(&self) -> usize {
        self.channels.len()
    }
}

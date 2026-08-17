//! Small helpers shared by every front end.
//!
//! These lived in `cli.rs` until the GUI needed them. `is_supported_input` in particular
//! is a correctness surface, not a convenience: a second copy of the extension list is a
//! second answer to "will mint accept this file?", and the two would drift the first time
//! a decoder is added. The formatters are here so the GUI prints the same figures as the
//! CLI rather than inventing its own rounding.

use std::path::Path;

/// Extensions `read_audio` can decode — the symphonia feature set in `Cargo.toml`
/// (`wav`, `aiff`, `pcm`) expressed as the filter the input globs are sieved through.
///
/// Public as a *list* and not only as [`is_supported_input`]'s predicate because a native
/// file dialog cannot be driven by a predicate: it wants the extensions up front. Given
/// only the predicate, a front end writing an `add_filter` call would write this list a
/// second time, which is the drift the module doc above is about.
pub const SUPPORTED_INPUT_EXTENSIONS: &[&str] = &["wav", "wave", "aif", "aiff"];

/// Whether mint can decode the file at `path`, judged by its extension.
pub fn is_supported_input(path: &Path) -> bool {
    let ext = match path.extension().and_then(|x| x.to_str()) {
        Some(ext) => ext.to_ascii_lowercase(),
        None => return false,
    };

    SUPPORTED_INPUT_EXTENSIONS.contains(&ext.as_str())
}

/// Byte count for humans. `MB` is 10^6, matching how storage and DSD data rates are
/// quoted. Both ends need a step: DSD512 files run into the gigabytes, and a short clip
/// would otherwise be reported as "0.0 MB".
pub fn fmt_bytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.2} GB", mb / 1000.0)
    } else if mb < 1.0 {
        format!("{:.0} kB", bytes as f64 / 1000.0)
    } else {
        format!("{mb:.1} MB")
    }
}

/// Playing time of a delivered file from its own frame count and rate. Correct for DSD
/// too: there `frames` is the 1-bit sample count and `rate` the 1-bit rate, so the
/// quotient is still seconds. Guards `rate == 0` because a report is not the place to
/// discover a division by zero.
pub fn duration_seconds(frames: u64, rate: u32) -> f64 {
    if rate == 0 { 0.0 } else { frames as f64 / rate as f64 }
}

/// Format a dB/LU/LUFS figure, or "n/a" when the meter returned a non-finite value
/// (silence, or a clip shorter than the meter's integration time).
pub fn fmt_db(v: f64) -> String {
    if v.is_finite() { format!("{v:.1}") } else { "n/a".to_string() }
}

pub mod audio;
pub mod chain;
pub mod cli;
pub mod config;
pub mod diagnostics;
pub mod dither;
pub mod json_report;
pub mod ops;
pub mod pipeline;
pub mod progress;
pub mod util;

/// Whether this build can actually encode MP3. Without it an `codec = "mp3"` target
/// resolves, validates, passes `delivery_blocker` and runs the *entire* chain before
/// failing in the writer — so a front end that can grey the option out should.
pub const MP3_AVAILABLE: bool = cfg!(feature = "mp3");

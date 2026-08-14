//! `mint-gui` — placeholder for the egui workbench described in `docs/gui-proposal.md`.
//!
//! Today it is a boundary smoke test, not an application: it calls exactly the library
//! surface Phase 1 is built on and prints what comes back. That is the point — the whole
//! design rests on the GUI *never re-deriving* anything (§5.2 rule 1), so if a screen can
//! be described entirely as "call these functions and render the result", the boundary
//! holds. Anything this file has to compute for itself is a finding.
//!
//! Run: `cargo run -p mint-gui -- [path/to/mint.toml]`

use std::path::{Path, PathBuf};

use anyhow::Result;
use mint::config::{
    BitDepthFormat, Config, DitherCorrelation, DitherMode, DitherStrength, LimiterCharacter,
    OnClipPolicy, OutputCodec, PRESET_NAMES, ResampleQuality,
};

/// Stand-in for a probed source until the Sources pane exists. 96 kHz stereo is the
/// case that makes the derivation interesting: it forces a resample on every 44.1 kHz
/// target and sits above FLAC's ceiling.
const NOMINAL_SOURCE_RATE: u32 = 96_000;

fn main() -> Result<()> {
    let path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("examples/mint.toml"));

    println!("mint-gui boundary check — mp3 support in this build: {}", mint::MP3_AVAILABLE);
    println!();

    dump_enum_domains();
    println!();

    // Read the bytes ourselves and parse via `from_toml_str` rather than `from_path`:
    // that is the call the editor pane will make against an unsaved buffer, so it is
    // the one worth exercising.
    let text = std::fs::read_to_string(&path)?;
    let config = Config::from_toml_str(&text)?;
    dump_targets(&config, &path)?;

    Ok(())
}

/// Every dropdown the target form will need, from the library's own `ALL` arrays and
/// `Display` impls. A hardcoded list here would be the exact staleness `PRESET_NAMES`
/// was introduced to fix.
fn dump_enum_domains() {
    println!("enum domains (T0-1/T0-2/T0-3):");
    println!("  codec           {}", join(&OutputCodec::ALL));
    println!("  format          {}", join(&BitDepthFormat::ALL));
    println!("  quality         {}", join(&ResampleQuality::ALL));
    println!("  on_clip         {}", join(&OnClipPolicy::ALL));
    println!("  limiter         {}", join(&LimiterCharacter::ALL));
    println!("  dither          {}", join(&DitherMode::ALL));
    println!("  dither_strength {}", join(&DitherStrength::ALL));
    println!("  correlation     {}", join(&DitherCorrelation::ALL));
    println!("  preset          {}", PRESET_NAMES.join(", "));
}

fn dump_targets(config: &Config, path: &Path) -> Result<()> {
    println!("targets in {} (source assumed {NOMINAL_SOURCE_RATE} Hz):", path.display());
    for name in config.target_names() {
        let target = config.resolve_target(&name)?;
        let out = target.render_output_path(Path::new("master.wav"), NOMINAL_SOURCE_RATE)?;
        println!("\n  [{name}] -> {}", out.display());

        // `delivery_blocker` needs the *input* to answer, which is why the plan matrix
        // cannot be filled in from the config alone. Channel count is unknown here.
        if let Some(blocker) = target.delivery_blocker(NOMINAL_SOURCE_RATE, None) {
            println!("    ! {blocker}");
        }
        for step in target.describe(NOMINAL_SOURCE_RATE) {
            println!("    - {step}");
        }
    }
    Ok(())
}

fn join<T: std::fmt::Display>(values: &[T]) -> String {
    values.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ")
}

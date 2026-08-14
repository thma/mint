//! `mint-gui` — window boot, renderer selection, and a headless self-test.
//!
//! Usage:
//! ```text
//! mint-gui [path/to/mint.toml]      # open the workbench
//! mint-gui --renderer glow [path]   # force the OpenGL backend (also MINT_RENDERER=glow)
//! mint-gui --selftest [path]        # run one frame headless and exit; no window
//! ```

use std::path::PathBuf;

use anyhow::Result;
use mint_gui::app::MintApp;
use mint_gui::state::AppState;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut path: Option<PathBuf> = None;
    let mut renderer: Option<String> = std::env::var("MINT_RENDERER").ok();
    let mut selftest = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--renderer" => renderer = args.next(),
            "--selftest" => selftest = true,
            other => path = Some(PathBuf::from(other)),
        }
    }
    let path = path.unwrap_or_else(|| PathBuf::from("mint.toml"));
    let state = AppState::load(&path);

    if selftest {
        return selftest_report(&state);
    }

    let options = eframe::NativeOptions {
        // Both backends are compiled in and the choice is made here (spike gate S7).
        // wgpu is the default because it is the one that gets the attention upstream;
        // glow is the escape hatch for a machine where wgpu comes up as a black window,
        // and an escape hatch that needs a rebuild is not one.
        renderer: match renderer.as_deref() {
            Some("glow") => eframe::Renderer::Glow,
            _ => eframe::Renderer::Wgpu,
        },
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([820.0, 520.0])
            .with_title("mint"),
        ..Default::default()
    };

    eframe::run_native(
        "mint",
        options,
        Box::new(|_cc| Ok(Box::new(MintApp::new(state)))),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
}

/// No window, no GPU, no display: load the config, print what the library says about it,
/// and exit non-zero if it does not resolve.
///
/// This is what a smoke job runs on a clean CI image to prove the shipped binary actually
/// links and starts (proposal §9: "bundle verification happens on CI artefacts in clean
/// VMs, never on a dev box"). It also makes the app runnable over SSH, which is how most
/// of this spike was exercised.
fn selftest_report(state: &AppState) -> Result<()> {
    println!("config: {}", state.path.display());

    // S2's real question is whether validation can run *per frame*, so measure it as a
    // frame would experience it: warm, repeated, and reported as a median rather than as
    // the one cold sample that includes first-touch costs.
    let mut warm = AppState::load(&state.path);
    let mut samples: Vec<u128> = Vec::new();
    for _ in 0..200 {
        warm.revalidate();
        samples.push(warm.validate_micros);
    }
    samples.sort_unstable();
    println!(
        "validate: cold {} µs, warm median {} µs, worst {} µs  (16 600 µs is one frame at 60 Hz)",
        state.validate_micros,
        samples[samples.len() / 2],
        samples[samples.len() - 1],
    );
    match state.error() {
        Some(error) => {
            println!("status: REJECTED");
            println!("{error}");
            std::process::exit(1);
        }
        None => println!("status: ok, {} targets", state.target_names().len()),
    }
    for name in state.target_names() {
        let config = state.config().expect("checked above");
        let (_, explained) = config.resolve_target_explained(&name)?;
        let flagged = explained
            .fields
            .iter()
            .filter(|f| f.origin.dropped.is_some() || f.origin.illegal_here)
            .count();
        println!("  {name}: {} fields, {flagged} flagged", explained.fields.len());
    }
    Ok(())
}

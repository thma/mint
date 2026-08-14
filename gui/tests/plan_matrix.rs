//! Spike gate S4: the plan matrix at size, measured rather than eyeballed.
//!
//! The gate says "20×8 scrolling at 60 fps". Frames per second is not something a headless
//! harness can honestly report — there is no compositor and no vsync — but the question
//! underneath it is answerable and is the one that decides the design: **how long does one
//! frame of the matrix take to build?** Every cell calls `delivery_blocker`,
//! `render_output_path` and `estimated_output_bytes` on the real target, so the risk was
//! always that per-cell library work, not egui's painting, is what blows the budget.
//!
//! One frame at 60 Hz is 16 600 µs. This prints what it costs; the assertion is set an
//! order of magnitude above the measured figure so it fails on a regression rather than on
//! a slow CI box.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui_kittest::Harness;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint_gui::app::{MintApp, Tab};
use mint_gui::state::AppState;

/// Eight targets covering every codec branch, so the per-cell work is representative
/// rather than eight copies of the cheapest case.
const EIGHT_TARGETS: &str = r#"
[target.cd]
preset = "cd"
lufs = -12.0
[target.streaming]
preset = "apple-music"
[target.broadcast]
preset = "broadcast-ebu-r128"
bwf = true
[target.hires]
preset = "hires"
[target.archive]
format = "f32"
[target.lossless]
codec = "flac"
format = "s24"
rate = 48000
[target.sacd]
preset = "sacd"
[target.dsd256]
codec = "dsf"
dsd = "dsd256"
"#;

fn write_master(path: &Path) {
    let frames = 48_000;
    let tone: Vec<f64> = (0..frames)
        .map(|n| 0.3 * (2.0 * std::f64::consts::PI * 997.0 * n as f64 / 48_000.0).sin())
        .collect();
    let buffer = AudioBuffer {
        channels: vec![tone.clone(), tone],
        sample_rate: 48_000,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: 2,
            sample_rate: 48_000,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    io_write::write_wav(path, &buffer, None).expect("write master");
}

#[test]
fn a_twenty_by_eight_matrix_builds_well_inside_a_frame() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("mint-gui-plan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let master = dir.join("master.wav");
    write_master(&master);

    let config = dir.join("mint.toml");
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    std::fs::write(&config, format!("[defaults]\ndir = \"{escaped}\"\n{EIGHT_TARGETS}"))
        .expect("write config");

    let mut state = AppState::load(&config);
    assert!(state.error().is_none(), "fixture must resolve: {:?}", state.error());
    assert_eq!(state.target_names().len(), 8);
    state.inputs.push(master);

    let mut app = MintApp::new(state);
    app.select_tab(Tab::Plan);
    // The rows are the real source repeated, so all 160 cells do genuine work; only the
    // probe is shared, which is what a real batch of takes from one session looks like.
    app.set_plan_rows(20);
    let mut harness =
        Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), app);

    harness.run(); // warm: font atlas, table layout state, the probe

    const FRAMES: u32 = 30;
    let started = Instant::now();
    for _ in 0..FRAMES {
        harness.step();
    }
    let per_frame = started.elapsed().as_micros() / u64::from(FRAMES) as u128;

    println!(
        "plan matrix 20x8 = 160 cells: {per_frame} µs/frame \
         (16 600 µs is one frame at 60 Hz; this is a debug build)"
    );
    assert!(
        per_frame < 16_600,
        "one frame of the matrix took {per_frame} µs, over the whole 60 Hz budget"
    );
}

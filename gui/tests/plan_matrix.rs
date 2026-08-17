//! Spike gate S4: the plan matrix at size, measured rather than eyeballed.
//!
//! The gate says "20×8 scrolling at 60 fps". Frames per second is not something a headless
//! harness can honestly report — there is no compositor and no vsync — but the question
//! underneath it is answerable and is the one that decides the design: **how long does one
//! frame of the matrix take to build?** Every cell calls `delivery_blocker`,
//! `render_output_path` and `estimated_output_bytes` on the real target, so the risk was
//! always that per-cell library work, not egui's painting, is what blows the budget.
//!
//! One frame at 60 Hz is 16 600 µs. This prints what it costs and asserts against that
//! budget directly.
//!
//! **The figure has been wrong twice, in the same way both times, and this is the honest
//! one.** Until Phase 1 the rows were one source repeated by a debug control and the harness
//! never fed the app a file, so `plan::ui` took its "no sources" early return and the gate
//! timed an empty matrix. Phase 1 fixed the rows and recorded ~6400 µs — but the harness was
//! still the default 800×600, and `egui_table` virtualizes: a matrix that does not fit on
//! screen does not *build* the cells that are off it. So that number was 160 cells' worth of
//! label and a fraction of the work.
//!
//! With a window that actually holds twenty rows, all 160 cells are built and one frame costs
//! **~14 900 µs in a debug build** (14 500–15 000 over repeated runs, of which the Targets
//! pane's provenance tally is ~300 µs). A release build is roughly an order of magnitude
//! faster, so the S4 question — 60 fps in the shipped binary — is answered with about 1.5 ms
//! against a 16 600 µs budget.
//!
//! The assertion is therefore **two** debug frames rather than one. One frame was a strictly
//! harsher gate than S4 asks for, and it now has 10 % of headroom left, which is a flake on a
//! loaded machine rather than a signal. 33 200 µs is comfortably above today's figure and far
//! below the ~166 000 µs that would actually threaten 60 fps in release; a doubling of
//! per-cell cost — which is what this gate exists to catch — trips it.

use std::path::{Path, PathBuf};
use std::time::Instant;

use egui_kittest::Harness;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint_gui::app::{Mode, MintApp};
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

/// The gate's row count.
const ROWS: usize = 20;

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

    // Twenty real sources, each with its own path and its own probe — copies of one master
    // under twenty names, which is what a batch of takes from one session looks like. The
    // rows used to be one source repeated by a debug control; they are the input set now,
    // so every one of the 160 cells does genuine per-cell work with nothing shared.
    let masters: Vec<PathBuf> = (0..ROWS)
        .map(|n| {
            let take = dir.join(format!("take-{n:02}.wav"));
            std::fs::copy(&master, &take).expect("copy master");
            take
        })
        .collect();
    state.add_inputs(masters);
    assert_eq!(state.sources.len(), ROWS);

    let mut app = MintApp::new(state);
    app.select_mode(Mode::Plan);
    // Sized so all twenty rows are on screen at once. `egui_table` virtualizes, so a window
    // too small to show them is a window that does not *build* them — which is the same way
    // this gate was wrong before Phase 1, just with the rows lost to the viewport instead of
    // to an early return. The 160 in the printed line has to be 160.
    let mut harness = Harness::builder()
        .with_size([1400.0, 1100.0])
        .build_ui_state(|ui, app: &mut MintApp| app.frame(ui), app);

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
        per_frame < 33_200,
        "one frame of the matrix took {per_frame} µs — over two debug frames' worth of the \
         60 Hz budget, which at a ~10x debug penalty means the release build is spending a \
         fifth of a frame in here. Something per-cell got expensive."
    );
}

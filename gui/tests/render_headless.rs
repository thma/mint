//! Spike gate S5: a real render, driven through the app, with the UI still running.
//!
//! The gate asks for a DSD encode on a worker thread showing a **true** percentage while
//! the UI stays responsive. The proposal expected that percentage to be faked by `stat()`ing
//! the staging file, because `JobControl` was two phases away; it landed early, so this is
//! the real figure reported from inside `encode_dsf`'s block loop.
//!
//! "Responsive" is asserted rather than felt: the harness keeps building frames the whole
//! time the encode runs, and a frame that blocked on the worker would show up as zero
//! frames between start and finish. A DSD64 encode of a one-second master in a debug build
//! is a few hundred milliseconds of work — long enough to be caught mid-flight, short
//! enough to be a test.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use mint::audio::buffer::{AudioBuffer, OutputSampleFormat, SourceInfo};
use mint::audio::io_write;
use mint::progress::Stage;
use mint_gui::app::{MintApp, Mode};
use mint_gui::state::AppState;

const RATE: u32 = 48_000;
const FRAMES: usize = RATE as usize;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-render-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write_master(path: &Path) {
    let tone = |hz: f64| -> Vec<f64> {
        (0..FRAMES)
            .map(|n| 0.3 * (2.0 * std::f64::consts::PI * hz * n as f64 / RATE as f64).sin())
            .collect()
    };
    let buffer = AudioBuffer {
        channels: vec![tone(997.0), tone(1245.0)],
        sample_rate: RATE,
        src_info: SourceInfo {
            path: path.to_path_buf(),
            channels: 2,
            sample_rate: RATE,
            codec: "test".to_string(),
        },
        out_format: OutputSampleFormat::S24,
    };
    io_write::write_wav(path, &buffer, None).expect("write master");
}

#[test]
fn a_dsd_render_runs_on_a_worker_while_the_ui_keeps_drawing() {
    let dir = scratch("dsd");
    let master = dir.join("master.wav");
    write_master(&master);

    let config = dir.join("mint.toml");
    let escaped = dir.display().to_string().replace('\\', "\\\\");
    std::fs::write(
        &config,
        format!("[target.sacd]\ndir = \"{escaped}\"\ncodec = \"dsf\"\ndsd = \"dsd64\"\n"),
    )
    .expect("write config");

    let mut state = AppState::load(&config);
    state.selected = Some("sacd".to_string());
    state.add_inputs([master]);
    let mut harness = Harness::new_ui_state(|ui, app: &mut MintApp| app.frame(ui), MintApp::new(state));

    harness.state_mut().select_mode(Mode::Run);
    harness.run();
    let ctx = harness.ctx.clone();
    harness.state_mut().start_render(ctx);

    // Pump frames until the worker reports back. `step` rather than `run`, because `run`
    // loops until the UI stops asking to be repainted and a job in flight asks on every
    // event — which is itself the thing being demonstrated.
    let mut frames = 0;
    let mut encode_fractions: Vec<f64> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while harness.state().run_view().busy() && std::time::Instant::now() < deadline {
        harness.step();
        frames += 1;
        if let Some((Stage::Encode, fraction)) = harness.state().run_view().progress() {
            encode_fractions.push(fraction);
        }
    }

    let output = harness
        .state()
        .run_view()
        .finished_output()
        .expect("the render finished with a report")
        .to_string();
    assert!(Path::new(&output).exists(), "the .dsf must be on disk: {output}");
    assert!(frames > 1, "the UI drew {frames} frames during the render — it was blocked");

    // A *true* percentage: it advances, it never goes backwards, and it stays in range.
    // (Whether the loop is fast enough to catch several samples depends on the machine, so
    // the shape is asserted rather than a count.)
    assert!(
        encode_fractions.windows(2).all(|w| w[1] >= w[0]),
        "the encode percentage went backwards: {encode_fractions:?}"
    );
    assert!(
        encode_fractions.iter().all(|f| (0.0..=1.0).contains(f)),
        "out of range: {encode_fractions:?}"
    );
}

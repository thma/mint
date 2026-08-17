//! The verdict column, and the two §7 rules the Report view is shaped around.
//!
//! The whole reason this screen exists is that `pipeline` already computes the judgement —
//! `LUFS_TOLERANCE`, ±0.1 LU — and then buries it in a warning string that scrolls past
//! once. So the assertions are: a delivery that misses its declared `lufs` renders `✗`, one
//! that hits it renders `✓`, and one that declared nothing renders neither (a green tick
//! over a target that promised nothing is a verdict on a question nobody asked).
//!
//! The failure this exists to catch is not "the glyph is wrong". It is the glyph being
//! computed *here* — one `abs() <= 0.1` in the view — which would pass on the day it was
//! written and drift from the warning printed on the same row the moment the tolerance
//! moved. Hence the boundary case below: a report 0.1 LU out is a **pass**, and it is a pass
//! because `meets_target` says so, not because this file agrees.
//!
//! The other two assertions are §7 items 9 and 14: every warning is on screen in full
//! without anything being clicked, and the failures are their own section carrying mint's
//! text verbatim.

use std::path::PathBuf;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use mint::diagnostics::{FailureKind, WarningKind};
use mint::ops::loudness::{LUFS_TOLERANCE, LoudnessAnalysis};
use mint::pipeline::{Conflict, ProcessingReport, TaskFailure};
use mint_gui::report::ReportView;
use mint_gui::state::AppState;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mint-gui-verdict-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn analysis(integrated: f64, true_peak: f64, range: f64) -> LoudnessAnalysis {
    LoudnessAnalysis {
        integrated_lufs: integrated,
        loudness_range_lu: range,
        max_momentary_lufs: integrated + 2.0,
        max_short_term_lufs: integrated + 1.0,
        true_peak_dbtp: true_peak,
    }
}

/// A synthesized report rather than a real render: what is under test is the rendering of
/// the verdict, and a synthesized delivery can be placed exactly on the tolerance boundary,
/// which no amount of real audio can be asked to do.
fn report(target: &str, delivered_lufs: f64, declared: Option<f64>) -> ProcessingReport {
    ProcessingReport {
        target: target.into(),
        input: "/masters/take.wav".into(),
        output: format!("/out/take-{target}.wav"),
        in_rate: 96_000,
        out_rate: 44_100,
        channels: 2,
        frames: 44_100,
        file_bytes: 176_400,
        elapsed_ms: 1234,
        source: analysis(-18.3, -3.1, 9.2),
        delivered: analysis(delivered_lufs, -1.0, 7.1),
        declared_lufs: declared,
        declared_ceiling_dbtp: -1.0,
        limiter_gain_reduction_db: Some(2.3),
        limiter_avg_gain_reduction_db: Some(0.8),
        limiter_active_percent: Some(4.1),
        loudness_gain_db: Some(3.4),
        post_resample_gain_db: Some(0.0),
        src_quality: Some("vhq"),
        dither_mode: Some("adaptive"),
        dither_strength: Some("low"),
        seed: Some(7),
        dithered: true,
        shaped: false,
        shaping_curve: None,
        codec: "wav".into(),
        bwf: false,
        format: "s24".into(),
        dsd: None,
        warnings: vec![WarningKind::LoudnessTargetMissed {
            target_lufs: -14.0,
            delivered_lufs,
        }],
    }
}

/// The view, its document and one batch's outcome, in the shape `MintApp` hands them over.
struct Fixture {
    view: ReportView,
    state: AppState,
    reports: Vec<ProcessingReport>,
    failures: Vec<TaskFailure>,
}

fn harness(reports: Vec<ProcessingReport>, failures: Vec<TaskFailure>) -> Harness<'static, Fixture> {
    let dir = scratch("view");
    let config = dir.join("mint.toml");
    std::fs::write(&config, "[target.cd]\npreset = \"cd\"\n").expect("write config");
    Harness::new_ui_state(
        |ui, fixture: &mut Fixture| {
            fixture.view.ui(ui, &fixture.state, &fixture.reports, &fixture.failures)
        },
        Fixture {
            view: ReportView::default(),
            state: AppState::load(&config),
            reports,
            failures,
        },
    )
}

#[test]
fn a_miss_renders_a_cross_and_a_hit_renders_a_tick() {
    let mut harness = harness(
        vec![
            report("cd", -14.4, Some(-14.0)),      // 0.4 LU out — a miss
            report("hires", -14.02, Some(-14.0)),  // inside the tolerance — a hit
            report("archive", -20.0, None),        // declared nothing at all
        ],
        vec![TaskFailure {
            target: "sacd".into(),
            input: "/masters/take.wav".into(),
            error: "refusing to overwrite the input file without --force".into(),
            kind: FailureKind::OutputConflict(Conflict::Input),
        }],
    );
    harness.run();

    assert!(
        harness.query_all_by_label_contains("✗ -14.4").next().is_some(),
        "a delivery 0.4 LU from its target must read as a miss"
    );
    assert!(
        harness.query_all_by_label_contains("✓ -14.0").next().is_some(),
        "one inside the tolerance must read as a hit"
    );
    assert!(
        harness.query_all_by_label_contains("n/a").next().is_some(),
        "a target that declared no lufs gets no verdict — not a green tick"
    );

    // §7 item 9: the warning is on screen, whole, with nothing clicked and nothing unfolded.
    assert!(
        harness.query_all_by_label_contains("loudness target not met").next().is_some(),
        "every warning is rendered in full"
    );
    // §7 item 14: the failure is its own row, in mint's words.
    assert!(
        harness.query_all_by_label_contains("refusing to overwrite").next().is_some(),
        "the failure text is mint's own, verbatim"
    );
    // The figure no other mint surface prints: what hitting the target cost in dynamics.
    assert!(
        harness.query_all_by_label_contains("LU spent").next().is_some(),
        "the LRA cost belongs on the detail card"
    );
}

#[test]
fn the_boundary_belongs_to_the_library_and_is_a_pass() {
    // Exactly `LUFS_TOLERANCE` out, and *exactly* is the whole point: that distance is the
    // only one at which `<=` and `<` disagree, so it is the only fixture that can tell a
    // verdict delegated to `meets_target` apart from a plausible-looking copy of it here.
    //
    // Hence the unrealistic figures. A pair like -14.0 / -14.1 is 0.0999999999999996 apart —
    // the boundary test that never reaches the boundary, which any threshold in the
    // neighbourhood passes. `-t` and `-2t` do reach it for any `t`: doubling is exact in
    // binary, and so is subtracting two values within a factor of two of each other, so the
    // difference below *is* the tolerance rather than the tolerance plus a rounding error.
    let declared = -LUFS_TOLERANCE;
    let delivered = 2.0 * declared;
    assert_eq!(
        (delivered - declared).abs(),
        LUFS_TOLERANCE,
        "the fixture must sit exactly on the line — re-pick it if the tolerance moved"
    );

    let report = report("cd", delivered, Some(declared));
    assert_eq!(report.lufs_verdict(), Some(true), "the library's own answer: `<=` is a pass");

    let mut harness = harness(vec![report], Vec::new());
    harness.run();
    assert!(
        harness.query_all_by_label_contains("✓ -0.2").next().is_some(),
        "a delivery exactly on the tolerance renders as a pass"
    );
}

#[test]
fn sorting_a_column_does_not_lose_the_verdicts() {
    // The header click path is the only mutable thing in this view; a comparator that
    // panicked on a `-inf` meter reading, or a sort that reversed the vector and shuffled
    // ties, would show up here rather than at a listening session.
    let mut harness = harness(
        vec![
            report("cd", -14.4, Some(-14.0)),
            report("hires", -14.02, Some(-14.0)),
            report("silent", f64::NEG_INFINITY, Some(-14.0)),
        ],
        Vec::new(),
    );
    harness.run();
    // Exact labels: `LUFS` also appears inside every delivered/source figure on the detail
    // card, and the header is the one carrying the sort arrow. Twice, because the second
    // click on the same header flips the direction.
    harness.get_by_label("LUFS").click();
    harness.run();
    harness.get_by_label("LUFS ▲").click();
    harness.run();
    assert!(harness.query_all_by_label_contains("✗ -14.4").next().is_some());
    assert!(harness.query_all_by_label_contains("✓ -14.0").next().is_some());
}

/// §7 item 9 has a geometric half that a label query cannot see.
///
/// "Every warning is on screen in full" was asserted by looking the sentence up by its
/// accessibility label — and a label survives being clipped. A `Grid` briefly replaced the
/// warnings list here and, because an egui grid cell lays out with `TextWrapMode::Extend`
/// rather than `Wrap`, every long warning was drawn as one line running past the right edge
/// of a vertical-only `ScrollArea`: measured 986 px of text in 784 px of panel, with the
/// remedy — the part that says *what to do about it* — off screen. The old assertion passed
/// throughout.
///
/// So this one measures. `HeavyLimiting` is the longest sentence mint produces and the one
/// whose tail carries the advice, which makes it the right fixture.
#[test]
fn a_long_warning_wraps_instead_of_running_off_the_right_edge() {
    let warning = WarningKind::HeavyLimiting {
        gain_reduction_db: 4.2,
        target_lufs: -14.0,
        ceiling_dbtp: -1.0,
        source_true_peak_dbtp: -0.2,
    };
    let text = warning.message();
    // ~171 characters, which at this font is roughly 1 200 px against a 784 px panel.
    assert!(text.len() > 150, "the fixture must be long enough to overflow: {} chars", text.len());
    let tail = "consider a lower target";
    assert!(text.contains(tail), "the advice is the part that gets clipped first");

    let mut report = report("cd", -14.4, Some(-14.0));
    report.warnings = vec![warning];
    let mut harness = harness(vec![report], Vec::new());
    harness.run();

    // Asked as "did it wrap?" rather than "does it fit?": the harness reports no screen rect,
    // so the reference is one text line — taken from the `⚠` glyph beside the sentence, which
    // is a single line by construction and laid out by the same code in the same row.
    let line = harness
        .query_all_by_label_contains("⚠")
        .map(|node| node.rect().height())
        .fold(f32::INFINITY, f32::min);
    assert!(line.is_finite(), "the warning rows must render their glyph");
    let node = harness
        .query_all_by_label_contains(tail)
        .next()
        .expect("the whole sentence, advice included, must be rendered");
    let drawn = node.rect();
    assert!(
        drawn.height() > line * 1.5,
        "the warning was laid out {} px tall against a {line} px line, i.e. on one line, so a \
         {} px sentence is running off the right edge of a vertical-only ScrollArea — a \
         warning you can only read half of is collapsed by another name (§7 item 9). An \
         `egui::Grid` cell lays out with TextWrapMode::Extend; `horizontal_wrapped` wraps.",
        drawn.height(),
        drawn.width()
    );
}

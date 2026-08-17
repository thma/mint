//! The derived chain: `ResolvedTarget::chain` as data, and `describe()` as its formatter.
//!
//! Two things are pinned here, and the first one is why the file exists.
//!
//! 1. **The prose did not move.** `describe()` used to build its sentences inline; it now
//!    maps `ChainStep::line` over `chain()`. `tests/fixtures/chain_golden.txt` is the full
//!    dry-run text for a matrix that walks every `ALL` array and every branch of the
//!    derivation — **captured from the pre-refactor implementation** and compared
//!    byte-for-byte. `README.md`'s dry-run sample and ~15 substring assertions in
//!    `config_parse.rs` / `dsd_pipeline.rs` read these bytes, and a substring assertion
//!    cannot notice a line that changed somewhere it does not look.
//!
//!    Regenerate deliberately, never reflexively: `MINT_BLESS_CHAIN_GOLDEN=1 cargo test
//!    --test chain`. A diff here means the dry-run's wording changed, which is a README
//!    change too.
//!
//! 2. **There is one producer.** `describe()` must be exactly `chain().map(line)` — the
//!    property that stops `ChainStep` from becoming a parallel third representation of the
//!    chain alongside `pipeline::process` and `estimated_output_bytes`.
//!
//! The matrix is generated from the `ALL` arrays rather than written out, so a new enum
//! variant lands in the golden automatically and shows up as a diff — the same tripwire
//! `tests/enum_domains.rs` is, one level further out.

use std::fmt::Write as _;

use mint::chain::{ChainStep, ChainStepKind};
use mint::config::{
    BitDepthFormat, Config, DitherMode, DitherStrength, LimiterCharacter, OnClipPolicy,
    ResampleQuality, ResolvedTarget,
};
use mint::ops::dsd::DsdRate;

const GOLDEN: &str = "tests/fixtures/chain_golden.txt";

/// One row of the matrix: a label, the target's TOML body, and the source rate to derive at.
struct Case {
    label: String,
    toml: String,
    source_rate: u32,
}

fn case(label: impl Into<String>, body: &str, source_rate: u32) -> Case {
    Case {
        label: label.into(),
        toml: format!("[target.t]\ndir = \"./out\"\n{body}"),
        source_rate,
    }
}

/// Every branch of the derivation, walked from the `ALL` arrays wherever there is one.
fn matrix() -> Vec<Case> {
    let mut cases = Vec::new();

    // 1. The quantize tail: format × dither mode × strength. Covers `effective_dither_tag`'s
    //    s16-only downgrade in both directions and the float passthrough.
    for format in BitDepthFormat::ALL {
        for dither in DitherMode::ALL {
            for strength in DitherStrength::ALL {
                cases.push(case(
                    format!("pcm {format} / {dither} / {strength}"),
                    &format!(
                        "format = \"{format}\"\ndither = \"{dither}\"\n\
                         dither_strength = \"{strength}\"\n"
                    ),
                    44_100,
                ));
            }
        }
    }

    // 2. Containers. FLAC refuses the float formats, so it is asked only about the integer
    //    ones; `bwf` is WAV-only for the same reason.
    for format in [BitDepthFormat::S16, BitDepthFormat::S24] {
        cases.push(case(
            format!("flac {format}"),
            &format!("format = \"{format}\"\ncodec = \"flac\"\n"),
            44_100,
        ));
    }
    cases.push(case("bwf wav", "format = \"s24\"\nbwf = true\n", 48_000));
    cases.push(case("mp3 default", "codec = \"mp3\"\n", 44_100));
    cases.push(case("mp3 320", "codec = \"mp3\"\nmp3_bitrate = 320\n", 44_100));

    // 3. Rate handling: kept, converted, and converted at each quality tier. The
    //    `PostSrcVerify` line has two forms, so both are asked with and without `lufs`.
    for quality in ResampleQuality::ALL {
        cases.push(case(
            format!("resample / {quality}"),
            &format!("format = \"s16\"\nrate = 44100\nquality = \"{quality}\"\n"),
            96_000,
        ));
    }
    cases.push(case("rate kept", "format = \"s24\"\nrate = 48000\n", 48_000));
    cases.push(case("rate follows source", "format = \"s24\"\n", 88_200));
    cases.push(case(
        "resample with lufs",
        "format = \"s16\"\nrate = 44100\nlufs = -14.0\n",
        96_000,
    ));
    cases.push(case("resample without lufs", "format = \"s16\"\nrate = 44100\n", 96_000));

    // 4. The loudness line: every clip policy against every limiter character, both
    //    link/soft-clip flags, and the `maximize` cap that only one of them prints.
    for on_clip in OnClipPolicy::ALL {
        for character in LimiterCharacter::ALL {
            for soft_clip in [false, true] {
                for link in [false, true] {
                    cases.push(case(
                        format!("loudness / {on_clip} / {character} / soft={soft_clip} / link={link}"),
                        &format!(
                            "format = \"s24\"\nlufs = -12.5\nceiling_dbtp = -0.3\n\
                             on_clip = \"{on_clip}\"\nlimiter_character = \"{character}\"\n\
                             limiter_soft_clip = {soft_clip}\nlimiter_link_channels = {link}\n\
                             max_limiting_db = 6.5\n"
                        ),
                        48_000,
                    ));
                }
            }
        }
    }
    // The same two lines without any `lufs`, which is the other opening step entirely.
    cases.push(case("no loudness", "format = \"s24\"\n", 48_000));

    // 5. DSD. Every rate, at a 44.1- and a 48-family source, with and without an explicit
    //    intermediate `rate` (the `, derived` suffix) and with and without `lufs`. A DSD
    //    target refuses `format`, so none of these state one.
    for dsd in DsdRate::ALL {
        for source in [44_100_u32, 48_000, 96_000, 192_000] {
            cases.push(case(
                format!("dsd {} / source {source} / derived", dsd.tag()),
                &format!("codec = \"dsf\"\ndsd = \"{}\"\n", dsd.tag()),
                source,
            ));
        }
        cases.push(case(
            format!("dsd {} / rate stated", dsd.tag()),
            &format!("codec = \"dsf\"\ndsd = \"{}\"\nrate = 88200\n", dsd.tag()),
            96_000,
        ));
        cases.push(case(
            format!("dsd {} / lufs", dsd.tag()),
            &format!("codec = \"dsf\"\ndsd = \"{}\"\nlufs = -16.0\n", dsd.tag()),
            96_000,
        ));
    }
    cases.push(case("sacd preset", "preset = \"sacd\"\n", 96_000));

    cases
}

fn resolve(case: &Case) -> ResolvedTarget {
    Config::from_toml_str(&case.toml)
        .unwrap_or_else(|err| panic!("case '{}' does not load: {err:#}\n{}", case.label, case.toml))
        .resolve_target("t")
        .unwrap_or_else(|err| panic!("case '{}' does not resolve: {err:#}", case.label))
}

/// The dry-run text for the whole matrix, in one document.
fn render_matrix() -> String {
    let mut out = String::new();
    for case in matrix() {
        let target = resolve(&case);
        writeln!(out, "## {} @ {} Hz", case.label, case.source_rate).unwrap();
        for step in target.describe(case.source_rate) {
            writeln!(out, "  - {step}").unwrap();
        }
    }
    out
}

#[test]
fn the_dry_run_text_is_byte_identical_to_the_golden() {
    let rendered = render_matrix();
    if std::env::var_os("MINT_BLESS_CHAIN_GOLDEN").is_some() {
        std::fs::write(GOLDEN, &rendered).expect("write golden");
        return;
    }
    let golden = std::fs::read_to_string(GOLDEN).unwrap_or_else(|err| {
        panic!("{GOLDEN} is missing ({err}); regenerate with MINT_BLESS_CHAIN_GOLDEN=1")
    });
    if rendered == golden {
        return;
    }
    // Report the first divergence rather than two 1 500-line blobs.
    let first = rendered
        .lines()
        .zip(golden.lines())
        .enumerate()
        .find(|(_, (a, b))| a != b)
        .map(|(n, (a, b))| format!("line {}:\n  now:    {a}\n  golden: {b}", n + 1))
        .unwrap_or_else(|| {
            format!(
                "same prefix, different length: {} lines now, {} in the golden",
                rendered.lines().count(),
                golden.lines().count()
            )
        });
    panic!("the derived chain's prose changed.\n{first}\n\nIf that was intended, update README.md's dry-run sample too, then re-bless with MINT_BLESS_CHAIN_GOLDEN=1.");
}

#[test]
fn describe_is_exactly_the_formatter_over_chain() {
    for case in matrix() {
        let target = resolve(&case);
        let structured: Vec<String> =
            target.chain(case.source_rate).iter().map(ChainStep::line).collect();
        assert_eq!(
            target.describe(case.source_rate),
            structured,
            "case '{}': describe() and chain()+line() disagree, so there are two producers",
            case.label
        );
    }
}

#[test]
fn every_step_carries_a_kind_and_a_stage_that_match_its_role() {
    for case in matrix() {
        let target = resolve(&case);
        for step in target.chain(case.source_rate) {
            let kind = step.kind();
            // The steps that report no stage, enumerated rather than reasoned about: a step
            // with `None` is one a progress-driven front end would wait on forever, so the
            // set has to be deliberate. `KeepRate` and `WriteFloat` describe an absence;
            // `WriteContainer` is the WAV/FLAC writer, which is one blocking call into an
            // encoder crate and reports nothing (only the MP3 arm reports `Encode`). If
            // progress is ever added to a writer, this assertion is where it is noticed.
            match (kind, step.stage()) {
                (ChainStepKind::Note, None) => assert!(step.is_note()),
                (_, None) => assert!(
                    matches!(
                        step,
                        ChainStep::KeepRate { .. }
                            | ChainStep::WriteFloat { .. }
                            | ChainStep::WriteContainer { .. }
                    ),
                    "case '{}': {step:?} reports no pipeline stage",
                    case.label
                ),
                (ChainStepKind::Note, Some(stage)) => {
                    panic!("case '{}': a note claims to run {stage:?}", case.label)
                }
                (_, Some(_)) => assert!(!step.is_note()),
            }
        }
    }
}

/// The three DSD steps are one `Stage::Encode`, because `encode_dsf` does all of it inside
/// one block loop. Pinned so a future split of that loop has to say so here.
#[test]
fn the_dsd_tail_maps_onto_the_single_encode_stage() {
    let config = Config::from_toml_str(
        "[target.t]\ndir = \"./out\"\ncodec = \"dsf\"\ndsd = \"dsd512\"\n",
    )
    .unwrap();
    let steps = config.resolve_target("t").unwrap().chain(96_000);

    let encode: Vec<_> = steps
        .iter()
        .filter(|s| s.stage() == Some(mint::progress::Stage::Encode))
        .collect();
    assert_eq!(encode.len(), 3, "interpolate + modulate + write, one stage: {steps:?}");
    assert!(
        matches!(encode[0], ChainStep::Interpolate { .. })
            && matches!(encode[1], ChainStep::Modulate { .. })
            && matches!(encode[2], ChainStep::WriteDsf),
    );
    // dsd512's advisory is the last entry and is a note, not a step.
    assert!(steps.last().unwrap().is_note(), "{steps:?}");
    // The modulator is the DSD quantizer — the reason `bitdepth::apply` is skipped in front
    // of it. If this ever reads `Write`, that connection has been lost.
    assert_eq!(encode[1].kind(), ChainStepKind::Depth);
    assert!(
        !steps.iter().any(|s| matches!(s, ChainStep::Quantize { .. })),
        "a DSD chain must contain no PCM quantize step: {steps:?}"
    );
}

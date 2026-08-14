//! Per-field provenance: which layer won, and what became of the ones that didn't.
//!
//! The value of `resolve_target_explained` is entirely in the cases where the four-layer
//! merge does *not* simply pick the topmost value, and those cases are exactly the ones no
//! existing surface shows. `--dry-run` prints the chain, by which point every value has
//! been flattened to one answer; `--json` reports the delivered file; the error messages
//! only fire when something is already fatal. So this file is built around the proposal's
//! demo config, the one target that exercises every fate at once.
//!
//! One thing deliberately *not* tested here: that the explanation's precedence matches the
//! merge's. It cannot drift — `Layers` is one ordered array that both the fold and the
//! explanation read — and a test asserting it would be asserting that an array equals
//! itself. What is worth pinning is the exceptions, which are hand-written per field.

use mint::config::{Config, Fate, Group, Layer};

/// The proposal's demo config: one DSD target under a PCM-shaped `[defaults]`, which is the
/// realistic way a heterogeneous config goes wrong.
const DEMO: &str = r#"
[defaults]
dir = "./out"
quality = "vhq"
format = "s24"
rate = 48000
bwf = true
dither = "psychoacoustic"
preset = "cd"

[target.sacd]
preset = "sacd"
lufs = -16.0
"#;

fn explain(toml: &str, target: &str) -> mint::config::TargetExplanation {
    Config::from_toml_str(toml)
        .unwrap_or_else(|e| panic!("config did not load: {e:#}"))
        .resolve_target_explained(target)
        .unwrap_or_else(|e| panic!("target did not resolve: {e:#}"))
        .1
}

#[test]
fn every_field_is_explained_exactly_once() {
    let explanation = explain(DEMO, "sacd");

    // 22 = every field of `RawTarget`. The compile-time half of this check is the exhaustive
    // destructure at the top of `config::explain`, which stops building when a field is
    // added; this half catches a row that was added twice or in the wrong group.
    assert_eq!(explanation.fields.len(), 22, "one row per RawTarget field");
    let mut names: Vec<&str> = explanation.fields.iter().map(|f| f.field).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    assert_eq!(names.len(), unique, "a field is explained twice");

    // Grouped in inspector order, not interleaved.
    let groups: Vec<Group> = explanation.fields.iter().map(|f| f.group).collect();
    let mut seen = Vec::new();
    for group in groups {
        if seen.last() != Some(&group) {
            assert!(!seen.contains(&group), "{group} appears in two separate runs of rows");
            seen.push(group);
        }
    }
}

#[test]
fn an_inherited_rate_is_dropped_on_a_dsd_target_rather_than_applied() {
    let explanation = explain(DEMO, "sacd");
    let rate = explanation.field("rate").expect("rate row");

    // Nothing in the target's own layer set it, so it is derived — even though `[defaults]`
    // very much did set one.
    assert_eq!(rate.origin.layer, None, "no own layer stated rate, so nothing won it");
    let dropped = rate.origin.dropped.as_ref().expect("the [defaults] rate must show as dropped");
    assert_eq!(dropped.layer, Layer::Defaults);
    assert_eq!(dropped.value, "48000");
    assert!(dropped.why.contains("power of two"), "the reason must teach the rule: {}", dropped.why);

    // ...and the inherited value is still visible, which is what lets the UI show what was
    // passed over instead of just saying "derived".
    assert!(
        rate.origin.stated.iter().any(|(l, v)| *l == Layer::Defaults && v == "48000"),
        "the dropped value stays in `stated`: {:?}",
        rate.origin.stated
    );
}

#[test]
fn an_inherited_format_is_inert_and_would_be_fatal_if_stated_here() {
    let explanation = explain(DEMO, "sacd");
    let format = explanation.field("format").expect("format row");

    // The asymmetry the whole facility exists for: same value, two different fates
    // depending on which layer states it.
    assert_eq!(format.origin.layer, Some(Layer::Defaults), "[defaults] s24 is carried");
    assert!(matches!(format.origin.fate, Fate::Inert(_)), "but nothing on a DSD target reads it");
    assert!(format.origin.illegal_here, "stating it on this target is a load-time error");
    assert!(format.value.contains("1-bit"), "the row says what actually ships: {}", format.value);

    // And the claim is checked, not asserted: the same value stated here really is rejected.
    let stated_here = format!("{DEMO}format = \"s24\"\n");
    let err = Config::from_toml_str(&stated_here).expect_err("format on a DSD target must fail");
    assert!(format!("{err:#}").contains("no meaning for a DSD target"), "{err:#}");
}

#[test]
fn an_inherited_bwf_is_dropped_off_wav_and_would_be_fatal_if_stated_here() {
    let explanation = explain(DEMO, "sacd");
    let bwf = explanation.field("bwf").expect("bwf row");

    assert_eq!(bwf.value, "false", "no bext chunk is written on a .dsf");
    assert_eq!(bwf.origin.layer, None, "the effective false came from the codec, not a layer");
    let dropped = bwf.origin.dropped.as_ref().expect("[defaults] bwf = true must show as dropped");
    assert_eq!(dropped.layer, Layer::Defaults);
    assert_eq!(dropped.value, "true");
    assert!(bwf.origin.illegal_here);

    let stated_here = format!("{DEMO}bwf = true\n");
    let err = Config::from_toml_str(&stated_here).expect_err("bwf=true off WAV must fail");
    assert!(format!("{err:#}").contains("only valid for codec=wav"), "{err:#}");
}

#[test]
fn a_defaults_preset_is_surfaced_as_the_bug_it_is() {
    // report.md defect #14: `[defaults] preset` parses (it is a RawTarget field) but
    // `resolve_target` reads `raw.preset`, never `merged.preset`. A GUI that reproduced the
    // bug would apply it; this makes it visible instead.
    let explanation = explain(DEMO, "sacd");
    let preset = explanation.field("preset").expect("preset row");

    assert_eq!(preset.origin.layer, Some(Layer::Explicit));
    assert_eq!(preset.value, "sacd");
    let dropped = preset.origin.dropped.as_ref().expect("the [defaults] preset must show as dropped");
    assert_eq!(dropped.value, "cd");
    assert!(dropped.why.contains("never applied"), "{}", dropped.why);
}

#[test]
fn inert_is_a_claim_about_a_code_path_not_a_guess() {
    // Every one of these is checkable against the source, which is the standard an "inert"
    // badge has to meet — a wrong one teaches the user something false.
    let explanation = explain(DEMO, "sacd");
    for field in ["dither", "dither_strength", "dither_correlation", "mp3_bitrate"] {
        let row = explanation.field(field).expect(field);
        assert!(matches!(row.origin.fate, Fate::Inert(_)), "{field} must be inert on a DSD target");
    }
    // ...and the ones that genuinely feed the DSD path are not swept up with them: the
    // interpolation to the 1-bit rate runs on `quality`.
    let quality = explanation.field("quality").expect("quality row");
    assert_eq!(quality.origin.fate, Fate::Applies);
    assert_eq!(quality.origin.layer, Some(Layer::Defaults));
}

#[test]
fn dither_strength_is_inert_unless_the_adaptive_path_actually_engages() {
    // `bitdepth::quantize_in_place` reads strength (and correlation) in the adaptive branch
    // only — the fixed curves and flat TPDF ignore both. That is report.md §9's claim, and
    // it is invisible everywhere else.
    let engaged = explain(
        "[target.t]\nformat = \"s16\"\ndither = \"adaptive\"\n",
        "t",
    );
    assert_eq!(engaged.field("dither_strength").unwrap().origin.fate, Fate::Applies);
    assert_eq!(engaged.field("dither_correlation").unwrap().origin.fate, Fate::Applies);

    // Same request one bit depth up: shaping is s16-only, so it degrades to flat TPDF and
    // the two knobs stop being read.
    let degraded = explain("[target.t]\nformat = \"s24\"\ndither = \"adaptive\"\n", "t");
    assert!(matches!(degraded.field("dither_strength").unwrap().origin.fate, Fate::Inert(_)));

    // And a mode that never reads them at all.
    let flat = explain("[target.t]\nformat = \"s16\"\ndither = \"tpdf\"\n", "t");
    assert!(matches!(flat.field("dither_strength").unwrap().origin.fate, Fate::Inert(_)));
    assert_eq!(flat.field("dither").unwrap().origin.fate, Fate::Applies);
}

#[test]
fn the_limiter_knobs_are_inert_under_a_policy_that_never_limits() {
    let limiting = explain("[target.t]\nformat = \"s16\"\nlufs = -14.0\non_clip = \"limit\"\n", "t");
    for field in ["limiter_character", "limiter_soft_clip", "limiter_link_channels"] {
        assert_eq!(limiting.field(field).unwrap().origin.fate, Fate::Applies, "{field}");
    }
    // max_limiting_db is maximize's guard; under `limit` nothing reads it.
    assert!(matches!(limiting.field("max_limiting_db").unwrap().origin.fate, Fate::Inert(_)));
    assert_eq!(limiting.field("warn_limiting_db").unwrap().origin.fate, Fate::Applies);

    let never = explain("[target.t]\nformat = \"s16\"\nlufs = -14.0\non_clip = \"reduce_gain\"\n", "t");
    for field in ["limiter_character", "limiter_soft_clip", "limiter_link_channels"] {
        assert!(matches!(never.field(field).unwrap().origin.fate, Fate::Inert(_)), "{field}");
    }

    // Under maximize the pair swaps roles: the cap is read, the warning is suppressed.
    let maximize = explain("[target.t]\nformat = \"s16\"\nlufs = -14.0\non_clip = \"maximize\"\n", "t");
    assert_eq!(maximize.field("max_limiting_db").unwrap().origin.fate, Fate::Applies);
    assert!(matches!(maximize.field("warn_limiting_db").unwrap().origin.fate, Fate::Inert(_)));
}

#[test]
fn an_inherited_dsd_rate_is_ignored_on_a_pcm_target() {
    let explanation = explain(
        "[defaults]\ndsd = \"dsd128\"\n\n[target.t]\nformat = \"s24\"\n",
        "t",
    );
    let dsd = explanation.field("dsd").expect("dsd row");

    assert_eq!(dsd.value, "—");
    assert_eq!(dsd.origin.layer, None, "the inherited value never became the effective one");
    assert_eq!(dsd.origin.dropped.as_ref().map(|d| d.value.as_str()), Some("dsd128"));
    assert!(dsd.origin.illegal_here, "stating it here needs codec = dsf");

    // The counterfactual, so the badge is not just decoration.
    let err = Config::from_toml_str("[target.t]\nformat = \"s24\"\ndsd = \"dsd128\"\n")
        .expect_err("dsd without codec=dsf must fail");
    assert!(format!("{err:#}").contains("needs codec=\"dsf\""), "{err:#}");
}

#[test]
fn a_plain_target_has_nothing_surprising_to_report() {
    // The negative case matters as much as the demo: if every ordinary field came back
    // flagged, the flags would carry no information.
    let explanation = explain("[target.cd]\npreset = \"cd\"\n", "cd");

    let dropped: Vec<&str> = explanation
        .fields
        .iter()
        .filter(|f| f.origin.dropped.is_some())
        .map(|f| f.field)
        .collect();
    assert!(dropped.is_empty(), "nothing should be dropped here: {dropped:?}");

    // `illegal_here` is the exception, and it is not noise: it is the answer to "can I type
    // this field on this target?", i.e. which controls a form must disable. On a WAV target
    // that is `dsd` and nothing else — `format` and `bwf` are both legal here.
    let illegal: Vec<&str> = explanation
        .fields
        .iter()
        .filter(|f| f.origin.illegal_here)
        .map(|f| f.field)
        .collect();
    assert_eq!(illegal, ["dsd"], "only dsd is unavailable on a PCM target");

    // The preset's own contribution is attributed to it by name, not to "a preset".
    let codec = explanation.field("rate").expect("rate row");
    assert_eq!(codec.origin.layer, Some(Layer::Preset("cd".to_string())));
    assert_eq!(codec.value, "44100 Hz");
}

#[test]
fn explaining_a_target_resolves_it_identically() {
    let config = Config::from_toml_str(DEMO).expect("config");
    let plain = config.resolve_target("sacd").expect("resolve");
    let (explained, _) = config.resolve_target_explained("sacd").expect("resolve explained");

    // The two go through one code path; this is here so that if they are ever split, the
    // split is caught rather than discovered from a GUI showing different values.
    assert_eq!(format!("{plain:?}"), format!("{explained:?}"));
}

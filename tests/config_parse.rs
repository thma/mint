use std::path::Path;

use mint::audio::buffer::OutputSampleFormat;
use mint::config::Config;

/// Temp path unique to the config content, so tests running in parallel never clobber
/// each other's file (keying on length alone races when two configs share a length).
fn config_temp_path(toml: &str) -> std::path::PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    toml.hash(&mut hasher);
    let dir = std::env::temp_dir().join(format!("mint-cfg-{:x}", hasher.finish()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("config.toml")
}

fn config_from_str(toml: &str) -> Config {
    // Round-trip through a temp file so we exercise the same Config::from_path path
    // used in production (parse + validate).
    let path = config_temp_path(toml);
    std::fs::write(&path, toml).unwrap();
    Config::from_path(&path).expect("config should parse")
}

#[test]
fn parses_minimal_target_config() {
    let config = Config::from_path(Path::new("tests/fixtures/minimal_targets.toml"))
        .expect("config should parse");

    assert_eq!(config.targets.len(), 1);
    let archive = config.resolve_target("archive").expect("resolve archive");
    assert_eq!(archive.format, OutputSampleFormat::F32);
    // Built-in fallbacks fill the rest.
    assert_eq!(archive.ceiling_dbtp, -1.0);
    assert!(!archive.overwrite);
}

#[test]
fn preset_resolves_and_explicit_fields_override() {
    let config = config_from_str(
        r#"
        [target.cd]
        preset = "cd"
        lufs = -12.0
        "#,
    );

    let cd = config.resolve_target("cd").expect("resolve cd");
    // From the preset:
    assert_eq!(cd.rate, Some(44_100));
    assert_eq!(cd.format, OutputSampleFormat::S16);
    assert_eq!(cd.ceiling_dbtp, -0.1);
    // Explicit field wins over the preset (which leaves cd loudness unset):
    assert_eq!(cd.lufs, Some(-12.0));
}

#[test]
fn defaults_apply_but_lose_to_preset_and_explicit() {
    let config = config_from_str(
        r#"
        [defaults]
        quality = "hq"
        ceiling_dbtp = -3.0

        [target.s]
        preset = "spotify"      # ceiling -1.0 beats the -3.0 default
        quality = "lq"          # explicit beats the default
        "#,
    );

    let t = config.resolve_target("s").expect("resolve s");
    assert_eq!(t.ceiling_dbtp, -1.0); // preset over default
    // quality has no public accessor; assert indirectly via the dry-run description.
    assert!(t.describe(96_000).iter().any(|s| s.contains("(lq)")));
}

#[test]
fn added_presets_are_available() {
    let config = config_from_str(
        r#"
        [target.a]
        preset = "48k"
        [target.b]
        preset = "hires"
        "#,
    );

    let a = config.resolve_target("a").unwrap();
    assert_eq!(a.rate, Some(48_000));
    assert_eq!(a.format, OutputSampleFormat::S16);
    assert_eq!(a.lufs, Some(-16.0));

    let b = config.resolve_target("b").unwrap();
    assert_eq!(b.rate, Some(96_000));
    assert_eq!(b.format, OutputSampleFormat::S24);
    assert_eq!(b.ceiling_dbtp, -0.1);
}

/// The valid-name list in the error message used to be a third hand-maintained copy of the
/// preset table; this holds it to the arms in `preset()`.
#[test]
fn every_listed_preset_resolves_and_the_error_offers_all_of_them() {
    for name in mint::config::PRESET_NAMES {
        let config = config_from_str(&format!("[target.x]\npreset = \"{name}\"\n"));
        config
            .resolve_target("x")
            .unwrap_or_else(|e| panic!("preset '{name}' is listed but does not resolve: {e:#}"));
    }

    let err = config_err("[target.x]\npreset = \"nope\"\n");
    for name in mint::config::PRESET_NAMES {
        assert!(err.contains(name), "'{name}' missing from: {err}");
    }
}

/// `sacd` is the only preset that is not PCM, and the only one whose value lies in what it
/// does *not* set: a `format` would be a hard error and a `rate` would silently switch off
/// the derivation of the intermediate PCM rate.
#[test]
fn the_sacd_preset_is_a_complete_dsd_target_on_one_line() {
    let config = config_from_str("[target.sacd]\npreset = \"sacd\"\n");
    let t = config.resolve_target("sacd").expect("resolve sacd");

    assert_eq!(t.dsd.map(|d| d.tag()), Some("dsd64"));
    assert_eq!(t.rate, None, "a preset-set rate would suppress the derivation");
    assert_eq!(t.ceiling_dbtp, -1.0);
    assert_eq!(t.lufs, None, "DSD has no loudness standard, like cd");
    assert_eq!(t.format_placeholder(), "1bit");
    assert_eq!(t.pcm_stage_rate(96_000), 88_200);
    assert_eq!(t.delivery_rate(96_000), 2_822_400);

    let steps = t.describe(96_000);
    assert!(
        steps.iter().any(|s| s.contains("88200") && s.contains("derived")),
        "got: {steps:?}"
    );
    assert!(steps.iter().any(|s| s.contains("interpolate x32")), "got: {steps:?}");
    assert!(!steps.iter().any(|s| s.contains("quantize")), "got: {steps:?}");
}

/// Every PCM preset sets `format`, which a DSD target rejects — so the natural "start from
/// a preset and swap the codec" move cannot work, and the error has to say why.
#[test]
fn a_pcm_preset_cannot_be_repointed_at_a_dsd_codec() {
    let err = config_err(
        r#"
        [target.x]
        preset = "hires"
        codec  = "dsf"
        dsd    = "dsd64"
        "#,
    );
    assert!(err.contains("format"), "got: {err}");
    assert!(err.contains("preset"), "the error must point at the preset: {err}");
}

/// `bwf` used to be checked against the merged value, so a broadcast-oriented
/// `[defaults] bwf = true` made every FLAC/MP3/DSD sibling a load-time error. Same
/// own-layer rule as `format` and `rate`: rejected when *this* target asks for it, dropped
/// when it is only inherited.
#[test]
fn an_inherited_bwf_does_not_break_non_wav_siblings() {
    let config = config_from_str(
        r#"
        [defaults]
        bwf = true

        [target.broadcast]
        preset = "broadcast-ebu-r128"

        [target.lossless]
        preset = "hires"
        codec  = "flac"

        [target.sacd]
        preset = "sacd"
        "#,
    );

    assert!(config.resolve_target("broadcast").unwrap().bwf, "wav still gets its bext chunk");
    assert!(!config.resolve_target("lossless").unwrap().bwf);
    assert!(!config.resolve_target("sacd").unwrap().bwf);
}

#[test]
fn unknown_preset_is_rejected() {
    let dir = std::env::temp_dir().join("mint-cfg-badpreset");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[target.x]\npreset = \"nope\"\n").unwrap();

    let err = Config::from_path(&path).expect_err("unknown preset must fail");
    assert!(err.to_string().contains("unknown preset") || format!("{err:#}").contains("unknown preset"));
}

#[test]
fn missing_format_is_rejected() {
    let dir = std::env::temp_dir().join("mint-cfg-noformat");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(&path, "[target.x]\nlufs = -14.0\n").unwrap();

    let err = Config::from_path(&path).expect_err("missing format must fail");
    assert!(format!("{err:#}").contains("format"));
}

#[test]
fn colliding_output_paths_are_rejected() {
    // Two targets, same dir, naming without a distinguishing placeholder -> collision.
    let dir = std::env::temp_dir().join("mint-cfg-collision");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.toml");
    std::fs::write(
        &path,
        r#"
        [defaults]
        naming = "{stem}.wav"

        [target.a]
        format = "s16"
        [target.b]
        format = "s24"
        "#,
    )
    .unwrap();

    let err = Config::from_path(&path).expect_err("colliding outputs must fail");
    assert!(format!("{err:#}").contains("same output path"));
}

#[test]
fn derived_chain_skips_resample_when_rate_matches() {
    let config = config_from_str(
        r#"
        [target.t]
        preset = "hires"   # 96000 Hz
        "#,
    );
    let t = config.resolve_target("t").unwrap();

    // Source already at 96 kHz: no resample step in the plan.
    let same = t.describe(96_000);
    assert!(same.iter().any(|s| s.contains("keep sample rate")));
    assert!(!same.iter().any(|s| s.contains("resample")));

    // Source at 48 kHz: a resample step appears.
    let diff = t.describe(48_000);
    assert!(diff.iter().any(|s| s.contains("resample 48000 -> 96000")));
}

#[test]
fn shaped_dither_parses_and_dry_run_reflects_s16_gating() {
    let config = config_from_str(
        r#"
        [target.cd]
        preset = "cd"            # s16
        dither = "shaped"

        [target.archive]
        preset = "hires"         # s24
        dither = "shaped"
        "#,
    );

    // s16: shaping engages.
    let cd = config.resolve_target("cd").unwrap();
    assert!(
        cd.describe(44_100).iter().any(|s| s.contains("noise-shaping")),
        "s16 dry-run should advertise noise shaping"
    );

    // s24: "shaped" gracefully degrades to flat TPDF, surfaced in the dry-run.
    let archive = config.resolve_target("archive").unwrap();
    assert!(
        archive.describe(96_000).iter().any(|s| s.contains("shaping skipped")),
        "s24 dry-run should note that shaping was skipped"
    );
}

#[test]
fn psychoacoustic_dither_parses_and_dry_run_names_the_curve() {
    let config = config_from_str(
        r#"
        [target.cd]
        preset = "cd"            # s16
        dither = "psychoacoustic"

        [target.archive]
        preset = "hires"         # s24
        dither = "psychoacoustic"
        "#,
    );

    // s16: the psychoacoustic curve engages and the dry-run names it (so it can be
    // told apart from the gentle "shaped" curve).
    let cd = config.resolve_target("cd").unwrap();
    assert!(
        cd.describe(44_100).iter().any(|s| s.contains("noise-shaping (psychoacoustic)")),
        "s16 dry-run should name the psychoacoustic curve"
    );

    // s24: same s16-only gating — degrades to flat TPDF.
    let archive = config.resolve_target("archive").unwrap();
    assert!(
        archive.describe(96_000).iter().any(|s| s.contains("shaping skipped")),
        "s24 dry-run should note that shaping was skipped"
    );
}

#[test]
fn adaptive_parses_and_dry_run_reflects_s16_gating() {
    let config = config_from_str(
        r#"
        [target.cd]
        preset = "cd"            # s16
        dither = "adaptive"
        dither_strength = "high"

        [target.archive]
        preset = "hires"         # s24
        dither = "adaptive"
        dither_strength = "low"
        "#,
    );

    let cd = config.resolve_target("cd").unwrap();
    assert!(
        cd.describe(44_100)
            .iter()
            .any(|s| s.contains("adaptive high dither")),
        "s16 dry-run should advertise the selected adaptive strength"
    );

    let archive = config.resolve_target("archive").unwrap();
    assert!(
        archive
            .describe(96_000)
            .iter()
            .any(|s| s.contains("adaptive") && s.contains("skipped") && s.contains("low")),
        "s24 dry-run should note that adaptive was skipped"
    );
}

/// Round-trip a config that is expected to be *rejected*; returns the error string.
fn config_err(toml: &str) -> String {
    let path = config_temp_path(toml);
    std::fs::write(&path, toml).unwrap();
    format!("{:#}", Config::from_path(&path).expect_err("config should be rejected"))
}

#[test]
fn flac_codec_resolves_and_cext_follows_codec() {
    let config = config_from_str(
        r#"
        [target.master]
        preset = "hires"   # s24, 96k
        codec  = "flac"
        "#,
    );
    let t = config.resolve_target("master").unwrap();
    // The default naming uses {cext}, which tracks the codec.
    let path = t.render_output_path(Path::new("masters/song.wav"), 96_000).unwrap();
    assert_eq!(path, Path::new("./out/master/song.flac"));
    // FLAC keeps the integer bit depth from the preset.
    assert_eq!(t.format, OutputSampleFormat::S24);
}

#[test]
fn mp3_codec_allows_missing_format_and_sets_bitrate() {
    // MP3 is lossy, so `format` is not required.
    let config = config_from_str(
        r#"
        [target.lossy]
        codec = "mp3"
        mp3_bitrate = 256
        "#,
    );
    let t = config.resolve_target("lossy").unwrap();
    assert_eq!(t.mp3_bitrate, 256);
    let path = t.render_output_path(Path::new("a/b.wav"), 44_100).unwrap();
    assert_eq!(path, Path::new("./out/lossy/b.mp3"));
    // The dry-run advertises a lossy MP3 encode, not a quantize step.
    assert!(t.describe(44_100).iter().any(|s| s.contains("MP3 256 kbps")));
}

#[test]
fn flac_with_f32_is_rejected() {
    let err = config_err(
        r#"
        [target.x]
        format = "f32"
        codec  = "flac"
        "#,
    );
    assert!(err.contains("flac") && err.contains("f32"), "got: {err}");
}

#[test]
fn flac_with_f64_is_rejected() {
    let err = config_err(
        r#"
        [target.x]
        format = "f64"
        codec  = "flac"
        "#,
    );
    assert!(err.contains("flac") && err.contains("f64"), "got: {err}");
}

#[test]
fn f64_format_resolves_and_plans_a_passthrough() {
    let config = config_from_str(
        r#"
        [target.archive]
        format = "f64"
        "#,
    );
    let t = config.resolve_target("archive").unwrap();
    assert_eq!(t.format, OutputSampleFormat::F64);
    // No quantize/dither step may be planned; f64 is a pure passthrough.
    let steps = t.describe(48_000);
    assert!(
        steps.iter().any(|s| s.contains("write f64 (no quantization)")),
        "got: {steps:?}"
    );
    assert!(!steps.iter().any(|s| s.contains("quantize")), "got: {steps:?}");
}

#[test]
fn bwf_on_non_wav_is_rejected() {
    let err = config_err(
        r#"
        [target.x]
        preset = "hires"
        codec  = "flac"
        bwf    = true
        "#,
    );
    assert!(err.contains("bwf"), "got: {err}");
}

#[test]
fn invalid_mp3_bitrate_is_rejected() {
    let err = config_err(
        r#"
        [target.x]
        codec = "mp3"
        mp3_bitrate = 333
        "#,
    );
    assert!(err.contains("mp3_bitrate"), "got: {err}");
}

#[test]
fn output_path_uses_placeholders() {
    let config = config_from_str(
        r#"
        [defaults]
        dir = "out"
        naming = "{target}/{stem}_{rate}.{format}.wav"

        [target.cd]
        preset = "cd"
        "#,
    );
    let cd = config.resolve_target("cd").unwrap();
    let path = cd
        .render_output_path(Path::new("masters/song.wav"), 44_100)
        .unwrap();
    assert_eq!(path, Path::new("out/cd/song_44100.s16.wav"));
}

#[test]
fn limiter_options_parse_and_appear_in_dry_run() {
    let config = config_from_str(
        r#"
        [target.t]
        format = "s16"
        on_clip = "limit"
        limiter_character = "transient"
        limiter_soft_clip = true
        limiter_link_channels = false
        "#,
    );

    let t = config.resolve_target("t").unwrap();
    assert!(t.limiter_soft_clip);
    assert!(!t.limiter_link_channels);
    let plan = t.describe(48_000).join(" | ");
    assert!(plan.contains("transient"), "plan: {plan}");
    assert!(plan.contains("unlinked"), "plan: {plan}");
    assert!(plan.contains("soft_clip=true"), "plan: {plan}");
}

#[test]
fn maximize_policy_parses_with_cap_and_appears_in_dry_run() {
    let config = config_from_str(
        r#"
        [target.loud]
        format = "s16"
        lufs = -9.0
        on_clip = "maximize"
        limiter_character = "dense"
        max_limiting_db = 4.5
        "#,
    );

    let t = config.resolve_target("loud").unwrap();
    assert_eq!(t.max_limiting_db, 4.5);
    let plan = t.describe(48_000).join(" | ");
    assert!(plan.contains("maximize to target (cap 4.5 dB)"), "plan: {plan}");
    assert!(plan.contains("dense"), "plan: {plan}");
}

#[test]
fn max_limiting_db_defaults_to_8() {
    let config = config_from_str(
        r#"
        [target.t]
        format = "s16"
        "#,
    );
    assert_eq!(config.resolve_target("t").unwrap().max_limiting_db, 8.0);
}

#[test]
fn non_positive_max_limiting_db_is_rejected() {
    let err = config_err(
        r#"
        [target.t]
        format = "s16"
        on_clip = "maximize"
        max_limiting_db = 0.0
        "#,
    );
    assert!(err.contains("max_limiting_db"), "err: {err}");
}

#[test]
fn maximize_without_lufs_is_a_valid_config() {
    // on_clip may arrive via [defaults], so a no-lufs target must not fail resolution;
    // the pipeline degrades it to plain limiting with a warning at processing time.
    let config = config_from_str(
        r#"
        [defaults]
        on_clip = "maximize"

        [target.transcode]
        format = "f32"
        "#,
    );
    let t = config.resolve_target("transcode").unwrap();
    assert!(t.lufs.is_none());
}

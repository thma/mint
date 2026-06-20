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

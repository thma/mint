//! Spike gate S6: writing a field back must change that field and *nothing else*.
//!
//! Run against the real `examples/mint.toml` rather than a fixture, deliberately. That file
//! is the bootstrap template — `include_str!`-embedded into `cli.rs` and written out as
//! `./mint.toml` on a fresh install — and it is 92 lines of which most are teaching
//! material: aligned `=` columns, trailing comments, comment blocks between tables, and a
//! target order that is not alphabetical. Every one of those is something a careless writer
//! destroys, and a fixture built for the test would quietly avoid all of them.
//!
//! The assertions are byte comparisons per line, not "it still parses". A document that
//! still parses but has lost its comments has still lost the onboarding documentation.

use mint_gui::write_back::ConfigDocument;
use toml_edit::Value;

const TEMPLATE: &str = include_str!("../../examples/mint.toml");

/// Line numbers (0-based) whose bytes differ, plus a readable diff for the failure message.
fn changed_lines(before: &str, after: &str) -> (Vec<usize>, String) {
    let a: Vec<&str> = before.lines().collect();
    let b: Vec<&str> = after.lines().collect();
    let mut indices = Vec::new();
    let mut report = String::new();
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied(), b.get(i).copied());
        if x != y {
            indices.push(i);
            report.push_str(&format!("\n  line {i}:\n    - {x:?}\n    + {y:?}"));
        }
    }
    (indices, report)
}

fn target_order(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("[target.")?.strip_suffix(']'))
        .collect()
}

#[test]
fn setting_a_float_touches_exactly_one_line() {
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("template parses");
    doc.set("cd", "lufs", Value::from(-14.0)).expect("set lufs");
    let after = doc.text();

    let (changed, report) = changed_lines(TEMPLATE, &after);
    assert_eq!(changed.len(), 1, "exactly one line may change:{report}");
    let line = after.lines().nth(changed[0]).unwrap();
    assert!(line.contains("-14"), "the new value must be on it: {line:?}");
    // The template aligns the `=` in this table (`lufs   = -12.0`). That alignment is the
    // user's, and a writer that reflows it is editing something it was not asked to edit.
    assert!(line.starts_with("lufs   ="), "alignment must survive: {line:?}");
}

#[test]
fn setting_a_value_keeps_the_comment_that_follows_it() {
    // `dither = "shaped"          # TPDF + noise shaping; engages only for s16 targets`
    // The comment is decor on the *value*, so it is exactly what a naive assignment drops.
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    doc.set("cd", "dither", Value::from("psychoacoustic")).expect("set dither");
    let after = doc.text();

    let (changed, report) = changed_lines(TEMPLATE, &after);
    assert_eq!(changed.len(), 1, "exactly one line:{report}");
    let line = after.lines().nth(changed[0]).unwrap();
    assert!(line.contains("psychoacoustic"), "{line:?}");
    assert!(
        line.contains("# TPDF + noise shaping"),
        "the trailing comment must survive the write: {line:?}"
    );
}

#[test]
fn adding_a_field_that_was_not_there_disturbs_nothing_else() {
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    doc.set("cd", "rate", Value::from(44_100_i64)).expect("set rate");
    let after = doc.text();

    // One line appears; every pre-existing line survives somewhere, in order.
    assert_eq!(after.lines().count(), TEMPLATE.lines().count() + 1);
    let mut original = TEMPLATE.lines();
    let mut inserted = 0;
    for line in after.lines() {
        match original.clone().next() {
            Some(next) if next == line => {
                original.next();
            }
            _ => {
                inserted += 1;
                assert!(line.contains("rate"), "unexpected new line: {line:?}");
            }
        }
    }
    assert_eq!(inserted, 1, "exactly one line was inserted");
    assert_eq!(original.count(), 0, "every original line survived, in order");
}

#[test]
fn clearing_a_field_removes_its_line_and_only_its_line() {
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    assert!(doc.clear("cd", "lufs").expect("clear"), "the key was there to remove");
    let after = doc.text();

    assert_eq!(after.lines().count(), TEMPLATE.lines().count() - 1);
    // And it removes the key rather than writing the default in its place — those are
    // different documents, and only one of them keeps following `[defaults]`.
    let cd_table: Vec<&str> = after
        .lines()
        .skip_while(|l| !l.starts_with("[target.cd]"))
        .take_while(|l| !l.starts_with("[target.cd-adaptive]"))
        .collect();
    assert!(
        !cd_table.iter().any(|l| l.trim_start().starts_with("lufs")),
        "lufs must be gone, not defaulted: {cd_table:?}"
    );
}

#[test]
fn a_write_never_reorders_the_targets() {
    // `Config.targets` is a `BTreeMap`, so any path that goes through serde comes back
    // alphabetised. The template's order is deliberately not alphabetical, which is what
    // makes this detectable at all.
    let before = target_order(TEMPLATE);
    assert_ne!(
        before,
        {
            let mut sorted = before.clone();
            sorted.sort_unstable();
            sorted
        },
        "premise: the template's target order is not alphabetical"
    );

    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    doc.set("cd", "lufs", Value::from(-9.5)).expect("set");
    doc.set("streaming", "overwrite", Value::from(true)).expect("set");
    doc.clear("cd-adaptive", "dither").expect("clear");
    let after = doc.text();

    assert_eq!(target_order(&after), before, "the user's target order changed");
}

#[test]
fn a_preset_is_never_expanded_into_the_fields_it_implies() {
    // `preset = "cd"` resolves to a rate, a format and a ceiling. Writing those out would
    // be "helpful" and would also freeze today's preset into the user's file forever.
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    doc.set("cd", "lufs", Value::from(-11.0)).expect("set");
    let after = doc.text();

    let cd_table: Vec<&str> = after
        .lines()
        .skip_while(|l| !l.starts_with("[target.cd]"))
        .take_while(|l| !l.starts_with("[target.cd-adaptive]"))
        .collect();
    assert!(cd_table.iter().any(|l| l.contains("preset = \"cd\"")), "{cd_table:?}");
    for implied in ["rate", "format", "ceiling_dbtp"] {
        assert!(
            !cd_table.iter().any(|l| l.trim_start().starts_with(implied)),
            "the preset was expanded: {implied} appeared in {cd_table:?}"
        );
    }
}

#[test]
fn every_write_leaves_a_document_mint_still_accepts() {
    // The writer's output has to survive the real loader, not just the TOML parser: it is
    // `Config::from_toml_str` that enforces the four-layer merge and its exceptions.
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    doc.validate().expect("the template itself must be valid");

    doc.set("cd", "lufs", Value::from(-13.5)).expect("set");
    doc.set("cd", "dither_strength", Value::from("high")).expect("set");
    doc.set("archive-s24-none", "overwrite", Value::from(true)).expect("set");
    let config = doc.validate().expect("still valid after three writes");

    let target = config.resolve_target("cd").expect("cd resolves");
    assert_eq!(target.lufs, Some(-13.5));
    assert_eq!(target.dither_strength.to_string(), "high");
    assert!(config.resolve_target("archive-s24-none").unwrap().overwrite);
}

#[test]
fn a_write_that_makes_the_config_invalid_is_reported_not_swallowed() {
    // The GUI is allowed to hold an invalid document — a half-typed field is invalid for a
    // keystroke or two. What it must not do is decide for itself whether that is fine.
    let mut doc = ConfigDocument::parse(TEMPLATE).expect("parse");
    // `streaming` is `preset = "apple-music"`, and every PCM preset sets `format` — in the
    // target's *own* layer. So turning it into a DSD target is the exact collision the
    // own-layer rule produces, and the `dsd` rate is set too or mint stops one check
    // earlier to complain about that instead.
    doc.set("streaming", "codec", Value::from("dsf")).expect("write itself succeeds");
    doc.set("streaming", "dsd", Value::from("dsd64")).expect("write itself succeeds");

    let err = doc.validate().expect_err("a preset's format on a DSD target is a load-time error");
    assert!(
        format!("{err:#}").contains("no meaning for a DSD target"),
        "mint's own message must come through verbatim: {err:#}"
    );
}

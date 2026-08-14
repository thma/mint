//! The `ALL` arrays are what a front end enumerates to build its dropdowns, and a
//! variant missing from one is invisible: no compile error, no panic, just an option the
//! user cannot pick. This file is the tripwire.
//!
//! The trick is the `exhaustive` match inside the macro. It does nothing at runtime, but
//! adding a variant to any of these enums makes it fail to *compile* — which drags the
//! author into this file, where the membership assertion below then fails unless `ALL`
//! was updated too. Same spirit as the three exhaustive `match target.codec` arms: the
//! checklist is enforced by the compiler rather than by remembering it.

use mint::config::{
    BitDepthFormat, DitherCorrelation, DitherMode, DitherStrength, LimiterCharacter, OnClipPolicy,
    OutputCodec, ResampleQuality,
};

macro_rules! assert_all_covers {
    ($ty:ty, $($variant:path),+ $(,)?) => {{
        // Compile-time half: exhaustive over the enum, so a new variant breaks the build.
        fn exhaustive(v: $ty) -> $ty {
            match v { $($variant => $variant),+ }
        }
        // Run-time half: every variant listed above is actually reachable from ALL, and
        // ALL holds nothing extra (a duplicate would show up as a length mismatch).
        let listed = [$($variant),+];
        for v in listed {
            assert_eq!(exhaustive(v), v);
            assert!(
                <$ty>::ALL.contains(&v),
                "{v} is missing from {}::ALL — every front end's dropdown just lost it",
                stringify!($ty)
            );
        }
        assert_eq!(
            <$ty>::ALL.len(),
            listed.len(),
            "{}::ALL has a duplicate or a stale entry",
            stringify!($ty)
        );
    }};
}

#[test]
fn all_arrays_list_every_variant() {
    assert_all_covers!(
        OutputCodec,
        OutputCodec::Wav,
        OutputCodec::Flac,
        OutputCodec::Mp3,
        OutputCodec::Dsf,
    );
    assert_all_covers!(
        BitDepthFormat,
        BitDepthFormat::S16,
        BitDepthFormat::S24,
        BitDepthFormat::F32,
        BitDepthFormat::F64,
    );
    assert_all_covers!(
        ResampleQuality,
        ResampleQuality::Lq,
        ResampleQuality::Mq,
        ResampleQuality::Hq,
        ResampleQuality::Vhq,
    );
    assert_all_covers!(
        OnClipPolicy,
        OnClipPolicy::Limit,
        OnClipPolicy::ReduceGain,
        OnClipPolicy::Warn,
        OnClipPolicy::Maximize,
    );
    assert_all_covers!(
        LimiterCharacter,
        LimiterCharacter::Balanced,
        LimiterCharacter::Transient,
        LimiterCharacter::Dense,
    );
    assert_all_covers!(
        DitherMode,
        DitherMode::Tpdf,
        DitherMode::None,
        DitherMode::Shaped,
        DitherMode::Psychoacoustic,
        DitherMode::Adaptive,
    );
    assert_all_covers!(
        DitherStrength,
        DitherStrength::Low,
        DitherStrength::Normal,
        DitherStrength::High,
    );
    assert_all_covers!(
        DitherCorrelation,
        DitherCorrelation::Decorrelated,
        DitherCorrelation::Correlated,
        DitherCorrelation::MidSide,
    );
}

/// `Display` must spell a variant exactly the way the TOML parser accepts it, or a GUI
/// that writes back what it shows produces a config mint refuses to read. Round-tripped
/// through a real parse rather than compared against a second table.
#[test]
fn display_spellings_are_the_ones_toml_accepts() {
    for codec in OutputCodec::ALL {
        for format in BitDepthFormat::ALL {
            // DSD rejects `format` outright, and FLAC rejects the float depths — those
            // pairings are covered by the config tests. Here only the spelling matters.
            if codec.is_dsd() || codec == OutputCodec::Flac {
                continue;
            }
            let toml = format!(
                "[target.t]\ncodec = \"{codec}\"\nformat = \"{format}\"\nrate = 44100\n"
            );
            let config = mint::config::Config::from_toml_str(&toml)
                .unwrap_or_else(|e| panic!("codec={codec} format={format} did not parse: {e:#}"));
            let resolved = config.resolve_target("t").unwrap();
            assert_eq!(resolved.codec, codec);
        }
    }

    for dither in DitherMode::ALL {
        for strength in DitherStrength::ALL {
            for correlation in DitherCorrelation::ALL {
                let toml = format!(
                    "[target.t]\nformat = \"s16\"\nrate = 44100\ndither = \"{dither}\"\n\
                     dither_strength = \"{strength}\"\ndither_correlation = \"{correlation}\"\n"
                );
                let config = mint::config::Config::from_toml_str(&toml).unwrap_or_else(|e| {
                    panic!("dither={dither}/{strength}/{correlation} did not parse: {e:#}")
                });
                let resolved = config.resolve_target("t").unwrap();
                assert_eq!(resolved.dither, dither);
                assert_eq!(resolved.dither_strength, strength);
                assert_eq!(resolved.dither_correlation, correlation);
            }
        }
    }

    for quality in ResampleQuality::ALL {
        for policy in OnClipPolicy::ALL {
            for character in LimiterCharacter::ALL {
                let toml = format!(
                    "[target.t]\nformat = \"s24\"\nrate = 44100\nquality = \"{quality}\"\n\
                     on_clip = \"{policy}\"\nlimiter_character = \"{character}\"\n"
                );
                let config = mint::config::Config::from_toml_str(&toml).unwrap_or_else(|e| {
                    panic!("{quality}/{policy}/{character} did not parse: {e:#}")
                });
                let resolved = config.resolve_target("t").unwrap();
                assert_eq!(resolved.quality, quality);
                assert_eq!(resolved.on_clip, policy);
                assert_eq!(resolved.limiter_character, character);
            }
        }
    }
}

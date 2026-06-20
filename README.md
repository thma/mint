# mit

Cross-platform audio CLI in Rust. Render high-resolution masters into one or more
distribution targets defined declaratively in TOML — you describe the *deliverable*
(rate, bit depth, loudness, ceiling) and `mint` derives the required DSP steps.

What it does:
- Decode input audio (WAV and AIFF; 16/24/32-bit int and 32/64-bit float) into an
  internal f64 buffer with no precision loss at the input boundary.
- **Declarative targets**: each `[target.NAME]` is an output spec; the tool figures
  out which steps are needed (resample only if the rate differs, dither/quantize only
  for integer output, etc.) and runs them in the one correct order.
- **Built-in presets** (`spotify`, `apple-music`, `youtube`, `tidal`, `amazon-music`,
  `soundcloud`, `cd`, `broadcast-ebu-r128`, `48k`, `hires`) you can override per field.
- **Multiple targets per config** — render every distribution format from one file in
  a single pass.
- Resample via **libsoxr** by default for reference-grade SRC (multi-stage, ~150 dB
  stopband, on par with SoX/Weiss), with a pure-Rust `rubato` fallback
  (`--no-default-features`) for builds with no system dependencies. See **Build**.
- Loudness normalization via `ebur128` (integrated LUFS to target ±0.1 LU).
- Oversampled true-peak limiter with a band-limited (polyphase windowed-sinc) inter-sample
  peak detector; the configured ceiling is always enforced (even for a pure transcode with
  no loudness step), re-checked after every resample, and **verified against the BS.1770
  meter** so the delivered file genuinely respects the ceiling.
- Per-file metering report: integrated **LUFS**, **LRA** (loudness range), max
  **momentary**/**short-term**, **true peak**, and crest figures **PLR**/**PSR** — shown
  for both the source and the delivered signal so you can see what processing changed.
- Bit-depth conversion with seeded TPDF dither, quantizing the f64 buffer exactly once;
  optional error-feedback **noise shaping** for s16 deliverables — gentle
  (`dither = "shaped"`) or the published psychoacoustic curve (`dither = "psychoacoustic"`).
- Output formats: **WAV** (f32/s16/s24) — optionally **Broadcast WAV** (`bwf = true`,
  a `bext` chunk carrying real EBU R128 loudness metadata measured from the delivery) —
  **FLAC** (lossless, pure-Rust), and **MP3** (`--features mp3`, libmp3lame built from
  vendored source). Codec is a separate axis from bit depth: `codec = "wav" | "flac" | "mp3"`.
- Dry-run that prints the derived chain and planned output path per input × target.

## Config

```toml
[meta]
name = "album-masters"

# Inherited by every target; overridable per-target.
# Precedence: built-in defaults < [defaults] < preset < explicit field.
[defaults]
dir     = "./out"
quality = "vhq"            # lq | mq | hq | vhq
# naming defaults to "{target}/{stem}.wav"
# placeholders: {stem} {ext} {target} {rate} {format}

[target.streaming]
preset = "apple-music"     # -> 44100 / s16 / -16 LUFS / -1 dBTP

[target.cd]
preset = "cd"              # -> 44100 / s16 / -0.1 dBTP
lufs   = -12.0             # CD has no loudness standard; you choose
dither = "shaped"          # optional: TPDF + noise shaping (s16 only)

[target.archive]
format = "f32"             # no preset: keep rate, no quantization, just normalize
lufs   = -14.0
```

A target needs at least a `format` (directly or via a preset). Omitting `rate` keeps the
source rate (no resample); omitting `lufs` skips loudness normalization (pure transcode).
See `examples/distribution.toml`.

## Presets

A preset is a named bundle of the four *deliverable* fields — `rate`, `format`, `lufs`,
and `ceiling_dbtp` — reflecting a platform's published delivery/normalization spec. Apply
one with `preset = "NAME"`, then override any field on top of it.

| Preset | `rate` (Hz) | `format` | `lufs` | `ceiling_dbtp` | Notes |
|---|---|---|---|---|---|
| `spotify` | 44100 | s16 | -14 | -1.0 | |
| `apple-music` | 44100 | s16 | -16 | -1.0 | |
| `youtube` | 48000 | s16 | -14 | -1.0 | |
| `tidal` | 44100 | s16 | -14 | -1.0 | |
| `amazon-music` | 44100 | s16 | -14 | -2.0 | |
| `soundcloud` | 44100 | s16 | -14 | -1.0 | |
| `cd` | 44100 | s16 | *(unset)* | -0.1 | Red Book CD; no loudness standard — set `lufs` yourself to normalize. |
| `broadcast-ebu-r128` | 48000 | s24 | -23 | -1.0 | EBU R128 broadcast delivery. |
| `48k` | 48000 | s16 | -16 | -1.0 | Generic 48 kHz delivery. |
| `hires` | 96000 | s24 | -16 | -0.1 | High-resolution master. |

A preset sets **only those four fields.** Everything else (`on_clip`, `warn_limiting_db`,
`quality`, `dither`, `dir`, `naming`, `overwrite`) comes from the built-in defaults or your
`[defaults]` — so, e.g., dither stays `tpdf` and quality stays `vhq` unless you say
otherwise.

> Streaming targets drift over time; treat these as sensible starting points, not gospel.
> The authoritative table is `preset()` in `src/config.rs`.

### Built-in defaults

When a field is set by neither a preset nor an explicit value, these apply:

| Field | Default |
|---|---|
| `rate` | *keep source rate (no resample)* |
| `lufs` | *unset (skip loudness normalization)* |
| `ceiling_dbtp` | -1.0 |
| `on_clip` | `limit` |
| `warn_limiting_db` | 1.0 |
| `quality` | `vhq` |
| `dither` | `tpdf` |
| `dir` | `./out` |
| `naming` | `{target}/{stem}.wav` |
| `overwrite` | `false` |

`format` has no default — every target must get one from a preset or an explicit `format`.

### Overriding presets

Fields resolve lowest-to-highest precedence:

    built-in defaults  <  [defaults]  <  preset  <  explicit field

So an explicit field on a target always wins over the preset it sits on, while
`[defaults]` fills in anything a preset doesn't set (note: a preset's four fields beat
`[defaults]`). Examples:

```toml
# Applied to every target unless overridden.
[defaults]
dir     = "./distros"
quality = "vhq"
dither  = "psychoacoustic"   # s16 targets get the strong shaper; s24/f32 ignore it

# 1. Use a preset verbatim.
[target.streaming]
preset = "spotify"           # -> 44100 / s16 / -14 LUFS / -1.0 dBTP

# 2. Preset + one tweak: the Spotify spec, but a louder normalization target.
[target.loud]
preset = "spotify"
lufs   = -9.0                # overrides the preset's -14

# 3. Preset + several overrides: a 24-bit Apple Music variant with a custom ceiling.
[target.apple-hd]
preset       = "apple-music"
format       = "s24"         # s16 -> s24 (shaping no longer applies at s24)
ceiling_dbtp = -1.5

# 4. The cd preset leaves loudness unset — add it, and pick a shaper for this target.
[target.cd]
preset = "cd"                # -> 44100 / s16 / -0.1 dBTP
lufs   = -12.0
dither = "shaped"            # explicit -> overrides the [defaults] psychoacoustic

# 5. No preset at all — set the fields directly.
[target.archive]
format = "f32"               # keep source rate, no quantization, just normalize
lufs   = -14.0

# 6. Broadcast WAV: a bext chunk with EBU R128 loudness metadata is written from the
#    measured delivery (codec stays wav).
[target.broadcast]
preset = "broadcast-ebu-r128"   # 48k / s24 / -23 LUFS
bwf    = true

# 7. FLAC (lossless) and MP3 (lossy) — codec is independent of bit depth.
[target.flac]
preset = "hires"             # s24 / 96k
codec  = "flac"              # -> song.flac (FLAC keeps the s24 depth)

[target.mp3]
codec       = "mp3"          # lossy: bit depth/dither not applicable
mp3_bitrate = 320            # default 320; needs `--features mp3`
lufs        = -14.0
```

`codec` is `wav` (default), `flac`, or `mp3`; the default `naming` ends in `{cext}`, which
expands to the codec's extension. `bwf` is WAV-only; FLAC requires an integer format
(s16/s24). Run `--dry-run` to see the fully-resolved chain and output path for every
target before processing anything.

## Build

Prerequisites: a stable Rust toolchain (edition 2024) **and** the system **libsoxr** C
library + `pkg-config` — the default build links libsoxr for reference-grade
sample-rate conversion (resampling only; dither, quantization, and noise shaping stay in
this tool).

    # install the C library + pkg-config first (pkg-config is how the build finds libsoxr)
    brew install libsoxr pkg-config                 # macOS
    sudo apt-get install libsoxr-dev pkg-config     # Debian/Ubuntu
    sudo dnf install soxr-devel pkgconf-pkg-config  # Fedora/RHEL
    sudo pacman -S soxr pkgconf                      # Arch
    # Windows: vcpkg install soxr, then point pkg-config at it (set PKG_CONFIG_PATH to
    # <vcpkg>\installed\x64-windows\lib\pkgconfig) and add the bin dir to PATH

    cargo build

The `quality = lq|mq|hq|vhq` setting maps onto libsoxr's recipes. See
[Audio quality](#audio-quality) for what libsoxr buys you.

### No-system-deps fallback (rubato)

If you can't (or don't want to) install libsoxr, build with the pure-Rust `rubato`
backend instead — no system dependencies at all:

    cargo build --no-default-features

The SRC backend is selected at **compile time**; everything else (loudness, limiting,
dither, output formats) is identical between the two builds.

### MP3 output (opt-in)

MP3 is behind the `mp3` feature because `mp3lame-sys` compiles libmp3lame from vendored C
source — no system library is needed at runtime, but the build wants a C toolchain
(`cc`, plus `make` on Unix):

    cargo build --features mp3

WAV (incl. Broadcast WAV) and FLAC need no extra feature. AAC is intentionally not
supported (libfdk-aac licensing).

## Run

Dry run (show derived chain + planned paths, process nothing):

    cargo run -- --config examples/distribution.toml --dry-run "masters/*.wav"

Process all targets for all inputs:

    cargo run -- --config examples/distribution.toml "masters/*.wav" track.aif

Render only specific targets (repeat `--target`):

    cargo run -- --config examples/distribution.toml --target cd --target streaming "masters/*.wav"

### Machine-readable report (`--json`)

`--json` emits the full per-task metering (source + delivered loudness/dynamics, true
peak, PLR/PSR, applied limiting, warnings) plus any failures as JSON to stdout — the
human-readable report and banners are suppressed so stdout is pure JSON for QC pipelines:

    mint --config render.toml --json "masters/*.wav" | jq '.tasks[] | {target, lufs: .delivered.integrated_lufs}'

Non-finite meter values (silence, or clips shorter than a meter's window) serialize as
`null`. A non-zero exit still signals failures, which also appear in the `failures` array.

## Audio quality

Two choices affect output fidelity: the **resampler** (only matters when a target's rate
differs from the source) and the **dither / noise-shaping** mode (only matters when
quantizing to an integer format, i.e. s16/s24). Both are per-target config; `--dry-run`
and the per-file report show exactly what each target will do.

### Resampling

`mint` has two sample-rate-conversion backends behind the same `quality` knob:

| Backend | How to get it | Character |
|---|---|---|
| `libsoxr` *(default)* | built in (needs system libsoxr; see [Build](#build)) | Reference-grade multi-stage SRC (~150 dB stopband), same engine class as SoX/Weiss. |
| `rubato` | `--no-default-features` (pure Rust, no system deps) | High-quality windowed-sinc SRC; transparent for virtually all material. |

`quality = lq | mq | hq | vhq` (default `vhq`) sets the filter length/steepness and maps
onto each backend's internal recipes. `vhq` is the right choice for masters; the lower
settings trade stopband and transition-band performance for speed.

The resampler runs **only when a target's rate differs from the source** — same-rate
targets skip it entirely. Whenever it does run, the true-peak ceiling is re-checked and
re-limited afterwards, since sample-rate conversion can create new inter-sample peaks.

**Which should I use?** The default `libsoxr` is the right call for serious work — final
masters, large or awkward rate ratios (e.g. 96k → 44.1k), archival deliverables, or
matching a SoX-based reference. The pure-Rust `rubato` fallback (`--no-default-features`)
is excellent in its own right and needs nothing installed; reach for it when you can't
add the system dependency. The backend is chosen at **compile time** — the default binary
always resamples through libsoxr, a `--no-default-features` binary always uses rubato.
libsoxr does resampling only; dither, quantization, and noise shaping always stay in this
tool.

### Dither & noise shaping

Reducing bit depth (say, a 24-bit master down to a 16-bit CD or streaming file) discards
resolution. Done naïvely (truncation), that leaves correlated quantization distortion that
is especially ugly on quiet fades and reverb tails. **Dither** fixes it by adding a tiny,
statistically-correct noise that decorrelates the error — converting distortion into a
benign, steady noise floor. **Noise shaping** goes a step further: it moves that noise
floor out of the band where the ear is most sensitive (~2–5 kHz) and up toward Nyquist,
lowering the *perceived* noise (effective dynamic range can improve by ~10–15 dB) without
changing what is audible as program material.

Set the mode per target with `dither`:

| `dither` | What it does | Use it for |
|---|---|---|
| `tpdf` *(default)* | Flat TPDF dither — the textbook-correct, spectrally neutral choice. | Any integer output; the safe default and the only honest option at s24. |
| `shaped` | TPDF + a gentle `(1−z⁻¹)²` high-pass shaping curve. | s16 deliverables, conservatively — a modest perceived-noise win that stays safe if the file gets processed again. |
| `psychoacoustic` | TPDF + the published Lipshitz *minimally-audible* curve (deep notch at ~4 kHz). | Final s16 masters (CD, 16-bit streaming) — maximum perceived dynamic range; the POW-r / UV22-class option. |
| `none` | Plain truncation, no dither. | Rare; **rejected** when it would reduce bit depth, because it degrades quality. |

Things worth knowing:

- **Shaping only engages at s16.** At s24 the noise floor is already inaudible, so both
  `shaped` and `psychoacoustic` fall back to flat TPDF; at f32 there is no quantization to
  dither at all. The dry-run and report spell out what ran and name the curve — e.g.
  `quantize -> s16 + tpdf + noise-shaping (psychoacoustic)`.
- **`psychoacoustic` is tuned for 44.1 kHz** (its notch sits at ~4 kHz) and deliberately
  concentrates noise near Nyquist. Apply it as the **final** step before delivery — not if
  the output will be resampled or lossy-encoded afterwards; prefer `shaped` or `tpdf`
  there. It still helps at other s16 rates, just not optimally.
- Both shaped modes run **one independent shaper per channel**, so stereo imaging is
  preserved.
- `--seed <N>` makes the otherwise-random dither fully deterministic, so re-runs are
  bit-for-bit identical — useful for reproducible builds and A/B testing.

> The `psychoacoustic` curve uses coefficients from Lipshitz, Vanderkooy & Wannamaker,
> "Minimally Audible Noise Shaping," *JAES* 39(11), 1991 — the same family of curves behind
> commercial tools such as POW-r and Sony Super Bit Mapping.

## Test

    cargo test

## Notes

- Input files are never overwritten by default (use `--force` or `overwrite = true`).
- Output naming requires `{stem}`; targets that would collide on disk are rejected at load.
- Globs are expanded in the tool, so behavior is consistent across shells/OSes.
- The `quality` (resampler) and `dither` (quantizer noise) settings are explained in
  [Audio quality](#audio-quality) above.


## Room for improvement (ranked by impact)

[X] A. The true-peak limiter's ISP detection is the one real DSP weakness. limiter.rs:94 uses 4× linear-interpolation upsampling for peak detection. Linear interpolation is a poor reconstruction filter — it systematically under-estimates inter-sample peaks, worst exactly where ISPs are worst (bright, high-frequency, near-Nyquist content). The code comment concedes "~0.1 dBTP for signals well below Nyquist," but on hot/dense masters the under-read can be several tenths of a dB or more.

The consequence is subtle but important: your measurement (ebur128 true-peak) is proper BS.1770, but the limiter aims at the cruder linear-interp peak, and the pipeline never loops "measure → re-limit until ebur128 says we're under." So ceiling is always enforced is true against the limiter's own estimate, not against the true peak that Spotify/Apple/loudness-checkers will measure. A platform re-checking your file could find it a few tenths over your stated ceiling. Pro limiters (Ozone Maximizer, etc.) oversample detection 4–8× with a proper polyphase FIR.

▎ Fix options, cheapest first: (1) iterate against ebur128's true-peak after limiting and apply a residual trim until under ceiling; (2) replace linear interp with a real oversampling FIR (or reuse libsoxr to 4× upsample for detection); (3) add a small safety offset. I'd do (1)+(2).

[X] B. Loudness/metering breadth. You measure integrated LUFS and TP only. Pro tools also report LRA (loudness range), momentary/short-term max, PLR/PSR, and true-peak max — the numbers an engineer uses for QC. Two related gaps: no LRA-aware / dynamic processing (you hit the target with static gain + limiter only — fine, but tools like RX Loudness Control can compress to meet LUFS+TP simultaneously), and for >2 channels ebur128 isn't told channel roles, so BS.1770 surround channel weighting (+1.5 dB rear, LFE exclusion) isn't applied. Stereo is correct.

[X] C. Output/format breadth. WAV/AIFF in, WAV out only. Missing: lossy encode (MP3/AAC) + a codec-preview path (Ozone's signature QC feature — hear/measure what lossy does to your true peaks), FLAC, and — notably for your broadcast-ebu-r128 preset — broadcast WAV bext chunk + loudness metadata. Right now that preset normalizes correctly but writes a plain WAV with no R128 metadata, which a broadcast deliverable usually requires. Also no metadata/marker passthrough.

[ ] D. Limiter sophistication. Beyond the ISP issue: release is a single fixed exponential (30 dB/s, limiter.rs:10). Pros use program-dependent/adaptive release and offer multiple character modes (transient vs. balanced), optional soft-clip, and link/unlink. Your linked-only envelope is a good default for imaging, but it's the only option.

[ ] E. Redundant quantization (code-quality, low risk). bitdepth.rs quantizes to the grid, then io_write.rs:29-40 quantizes again on write. It's idempotent today (so no audible double-dither), but it's two sources of truth for the grid — and they disagree on convention: bitdepth uses a symmetric grid (±32767) while io_write uses an asymmetric clamp (−32768..32767). Consolidate to one quantization point so a future change can't desync them.

[X] F. No QC/verification output. No analysis dump (JSON of in/out LUFS, LRA, TP, GR per target), no before/after, no spectra. For an automation tool this would be a high-value, low-effort addition.

![mint](mint-logo.png)

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
  `soundcloud`, `cd`, `broadcast-ebu-r128`, `48k`, `hires`, `sacd`) you can override per
  field.

- **Multiple targets per config** — render every distribution format from one file in
  a single pass.

- Resample via **libsoxr** for reference-grade SRC (multi-stage, ~150 dB stopband, on par
  with SoX/Weiss). See **Build** for the system dependency.

- Loudness normalization via `ebur128` (integrated LUFS to target ±0.1 LU), plus opt-in
  **loudness maximization** (`on_clip = "maximize"`): makeup-gain convergence that actually
  hits hot targets where limiting eats loudness, bounded by a `max_limiting_db` peak
  gain-reduction cap.

- Oversampled true-peak limiter with a band-limited (polyphase windowed-sinc) inter-sample
  peak detector and a raised-cosine attack ramp over the look-ahead window (no gain-step
  modulation distortion); the configured ceiling is always enforced (even for a pure
  transcode with no loudness step), re-checked after every resample, and **verified against
  the BS.1770 meter** so the delivered file genuinely respects the ceiling. Limiter behavior
  is configurable per target: adaptive `balanced`/`transient`/`dense` release character,
  optional soft clipping, and linked/unlinked channel envelopes.

- Per-file metering report: integrated **LUFS**, **LRA** (loudness range), max
  **momentary**/**short-term**, **true peak**, and crest figures **PLR**/**PSR** — shown
  for both the source and the delivered signal so you can see what processing changed.

- Bit-depth conversion with seeded TPDF dither, quantizing the f64 buffer exactly once;
  optional s16-focused enhancement modes: gentle error-feedback shaping
  (`dither = "shaped"`), published psychoacoustic shaping (`dither = "psychoacoustic"`),
  or adaptive dithering (`dither = "adaptive"`) driven by a per-frame psychoacoustic
  masking analysis, with configurable stereo/multichannel dither correlation — an
  independent implementation comparable in approach to commercial adaptive dithers.

- Output formats: **WAV** (f32/f64/s16/s24; f64 output from an f64 source is a bit-exact
  passthrough) — optionally **Broadcast WAV** (`bwf = true`,
  a `bext` chunk carrying real EBU R128 loudness metadata measured from the delivery) —
  **FLAC** (lossless, pure-Rust), and **MP3** (`--features mp3`, libmp3lame built from
  vendored source). Codec is a separate axis from bit depth:
  `codec = "wav" | "flac" | "mp3" | "dsf"`.

- **DSD / SACD output** (`codec = "dsf"`, `dsd = "dsd64|dsd128|dsd256|dsd512"`): a 1-bit
  sigma-delta encoder written for this tool — order-7 CIFB loop with a noise transfer
  function designed per rate, 121 dB measured in-band SNR at DSD64. The intermediate PCM
  rate is derived rather than configured, the stage streams so even a 1.7 GB DSD512 file
  costs it about half a megabyte of RAM, and the report carries the two numbers that matter for a
  1-bit deliverable: peak modulation index and overload count. No extra build dependency —
  it reuses libsoxr. See [DSD](#dsd-1-bit--sacd).

- Dry-run that prints the derived chain, the planned output path and the expected file
  size per input × target.

## Config

```toml
[meta]
name = "album-masters"

# Inherited by every target; overridable per-target.
# Precedence: built-in defaults < [defaults] < preset < explicit field.
[defaults]
dir     = "./out"
quality = "vhq"            # lq | mq | hq | vhq
# naming defaults to "{target}/{stem}.{cext}"
# placeholders: {stem} {ext} {target} {rate} {format} {cext} {dsd}

[target.streaming]
preset = "apple-music"     # -> 44100 / s16 / -16 LUFS / -1 dBTP

[target.cd]
preset = "cd"              # -> 44100 / s16 / -0.1 dBTP
lufs   = -12.0             # CD has no loudness standard; you choose
dither = "shaped"          # optional: TPDF + noise shaping (s16 only)

[target.cd-adaptive]
preset = "cd"
lufs   = -12.0
dither = "adaptive"        # adaptive psychoacoustic mode (s16 only)

[target.archive]
format = "f32"             # no preset: keep rate, no quantization, just normalize
lufs   = -14.0

[target.sacd]
preset = "sacd"            # -> dsd64 / DSF / -1 dBTP; no `format`, no `rate`
```

A PCM target needs a `format` (directly or via a preset); MP3 and DSD have none of their
own, so they take neither. Omitting `rate` keeps the source rate — except on a DSD target,
where the intermediate rate is *derived* (see [DSD](#dsd-1-bit--sacd)). Omitting `lufs`
skips loudness normalization (pure transcode). See `examples/mint.toml`, which is also the
file `mint` writes out when you run it without a config.

## Presets

A preset is a named bundle of *deliverable* fields — for the PCM ones that is `rate`,
`format`, `lufs` and `ceiling_dbtp`, reflecting a platform's published
delivery/normalization spec. Apply one with `preset = "NAME"`, then override any field on
top of it.

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
| `sacd` | *(derived)* | *(n/a — 1-bit)* | *(unset)* | -1.0 | Also sets `codec = "dsf"` and `dsd = "dsd64"`. Like `cd`: a physical format, no loudness standard. |

A preset sets **only the fields in its row.** Everything else (`on_clip`,
`limiter_character`, `limiter_soft_clip`, `limiter_link_channels`, `warn_limiting_db`,
`max_limiting_db`, `quality`, `dither`, `dither_strength`, `dither_correlation`, `codec`,
`bwf`, `mp3_bitrate`, `dir`, `naming`, `overwrite`) comes from the built-in defaults or your
`[defaults]` — so, e.g., dither stays `tpdf` and quality stays `vhq` unless you say
otherwise.

> **`sacd` is the only preset that works with a DSD codec.** Every PCM preset sets
> `format`, and a DSD target rejects `format` outright, so `preset = "hires"` +
> `codec = "dsf"` is a load-time error rather than a 1-bit high-resolution target. Start
> from `sacd` and override `dsd` instead.

> Streaming targets drift over time; treat these as sensible starting points, not gospel.
> The authoritative table is `preset()` in `src/config.rs`.

### Built-in defaults

When a field is set by neither a preset nor an explicit value, these apply:

| Field | Default |
|---|---|
| `rate` | *keep source rate (no resample); on a DSD target, derive the intermediate rate* |
| `lufs` | *unset (skip loudness normalization)* |
| `ceiling_dbtp` | -1.0 |
| `on_clip` | `limit` |
| `limiter_character` | `balanced` |
| `limiter_soft_clip` | `false` |
| `limiter_link_channels` | `true` |
| `warn_limiting_db` | 1.0 |
| `max_limiting_db` | 8.0 |
| `quality` | `vhq` |
| `dither` | `tpdf` |
| `dither_strength` | `normal` |
| `dither_correlation` | `decorrelated` |
| `codec` | `wav` |
| `dsd` | *unset (and required when `codec = "dsf"`)* |
| `bwf` | `false` |
| `mp3_bitrate` | 320 |
| `dir` | `./out` |
| `naming` | `{target}/{stem}.{cext}` |
| `overwrite` | `false` — refuse to replace whatever is already at the output path |

`overwrite` governs *both* kinds of file that can be sitting at a target's output path: a
previous render and (if the naming points there) an input master. Left at `false`, mint
refuses either and says which one it found. `--force` is the same thing for one run, on
every target. See [Notes](#notes).

`format` has no default. Every WAV/FLAC target must get one from a preset or an explicit
`format`; MP3 ignores it, and a DSD target **rejects** it — the modulator is the quantizer,
so there is no bit depth to choose.

### Overriding presets

Fields resolve lowest-to-highest precedence:

    built-in defaults  <  [defaults]  <  preset  <  explicit field

So an explicit field on a target always wins over the preset it sits on, while
`[defaults]` fills in anything a preset doesn't set (note: a preset's own fields beat
`[defaults]`).

Three fields read a *narrower* stack, because for them an inherited value and a stated one
mean different things: `format` and `rate` on a DSD target, and `bwf` on any non-WAV
target, are judged by what the target itself says (preset + explicit), ignoring
`[defaults]`. An inherited value is simply dropped. What happens to a *stated* one depends
on the field — `format` on DSD and `bwf` off WAV are errors, while an explicit `rate` on a
DSD target is the deliberate escape hatch and is honored (see [DSD](#dsd-1-bit--sacd)).

The point of the narrower stack is the same in all three cases: a `[defaults] format =
"s24"` — perfectly normal, since `format` is required for WAV/FLAC — must not turn every
DSD target in the same file into a hard error, a `[defaults] rate = 48000` must not make
one unsolvable instead of simply derived, and a `[defaults] bwf = true` must not break
every FLAC, MP3 and DSD sibling. `[defaults]` is a baseline for a heterogeneous set of
targets, not part of any one target's description. (`dsd` itself goes the other way and
*is* inherited, so one `[defaults] dsd` can serve several DSD targets.) Examples:

```toml
# Applied to every target unless overridden.
[defaults]
dir     = "./distros"
quality = "vhq"
dither  = "psychoacoustic"   # s16 targets get the strong shaper; s24/f32 ignore it

# 1. Use a preset verbatim.
[target.streaming]
preset = "spotify"           # -> 44100 / s16 / -14 LUFS / -1.0 dBTP

# 2. A deliberately loud master: maximize actually reaches the hot target by re-adding
#    the loudness the limiter eats (bounded by max_limiting_db of peak reduction);
#    the dense character is the slow-release profile made for sustained limiting.
[target.loud]
preset            = "spotify"
lufs              = -9.0     # overrides the preset's -14
on_clip           = "maximize"
limiter_character = "dense"
max_limiting_db   = 6.0      # spend at most 6 dB of peaks on loudness

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

[target.cd-adaptive]
preset = "cd"
lufs   = -12.0
dither = "adaptive"          # adaptive psychoacoustic mode (s16 only)

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

# 8. DSD (1-bit). Neither `format` nor `rate` — see the DSD section.
[target.sacd]
preset = "sacd"              # -> dsd64 / DSF / -1.0 dBTP

[target.sacd256]
codec = "dsf"                # the same target spelled out
dsd   = "dsd256"
lufs  = -16.0

# 9. Limiter character controls (used when on_clip = "limit").
[target.punchy]
preset                = "spotify"
limiter_character     = "transient"   # faster adaptive release
limiter_soft_clip     = true
limiter_link_channels = false          # independent L/R envelopes
```

`codec` is `wav` (default), `flac`, `mp3`, or `dsf`; the default `naming` ends in `{cext}`,
which expands to the codec's extension. Compatibility rules, all enforced when the config
loads: `bwf` is WAV-only, FLAC requires an integer format (s16/s24) and a rate of at most
**96 kHz**, `dsf` requires `dsd`, `dsd` requires `dsf`, and a DSD target rejects `format`.
Run `--dry-run` to see the fully-resolved chain, output path and expected size for every
target before processing anything.

Two of those depend on the input file and so cannot be settled when the config loads — a
target that omits `rate` delivers at whatever the master runs at, and nothing in the TOML
mentions channels. Both are checked from the input's header before anything is decoded, and
`--dry-run` prints them as a `!` line against the target that will fail:

- **FLAC above 96 kHz.** mint's FLAC encoder tops out there. It is not a format limit —
  FLAC itself codes 176.4 and 192 kHz — so a 24/192 deliverable means `codec = "wav"` for
  now. Set `rate = 96000` on the FLAC target to keep a hi-res master shippable.
- **DSD above 5.1.** The DSF channel-type table stops at six channels.

## Build

Prerequisites: a stable Rust toolchain (edition 2024) **and** the system **libsoxr** C
library + `pkg-config` — mint links libsoxr for reference-grade sample-rate conversion
(resampling only; dither, quantization, and noise shaping stay in this tool).

    # install the C library + pkg-config first (pkg-config is how the build finds libsoxr)
    brew install libsoxr pkg-config                 # macOS
    sudo apt-get install libsoxr-dev pkg-config     # Debian/Ubuntu
    sudo dnf install soxr-devel pkgconf-pkg-config  # Fedora/RHEL
    sudo pacman -S soxr pkgconf                      # Arch
    # Windows: vcpkg install soxr, then point pkg-config at it (set PKG_CONFIG_PATH to
    # <vcpkg>\installed\x64-windows\lib\pkgconfig) and add the bin dir to PATH

    cargo build

The repository is a Cargo workspace whose root *is* the `mint` package, so `cargo build`
and `cargo test` here still mean the CLI and nothing else. The second member, `gui/`
(`mint-gui`), is built explicitly with `cargo build -p mint-gui` and depends on `mint`
one-way — nothing it pulls in reaches the CLI binary.

The `quality = lq|mq|hq|vhq` setting maps onto libsoxr's recipes. See
[Audio quality](#audio-quality) for what libsoxr buys you.

### MP3 output (opt-in)

MP3 is behind the `mp3` feature for two reasons. The build one: `mp3lame-sys` compiles
libmp3lame from vendored C source, so it wants a C toolchain (`cc`, plus `make` on Unix).
The licensing one, which is why it stays off by default: those bindings are **LGPL-3.0**,
and released mint binaries deliberately link no LGPL-3.0 code at all.

    cargo build --features mp3

`mint --version` tells you which kind of build you have. Two CI guards keep the two apart —
the release workflow refuses to build if libmp3lame appears in the dependency graph or if
the finished binary reports MP3 as available, and the licence policy in `about.toml` does
not accept LGPL-3.0, so generating the third-party notices with the feature on is an error.

WAV (incl. Broadcast WAV) and FLAC need no extra feature. AAC is intentionally not
supported (libfdk-aac licensing).

### Licensing and third-party code

mint is **Apache-2.0** (`LICENSE`). Two dependencies are copyleft and are handled
deliberately; `THIRD-PARTY-LICENSES.md` ships in every release archive with the full
notices, licence texts and source locations, and `mint --version` summarises them.

- **libsoxr** (the C library) is LGPL-2.1-or-later and is linked **dynamically**. On macOS
  and Linux, released binaries link the copy on your system — so you need
  `brew install libsoxr` / `apt install libsoxr0` to run one, and mint distributes no part
  of the library. On Windows, where there is no system package, `soxr.dll` ships beside
  `mint.exe` and can be replaced with your own build. Static linking would make the binary
  a combined work under LGPL-2.1 §6 and oblige mint to hand out the means to relink; a
  shared library is §6(b), which is the clause written for exactly this.
- **libsoxr / libsoxr-sys** (the Rust bindings) are LGPL-2.0-or-later and *are* statically
  linked, so §6 does apply to them. Their source is on crates.io at the pinned versions and
  mint's own build workflow is public and reproducible from each release tag.
- **Symphonia** (demuxing and decoding) is MPL-2.0 — file-level copyleft, unmodified here.

The exact libsoxr source used for releases is attached to each GitHub release as
`libsoxr_0.1.3.orig.tar.xz`, pinned by SHA-256.

`THIRD-PARTY-LICENSES.md` is generated, and CI fails if it is stale or if a dependency
arrives under a licence nobody accepted:

    cargo about generate about.hbs -o THIRD-PARTY-LICENSES.md

## Run

If `--config` is omitted, `mint` uses `./mint.toml`. When that file is missing,
`mint` creates it from `examples/mint.toml` and uses it immediately (an existing
`mint.toml` is never overwritten).

Dry run (show derived chain + planned paths + expected sizes, process nothing):

    cargo run -- --config examples/mint.toml --dry-run "masters/*.wav"

    master.wav [96000 Hz]
      [cd] -> ./out/cd/master.wav  (52.9 MB)
          - loudness -> -12.0 LUFS (ceiling -0.1 dBTP, limit; balanced release; linked; soft_clip=false)
          - resample 96000 -> 44100 Hz (vhq)
          - re-check true peak against the ceiling and re-verify loudness (corrected within the headroom)
          - quantize -> s16 + tpdf + noise-shaping (gentle) (normal)
          - write WAV
      [flac] -> ./out/flac/master.flac  (<= 173.3 MB)
          ...
      [sacd] -> ./out/sacd/master.dsf  (211.7 MB)
          ...

A path that already has a file at it is marked — `(replaces existing)` when the target sets
`overwrite = true`, `(exists — will fail without --force)` otherwise — and a target that
cannot deliver this particular input at all (FLAC above 96 kHz, DSD above 5.1) gets a `!`
line saying so.

The size is what the file will actually weigh: exact for WAV and DSF (every stage in
between has a deterministic length contract), `~` for MP3 (a nominal bitrate), and `<=` for
FLAC, whose real size only exists once the material has been encoded. It is omitted when
the input header does not declare a length.

Process all targets for all inputs:

    cargo run -- --config examples/mint.toml "masters/*.wav" track.aif

Running that a second time into the same output directory refuses every target: nothing at
an output path is replaced unless you say so. Add `--force` (or set `overwrite = true` under
`[defaults]`) to re-render over the previous deliverables — the dry-run marks which paths
that would replace.

Render only specific targets (repeat `--target`):

    cargo run -- --config examples/mint.toml --target cd --target streaming "masters/*.wav"

### Explaining a target (`--explain`)

`--dry-run` prints the *chain* — what will happen to the audio. By then every config value
has been flattened into one answer. `--explain` asks the other question: where did each
value come from, and what became of the ones that lost?

    mint --config mint.toml --explain sacd

It needs no input files and reads no audio. Each row carries an **origin** badge (`B`
builtin, `D` `[defaults]`, `P` preset, `E` this target, `—` nothing set it) and a **fate**
(`·` applies, `~` inert, `/` an inherited value was dropped, `!` a load-time error if
stated on this target), followed by a `why` section spelling out every flagged row.

This is the surface for the merge's one documented exception: `format`, `rate` and `bwf`
are judged against the target's *own* layer (preset + explicit), not the merged one. So a
`[defaults] rate = 48000` is silently dropped on a DSD target while `format = "s24"` stated
on that same target is fatal — same inheritance, three different outcomes, and until now
none of it was visible anywhere.

### Machine-readable report (`--json`)

`--json` emits the full per-task metering (source + delivered loudness/dynamics, true
peak, PLR/PSR, applied limiting, warnings) plus any failures as JSON to stdout — the
human-readable report and banners are suppressed so stdout is pure JSON for QC pipelines:

    mint --config mint.toml --json "masters/*.wav" | jq '.tasks[] | {target, lufs: .delivered.integrated_lufs}'

Non-finite meter values (silence, or clips shorter than a meter's window) serialize as
`null`. A non-zero exit still signals failures, which also appear in the `failures` array.

One case produces **no document at all**: a batch rejected before it starts — an unreadable
config, an unknown `--target`, an input that cannot be probed, or two inputs that would
render to one path. Those are setup errors rather than task failures, so they go to stderr
with a non-zero exit and stdout stays empty. Automation should treat "exit non-zero and
stdout empty" as "nothing ran".

The document opens with a `"schema_version"` integer. It is bumped only when a reader
could break — a field removed or renamed, or one whose meaning changes. **Adding** a field
does not bump it, so a consumer must tolerate keys it does not know. It currently reads
**2**; version 1 differed in one respect, described under warnings below.

Alongside the metering, each task reports what the render actually did, which is what makes
a run reproducible and auditable after the fact:

| field | meaning |
|---|---|
| `channels`, `frames` | shape of the delivered file; `frames` is 1-bit `sampleCount` on a DSD task |
| `file_bytes` | size on disk, measured after writing (not the `--dry-run` estimate) |
| `elapsed_ms` | wall-clock time for that one input × target job |
| `seed` | the `--seed` used, or `null` — **`null` means the render is not reproducible** |
| `src_quality` | resampler tier that ran, or `null` when no conversion was needed |
| `loudness_gain_db` | static gain the normalization applied; `null` without a `lufs` target |
| `post_resample_gain_db` | further gain from the post-resample correction; `null` if that pass never ran, `0.0` if it ran and found nothing |
| `declared_lufs`, `declared_ceiling_dbtp` | what the target asked for; `declared_lufs` is `null` when it asked for no loudness. They travel with the task so `delivered` can be graded without the config that stated them |
| `dither.mode`, `dither.strength` | what was **asked for**; `null` on MP3 and DSD, which have no bit-depth stage |
| `dither.applied`, `dither.noise_shaping` | what actually **happened** |

The last two rows are a pair on purpose: a target that asks for `adaptive` at s24 gets flat
TPDF (shaping is s16-only), and `mode: "adaptive"` with `noise_shaping: null` is how that
becomes visible instead of looking identical to a target that simply asked for `tpdf`.

Two encodings in that document are worth knowing about. `format` carries the delivered
sample format for PCM (`"s16"`, `"s24"`, …) but `"<N> kbps"` for MP3 and `"1-bit"` for DSD.
And a DSD task gains a nested `dsd` object:

```json
"dsd": { "rate": "dsd64", "rate_hz": 2822400, "intermediate_pcm_rate_hz": 88200,
         "interpolation_factor": 32, "modulator_order": 7,
         "max_modulation_percent": 45.6, "overload_samples": 0, "loop_resets": 0,
         "samples_per_channel": 846725120, "file_bytes": 211681372 }
```

`max_modulation_percent` and `overload_samples` are the QC numbers for a 1-bit
deliverable — see [DSD](#dsd-1-bit--sacd).

#### Warnings and failures carry a kind

A warning is an object, not a string — **this is the change from schema 1**, where
`warnings` was an array of bare sentences:

```json
"warnings": [
  { "kind": "heavy_limiting", "subject": "loudness",
    "message": "heavy limiting: 4.2 dB of peak gain reduction to reach -14.0 LUFS under the -1.0 dBTP ceiling (source true peak -0.2 dBTP); consider a lower target or more source headroom" }
]
```

`message` is the sentence mint prints on the console, unchanged and still the thing to show
a person. `kind` is a stable snake_case discriminant to branch, group and count on, so QC
automation never has to match on prose: `heavy_limiting`, `loudness_target_missed`,
`src_loudness_corrected`, `residual_true_peak`, `ceiling_enforced_by_limiter`,
`ceiling_enforced_by_gain`, `ceiling_not_enforced`, `reduce_gain_capped`,
`predicted_peak_over_ceiling`, `maximization_capped`, `maximize_without_lufs`,
`undithered_reduction`, `dsd_over_modulation`, `dsd_sigma_delta_overload`. `subject` is the
coarser grouping: `loudness`, `peak`, `dither` or `dsd`.

Each entry in `failures` gains a `kind` for the same reason. In this document it is one of
`output_exists`, `output_is_the_input`, `delivery_blocked` or `other` — the four ways a
single task can fail once the batch is under way. Of those, only `output_exists` is something
`--force` would have allowed; mint refuses to write over an input master under `--force` too.

The other kinds the type can express never reach this array, and that is worth stating rather
than leaving a reader to wonder. A cancelled job is not a failure — nothing was written and
nothing was refused, so it is neither a task nor a failure. And a batch collision
(`batch_output_collision`, `batch_writes_over_an_input`) is caught in the pre-flight, before
any audio is touched, which is one of the cases above that produces **no document at all**.

    mint --config mint.toml --json "masters/*.wav" | jq '[.tasks[].warnings[].kind] | group_by(.) | map({kind: .[0], n: length})'

## Audio quality

Three choices affect output fidelity: the **resampler** (whenever a target's rate differs
from the source — and always, on a DSD target), the **dither / noise-shaping** mode (only
when quantizing to an integer format, i.e. s16/s24), and for a 1-bit deliverable the **DSD
rate**. All are per-target config; `--dry-run` and the per-file report show exactly what
each target will do.

### Loudness measurement

`lufs` is an EBU R128 / ITU-R BS.1770 integrated measurement (via `ebur128`), and BS.1770
weights channels rather than just summing them. What mint uses:

| channels | weighting |
|---|---|
| 1 (mono) | counted **twice**, as dual mono |
| 2 | L, R at +0 dB |
| 5.1 (SMPTE order) | L, R, C at +0 dB; Ls, Rs at +1.5 dB; LFE excluded |
| 7 and above | channels 7+ are currently **excluded** from the measurement |

The mono row is the one that surprises people, and it is deliberate: a 1-channel deliverable
is played through both speakers, so the loudness that matters is the dual-mono one, 3.01 LU
(10·log₁₀2) above a single-channel reading. Metering it as one channel would put every mono
master 3.01 dB over its target on any normal playback system.

> **Compatibility note.** mint metered mono as a single channel before this was fixed, so a
> mono deliverable re-rendered now comes out **3.01 dB quieter** than the same target
> produced previously. Stereo and surround are unaffected, and true peak is unaffected at
> every channel count.

### Resampling

Sample-rate conversion goes through **libsoxr** (needs the system library; see
[Build](#build)): reference-grade multi-stage SRC with a ~150 dB stopband, the same
engine class as SoX and Weiss.

`quality = lq | mq | hq | vhq` (default `vhq`) sets the filter length/steepness and maps
onto libsoxr's internal recipes. `vhq` is the right choice for masters; the lower
settings trade stopband and transition-band performance for speed.

The resampler runs **only when a target's rate differs from the source** — same-rate PCM
targets skip it entirely. Whenever it does run, two things are re-verified afterwards,
because a conversion changes both of the numbers the target is judged on:

- **The true-peak ceiling**, re-checked and re-limited, since sample-rate conversion creates
  new inter-sample peaks.
- **The integrated loudness**, re-measured and pulled back onto the `lufs` target with a
  static gain. The normalization gain was computed at the *source* rate, and BS.1770's
  K-weighting keeps counting energy above 20 kHz — so band-limiting a 96 kHz master to
  44.1 kHz removes loudness the meter had already credited. Ordinary programme drifts about
  0.03 LU and nothing happens; an ultrasonic-heavy master can drift several LU.

  An upward correction is capped at the headroom under `ceiling_dbtp`. That cap is the point:
  within it no limiter runs, so the delivered dynamics are untouched and the correction cannot
  eat back the loudness it just added. The cost is that a target with no headroom left cannot
  be fully corrected — that residual is reported rather than bought with gain reduction.

Both passes apply under every `on_clip` policy, `warn` included: `on_clip` is a decision
about clipping, and neither pass can put a sample above the ceiling.

The "loudness target not met within ±0.1 LU" warning is measured on the **delivered** audio,
after both of those, so it describes the file that ships rather than an intermediate one.

DSD targets are the exception on both counts: they resample to a *derived* intermediate
rate (which is usually not the source rate), and the ×2ⁿ interpolation into the modulator
then runs unconditionally on top of it. So `quality` is never irrelevant on a DSD target —
it selects the filter feeding the most level-sensitive stage in the chain.

libsoxr handles large or awkward rate ratios (e.g. 96k → 44.1k) by deriving the
multi-stage decomposition itself, which is why it is the only backend here. It does
resampling only; dither, quantization, and noise shaping always stay in this tool.

### Clip policy

`on_clip` decides what happens when the delivered signal would exceed `ceiling_dbtp`:

| `on_clip` | What it does |
|---|---|
| `limit` *(default)* | Apply the full normalization gain and let the true-peak limiter hold the ceiling. |
| `maximize` | As `limit`, plus makeup-gain convergence so a hot `lufs` target is actually reached — see [Loudness maximization](#loudness-maximization). |
| `reduce_gain` | Never limit: cap the normalization gain (and, if needed, attenuate) so the peak fits. Dynamics are preserved exactly; the loudness target may be missed, and a warning says so. |
| `warn` | Do neither. Report the overshoot and deliver it as-is. Diagnostic use — this is the one policy that can hand you a file above its own ceiling. |

One caveat on the last two: neither post-resample pass (see
[Resampling](#resampling)) consults `on_clip`. Sample-rate conversion creates new
inter-sample peaks, so a target that resamples always gets those trimmed by the limiter and
its loudness re-verified, and a `warn` target can therefore come out at its ceiling after
all. `warn` only truly delivers an overshoot when no resample happens.

### Limiter behavior

When `on_clip = "limit"` or `"maximize"`, limiter behavior is configurable per target:

| Field | Values | Meaning |
|---|---|---|
| `limiter_character` | `balanced` (default), `transient`, `dense` | Program-dependent release profile and look-ahead length (which also sets the raised-cosine attack-ramp length: 1.2 ms `balanced`, 0.6 ms `transient`, 2.0 ms `dense`). `balanced` is smoother; `transient` recovers faster and preserves attack more aggressively; `dense` recovers much slower — the mastering-style choice for *sustained* gain reduction (pair it with `maximize`), riding between peaks instead of pumping. |
| `limiter_soft_clip` | `true` / `false` (default) | Applies a gentle saturating curve near the ceiling after limiting to tame residual needles. |
| `limiter_link_channels` | `true` (default), `false` | `true`: one shared gain envelope (stable stereo image). `false`: independent channel envelopes (can preserve more per-channel punch, but may shift image under heavy limiting). |
| `max_limiting_db` | dB > 0 (default 8.0) | `maximize` only: the peak gain reduction the makeup loop may spend before it stops adding gain (with a warning). The guard that keeps a mis-set hot target from grinding a master flat. |

These options are ignored unless `on_clip = "limit"` or `"maximize"`.

### Loudness maximization

`on_clip = "limit"` treats limiting as a safety net: the makeup gain is computed once
from the source loudness, and when limiting then eats loudness, hot targets undershoot
(reported via the "loudness target not met" warning). `on_clip = "maximize"` makes the
loudness target authoritative instead: after each limiting pass the delivered LUFS is
re-measured and the shortfall re-added as gain — restarting from the pristine source
each time, so the delivered audio always carries exactly one limiter generation — until
the target is hit (±0.1 LU), `max_limiting_db` of peak reduction is reached, or the
signal stops getting louder. The heavy-limiting warning is suppressed (limiting is the
point); honesty comes from the report instead: peak **and** mean gain reduction plus the
percentage of the signal limited, alongside the delivered PLR/PSR. Expect transparent
results up to roughly 3–6 dB of peak reduction with `limiter_character = "dense"`;
beyond that a single-band brickwall audibly flattens the program.

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
| `adaptive` | TPDF + a noise-shaping filter **redesigned per frame** from an MPEG-1 model-1 masking analysis of the actual material, interpolated between frames. Keeps a fixed ATH-optimal curve as a floor, so quiet passages are never worse than `psychoacoustic`. | Final s16 masters where the material varies a lot. The most aggressive option; s16 only. |
| `none` | Plain truncation, no dither. | Expert-only. Allowed on request; the tool warns when reducing to s16 without dither. |

Two further fields tune `adaptive`, and **only** `adaptive` — the flat and fixed-curve
modes ignore both, even though the dry-run prints the strength on every integer target:

| Field | Values | Meaning |
|---|---|---|
| `dither_strength` | `low`, `normal` (default), `high` | Order of the per-frame FIR the masking analysis designs (5 / 7 / 9 taps). Higher follows the masking curve more closely and is less forgiving of later processing. |
| `dither_correlation` | `decorrelated` (default), `correlated`, `mid_side` | How the per-channel dither streams relate. `decorrelated` gives each channel its own noise (maximum SNR benefit, the right default); `correlated` shares one stream across channels (mono noise, lower SNR); `mid_side` dithers M/S and reconstructs L/R. |

Things worth knowing:

- **Shaping only engages at s16.** At s24 the noise floor is already inaudible, so both
  `shaped` and `psychoacoustic` fall back to flat TPDF; at f32/f64 there is no quantization
  to dither at all. The dry-run and report spell out what ran and name the curve — e.g.
  `quantize -> s16 + tpdf + noise-shaping (psychoacoustic)`.
- **Quantization runs exactly once.** Integer outputs are quantized in the bit-depth step
  (`ops/bitdepth`) using the shared grid in `OutputSampleFormat::int_grid()`. Container
  writers (`audio/io_write`) do not apply a second independent quantizer; they only
  materialize those already-quantized grid points as PCM integers.
- **`dither = "none"` is allowed but guarded by warnings.** Undithered reductions are
  accepted, and a warning is emitted for `* -> s16` with `dither = "none"` because
  correlated quantization distortion is more likely to be audible there. Undithered
  `* -> s24` is supported for users who prefer it.
- **`psychoacoustic` is tuned for 44.1 kHz** (its notch sits at ~4 kHz) and deliberately
  concentrates noise near Nyquist. Apply it as the **final** step before delivery — not if
  the output will be resampled or lossy-encoded afterwards; prefer `shaped` or `tpdf`
  there. It still helps at other s16 rates, just not optimally.
- Both shaped modes run **one independent shaper per channel**, so stereo imaging is
  preserved.
- `--seed <N>` makes the otherwise-random dither fully deterministic, so re-runs are
  bit-for-bit identical — useful for reproducible builds and A/B testing. This covers DSD
  too: the modulator's comparator dither is seeded from the same value.

> The `psychoacoustic` curve uses coefficients from Lipshitz, Vanderkooy & Wannamaker,
> "Minimally Audible Noise Shaping," *JAES* 39(11), 1991 — the same family of curves behind
> commercial tools such as POW-r and Sony Super Bit Mapping.

### DSD (1-bit / SACD)

```toml
[target.sacd]
preset = "sacd"       # dsd64 + codec = "dsf" + ceiling_dbtp = -1.0
dsd    = "dsd128"     # override the rate if you want; dsd64 | dsd128 | dsd256 | dsd512
```

DSD replaces the bit-depth stage rather than configuring it: there is no bit depth to pick,
because the sigma-delta modulator *is* the quantizer. So a DSD target takes no `format`
(setting one is an error, not a warning), and `dither`, `dither_strength` and
`dither_correlation` do not apply.

**Level: 0 dBFS is 50 % modulation, and that is not an attenuation.** The Scarlet Book /
SACD convention puts the 0 dB reference at a 50 % modulation index, with the maximum a
1-bit stream can represent at 71.43 % (the 20/28 rule, i.e. +3.1 dB above reference). mint
therefore feeds the modulator at ×0.5 — and a DAC reads 50 % modulation as *its* 0 dBFS, so
playback level is unchanged. `ceiling_dbtp` keeps its usual meaning on top of that: at
`-1.0` the stream peaks around 45 % modulation, comfortably inside the format.

**The intermediate rate is derived, not configured.** DSD rates are multiples of 44 100 Hz,
so the interpolation ratio is only an exact power of two out of the 44.1 kHz family. mint
picks

    intermediate = min( nearest 44.1-family rate , dsd_rate / 32 )

before interpolating by 2ⁿ. The cap is a low-pass we get for free: above ~30 kHz a DSD64
stream is shaped noise rather than signal, and program content up there buys nothing while
costing loop stability margin and inviting intermodulation back into the audio band.
Capping the input rate hands that filtering to the interpolator, which runs regardless.

| Source | dsd64 | dsd128 | dsd256 | dsd512 |
|---|---|---|---|---|
| 44.1 / 48 kHz | 44.1 ×64 | 44.1 ×128 | 44.1 ×256 | 44.1 ×512 |
| 88.2 / 96 kHz | 88.2 ×32 | 88.2 ×64 | 88.2 ×128 | 88.2 ×256 |
| 176.4 / 192 kHz | **88.2 ×32** | 176.4 ×32 | 176.4 ×64 | 176.4 ×128 |
| 352.8 kHz (DXD) | **88.2 ×32** | **176.4 ×32** | 352.8 ×32 | 352.8 ×64 |

**Bold** = the cap is what decided it. An explicit `rate` overrides the rule completely
(keeping ultrasonic content in a low-rate archival file is a legitimate choice); it is only
validated for reaching the DSD rate by a power of two, so a 48 kHz-family rate is rejected.

**The modulator** is an order-7 CIFB loop with a noise transfer function designed per rate
(`H_inf` 1.30, comfortably inside the Lee criterion), plus state clamping and a comparator
dither. Measured in-band SNR, 20 Hz – 20 kHz: **121.0 dB at DSD64**, and 164 / 174 / 175 dB
at DSD128 / 256 / 512. It is stable over the whole range the pipeline can produce — the
ceiling stage caps modulation at 50 %, and the loop keeps at least 2 dB of margin beyond
that; past the format's 71.43 % it overloads in a controlled way and recovers rather than
diverging.

**Sizes, and what the report tells you.** DSD is big: five stereo minutes is ~212 MB at
DSD64 and ~1.7 GB at DSD512, which is why `--dry-run` prints the exact size. The encode
itself streams — one libsoxr instance, one modulator per channel and a 4 kB-per-channel
container buffer — so the DSD stage adds about half a megabyte on top of the PCM buffer,
regardless of how long the file is or how high the rate. (Written out as f64 at DSD rate
the same audio would be ~13.5 GB, which is why it is never materialized.) Per file the
report adds:

    dsd64 via 88200 Hz x32 | order 7 | peak modulation 45.6% | overload 0 | 211.7 MB

`peak modulation` is the level QC number — a warning fires above 71.43 %, and `overload`
counts samples where the loop was driven past what it can track (0 is what you want). Note
it is measured *after* interpolation, so it is a true-peak figure: at `ceiling_dbtp = -1` it
reads ~45.6 % rather than the 44.6 % that `10^(-1/20)/2` predicts. That difference is
inter-sample peaks, not an error.

Worth knowing:

- **`dsd512` is a de-facto extension.** The DSF specification names only DSD64 and DSD128;
  every current reader accepts the higher rates, but player support is thin. It is allowed,
  and the dry-run says so once.
- **The stream is extended to a whole container block, and `sampleCount` counts the
  extension.** DSF blocks are a fixed 4096 bytes per channel, so a stream that ends
  mid-block has to be filled out. The spec (Annotation 3) says to pad with `0x00` and leave
  `sampleCount` at the true value; mint does neither. A DSD bit is ±1, so a zero byte is
  full-scale negative DC rather than silence — spliced onto a real tail it measures −1.0
  and clips at the transition. Instead mint keeps *modulating silence* until the block is
  full, so the tail is genuine modulator output with no splice at all, and reports the
  extended count, which removes the "unused sample data area" that ffmpeg, MPD and WavPack
  each handle differently. (The writer can still pad with a byte pattern; that path uses
  `0x69`, the silence constant, and is not reached from the encoder.) The file runs up to
  11.6 ms (DSD64) longer than the source as a result; for gapless album sets that is the
  one thing to be aware of.
- **Not supported, deliberately:** DFF/DSDIFF, DST compression, SACD image authoring, DSD
  *input*, DoP, 48 kHz-based DSD (3.072 MHz), and metadata — a `.dsf` from mint carries no
  ID3 tag (`metadataPointer` is 0, which is spec-legal), because mint carries no tag model
  for any codec.

> The noise-transfer functions are designed offline by `tools/design_ntf.py` (Chebyshev-II
> high-pass, exact CIFB realization) and baked in as a constant table; the measurement
> harnesses that produce the figures above are `tests/dsd_verification.rs`,
> `tests/dsd_format.rs` and `tests/dsd_pipeline.rs`. The full design rationale, including
> what was measured and rejected, is in `docs/dsd-design.md`.

## Test

    cargo test

## Notes

- **Nothing at an output path is replaced by default.** `overwrite = false` (the default)
  refuses both a previous render and an input master, with a different message for each;
  `--force` or `overwrite = true` allows it. So re-rendering into a directory you have
  already rendered into needs `--force` — that is the trade for the previous run's
  deliverables being safe.
- "The input file" is a question about *identity*, not spelling. A leading `./`, a `..` in
  the path, a symlink and a hard link all name the same bytes and are all caught; the
  comparison is by inode on Unix and by canonical path elsewhere.
- Writes are atomic: every codec renders to a sibling temp file and is renamed into place, so
  a failed or interrupted encode cannot destroy the deliverable that was already there. What
  this does *not* promise is durability across a power cut (there is no `fsync`), and a
  replaced file takes the new file's permissions rather than keeping its own.
- Output naming requires `{stem}`; targets that would collide on disk are rejected at load,
  and two *inputs* that would render to one path (same stem, different directories) are
  rejected before anything is decoded — as is a target whose output would land on another
  input of the same run. No naming placeholder carries an input's directory, so the fix for
  a same-stem collision is separate runs with different `dir`s, or a rename.
  `{cext}` (codec extension) and `{dsd}` (`"dsd64"`…, empty for PCM) are available as
  tie-breakers; on a DSD target `{rate}` renders the 1-bit rate and `{format}` renders
  `1bit`.
- Globs are expanded in the tool, so behavior is consistent across shells/OSes.
- The `quality` (resampler) and `dither` (quantizer noise) settings are explained in
  [Audio quality](#audio-quality) above.
- Fields that do not apply to a target's codec are ignored rather than rejected — `dither`
  on an MP3 or DSD target, `mp3_bitrate` on a WAV one. The exceptions are the combinations
  a target states about *itself* and that are outright contradictory (`format` on DSD,
  `bwf` off WAV, a float `format` on FLAC, `dsd` without `dsf` or the reverse), which fail
  when the config loads. The same values arriving from `[defaults]` are dropped instead.





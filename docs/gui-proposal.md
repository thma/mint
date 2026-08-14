# mint GUI — technology evaluation and proposal

**Date:** 2026-08-13 · **Repo state:** `main` @ `e61fe91` · **Status:** decision document. No code
was changed to produce it.

## How this document was produced, and how much to trust it

This is the output of a structured multi-agent evaluation run on 2026-08-13: two ground-truth
audits (mint's integration surface; the packaging and toolchain reality of shipping a Rust binary
with a system C dependency), six independent candidate deep-dives, one toolkit-agnostic UX and
information-architecture pass, and three adversarial judge panels — release engineering, product
design, and "the maintainer of mint in 2031" — each explicitly tasked with attacking the candidate
reports' claims rather than averaging them. Part I is the adjudicated synthesis; Parts II–VI are
the raw inputs, kept because the reasoning matters more than the verdict.

Three honesty notes about the evidence:

- **Claims about this repository were verified directly against the source** and carry file:line
  references. Confirmed by hand: `config.rs:724` (`resolve_target` calls `validate_fields`, so
  per-target validation is reachable from outside the crate); `atomic_write.rs:144`
  (`Staged::drop` removes the staging file when `!committed`, so cancellation cannot leave a
  partial deliverable); `release.yml` (static soxr 0.1.3 across five targets, the
  `grep -m1 '^version' Cargo.toml` tag guard, Linux runners on glibc 2.39, no
  `aarch64-pc-windows-msvc` leg, both workflows on `dtolnay/rust-toolchain@stable` with no
  `rust-toolchain.toml`); `Cargo.toml` has no `license` field and `LICENSE:189` still carries the
  `[yyyy] [name of copyright owner]` placeholder.
- **Claims about the outside world — crate versions, framework release history, upstream issue
  numbers, signing-programme pricing and policy — come from the agents' own web research**, cited
  inline where they were cited to me. They were current as researched on 2026-08-13 and are not
  independently re-verified here. Re-check any version-sensitive number before it drives a
  purchase or a contract.
- **One correction to Part I's Tier-0 list**, found on verification and not reflected in the
  synthesis text: five config enums need `PartialEq`, not eight. `DitherStrength` (config.rs:117),
  `DitherCorrelation` (:138) and `OutputCodec` (:217) already derive it. The five that do not are
  `ResampleQuality` (:85), `OnClipPolicy` (:94), `LimiterCharacter` (:107), `DitherMode` (:162) and
  `BitDepthFormat` (:205).

Nothing here commits anyone to a plan. Part I §11 lists the questions that are the project owner's
to answer, and several of them can invalidate the primary recommendation.

## Contents

- **Part I — The proposal**: recommendation, adjudication of the judges' disagreement,
  architecture, the ranked list of core changes, phasing, packaging plan, risks, open questions.
- **Part II — Candidate evaluations**: six stacks scored against seven criteria (pre-adjudication).
- **Part III — UX and information architecture**: toolkit-agnostic; screens, the provenance model,
  wireframes, and the capability checklist the stacks were scored against.
- **Part IV — Integration-surface audit**: what a GUI can stand on in the mint core today.
- **Part V — Packaging and toolchain audit**: libsoxr, signing, notarization, CI, licensing.
- **Part VI — Judge panels**: three adversarial lenses and the claims they overturned.

---

# Part I — The proposal

**Date:** 2026-08-13 · **Repo:** `/Users/thomas.mahler/code/mint` @ `main` (`e61fe91`) · **Status:** decision document, no code changed

---

## 1. Recommendation

**Build the GUI with `egui`/`eframe` (0.36.1), in-process, as a second crate in a Cargo workspace whose root stays the existing `mint` package.**

**Runner-up: Tauri 2.11.5.** Switch to it if — and only if — *all three* of these become true: (a) the GUI is a distributed, visually-branded product where a designer will be hired and visual identity is a differentiator, (b) Linux is explicitly demoted to tier-2 support with an AppImage plus a `WEBKIT_DISABLE_DMABUF_RENDERER=1` launcher wrapper as the accepted answer to blank-window reports, and (c) you accept a permanent second toolchain (Node/pnpm, a second lockfile, `tauri-specta` codegen) in a repo whose entire proposition is verifiable determinism. If only (a) is true, that is not enough — see §3.1.

**Third place, named because two of three judge panels ranked it second: Slint 1.17.1.** It is genuinely better than egui on retained-mode aesthetics, native macOS menus and accessibility. It is blocked by a licence decision that is yours, not mine (§11, Q3), and by a concrete widget gap that eliminates a "non-negotiable" screen (§3.2).

**Not viable: Dioxus 0.7** (webview risk without webview ecosystem; no signing support in `dx bundle`; an open, reproduced bug shipping Linux `.deb` packages with no CSS or icons; three-person vendor planning a platform-API overhaul in the next minor). **Not viable as specified: Iced 0.14** (no numeric input widget for six decimal fields; `table` is an eager layout helper with no sorting, selection or virtualization — it fails the two mandatory widget requirements the primary screen is made of; zero accessibility with the most complete AccessKit PR closed unmerged in twelve hours).

---

## 2. Why egui, in one argument

The UX analysis concluded — correctly — that this app is **a plan-and-evidence workbench, not a config editor**. Its primary object is the (input × target) plan: derived chain, output path, predicted bytes, blockers; and after a run, delivered LUFS/dBTP against what was declared. Every screen in it is a projection of functions mint already exports: `probe_source`, `resolve_target`, `describe`, `estimated_output_bytes`, `delivery_blocker`, `render_output_path`, `check_planned_outputs`.

`CLAUDE.md` is organised around one anxiety: the derived chain already has **three** parallel representations (`pipeline.rs`, `describe()`, `estimated_output_bytes()`) that "are separate code paths and drift silently", and the codebase spends real design effort — three deliberately exhaustive `match target.codec` arms, a regression test in `tests/dsd_pipeline.rs` for the catch-all failure mode — making a forgotten step a *compile error*.

egui adds **zero** new representations. The form's validation error text is mint's own `anyhow` string. I verified the load-bearing fact myself: `config.rs:724` — `resolve_target` calls `resolved.validate_fields()?` before returning, so every per-target rule, including the own-layer `format`/`rate`/`bwf` exception that makes `preset = "sacd"` work, is **already reachable from outside the crate as a plain function call.** Only cross-target `check_collisions` (config.rs:1130) and whole-config `validate` (:729) are private, and both are one-line `pub`.

Every other family adds a fourth representation:

- **Tauri/Dioxus/Electron:** a TypeScript mirror of 23 fields, 11 presets, 9 enums and the four-layer merge, checked by nothing at Rust compile time.
- **Sidecar:** the escape from duplication is to make mint grow and *forever version* three or four new machine-readable contracts (NDJSON task events, `--dry-run --json`, `mint schema --json`, `mint config set --json`). That is not avoiding a fourth code path — it is building four of them and calling them an API.

Secondary, but not small:

- **Packaging is already 80 % done and egui consumes 100 % of it.** `release.yml` static-links soxr 0.1.3 on five targets (Debian orig tarball + `-DBUILD_SHARED_LIBS=OFF` on Linux/macOS; vcpkg `x64-windows-static-md` plus a hand-forged `soxr.pc` on Windows, because `libsoxr-sys` 0.1.4 probes exclusively via pkg-config with no fallback). An egui binary inherits all of it with one extra `cargo build` line per matrix leg. No webview engine on the user's machine, no `libwebkit2gtk-4.1-0` runtime dependency, no WebView2 bootstrapper decision, no Node.
- **The future waveform/spectrum path is free.** The IA analysis found a 1:512 min/max envelope costs ~0.4 % of the buffer (~1.8 MB for a five-minute 96 kHz stereo master) computed inside the decode loop that already runs. In-process, that reaches a `Shape::Path` with no copy. In the sidecar shape there is *no peak-data path at all* — `--json` carries no frames, no channels, no envelope — so you would write a second decoder that has to re-derive symphonia's AIFF SSND 8-byte overcount correction and the `0xFFFFFFFF` streamed-WAV cap that `io_read::probe_source` already encodes.
- **Cancellation is already safe.** `atomic_write::Staged::drop` removes the staging file on unwind, so a cancelled or panicked job leaves the previous render intact and no partial deliverable. That property is what makes a Cancel button acceptable at all, and it exists today.

---

## 3. Where the judges disagreed — adjudicated

Two panels (release engineering, 2031-maintainer) ranked egui first. One panel (product design) ranked egui **fourth** and Tauri first. That is the only real disagreement and it deserves a straight answer.

### 3.1 The design panel's case, and why it does not flip the decision

The design judge's technical core is correct and I am not going to soften it: **none of egui, iced or Slint exposes OpenType feature control.** You cannot toggle `tnum` for tabular figures, there is no baseline alignment, no subgrid, no optical decimal alignment. For a UI made almost entirely of LUFS/dBTP/GR/byte columns, that is a real, unfixable-in-toolkit ceiling, and CSS gives all of it in one line.

Three things blunt it for *this* app:

1. **The `tnum` problem has a concrete one-time fix.** Ship a bundled font with tabular figures permanently applied — either a font whose default figures are already tabular (JetBrains Mono, IBM Plex Mono, Roboto Mono for the numeric columns) or a proportional face frozen with `fonttools pyftfeatfreeze -f tnum`. That is a build-step artefact, not a per-screen fight. For a mastering tool, a monospaced numeric column is idiomatic anyway, not a compromise.
2. **The design judge itself corrected egui's biggest claimed hole in egui's favour.** The UX brief named "text editor with TOML syntax highlighting and error spans" as *the requirement most likely to eliminate a stack*. `egui::TextEdit::layouter` returns a `LayoutJob` whose per-section `TextFormat` carries colour, background and an underline `Stroke`; egui ships a `code_editor` demo built on exactly this. For a 92-line `mint.toml` with spans recovered from `toml::de::Error::span()`, this is days of work, not a blocker. **egui passes all twelve mandatory items on the UX checklist. Slint fails #3. Iced fails #1 and #2.**
3. **The design panel's own runner-up undercuts its winner.** It notes that Tauri means three text rasterizers and three CSS feature sets (WKWebView, Chromium, WebKitGTK 2.36→2.52), so a design tuned in DevTools is pixel-identical nowhere — and that Electron's single pinned Chromium is therefore *better for elegance than Tauri*. But Electron loses on every other axis for this codebase. The design lens' internal ordering is unstable in a way its top pick doesn't survive.

**Adjudication:** the design panel wins the sub-argument (web is the higher ceiling; egui's default look is a debug menu) and loses the decision, because the ceiling it buys is paid for with a defect class on one platform that cannot be fixed, tested or regression-pinned inside this repo — in a project whose identity is `to_bits()` equality and golden headers. I am also honoring its correction of egui's own report: **the theming pass is 3–6 weeks of designer-developer time with no hot reload, not "3–5 days"** (§8, Phase 5). Budget it honestly or do not claim elegance.

### 3.2 Slint: two second-places, and why it is third here

Release-engineering and maintainer panels both put Slint second, on three years of unbroken 1.x, native `muda` menus, AccessKit, and a real retained scene graph. The design panel scored it 4 on UI ceiling — and then discovered the gap it had missed: **editable syntax-highlighted text is maintainer-confirmed unsupported** (slint-ui/slint discussion #3401 → issue #2723; the underlying "no text document model" gap is #1325, open since June 2022). `StyledText` (1.16) is read-only markdown display. Stack that on `SpinBox` being `int`-only (mint has four `f64` fields: `lufs`, `ceiling_dbtp`, `warn_limiting_db`, `max_limiting_db`), text-only `StandardTableView` cells, no modal dialogs, and external file drag-and-drop reachable only through the explicitly-removable `unstable-winit-030` feature — and you build four widgets before you design one screen.

Survivable by redesigning the config drawer as a read-only highlighted view plus "open in $EDITOR" and a file watch. Arguably a better product anyway. But it plus the licence gate (§11 Q3) puts Slint behind egui, not ahead.

### 3.3 Judge-caught inflated claims I am honoring

| Claim in the dossier | Correction | Effect |
|---|---|---|
| "cargo-packager does not sign or notarize" (scored as a penalty against egui, Iced **and** Slint) | **False.** I verified `cargo_packager::config::MacOsConfig` exposes `signing_identity`, `entitlements`, `provider_short_name` ("for notarization purposes") and `notarization_credentials`; `WindowsConfig` has `certificate_thumbprint`, `timestamp_url`, `sign_command`. It descends from tauri-bundler and inherited the path. | Pure-Rust packaging score **+1**; Tauri loses a claimed differentiator. |
| "libsoxr FFI segfault is uncatchable and takes the window down" (cited by egui *and* Iced reports) | FUD. libsoxr is pure computation, no I/O; every `Soxr` is built through one lock-guarded site (`ops::resample::create_soxr`); `tests/soxr.rs` guards it under concurrency. | Removes a false argument for process isolation. |
| "An OOM kills the editor and the user's unsaved project" | Overstated. The UX design makes `mint.toml` on disk the source of truth and assumes text-editor co-editing. A crash costs a spinner. Mitigation is one integer (cap `--jobs`), which *every* stack needs since `cli.rs` defaults to `available_parallelism()`. | Weakens the sidecar case. |
| Dioxus `uiCeiling: 5` | `dioxus-primitives` is **0.0.0 on crates.io** — a 12-line placeholder from April 2025. No CodeMirror, TanStack, Radix or wavesurfer.js comes along in RSX. Scored **3**. | Dioxus drops to last. |
| Sidecar family `uiCeiling: 5` | Carried entirely by the Electron member. Flutter draws its own glyphs (no OS text match, documented NVDA/UIA semantics failures); Avalonia's route to distinctive runs through paid control suites. Family average is 3. | Sidecar's headline strength is one member's. |
| Dioxus report's `otool -L` libsoxr panic (install_name_tool, Frameworks relocation) | The observation is the most valuable in the dossier; the conclusion is inverted. CI links soxr **statically**, so there is no dylib to relocate. The real lesson: **dev-box and CI builds have different linkage, so bundle verification on a laptop proves nothing.** | Baked into §9 as a hard CI rule. |
| "egui forces both CI workflows onto egui's toolchain schedule" | Half wrong. eframe 0.36.1 does need rustc ≥ 1.95 (this machine is on 1.92 and `cargo build` refuses). But `ci.yml` and `release.yml` both use `dtolnay/rust-toolchain@stable` and there is no `rust-toolchain.toml` in the repo — CI already tracks stable. It is a `rustup update`, not a pipeline change. | Velocity penalty removed. |
| "release.yml's `grep -m1 '^version' Cargo.toml` breaks under a workspace" | True **only if the root becomes a virtual manifest.** Avoidable entirely — see §5.1. | Dealbreaker eliminated by design. |
| "in-process = validation is shared, so provenance is free" (egui, Slint, Dioxus reports) | True for **error strings**, false for **provenance**. `overlay()`, `builtin_fallback()`, `preset()` are private and `resolve_target` discards which layer supplied each value. `resolve_target_explained` is required **regardless of toolkit**. | Moved into the core-change list as a first-class item (§6, T2-1). |
| Progress requires invasive DSP surgery (used to justify the process boundary) | The maintainer panel's best catch: `estimated_output_bytes` is **byte-exact** for WAV and DSF, and `atomic_write` stages to a documented `.<name>.<pid>.<n>.mint-tmp` sibling. A `stat()` on the staging file yields a *true* percentage with **zero** changes to `ops/`. | Becomes Phase 1's progress mechanism (§8). |

---

## 4. Comparison table (adjudicated)

Scores are the candidate evaluations' own, corrected per §3.3. Axes are unweighted and deliberately **not** summed — the two things that actually decide this (Slint's licence gate, Tauri's WebKitGTK failure mode) sit on no axis.

| Stack | Cross-plat | Native dep | Packaging | UI ceiling | Velocity | Longevity | A11y | Verdict |
|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|---|
| **egui / eframe 0.36.1** | 4 | 5 | **5** | 3 | 4 | 3 | 3 | **Recommended.** Passes all 12 mandatory UX items. Ceiling is "instrument", not "designed". |
| **Tauri 2.11.5** | **2** | 5 | 4 | **5** | 4 | 4 | 4 | Runner-up. Highest ceiling; Linux is a defect class you cannot own. |
| **Slint 1.17.1** | 4 | 5 | 4 | 3 | 4 | 4 | 4 | Third. Licence gate + no editable syntax-highlighted text. |
| **Sidecar (Electron 43)** | 4 | 5 | 3 | 5 | 3 | 3 | 4 | Viable; costs 3–4 new versioned mint contracts + two CI graphs. |
| **Iced 0.14.0** | **2** | 5 | 4 | **2** | 3 | 3 | **1** | Not viable as specified. |
| **Dioxus 0.7** | 2 | 4 | **2** | 3 | 4 | **2** | 4 | Not viable. |

Deltas from the source evaluations: pure-Rust packaging +1 (cargo-packager signing); Tauri and Iced cross-platform 3→2 (blank-window ladder in current docs; iced's own docs say Wayland file-drop is unimplemented); Iced UI ceiling 3→2 (fails mandatory grid + table); Dioxus UI ceiling 5→3, packaging 3→2; Slint UI ceiling 4→3.

---

## 5. Recommended architecture

### 5.1 Repo shape — workspace with the root staying a real package

```
mint/
  Cargo.toml          # [package] name = "mint"  +  [workspace] members = ["gui"]
  src/                # unchanged: lib + bin, no egui in its graph
  gui/
    Cargo.toml        # [package] name = "mint-gui"
                      # mint = { path = ".." }, eframe, egui_extras, egui_table,
                      # toml_edit = "0.22", rfd, notify
    src/
```

Three deliberate choices:

- **The root manifest stays a package, not a virtual manifest.** `[workspace] members = ["gui"]` goes *into* mint's own `Cargo.toml`. This preserves `release.yml`'s `grep -m1 '^version' Cargo.toml` guard exactly as written and eliminates the release-engineering panel's cross-cutting dealbreaker without touching CI.
- **No `mint-core` / `mint-cli` split.** Dependency direction is one-way (`mint-gui → mint`), so eframe and wgpu never enter the `mint` binary's graph. `main.rs`'s `mint::cli::run()` shim is untouched. A split buys nothing and costs a refactor.
- **`mp3` stays off in GUI builds.** Do not declare `mint = { path = "..", features = ["mp3"] }` in `gui/Cargo.toml`. Add a CI job asserting the default GUI build links no LAME, keeping LGPL-3.0 out of the bundle (see §11 Q4). Edition 2024 implies resolver 3, but `--workspace` still unions features.

### 5.2 GUI ↔ core boundary

**In-process, one binary, no IPC, no DTO layer.** The rules:

1. **The GUI never re-derives.** It calls `resolve_target`, `describe`, `estimated_output_bytes`, `delivery_blocker`, `render_output_path`, `check_planned_outputs`, `probe_source`. It never models the four-layer merge, the FLAC 96 kHz rule, the DSD 5.1 rule, or the own-layer exception.
2. **The GUI never constructs a `ResolvedTarget`.** All 22 fields are `pub` and it *can* — the integration audit calls that "the biggest single opportunity and biggest footgun". Under a maintenance lens it is only a footgun: a hand-built target bypasses `validate_fields` and instantly becomes the fourth parallel code path. `ResolvedTarget` is receive-only, from `resolve_target`.
3. **`json_report.rs` stays the CLI's stable QC contract and is not the GUI's data model.** `ProcessingReport`, `LoudnessAnalysis`, `DsdReport`, `TaskFailure` are all `pub`-field + `Clone` and cross an `mpsc::Sender` as Rust values. Run reports persist to `.mint-gui/runs/<iso8601>.json` in **exactly the `--json` schema** so `jq` still works on them — the GUI *writes* the schema, it does not *depend* on it.
4. **Threading:** own rayon pool (never the global one — `cli.rs:147` already builds a scoped pool), `std::panic::catch_unwind` per job, worker `mpsc::Sender<JobEvent>` → `ctx.request_repaint()`. `catch_unwind` is sound here: no cross-job shared mutable state (guarded by `tests/soxr.rs`), no `panic = "abort"` profile, and `create_soxr`'s `INIT_LOCK` already de-poisons with `.unwrap_or_else(|e| e.into_inner())`.
5. **Worker cap from free RAM, not `available_parallelism()`.** Measured peak is 485–700 MB per job, ~2× under `on_clip = "maximize"` (`loudness::apply_maximize` clones `buffer.channels` at loudness.rs:165). Default to `min(cores, free_ram_gb / 1.5)`, surfaced and overridable, with the reason stated in the UI.
6. **Renderer:** compile **both** `glow` and `wgpu`; default wgpu, expose `--renderer glow` / `MINT_RENDERER=glow`. Studio boxes get run over RDP and headless; a black window is not an acceptable failure. This also saves ~3.6 MB when glow is selected.

### 5.3 Config round-trip

**`mint.toml` IS the project file. The GUI owns no config format.** `Config::from_path` must be able to read the same bytes. Consequences:

- **Writes go through `toml_edit`, surgically.** `toml_edit 0.22.27` is already in `Cargo.lock` transitively via `toml 0.8.23`, so adding `toml_edit = "0.22"` as a direct dep in `gui/` costs **zero new crates** and dedupes. Do not jump to 0.25 — that duplicates the tree unless mint bumps to `toml 1.x`, which is a real bump (integer handling changed in 0.9), not a patch.
- **Never re-serialize the document wholesale.** Touch only the key that changed. Never reorder tables (`Config.targets` is a `BTreeMap` — a serde round-trip would silently alphabetise the user's targets on every save). Never normalise whitespace or quotes. Never expand `preset = "cd"` into 22 explicit fields. Removing a value removes the key; it does not write the default. `examples/mint.toml` is 92 lines of which ~55 are pedagogy, and it is `include_str!`-embedded as the bootstrap template — a Save that eats the comments has eaten the onboarding documentation.
- **Derive `Serialize` on `RawTarget` anyway, for a different job.** Not to write files. To (a) get serde's enum→string spelling for free instead of hand-writing a second spelling table, by serialising one target into a `toml_edit::Table` and splicing only the changed keys, and (b) to snapshot for undo. This requires `#[serde(skip_serializing_if = "Option::is_none")]` on **all 22 fields** — `toml_edit`'s serializer returns `Error::UnsupportedNone` on a single unskipped `None`. Add it to the CLAUDE.md "new target field" checklist, or a new field silently vanishes on save.
- **Inputs live in a sidecar, not in `mint.toml`.** `mint.toml` has no input field; inputs are `argv`, so a "project" (config + input set) is not expressible in the contractual format. Do **not** add `[inputs]` — `deny_unknown_fields` means older mint binaries would hard-error on newer configs. Use `.mint-gui/project.toml` next to the config, holding only things that cannot change the audio: input globs and their expansion cache, selected targets, pane layout, run-history index, last-used `seed`/`jobs`.
- **File-watch `mint.toml` and treat external edits as the normal case**, not a conflict. `notify` crate; on change, re-parse and re-derive with a non-modal toast. If the GUI has unsaved edits, show both; never silently pick.
- **A persistent "Copy as CLI command" button** rendering `mint --config mint.toml --target sacd --target cd --seed 42 --json "masters/*.wav"`. This is what keeps a mixed CLI/GUI workflow reconciled and it is nearly free.
- **Add a round-trip property test** to `tests/`: load `examples/mint.toml`, poke every field type via the writer, assert comments/order/whitespace are byte-identical outside the touched keys.

### 5.4 Progress and cancellation

Two tiers, deliberately sequenced so Phase 1 ships with **zero** changes to `ops/`.

**Tier A — zero core changes (Phase 1).**
- Batch progress = jobs completed / total. A real number.
- Per-job: state + elapsed + predicted bytes.
- **Per-job percentage for the long jobs, for free:** `estimated_output_bytes` is byte-exact for WAV and DSF (deterministic length contracts end to end) and `atomic_write` stages to `.<final name>.<pid>.<n>.mint-tmp` in the output directory. A UI-thread timer `stat()`s that path at 4 Hz and divides. For the DSD512 encode — the only stage that runs for minutes — this is a *true* percentage, not an extrapolation.
- **No fake percentages anywhere else.** A DSD512 encode and a same-rate WAV transcode differ by 100×; a lying bar is worse than none.
- Cancel = "stop launching new jobs, let in-flight ones finish." Honest and achievable today.

**Tier B — the real thing (Phase 3).** One new borrowed control struct, threaded down; `process()` kept as a wrapper so the CLI and every test are untouched:

```rust
// src/progress.rs
pub enum Stage { Probe, Decode, AnalyzeSource, Loudness, Resample,
                 PostSrcVerify, Ceiling, AnalyzeDelivered, Quantize, Encode }

#[derive(Default, Clone, Copy)]
pub struct JobControl<'a> {
    pub cancel:   Option<&'a AtomicBool>,
    pub progress: Option<&'a (dyn Fn(Stage, f64) + Sync)>,
}

pub fn process(i: &Path, t: &ResolvedTarget, seed: Option<u64>) -> Result<ProcessingReport>
    { process_with(i, t, seed, JobControl::default()) }
pub fn process_with(i: &Path, t: &ResolvedTarget, seed: Option<u64>,
                    ctl: JobControl<'_>) -> Result<ProcessingReport>;
```

Why this respects "the buffer is the budget": two pointers. Nothing allocated, nothing sized in frames, no audio copied, and the callback **must never see or copy audio**. The sink is `Sync`, not just `Send`, because jobs run under rayon. With `JobControl::default()` both branches fold out, so **the CLI's codegen is unchanged.**

Hook points, in order of value: `ops::dsd::encode_dsf` (`while pos < frames`, dsd/mod.rs:182, step 64–1024 frames — the one that matters), `ops::resample::apply` (resample.rs:67, 16 384-frame blocks), `ops::limiter::limit_channels` (limiter.rs:439, 4096 — pass the control through *both* `limit_and_verify`'s convergence loop and `apply_maximize`, or progress stalls), `ops::loudness::analyze` (loudness.rs:363, already hops in 100 ms slabs), `io_read::read_audio` (io_read.rs:165, per-packet — decoding a 461 MB master is currently seconds of uninterruptible work), `bitdepth::quantize_in_place` (coarse per-channel).

Stays blind, and that is fine: `measure_lufs`/`measure_true_peak_dbtp` feed the whole buffer in one `add_frames_planar_f64` call; the FLAC/MP3/WAV writers are single blocking calls into encoder crates. Report those as indeterminate.

**The proof it changed nothing is already written:** `ops::limiter` and `ops::resample` each keep the whole-file implementation they replaced in a `#[cfg(test)] mod reference` and assert `to_bits()` equality. Run them. Do not relax them.

Cancellation semantics come out right for free: `ctl.check()?` propagates as an `Err` through the same `?` path, unwinding past `Staged::drop`. **A cancelled job leaves the previous render intact and no partial file.** State that in the API doc — it is what a mastering engineer will assume and would be furious to find absent.

---

## 6. Changes required in the mint core, ranked by invasiveness

### Tier 0 — mechanical, zero risk to shipped audio (do these in the spike)

> **Status 2026-08-14: done**, along with the workspace conversion (§5.1) and the `license`
> / `LICENSE` fixes from §9. Two deltas from the table below: T0-4 shipped as
> `Config::from_toml_str`, not `from_str` (an inherent `from_str` collides with the
> `FromStr` trait and trips `clippy::should_implement_trait`); and `ALL` gained a tripwire,
> `tests/enum_domains.rs`, so a future variant missing from an `ALL` array is a build break
> rather than a silently absent dropdown entry. `DitherCorrelation` got a `tag()` it did not
> have, since the dry-run never rendered it.

| # | Change | Location | Why the GUI needs it |
|---|---|---|---|
| T0-1 | `PartialEq` on `ResampleQuality`, `OnClipPolicy`, `LimiterCharacter`, `DitherMode`, `DitherStrength`, `DitherCorrelation`, `BitDepthFormat`, `OutputCodec` | config.rs:85–225 | egui's `selectable_value` and every toolkit's dropdown require it. Currently blocks the form outright. |
| T0-2 | `pub` on `quality_tag`, `on_clip_tag`, `limiter_character_tag`, `dither_tag`, `dither_strength_tag`, `effective_dither_tag` (+ `Display` impls) | config.rs:1173–1271 | CLI-identical labels. `effective_dither_tag` encodes the s16-only shaping downgrade — reimplementing it is a guaranteed drift bug. |
| T0-3 | `ALL: [T; N]` per enum, mirroring `DsdRate::ALL` | config.rs, dsd/mod.rs:65 | Nothing enumerates variants today except `PRESET_NAMES`. A hardcoded GUI list is the exact staleness `PRESET_NAMES` was introduced to fix. |
| T0-4 | `pub fn Config::from_str(&str) -> Result<Config>` (parse + `validate`) and `pub fn validate` | config.rs:613, 729 | Live validation of an unsaved buffer currently needs a temp file. |
| T0-5 | `pub` on `check_collisions` | config.rs:1130 | Two targets resolving to one output path is otherwise undetectable without reimplementing the probe-stem trick. (`validate_fields` does **not** need to be public — `resolve_target` already calls it, and hand-built targets are not a supported path.) |
| T0-6 | Move `is_supported_input`, `fmt_bytes`, `fmt_db` to a `pub mod util` | cli.rs:509, 363, 376 | The `wav\|wave\|aif\|aiff` list is a correctness surface, not a convenience. |
| T0-7 | `pub const MP3_AVAILABLE: bool = cfg!(feature = "mp3")` | lib.rs | Today an MP3 target resolves, validates, passes `delivery_blocker`, runs the *entire* chain, and fails at io_write.rs:438. The GUI must grey it out. |

### Tier 1 — small, additive, low risk

| # | Change | Location | Note |
|---|---|---|---|
| T1-1 | `"schema_version"` in the JSON document | json_report.rs:13–18 | Highest value-per-line in the dossier. The GUI writes run history in this schema; unversioned history is unreadable in two years. |
| T1-2 | Report the figures already computed and discarded: `gain_db` (`LoudnessApplyResult.gain_db`, loudness.rs:30, computed then thrown away — report.md §9 item 6), dither mode/strength/seed, SRC quality tier, output `file_bytes`, channels, frames, elapsed | pipeline.rs:15–53 → json_report.rs → cli.rs:380 | Touches the 3-place report checklist. `dither: {applied, noise_shaping}` currently cannot distinguish `tpdf` from `adaptive`-degraded-to-tpdf; reproducibility is unreportable. |
| T1-3 | `pub fn output_conflict(input, output, overwrite) -> Option<Conflict>` wrapping `guard_output_path` | pipeline.rs:102 | Must stay a *query*. CLAUDE.md is explicit the policy must not move into a writer. The `(dev, ino)` hard-link comparison in `same_file` is the subtle part the GUI must not clone. |

### Tier 2 — the two that make it a desktop app rather than a wrapper

| # | Change | Effort | Note |
|---|---|---|---|
| **T2-1** | `pub fn Config::resolve_target_explained(&self, name) -> Result<(ResolvedTarget, FieldOrigins)>`, where `FieldOrigins` records `(Layer, Fate)` per field. `Layer` = Builtin \| Defaults \| Preset(&str) \| Explicit. `Fate` = Applies \| Inert \| Dropped \| WouldError. Computed **inside** `overlay`/`resolve_target`, never re-derived. | ~1 day | **The highest-value screen in the whole design and it is impossible in every stack today** — the design panel's levelling correction. `overlay()`, `builtin_fallback()`, `preset()` are private and `resolve_target` discards provenance. The `Fate` axis is the one nobody expects and it is invisible in `--dry-run`, `--json` and every error message: `[defaults] rate = 48000` is *dropped* on a DSD target (config.rs:698) while an explicit `format` on the same target is a hard *error*. Belongs in the library regardless — it also gives the CLI `mint --explain <target>`. |
| **T2-2** | `JobControl` + `process_with`, threaded to the six loops in §5.4 | ~1 week incl. review | Touches DSP call signatures, not DSP math. Proof of no change: the `#[cfg(test)] mod reference` bit-equality tests in `ops::limiter` and `ops::resample`. |

### Tier 3 — defer past v1, listed so they are not forgotten

`ChainStep` + `chain()` with `describe()` reimplemented as its renderer (~100–140 lines, no DSP touched — but ~15 assertions in `tests/config_parse.rs` and `tests/dsd_pipeline.rs` read the strings byte-for-byte, and it must be **one** producer plus a formatter, never a parallel third representation); structured `WarningKind`/`FailureKind` alongside the prose; `process_buffer` so the GUI can hand over already-decoded audio; chunking the `measure_lufs` feed. **Not needed at all in-process:** `--dry-run --json` and NDJSON task events. Those exist only to serve a sidecar, and choosing egui deletes them from the roadmap.

---

## 7. What the GUI must not do

Carried verbatim from the UX analysis because these are the failure modes a well-meaning implementation walks into:

1. **No per-step DSP toggles.** Derivation *is* the product. The moment a user can force a step, `describe()`, `estimated_output_bytes()` and the pipeline stop being three views of one truth.
2. **Never write resolved values back.** `preset = "cd"` must never become 22 explicit fields.
3. **Never reformat the user's TOML.**
4. **No wizard that hides derivation.** The chain must be visible *while* choosing.
5. **No GUI-only state that changes the audio, and no GUI-only presets.** A new preset goes in `preset()` + `PRESET_NAMES` + README, same checklist as always. `--force`/`--seed`/`--jobs` are legitimately run-scoped but must appear in the copyable CLI line.
6. **No album-normalization toggle.** mint normalizes per track (report.md §7.3). A checkbox promising otherwise is a lie until the library does it.
7. **No silent auto-fix of output collisions.** Offer the *same two* fixes mint's error prose names, as buttons. Do not invent a third by appending `_1`.
8. **No progress bar that lies, and no ETA.**
9. **No collapsing warnings into a green checkmark.** "Heavy limiting: 4.2 dB", "loudness target not met", "sigma-delta overload on 118 samples" *are* the product.
10. **No audio playback, waveform-scrub, or live meters in v1.** Scope creep into a DAW, and "the buffer is the budget" forbids the second copy a preview engine wants. The 1:512 envelope (§2) is the exception because it is 0.4 %.
11. **Present the presets with the README's own "starting points, not gospel" caveat.** report.md §7 argues the streaming presets encode a mistaken model (no platform asks for 16-bit or a pre-converted rate; published LUFS figures are playback ceilings, not delivery targets). A GUI that makes a wrong preset look authoritative is worse than a CLI that makes you type it. Show each preset's fields inline in the picker, as a diff against current resolved values.
12. **Surface `[defaults] preset = "..."` as the bug it is.** It parses (it is a `RawTarget` field) but is never applied — `resolve_target` reads `raw.preset`, not `merged.preset` (report.md defect #14). Render it struck through with "ignored — parsed but never applied". Do not reproduce the bug.

---

## 8. Phased delivery

### Phase 0 — de-risking spike (1 week, throwaway code, `gui/` branch)

Not a prototype of the app. A list of yes/no answers. **Do not proceed to Phase 1 until all seven are green.**

| # | Must prove | Fail signal |
|---|---|---|
| S1 | `rustup update` to ≥1.95, workspace with root-as-package builds on macOS + a Linux box + a Windows box; `release.yml`'s `grep -m1 '^version' Cargo.toml` still resolves | MSRV or workspace surprise |
| S2 | Tier 0 changes applied; a live-validating form of **five** representative fields (`lufs` f64, `format` enum, `codec` enum, `dir` path, `preset` picker) calling `resolve_target` per frame, rendering mint's own `anyhow` error text | Any need to reimplement a rule in the GUI |
| S3 | `TextEdit::layouter` + a cached TOML highlighter + an error squiggle at `toml::de::Error::span()` on the real `examples/mint.toml` | The config drawer is a from-scratch text engine → reconsider stack |
| S4 | `egui_table` renders a 20×8 plan matrix with per-cell status glyph, output filename and `SizeEstimate` qualifier, scrolling at 60 fps | Table story is worse than advertised |
| S5 | A real render of a five-minute 96 kHz master to DSD256 on a worker thread, with the staging-file `stat()` progress trick showing a monotonic true percentage, UI responsive throughout | The Tier-A progress mechanism does not work → Tier B moves into Phase 1 |
| S6 | `toml_edit` write-back of three fields into `examples/mint.toml`, diffed: comments, order, alignment, untouched keys byte-identical | Round-trip is lossy → redesign the writer |
| S7 | Both `glow` and `wgpu` renderers launch; **wgpu tested over RDP or in a VM with no GPU** and either works or falls back cleanly | Black-window class on real studio hardware |

Deliberately **not** in the spike: theming, signing, provenance. Those are known-cost, not known-risk.

### Phase 1 — read-only plan-and-evidence workbench (2 weeks)

Sources pane (`probe_source` per file, cached by path+mtime), Targets pane, Plan matrix, live Chain preview from `describe()`, Problems panel (the compiler-style list that fixes the `bail!`-on-first-error ergonomics — the single best value-per-effort item in the design), Run view with Tier-A progress and job-boundary cancel, Report view with the verdict column mint already computes (`LUFS_TOLERANCE`, pipeline.rs:316) and buries in a warning string. Persist runs to `.mint-gui/runs/*.json`. "Copy as CLI command". **Core changes: Tier 0 only.** Ships useful without a single line of DSP touched.

### Phase 2 — provenance + editing (2 weeks)

T2-1 (`resolve_target_explained`) plus the Target Inspector with origin badges (`B`/`D`/`P`/`E`) and fate glyphs (`·` applies / `~` inert / `/` dropped / `!` would error), the expandable "why is `rate = 88200 Hz`?" panel, the preset picker as a resolved-value diff, and `toml_edit` write-back with the round-trip property test. Add file-watch + hot-reload. **This is the phase that makes the GUI something a TOML-fluent engineer opens daily.**

### Phase 3 — real progress and cancellation (1.5 weeks)

T2-2. Run the `#[cfg(test)] mod reference` bit-equality suites before and after; run the DSD, dither and limiter verification harnesses with `--nocapture` and compare the printed dB figures against the numbers in the file headers. T1-1 and T1-2 land here too (the report gains `gain_db`, elapsed, `file_bytes`, dither mode/seed).

### Phase 4 — packaging, signing, notarization (1 week + certificate lead time)

§9. Start Apple Developer enrolment **now**, in parallel with Phase 0 — it has latency and nothing else unblocks a macOS GUI.

### Phase 5 — the theming pass (3–6 weeks of designer-developer time, honestly)

Bundled `tnum`-frozen font; custom `FloatField` (drag + suffix + range), status badges, table headers, a painted GR/LUFS bar; a real `Visuals` palette for dark and light. **This is the estimate the design panel corrected upward from egui's own "3–5 days", and it needs someone with taste, not just someone with Rust.** There is no hot reload; a `cargo run` is 3–6 s warm and restarts the app. If nobody on the team wants this job, that is the strongest single signal to reopen the Tauri question (§11 Q2).

**Total to a defensible, signed v1: ~8 weeks of engineering plus the theming pass.**

---

## 9. Packaging and CI plan

### Cross-cutting rules

- **Bundle verification happens on CI artefacts in clean VMs, never on a dev box.** The Dioxus report's `otool -L` finding proves dev builds link `/opt/homebrew/opt/libsoxr/lib/libsoxr.0.dylib` dynamically while CI links statically — so a bundle that launches on your laptop proves nothing about the artefact. Add a `smoke` job per OS that downloads the release artefact, launches it headless (`--version` on the CLI, a `--selftest` flag or `egui_kittest` headless frame on the GUI) on a runner image *with no Homebrew, no vcpkg, no dev packages*.
- **`--features mp3` stays off.** `release.yml` does not pass it today, so LGPL-3.0 LAME exposure is currently zero. Keep it that way; per the packaging audit, `mp3lame-sys`'s build script selects its path with host `#[cfg(unix)]`/`#[cfg(windows)]`, so it cannot be cross-compiled to MSVC from a Unix host at all.
- **Linux build-time additions:** `libxkbcommon-dev`, `libwayland-dev`, `libx11-dev`/`libxcb` headers for winit. No *runtime* additions — that is the whole point of not choosing a webview.

### Per OS

| OS | Artefact | Tooling | Specifics |
|---|---|---|---|
| **macOS** | `.app` in a `.dmg`, universal | `cargo-packager` (`MacOsConfig.signing_identity` + `entitlements` + `notarization_credentials` + `provider_short_name` — verified present) | **Add a `lipo` merge job.** Today `x86_64-apple-darwin` builds on `macos-15-intel` and `aarch64` on `macos-latest` as separate jobs with no merge step; no universal producer exists for any stack. `macos-15-intel` is the last x86_64 image (GitHub ends x86_64 macOS Aug 2027) — plan to cross-compile both slices on arm64. Hardened runtime is mandatory for notarization; `codesign --deep` is deprecated, sign inner binaries first. Sign **after** `lipo`. `Info.plist`: `NSHighResolutionCapable`, bundle identifier, `LSApplicationCategoryType`, `CFBundleDocumentTypes` for WAV/AIFF drops on the Dock icon, and the TCC usage strings (`NSDesktopFolderUsageDescription` etc.) — engineers point this at arbitrary folders. **Do not put `notarytool --wait` on a critical timed path**; there are Feb-2026 reports of 16-hour "In Progress" stalls while the service reported operational. Make releases resumable. |
| **Windows** | `.msi` (WiX) or NSIS | `cargo-packager` (`WindowsConfig.certificate_thumbprint`, `timestamp_url`, `sign_command`) | `#![windows_subsystem = "windows"]` or every launch flashes a console. **EV buys nothing since Aug 2024** — Microsoft no longer distinguishes OV from EV for SmartScreen, so EV's instant-reputation bypass is gone. Azure Artifact Signing at $9.99/mo is the cheapest credible route, but **individual sign-up is US/Canada only**, so this needs the ista entity (§11 Q5). Budget for a SmartScreen reputation grind on the first release regardless of cert class. **Gap: `aarch64-pc-windows-msvc` is missing from the five-target matrix** — Windows-on-ARM users get x64 under emulation for a CPU-bound DSP core, or nothing. Adding it needs a vcpkg ARM64 static triplet. |
| **Linux** | AppImage (primary) + `.deb` | `cargo-packager` | **Live defect today, independent of the GUI: the glibc floor.** Builds run on `ubuntu-latest` / `ubuntu-24.04-arm` = glibc 2.39, so current release binaries do **not** start on Debian 12 (2.36), RHEL/Rocky 9 (2.34) or Ubuntu 22.04 (2.35). Static soxr bought portability from the package manager, not from libc. Fix with `cargo-zigbuild` (`--target x86_64-unknown-linux-gnu.2.28`) or an older build container. With static soxr and no webview there is genuinely no runtime dependency to declare beyond glibc. |

### Compliance items to fix before the GUI, not after

1. **LGPL-2.1.** `release.yml:149,157` packages only `README.md` and `LICENSE` (Apache-2.0), while every published binary statically links LGPL-2.1 libsoxr. §6 of LGPL-2.1 wants the licence text, the copyright notice, and a relinking means (object files or a written offer). Add a `THIRD-PARTY-LICENSES` file to the archives and the bundle, and point at the tagged public CI build that reproduces it. A signed single-file GUI bundle makes this harder to discharge, not easier.
2. **`Cargo.toml` has no `license` field** (verified) and `LICENSE` still carries the literal `Copyright [yyyy] [name of copyright owner]` placeholder (line 189). Fill both in the same commit as the workspace conversion.
3. **Mac App Store is already closed** — LGPL §6 relinking vs App Store terms is the classic VLC conflict. Direct Developer ID distribution is unaffected and is the right channel for this audience. This is *not* an additional Slint cost; it predates the GUI.

---

## 10. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| **The theming pass never happens and the app looks like a debug menu** — the design panel's core objection, and the single most likely way this proposal disappoints | Medium-high | High (this is what "elegant" means in the brief) | Budget 3–6 weeks with someone who has taste, not 3–5 days. Bundle a `tnum`-frozen font as a build artefact on day one. Gate v1 release on a design review, not on feature completeness. If nobody wants this job → reopen Q2. |
| **egui's release treadmill** (five breaking minors in ten months: 0.34 renamed `App::update`→`App::ui`; 0.35 deleted deprecated items; 0.36 moved `Modifiers` out of `RawInput`) | Certain | Low-medium | Half a day per minor, and **do not fall three releases behind** or the migrations compound. Freezing on a version is legal and cheap (MIT/Apache-2.0, no runtime engine to go stale). |
| **Bus factor:** emilk has ~3 018 commits, the next contributor 189; development funded by Rerun, a VC-backed startup | Low-medium | Medium | Forking a GUI toolkit is not a plan, but freezing on a working version *is* — unlike a webview stack, a frozen egui keeps working because no engine on the user's machine changes underneath it. |
| **In-process OOM under parallel jobs** (485–700 MB/job, ~2× under `maximize`) | Medium if `available_parallelism()` is inherited | Medium (crash, no data loss — `mint.toml` is on disk) | Cap workers from free RAM (§5.2), surface the number and the reason, `catch_unwind` per job. Not a reason for a sidecar; the cap is one integer that every architecture needs. |
| **wgpu black window on an old studio box, a VM, or RDP** | Medium | High if unmitigated | Compile both backends, ship `--renderer glow` / `MINT_RENDERER`, and prove it in spike S7. |
| **No native macOS menu bar** (egui draws menus in-window; no `NSMenu`) — the loudest "not a real app" tell on the platform most of this audience uses | Certain | Medium | Add `muda` for the real `NSMenu` (Cmd+Q, Cmd+,, Services), accepting a winit-version coupling. Decide in Phase 5. |
| **`toml_edit` write-back loses comments or reorders targets** | Low with discipline, catastrophic in perception | High (reads as data loss) | Round-trip property test in `tests/` from Phase 2; enforce "touch only the changed key" in review; never re-serialize `DocumentMut` wholesale. |
| **T2-2 changes DSP output** | Low | Very high | The `#[cfg(test)] mod reference` bit-equality suites in `ops::limiter` and `ops::resample` exist exactly for this. Run the DSD/dither/limiter verification harnesses with `--nocapture` and diff against the header numbers. Do not relax any comparison to a tolerance. |
| **Signing/notarization is a recurring cost and an external dependency in the release path** ($99/yr Apple + ~$120/yr Windows via ista's Azure tenant) | Certain | Medium | Start enrolment in Phase 0. Make releases resumable; never block a timed release on `notarytool --wait`. |
| **libsoxr's bindings are abandoned** (`libsoxr-sys` 0.1.4 and `libsoxr-rs` last committed 2023-04-12; the `soxr` crate depends on the same sys crate) | Certain (already true) | Medium, and **orthogonal to the GUI** | See §11 Q6. Not a GUI decision; do not let the GUI project own it. |
| **`vhq` is already not bit-reproducible across platforms** (macOS arm64 → `cr64`, Debian amd64 → `cr64s`, LiveKit's vendored crate → `cr32`), contradicting `README.md:571` | Certain (already true) | Medium | Add a test pinning `soxr_engine()` per quality tier — a one-line FFI call that would have caught the LiveKit downgrade and catches the platform divergence. Independent of the GUI, but a GUI makes the claim more visible to users. |
| **A workspace conversion silently defeats the release version guard** | Would be certain with a virtual manifest | High | Eliminated by design: root stays the `mint` package (§5.1). Verify in spike S1 anyway. |

---

## 11. Open questions — yours to answer, not mine

**Q1 — Who is this for?** A private tool for you, a thing you hand to a handful of mastering engineers, or an ista-branded distributed product? This changes the signing spend, whether accessibility is a requirement (which would eliminate Iced outright and matter for egui's AccessKit-via-eframe story), and whether the Phase 5 theming budget is justified at all. **Everything below inherits from this answer.**

**Q2 — Will a designer be involved, and is a distinctive visual identity a differentiator or a nice-to-have?** If yes-and-differentiator, and if Linux can be tier-2, the Tauri switch condition in §1 fires and this proposal's primary recommendation changes. egui excludes designers *completely* — every visual decision is a Rust literal in a Rust file, with no styling file, no Figma path and no hot reload. That is a permanent constraint, not a temporary gap.

**Q3 — Is Slint's licence acceptable?** Only relevant if you want to reopen the third-place option. The choices are: GPL-3.0-only (free, relicenses the shipped GUI binary as copyleft — legal since Apache-2.0 is one-way GPLv3-compatible, and the Mac App Store is already closed to you anyway, but Debian/Fedora/nixpkgs would need to rebuild); LicenseRef-Slint-Royalty-free-2.0 (free, permits closed desktop distribution, non-OSI, so distros cannot ship it and the commitment rests on a company CLA); or paid. This is a legal/strategy call, and it needs the `Cargo.toml` `license` field and the `LICENSE` copyright placeholder filled in regardless.

**Q4 — Should the GUI expose MP3?** Enabling it puts LGPL-3.0 libmp3lame into the bundle alongside LGPL-2.1 soxr — two differently-worded copyleft regimes in one signed blob — and adds a C build to five release legs that cannot cross-compile to MSVC from Unix. My recommendation is off-by-default until someone asks, consistent with the existing AAC exclusion (README:326). But it is a product call.

**Q5 — Windows signing: ista entity or personal?** Azure Artifact Signing's individual path is US/Canada only, so the German individual route is closed. Options are (a) sign under the ista legal entity via Azure Artifact Signing (~$120/yr, needs internal sponsorship of an Azure subscription, easily clears the 3-year incorporation bar), (b) a conventional OV certificate at ~$200–400/yr, or (c) ship Windows unsigned initially and accept SmartScreen friction while macOS is signed properly. Option (a) has organisational lead time you should start now if you want it.

**Q6 — Must `vhq` be bit-reproducible across platforms, and do you want to vendor soxr?** Today it is not: `cr64` on macOS arm64, `cr64s` (AVX) on Debian amd64. Vendoring LiveKit's `soxr-sys` 0.1.3 fixes reproducibility *but silently downgrades every quality tier to the single-precision `cr32` engine* unless you fork it to add `cr64.c` and flip `WITH_CR64`/`WITH_HI_PREC_CLOCK` — measured deviation from the system library at VHQ was −127.2 dBFS unmodified versus 1 ULP of f64 with `cr64` enabled. This is a DSP-policy decision about what `README.md:571` promises, and it is **entirely independent of the GUI**. Do not let the GUI project absorb it; resolve it separately, and add the `soxr_engine()` pinning test either way.

**Q7 — Should the GUI surface report.md §7's critique of the streaming presets?** The README already prints a "starting points, not gospel" caveat. A GUI dropdown makes a preset look authoritative in a way a CLI flag does not. My recommendation (§7 item 11) is to show each preset's fields inline and carry the caveat, but whether to go further — deprecating or renaming `spotify`/`apple-music` — is a product decision about the tool's opinions, not a UI one.

---

## 12. If only three things get built

**T2-1 (`resolve_target_explained`)**, **T2-2 (`JobControl`)**, and the **Problems panel**.

The first is the one screen no CLI can ever produce and the one the four-layer merge most needs — and it is required regardless of which toolkit wins. The second is the difference between a desktop application and a wrapper around a frozen window. The third turns `bail!`-on-first-error into a compiler-style list and costs almost nothing, because every piece of machinery it needs (`probe_source`, `render_output_path`, `estimated_output_bytes`, `delivery_blocker`, `check_planned_outputs`) is already public, already cheap, and already side-effect-free.

---

# Part II — Candidate evaluations

Six stacks, each researched independently against the same brief and the same seven criteria. Scores below are each agent's own self-assessment; Part I §3.3 and Part VI record where a judge panel overturned one. The verdicts here are therefore *pre-adjudication* — Part I §4 has the corrected table, and it is the one that decides.

| Stack | Self-assessed verdict | Cross-platform parity | Native-dep integration | Packaging & signing | UI ceiling | Dev velocity | Longevity | Accessibility |
|---|---|:--:|:--:|:--:|:--:|:--:|:--:|:--:|
| **Tauri 2** | viable-with-caveats | 3 | 5 | 4 | 5 | 4 | 4 | 4 |
| **egui / eframe** | recommend | 4 | 5 | 4 | 3 | 4 | 3 | 3 |
| **Iced** | viable-with-caveats | 3 | 5 | 4 | 3 | 3 | 3 | 1 |
| **Slint** | recommend | 4 | 5 | 3 | 4 | 4 | 4 | 4 |
| **Dioxus** | viable-with-caveats | 3 | 4 | 3 | 5 | 4 | 2 | 4 |
| **Foreign UI + mint-as-sidecar** | viable-with-caveats | 4 | 5 | 3 | 5 | 3 | 3 | 4 |

## Tauri 2

**Full stack as evaluated:** Tauri 2.x (Rust backend, system webview frontend) with a SvelteKit/Solid/React UI, linking `mint` as an in-process path dependency

**Version researched:** `tauri` 2.11.5, published 2026-07-01 (crates.io max_stable_version; matches tauri.app/release/core). `tauri-cli` 2.11.4 (2026-06-28). Line history: 2.0.0 stable 2024-10-02; 2.10.0 on 2026-02-03 bumped to wry 0.54 + webkit2gtk-rs 2.0.2 (only breaking change scoped to `with_webview` users) and added updater support for Deb/RPM/AppImage/NSIS/MSI; 2.10.1 Feb/Apr 2026; 2.11.x patches May-Jul 2026. Repo tauri-apps/tauri: 110,168 stars, last push 2026-08-13 (today), 1,434 open issues, Apache-2.0 (crates dual MIT/Apache-2.0). `tauri-plugin-dialog` gained the `xdg-portal` feature in 2.5.0.

**Self-assessed verdict:** `viable-with-caveats`

Tauri is the only web-UI option where the backend *is* Rust, so mint links in as a crate: one process, one binary, zero serialization of audio, and no second copy of `AudioBuffer` — which matters because CLAUDE.md declares a second copy a bug and the measured peak is already 485-700 MB per job. The existing release.yml (static libsoxr 0.1.3 from the Debian orig tarball, hand-written soxr.pc on Windows/vcpkg) carries over verbatim, because `tauri build` just shells out to `cargo build`; PKG_CONFIG_PATH is inherited. For mint's actual UI — ~22 optional fields across a dozen targets, a 4-layer merge that needs tri-state "inherited vs preset vs explicit" display, a virtualized batch table, LUFS/GR meters — HTML/CSS is genuinely the best tool available, and native signing/notarization/installers come free from the bundler. The disqualifying risk is Linux: WebKitGTK is a third engine you neither control nor can pin (2.36 on Ubuntu 22.04 through 2.52.3 on Ubuntu 26.04, released 2026-04-23), and its NVIDIA/Wayland failure mode is a *blank window*, still documented by Tauri itself in Aug 2026 behind a four-step env-var ladder. Everything hard about the integration — progress and cancellation — is a mint change, not a Tauri one; Tauri supplies the transport (`ipc::Channel`) and contributes nothing to threading a `Sync` progress sink through `pipeline.rs`.

| Criterion | Score |
|---|:--:|
| Cross-platform parity | 3 / 5 |
| Native-dep integration | 5 / 5 |
| Packaging & signing | 4 / 5 |
| UI ceiling | 5 / 5 |
| Dev velocity | 4 / 5 |
| Longevity | 4 / 5 |
| Accessibility | 4 / 5 |

### Integration with the mint core

**In-process crate, not a sidecar.** Add `src-tauri/` with `mint = { path = ".." }`. Tauri's backend is a normal Rust binary, so `pipeline::process` is a direct function call — no IPC for audio, no marshalling, no second buffer. This is the single structural reason to prefer Tauri over Electron/Flutter/anything with a process boundary.

**Threading — one real footgun.** Tauri 2 runs *synchronous* `#[tauri::command]` functions **on the main thread**. A 5-minute 96 kHz master through the DSD256 path there would hard-freeze the window. The correct shape: `async fn` command (spawned on Tauri's async runtime) that immediately hands off to a `rayon::ThreadPool` held in `tauri::State`, awaiting a `tokio::sync::oneshot`. Do *not* call `par_iter` from inside a tokio worker. `ResolvedTarget` and `ProcessingReport` are plain data (`Debug + Clone`, no `Rc`/`RefCell`), so both are `Send` and move into the worker cleanly. `ops::resample`'s process-wide `INIT_LOCK` (src/ops/resample.rs:115) already serializes `Soxr::create` and is agnostic to who owns the pool.

**The library is unusually GUI-ready.** Zero `println!`/`eprintln!`/`log::` calls outside `cli.rs` and `main.rs` — verified by grep. Nothing in the DSP writes to stdout or exits the process. `audio::atomic_write` (staging sibling + rename) means a killed or crashed job never leaves a torn file on the final path, which is exactly the semantics a cancel button needs.

**Getting `ProcessingReport` to the UI — solved today, free.** `ProcessingReport` is *not* `Serialize`; only the `json_report.rs` DTOs are, and they are borrow-based (`&'a str`). The zero-change answer is to call `json_report::render(config_name, &reports, &failures)` in the command and send the resulting JSON string over the channel — the GUI then consumes the *same stable schema* `--json` already guarantees for QC automation. Adding `Serialize` to `ProcessingReport`/`DsdReport`/`LoudnessAnalysis` would duplicate the DTO layer's stated purpose; don't.

**Progress — the actual work item, and it is mint's, not Tauri's.** `tauri::ipc::Channel<T>` is the right primitive (ordered, passed as a command arg, explicitly documented as the high-throughput alternative to events, which serialize as JSON strings and can reorder with async listeners). But mint has no hooks. The insertion points already exist as block loops: `ops::resample::apply` (`while pos < in_frames`, BLOCK_FRAMES = 16_384), `ops::limiter::limit_channels` (BLOCK_FRAMES = 4096), `ops::dsd::encode_dsf` (`while pos < frames`, src/ops/dsd/mod.rs:182), plus the coarse stage boundaries in `pipeline::process`. Realistic API: a `process_with_progress(input, target, seed, sink: &(dyn Fn(Stage, f32) + Sync))` that `process()` delegates to with a no-op sink. The sink must be `Sync`, not just `Send`, because jobs run concurrently under rayon.

**Cancellation — nothing exists.** Cheapest honest version is an `&AtomicBool` checked at the same block boundaries; `atomic_write` makes the abort clean by construction. Tauri neither helps nor hinders.

**What mint would have to change (all stack-agnostic, all small):**
1. `Config::from_path` is the *only* public loader and there is no `Config::from_str`; `validate()` (config.rs:729) is private. Live keystroke validation of an in-memory buffer therefore requires either a temp file or a new `pub fn from_str`. Adding it is ~5 lines and buys the GUI mint's own error text for free — a large win, since the GUI then never reimplements the own-layer `format`/`rate`/`bwf` rules.
2. `RawTarget`/`Config` are `Deserialize`-only. Write-back needs `Serialize` on 2 structs + ~8 enums, and then `toml::to_string` **destroys the comments** in `examples/mint.toml` — which is `include_str!`-embedded as the bootstrap template. Use `toml_edit` for surgical edits instead.
3. The batch orchestration in `cli.rs` — `probe_inputs`, `plan_outputs`, `print_dry_run`, `resolve_inputs` — is all private. The GUI either re-implements ~150 lines against the public API or those get `pub`. `config::check_planned_outputs` and `ResolvedTarget::{describe, estimated_output_bytes, render_output_path, delivery_blocker}` are already `pub`, so the dry-run panel is essentially free.

**Binary payloads.** For a future waveform overview, `tauri::ipc::Response` returns `Vec<u8>` straight to an `ArrayBuffer` with no JSON — downsample peak min/max pairs in Rust and never hand JS the f64 buffer, or you have just reintroduced the second copy CLAUDE.md forbids.

**Adopt `tauri-specta` on day one** to generate TS types from the command signatures, or the IPC boundary is stringly-typed and rots.

### Packaging & distribution

**Shipped artifact: one app, DSP inside it.** macOS `.app`/`.dmg`, Windows `-setup.exe` (NSIS) or `.msi` (WiX), Linux `.deb`/`.rpm`/`.AppImage`. Because the Tauri backend is Rust, `pipeline::process` compiles into the app binary — there is no separate DSP process, no sidecar, no second engine. If you *also* want to ship the `mint` CLI (and you should — the `--json` QC path is a documented feature), `bundle.externalBin` bundles the existing binary target-triple-suffixed into the same installer, giving an honest "one install, app + CLI" story. Caveats: sidecar path resolution is under-documented and NSIS reportedly does not replace the sidecar on reinstall (tauri#15134).

**libsoxr: already solved, unchanged.** release.yml builds soxr 0.1.3 statically (Debian orig tarball, `-DBUILD_SHARED_LIBS=OFF`, `-DCMAKE_POLICY_VERSION_MINIMUM=3.5`) on Linux/macOS and uses vcpkg's `x64-windows-static-md` triplet with a hand-written `soxr.pc` on Windows. `tauri build` invokes `cargo build`, so `PKG_CONFIG_PATH` from the workflow env carries through with zero modification. Static means nothing to bundle and no `install_name_tool` dance. (If you ever went dynamic instead, `bundle.macOS.frameworks` does accept `.dylib` paths and rewrites them to `@rpath` — support added in bundler 1.2.0, 2022-12-31 — but static is strictly better and already built.)

**Signing/notarization: first-class, and better than what mint has today.** The bundler signs and notarizes macOS natively via `APPLE_CERTIFICATE` + `APPLE_CERTIFICATE_PASSWORD`, and either `APPLE_API_ISSUER`/`APPLE_API_KEY`/`APPLE_API_KEY_PATH` or `APPLE_ID`/`APPLE_PASSWORD`/`APPLE_TEAM_ID`, including stapling with a `--skip-stapling` escape hatch. Windows Authenticode via a `signCommand`. **New recurring cost:** mint currently ships unsigned tarballs, which is acceptable for a CLI. A GUI that a mastering engineer double-clicks needs an Apple Developer ID ($99/yr) and a Windows OV/EV cert or Azure Trusted Signing (~$200-400/yr) or every user meets Gatekeeper and SmartScreen.

**CI cross-build: feasible, and the matrix already exists.** Tauri cannot cross-compile macOS bundles or Linux packages; you need a runner per OS. release.yml already has all five (ubuntu-latest, ubuntu-24.04-arm, macos-15-intel, macos-latest, windows-latest). Two additions: a Node/pnpm setup + frontend build step (the repo has *zero* JS today), and `cargo tauri build`. One real gap: **linuxdeploy cannot cross-compile ARM AppImages**, so the aarch64 Linux AppImage must be built on the arm runner — which you have.

**Sizes (honest, mint-specific).** `target/release/mint` measured today: **5.2 MB** on arm64 macOS. Add roughly 3-6 MB for the Tauri shell (tauri, wry, tao, tokio, serde, plugins) plus the frontend bundle. Expect ~10-14 MB `.app`, ~5-8 MB compressed DMG. Linux `.deb` ~10-12 MB but **depends on system `libwebkit2gtk-4.1-0`**; the `.AppImage` jumps to **70+ MB** because it drags webkit2gtk in. You must pick: small-and-fragile (.deb/.rpm, breaks on any distro without 4.1) or fat-and-portable (AppImage). Neither is good.

**Windows WebView2 decision matrix:** `downloadBootstrapper` (default, 0 MB, needs internet at install), `embedBootstrapper` (+1.8 MB), `offlineInstaller` (+~127 MB), `fixedVersion` (+~180 MB, self-managed), `skip` (0 MB, not recommended). WebView2 ships preinstalled on Win11 and Win10 from April 2018, so `downloadBootstrapper` or `embedBootstrapper` is right — but for an offline studio machine on a locked-down network, `fixedVersion` at 180 MB is the only correct answer and it also eliminates the "Chromium updated under us" class of bug. NSIS defaults to per-user install into `%LOCALAPPDATA%` (no admin), which is right for this audience.

**Updater** plugin supports all bundle types since 2.10.0 (Feb 2026).

### UI capability ceiling

**Dense forms: best-in-class, and this is 90% of mint's UI.** ~22 optional fields per target, ~12 targets in the example config, and a 4-layer merge that needs three visual states per field (inherited from `[defaults]`, set by preset, set explicitly) plus the own-layer exception for `format`/`rate`/`bwf`. Ghosted inherited values, an "overridden" badge, and an inline "this preset sets X, Y, Z" diff are trivial in CSS and genuinely painful in an immediate-mode toolkit. Live validation against `config::validate` on every keystroke (once `from_str` exists) is cheap — no audio is touched — and gives the GUI mint's own error prose for free.

**Tables:** virtualized (input × target) job tables of 500+ rows via TanStack Virtual or equivalent, with the dry-run's `describe()` chain and `estimated_output_bytes()` inline. Nothing else on the list does this as well.

**Live meters/progress:** LUFS/true-peak/GR bars at 10-30 Hz over an `ipc::Channel` is nothing — the docs' throughput warnings are about events (JSON strings, can reorder with async listeners), and channels exist precisely for this. 8 concurrent jobs × 30 msg/s is well inside budget.

**Future waveform/spectrum: this is where the Linux ceiling drops.** Canvas2D over Rust-downsampled peak min/max pairs shipped as raw bytes (`ipc::Response` → `ArrayBuffer`) is fine on all three platforms and is also the only design that respects the one-copy budget. WebGL/WebGPU spectrograms are where WebKitGTK bites: it masks `WEBGL_debug_renderer_info` for fingerprinting protection, so a WebGL2 context can succeed while silently backed by a software rasterizer, and **you cannot detect it** — your spectrogram runs at 5 fps on a tester's machine and the browser reports nothing wrong. Plan Canvas2D as the floor, treat WebGL as a Win/Mac-only enhancement.

**HiDPI:** genuinely excellent on all three, handled by the webview. One of the clearest advantages over pixel-scaling native toolkits.

**Theming:** CSS. Unmatched — dark/light, system accent, `font-variant-numeric: tabular-nums` for the dB/LUFS readouts, real typography. A mastering tool that looks like a 2026 web app is arguably more appropriate than one that looks like a 2010 Qt app.

**Native file dialogs:** `tauri-plugin-dialog` (rfd-backed) → NSOpenPanel on macOS, IFileDialog on Windows — genuinely native. Linux default is a **GTK3** chooser, which looks foreign under KDE/Plasma; since 2.5.0 there is an `xdg-portal` feature routing through xdg-desktop-portal, but it requires zenity *and* a portal backend installed on the user's machine. That's the parity crack and it's small but visible.

**Drag-and-drop:** Tauri's native `onDragDropEvent` delivers **real filesystem paths**, which is exactly what mint wants (it takes globs and paths, not bytes) — better than a browser, where you'd only get opaque File handles. Caveat: `dragDropEnabled` defaults to true and **disables DOM drag events entirely**; they are mutually exclusive. So intra-app drag-reordering of targets must be built on pointer events, or you set `dragDropEnabled: false` and lose OS file drops. Documented as confusingly named (tauri#14373).

**Accessibility:** semantic HTML + ARIA maps well to AX (VoiceOver/WKWebView) and UIA (NVDA/JAWS/WebView2) — the best story available for a form-heavy app, and something no immediate-mode toolkit can claim. WebKitGTK does implement AT-SPI so Orca works in principle, but it is the least-exercised of the three paths, and the window chrome/menus are GTK3, i.e. outside the webview's a11y tree.

### Development velocity & maintenance

**Build: fast, if the team writes TypeScript.** Vite HMR gives sub-second UI iteration; Rust rebuilds only when you touch command signatures. A schema-driven form (zod/valibot + generated controls) reduces the ~22-field boilerplate to a field-metadata table — which could plausibly be derived from the same source as the README defaults table, keeping the "new target field" checklist in CLAUDE.md honest. Component ecosystem (shadcn/Radix, TanStack Table/Virtual, Melt UI) is enormous and free. Designer tooling is real: Figma → HTML/CSS is the only solved handoff path among the candidate stacks.

**Maintain: this is where it costs.** Your team is Rust-fluent, and Tauri makes it a two-language project: cargo *and* npm/pnpm, two lockfiles, two supply chains, and a JS toolchain that churns considerably faster than Rust's. For a 9.7k-LOC DSP repo with 18 direct cargo dependencies and zero JS today, the frontend's node_modules will plausibly outnumber the DSP by an order of magnitude in dependency count. Budget real time for that, not zero.

**Mandatory overhead specific to Tauri:** `tauri-specta` for generated TS types across the IPC boundary (skip it and the boundary rots silently), Tauri's capabilities/permissions ACL JSON (default-deny; every plugin command must be explicitly granted per window — correct security design, but it's another config surface that fails at runtime rather than compile time), and a CSP that you will fight the first time you inline a style.

**Rough effort for a small Rust-fluent team, assuming one member picks up the web side:** 2-3 weeks to a usable config editor + batch runner with real progress; the progress/cancellation plumbing inside mint is a separate ~1 week and is required regardless of stack. Add a week for the Linux triage you will inevitably do.

### Risks

- Linux WebKitGTK is a third rendering engine you neither control nor can pin, and its failure mode is a BLANK WINDOW, not a degraded one. Tauri's own docs (v2.tauri.app/develop/debug/linux-graphics, live Aug 2026) prescribe a four-step ladder: nvidia_drm.modeset=1, then __NV_DISABLE_EXPLICIT_SYNC=1, then WEBKIT_DISABLE_DMABUF_RENDERER=1, then WEBKIT_DISABLE_COMPOSITING_MODE=1. Yaak, a shipping Tauri app, has a public feedback thread literally titled 'Fix blank screen / Crash on Linux due to Wayland, NVIDIA, etc'. Mastering engineers on Linux with NVIDIA cards are not hypothetical. You will ship an app that shows nothing on some machines and there is no fix in your repo.
- Engine version spread you must test against: WebKitGTK 2.36 (Ubuntu 22.04) through 2.52.3 (Ubuntu 26.04, released 2026-04-23) — a four-year CSS/JS feature gap. Plus WKWebView (Safari-family, tied to the macOS release, and unsupported macOS versions get NO WebKit updates) and WebView2 (Chromium, evergreen unless you pay 180 MB for fixedVersion). Three engines, three cadences, one unpinnable. Safari-family quirks are a different bug class from Chromium's.
- pipeline::process is a single blocking call with NO progress and NO cancellation, and Tauri makes this look solved (ipc::Channel exists) while contributing nothing to the hard half. Threading a Sync progress sink and an AtomicBool kill switch through pipeline.rs, ops::resample (BLOCK_FRAMES 16_384), ops::limiter (4096) and ops::dsd::encode_dsf is the actual project. Without it, a DSD256 encode of a 5-minute master is a spinner with no number on it, and the user cannot stop it.
- Memory: AudioBuffer is the whole file in RAM at 485-700 MB peak per job. A 4-worker batch on 96 kHz stereo is 2-2.8 GB RSS, PLUS a webview process tree (WebView2 spawns a full multi-process browser; WebKitGTK spawns WebKitWebProcess + WebKitNetworkProcess). The GUI's floor is 150-300 MB before any audio is decoded. The CLI could default to --workers 8; the GUI cannot, and must explain why. Separately, any waveform view is a second copy of the audio unless it is downsampled in Rust and shipped as bytes — CLAUDE.md says a second copy is a bug.
- Sync #[tauri::command] functions run on the MAIN THREAD in Tauri 2. Getting this wrong once hard-freezes the window for the entire duration of a DSD encode, and it looks fine in dev with a 3-second test tone. It is a footgun that only fires on real masters.
- New recurring cost and CI surface the CLI never had: Apple Developer ID ($99/yr) + Windows code-signing cert (~$200-400/yr) or every user hits Gatekeeper/SmartScreen, plus a Node/pnpm toolchain injected into five release-matrix legs of a workflow that currently has zero JavaScript.
- Two languages and two supply chains for a Rust-fluent team. 18 direct cargo deps today; the frontend will add hundreds of npm packages to a project whose entire value proposition is verifiable, reproducible output.
- AppImage vs .deb is a lose-lose: AppImage bundles webkit2gtk and jumps to 70+ MB; .deb stays ~10 MB but hard-depends on system libwebkit2gtk-4.1-0 and breaks on any distro that lacks it. openSUSE still packages webkit2gtk3-devel under a different name.
- IPC schema drift between Rust command signatures and TS callers unless tauri-specta is adopted from commit one. Nothing catches a renamed field at compile time otherwise.
- dragDropEnabled is all-or-nothing: OS file drops (which give you real paths, exactly what mint needs) and DOM drag-and-drop (which you'd want for reordering targets) are mutually exclusive. Known confusing-by-design, tauri#14373.
- Config round-trip: RawTarget is Deserialize-only, and toml::to_string would strip the comments from examples/mint.toml — the file that is include_str!-embedded as the bootstrap template. Requires adopting toml_edit, plus there is no Config::from_str, so live validation of an unsaved buffer needs a new public entry point or a temp file.
- The Linux escape hatch is not credible on your timeline. GTK4 migration (tauri#7335) has been open since 2023; tauri-runtime-verso (Servo) has 132 stars, no per-window menus, no devtools, a hard-coded IPC Origin header and glibc issues with its prebuilts. Issue #14963 ('bundle chromium renderer') shows the frustration is still live in 2026. Do not plan around any of these landing.

### The strongest argument for

Tauri's backend *is* Rust, so mint links in as a path dependency: one process, one binary, `pipeline::process` called as a plain function. No IPC for audio, no serialization, no second copy of `AudioBuffer` — which is not a nicety here, it is the explicit invariant CLAUDE.md protects with reference implementations and bit-equality tests. Everything already built keeps working: the static-libsoxr release workflow drops in unchanged because `tauri build` shells out to `cargo build`; `json_report::render` gives the UI the same stable schema `--json` gives QC automation, today, with zero library changes; `atomic_write` already makes cancellation and crashes safe; and the library is provably GUI-clean (zero `println!`, `eprintln!` or `log::` calls outside `cli.rs`/`main.rs`). On top of that you get the best form/table/theming toolkit that exists for a 22-field × 12-target editor with tri-state inheritance display, native signing and notarization from the bundler, and real screen-reader support on two of three platforms. No other web-based stack gives you a Rust-native backend; no native-Rust stack gives you this UI ceiling for dense config editing.

### The strongest argument against

You would be betting the Linux third of a professional audio tool on WebKitGTK — an engine neither you nor Tauri controls, whose version spans 2.36 to 2.52.3 across supported distros, and whose signature failure is a blank window rather than a degraded one. Tauri's own current documentation answers it by telling the user to export `WEBKIT_DISABLE_DMABUF_RENDERER=1`. mint's entire identity is bit-exact, verifiable, reproducible output, pinned by golden headers and `to_bits()` comparisons; shipping a front end that renders differently — or not at all — on one platform is the tonal opposite of that, and it is the one defect class you cannot fix, test around, or regression-pin inside your own repo. And the second-order version: Tauri does not solve the part that is actually hard. Progress and cancellation are mint-side surgery through four block loops either way; Tauri hands you a `Channel` and a two-language build.

---

## egui / eframe

**Full stack as evaluated:** egui / eframe (immediate-mode, pure Rust; wgpu default backend, glow optional) + rfd for native dialogs + egui_extras/egui_plot for tables and charts, linking `mint` as a library crate in the same binary.

**Version researched:** eframe/egui **0.36.1**, published **2026-08-07** (verified via crates.io API: 0.36.0 on 2026-08-05, 0.35.0 on 2026-06-25, 0.34.3 on 2026-05-27, 0.33.0 on 2025-10-09). Repo last push 2026-08-12; 30,041 stars; MIT OR Apache-2.0; 21.4M egui downloads. Companions: **rfd 0.17.2** (2026-01-12, now defaults to `xdg-portal` on Linux — no GTK3 build dep), AccessKit bumped 0.19→0.24 across 0.34–0.36. **Caveat I verified the hard way: egui/eframe 0.36.1 requires rustc ≥ 1.95; this machine is on 1.92.0 and `cargo build` refuses outright.** My size measurements below are therefore on 0.33.3, the newest I could actually build here.

**Self-assessed verdict:** `recommend`

For mint specifically, egui's decisive property is not how it looks — it is that the GUI's validation logic *is* `config::resolve_target`, called every frame. mint's whole correctness story is "the derived chain has three descriptions (`pipeline.rs`, `describe()`, `estimated_output_bytes()`) that must never drift"; every out-of-process stack adds a fourth. egui adds zero: `describe()` already returns `Vec<String>`, `estimated_output_bytes()` already returns a `u64`, and `ProcessingReport` arrives over an mpsc channel as a Rust struct with no DTO in between. Packaging is the other easy win — `release.yml` already builds static libsoxr on five targets, so the app is one ~10–14 MiB self-contained executable with no runtime deps. The honest costs are three: egui's default look is unmistakably "developer tool" and reaching studio-grade requires a real custom-widget/theming pass that only a Rust dev can do; the library has no cancellation or progress at all, which is tolerable in a CLI and unacceptable in a GUI; and putting a 500–700 MB-per-job, `unwrap`-containing, FFI-linking DSP core in the same process as the window means every DSP failure is a crashed editor.

| Criterion | Score |
|---|:--:|
| Cross-platform parity | 4 / 5 |
| Native-dep integration | 5 / 5 |
| Packaging & signing | 4 / 5 |
| UI ceiling | 3 / 5 |
| Dev velocity | 4 / 5 |
| Longevity | 3 / 5 |
| Accessibility | 3 / 5 |

### Integration with the mint core

**In-process, single binary — no sidecar, no IPC, no serialization.** `src/lib.rs` already re-exports everything, and the API surface a GUI needs is public today: `Config::from_path`, `Config::target_names`, `Config::resolve_target`, `ResolvedTarget::describe(source_rate) -> Vec<String>`, `::estimated_output_bytes`, `::delivery_blocker`, `::render_output_path`, `io_read::probe_source`, `config::check_planned_outputs`, `pipeline::process`. The dry-run panel is a `for` loop over `describe()`; nothing is reformatted or re-derived.

**Threading.** `pipeline::process` is blocking and single-threaded per job (rayon is only used by `cli.rs` for fan-out, and flacenc's `par` is deliberately off). Standard pattern: own rayon pool or `std::thread::spawn` per job → `mpsc::Sender<JobEvent>` → `ctx.request_repaint()` from the worker. eframe is reactive by default, so an idle UI burns no CPU while the DSP saturates a core. One nice pre-existing detail: `ops::resample::create_soxr`'s process-wide `INIT_LOCK` already uses `.lock().unwrap_or_else(|e| e.into_inner())`, so a panicked job does not poison every subsequent resample in a long-lived process — that would have been a nasty GUI-only bug.

**Progress and cancellation: absent, and this is the real integration work.** There is no callback anywhere. Free granularity is per-job ("3 of 12 done"); anything finer means threading a `&AtomicBool` plus a `&dyn Progress` through `pipeline::process` into four block loops — `ops::dsd::encode_dsf` (already clocked in container blocks, the natural hook), `ops::limiter::limit_channels`, `ops::resample::apply`, and the ebur128 analysis passes. Cheap in code, expensive in caution: those loops are pinned by bit-exactness tests against `#[cfg(test)] mod reference` implementations, so any restructuring has to keep `to_bits()` equality.

**Reports and diagnostics.** `ProcessingReport` moves across the channel by value — `LoudnessAnalysis`, `limiter_gain_reduction_db`, `DsdReport.overload_samples`, `warnings: Vec<String>` — all live Rust, no `json_report.rs` DTO needed (that stays the CLI's stable contract). Notably the library emits **no logging at all**: `log = "0.4"` is declared in Cargo.toml but there are zero `log::`/`info!`/`warn!` call sites in `src/`, and zero `println!` outside `cli.rs`/`main.rs`. Good news (nothing pollutes the UI, `env_logger` is a CLI concern) and bad (there is no telemetry stream to drive a console pane; warnings only exist at job completion).

**What mint would have to change.** (1) Cargo workspace split: `crates/mint-core` / `mint-cli` / `mint-gui`, so eframe+wgpu never enter the CLI's dependency graph. Both CI workflows need updating. (2) `Config::from_str` + `pub fn validate` — `from_path` is the only constructor and `validate` is private, so live validation of an in-memory buffer currently requires a temp file. (3) `RawTarget` needs `Serialize` (with `skip_serializing_if`) or a `toml_edit` writer — see risks. (4) A cancellation token and a progress trait. (5) MSRV bump to whatever egui demands.

### Packaging & distribution

**Artifact quality: excellent. Tooling to produce it: DIY.**

*Measured on this machine* (aarch64-apple-darwin, `--release`, `strip="symbols"`, `lto="thin"`, eframe 0.33.3 + rfd 0.15, trivial app):
- glow backend: **4,846,128 B = 4.62 MiB** (173 crates)
- wgpu backend: **8,587,296 B = 8.19 MiB** (151 crates) — the +3.6 MiB is naga, matching the 5.9→8.3 MiB figure emilk quoted in egui#5889 when making wgpu the default
- mint's current CLI binary for reference: **5,409,264 B = 5.16 MiB** (120 crates)

So realistically **~10 MiB (glow) or ~14 MiB (wgpu)** for a combined GUI+DSP binary, before icons. That is roughly one-tenth of an Electron/Tauri-with-WebView2-bootstrapper story and there is no runtime dependency of any kind.

**libsoxr is already solved and the brief undersells this.** `release.yml` today builds soxr 0.1.3 statically from the Debian orig tarball on Linux/macOS and via vcpkg's `x64-windows-static-md` triplet on Windows, hand-writing the `soxr.pc` that upstream skips on WIN32 (because `libsoxr-sys` probes exclusively through pkg-config with no fallback). Five targets, all green. A GUI binary inherits that verbatim — one extra `cargo build` line per matrix entry. No dylib to ship, no `install_name_tool`, no `@rpath` surgery, no "brew install libsoxr" in the README.

**Installers.** No first-party story; egui/eframe give you a binary and stop. Options are `cargo-packager` (CrabNebula, actively maintained, produces .app/.dmg/MSI/NSIS/AppImage/deb) or the older `cargo-bundle`. **Neither signs nor notarizes.** You bolt on: macOS `codesign --deep --options runtime` + `xcrun notarytool submit --wait` + `stapler` (or the `apple-codesign` crate, which notably runs from Linux/Windows too); Windows `signtool` or Azure Trusted Signing. Budget 2–4 days of one-time CI plumbing on top of the existing `release.yml`, plus an Apple Developer account ($99/yr) and an EV/OV code-signing cert for Windows or SmartScreen will flag every download.

**CI cross-build.** Fine for the binary — the existing five-target matrix already proves it, and eframe adds no new C toolchain. Two wrinkles: (a) cold builds get much slower (wgpu+naga+winit; the incremental hello-world here was 33 s warm, cold from scratch on a CI runner is 3–6 min — cache `~/.cargo` and `target/`); (b) if you adopt `egui_kittest`'s wgpu snapshot tests you need a GPU or lavapipe/software Vulkan on the Linux runner.

**Gotchas.** macOS `.app` needs `NSHighResolutionCapable`, a real bundle identifier, and `LSApplicationCategoryType`; drag-and-drop of audio files onto the Dock icon needs `CFBundleDocumentTypes` declared. On Windows you must set `#![windows_subsystem = "windows"]` or every launch flashes a console. AppImage on Linux is the least-friction distribution given the static soxr.

### UI capability ceiling

**Brutally honest on the default look: it is not elegant.** Out of the box egui is uniform slate gray, 4 px rounded corners, one font size, ImGui-descended flat rectangles. The README lists "native looking interface" as an explicit *non-goal*. A mastering engineer's first read of unstyled egui will be "this is a debug menu", and no amount of arguing changes that first three seconds.

**What theming buys.** `Visuals`/`Style` gives you every colour, corner radius, stroke width, spacing constant and font; 0.34 added genuine per-widget styling and 0.35 added `Classes` (context-scoped widget behaviour). You can load a real typeface and get crisp hinted rendering — 0.34 switched from `ab_glyph` to `skrifa` + `vello_cpu`, which is why text finally looks sharp on Retina. The existence proof that this reaches professional is the **Rerun Viewer**, which is the one app the egui README itself points at and which is genuinely handsome. But note the incentive: Rerun *funds* egui, its designers work on egui itself, and even so Rerun looks like a bespoke instrument, not like a Mac app. What you cannot get is a cascade — styling is one global `Style` blob you push/pop (issue #3284, "CSS-like styling", is still open) — and there is no styling DSL, no theme file, no designer handoff. Past "themed egui" you are hand-painting with `Painter` in Rust. egui makes that unusually pleasant (it's a shape list, and `Shape::Path`/`Mesh` are right there), but it is developer pixel work.

**Dense forms: this is egui's home turf and mint's 22 fields fit it unusually well.** `Grid` + `DragValue` (with `.suffix(" LUFS")`, `.range(-40.0..=0.0)`, `.speed(0.1)`) + `ComboBox` for the 11 presets and the six enums is roughly 15 lines per field, ~350 lines for the whole target editor, with no data binding, no signals, no two-way-sync bugs. The 4-layer merge maps onto immediate mode almost too neatly: each frame, resolve the target for real and render the inherited value as dimmed placeholder text, switch to bold when the field is explicit, and put a ✕ next to explicit fields to revert to inherited. Live validation is free — call `Config::resolve_target` in the render function (microseconds) and paint its `anyhow` error string red under the offending row. **The GUI's validator is not a reimplementation; it is `config.rs` running at 60 Hz.** Critically, the own-layer exception (`format`/`rate`/`bwf` judged without `[defaults]`) comes along for the ride automatically, because you are calling the real function rather than modelling the merge in TypeScript.

**Live meters and progress:** trivial. `ProgressBar`, and LUFS / true-peak / gain-reduction meters are 30 lines of `Painter::rect_filled` each, animated at whatever rate you `request_repaint_after`.

**Future waveform/spectrum:** egui is *stronger* here than any retained-mode or web option. A decimated min/max envelope of a 5-minute 96 kHz master is a single `Shape::Path` you build yourself; `egui_plot` (split out to emilk/egui_plot) handles axes/zoom/pan; and with the wgpu backend you can install a custom `PaintCallback` and render a real spectrogram texture with your own shader, in-process, from the same `AudioBuffer` the DSP just produced. No canvas bridge, no `postMessage`, no copying 461 MB of f64 into a JS typed array.

**Tables:** `egui_extras::TableBuilder` is fine for a batch job list; `egui_table` (rerun-io) does virtualized large tables.

**HiDPI:** solid on all three — `pixels_per_point` from winit, hinted glyphs since 0.34. Wayland fractional scaling is soft, but that is an ecosystem problem, not an egui one.

**Text input / IME: historically egui's weakest widget, materially improved in this exact release window.** 0.35 added proper IME composition visuals (underline + cursor position) and `Memory::owns_ime_events`; 0.36 fixed autocomplete/autocorrect/IME on mobile. Still: `TextEdit` renders all its text without culling and gets sluggish with large content (issue #3086), there is no rich-text editor, and there are recurring platform-specific IME bugs (e.g. #7485, ibus-on-Wayland eating all but the first character). For mint's fields — paths, naming patterns, numbers — this is a non-issue. If you ever want an embedded TOML source editor with syntax highlighting, it becomes one.

**Structural ceiling.** Single-pass layout means anything that must size to a sibling's content needs a one-frame delay, which shows up as first-frame jitter when a panel opens. There is no animation framework. And on macOS egui draws its menu bar *inside the window* — there is no `NSMenu` — which is the single most visible non-nativeness on the platform your user probably runs.

**Accessibility for elegance's sake:** AccessKit is on by default in eframe and there is a real payoff beyond a11y — `egui_kittest` drives the AccessKit tree to write actual UI tests, and 0.35's `egui_inspection`/`egui_mcp` lets an agent read and operate the running app. That matches mint's existing "everything is pinned by a test" culture better than any other GUI stack offers.

### Development velocity & maintenance

**Fastest path to a working, correct, data-dense app of any option here, for a Rust-fluent team.** There is no IPC layer, no schema, no TypeScript toolchain, no build step, no data-binding runtime, no state-synchronisation class of bug. The whole app is functions that take `&mut Ui`. Debugging is `dbg!` inside the render function. A team that already wrote 12.4k lines of careful DSP will be productive on day one, and — the part that actually matters — will not have to learn a second language or a second mental model to change the UI when they add a target field.

**Realistic effort.** Walking skeleton (window + load `examples/mint.toml` + list targets + dry-run panel): 1–2 days. Full 22-field target editor with inheritance display and live validation: 3–4 days. Batch runner with off-thread jobs, per-job status table, cancel-at-job-boundary, and painted LUFS/TP/GR meters: 3–4 days. Config write-back via `toml_edit`: 2–3 days plus a round-trip property test. Theming pass to get out of default-gray: 3–5 days, and it needs someone with taste, not just someone with Rust. Call it **3–4 weeks for a genuinely shippable v1**, which is fast.

**The ~22-field form specifically.** Boilerplate is real but linear and dumb — roughly 15 lines per field, ~350 total. The mitigating factor is that in immediate mode there is nothing *else*: no model class, no view-model, no binding declaration, no validation schema, no serialization DTO. Compare against the CLAUDE.md "new target field" checklist: with egui the GUI adds exactly **one** new site (the widget), and it cannot silently lie, because the value it displays comes back out of `resolve_target`. With any out-of-process stack it adds three (Rust DTO, wire schema, frontend form field) and two of them *can* silently lie.

**No hot reload.** This is the honest velocity cost. Incremental rebuild of the GUI crate is a few seconds once wgpu/naga are compiled, but cold builds are 3–6 minutes and every `cargo build` restarts the app and loses UI state. There is no `egui-hot-reload` worth depending on. Compared to a Tauri frontend with Vite HMR, the tweak-a-colour-and-look loop is ~10× slower. For a form-heavy app this matters less than it sounds; for the theming pass it matters a lot.

**No designer tooling whatsoever.** No visual editor, no Figma import, no styling file a non-programmer can edit. Every visual decision is a Rust literal in a Rust file. If the team has (or wants) a designer, egui excludes them completely. That is a real, permanent constraint, not a temporary gap.

**Maintenance.** The tax is the release cadence: five breaking minors in ten months. 0.34 renamed `App::update`→`App::ui` and unified `SidePanel`/`TopBottomPanel` into `Panel`; 0.35 deleted every deprecated item; 0.36 moved `Modifiers` out of `RawInput` and removed `clip_rect_margin`. These are mechanical, well-documented, repo-wide edits — budget **half a day per egui minor**, plus a toolchain bump, and do not fall three releases behind or the migrations compound.

**Testing.** `egui_kittest` (AccessKit-driven interaction + wgpu snapshot regression) is a genuinely good story and would fit straight into `tests/`. The snapshot half needs a GPU or lavapipe in CI.

### Risks

- Single process, single blast radius — the sharpest risk and it is mint-specific. `AudioBuffer` is the whole file in RAM (measured 485–700 MB peak RSS for one 5-minute 96 kHz stereo job) and `cli.rs` defaults `--jobs` to `available_parallelism()`. Reuse that default inside the GUI and a 10-core machine tries to hold ~5 GB in the same process as the window. An OOM, or a panic in one of the 36 non-test `unwrap`/`expect`/`panic!` sites, or a libsoxr FFI segfault (which `catch_unwind` cannot catch at all) takes the editor and any unsaved config down with it. Mitigable — cap workers from free RAM, `catch_unwind` per job, autosave — but never eliminable in-process. A sidecar turns all of these into a red row in a job list.
- No cancellation and no progress anywhere in the library. A DSD512 render of a long master is a multi-minute, uninterruptible, un-instrumented block; in a GUI that reads as a hang. Fixing it means threading an `AtomicBool` and a progress sink through `pipeline::process` into four loops — `ops::dsd::encode_dsf`, `ops::limiter::limit_channels`, `ops::resample::apply`, and the ebur128 passes — every one of which is pinned by bit-exact `#[cfg(test)] mod reference` tests. Cheap in lines, expensive in review.
- Config write-back has exactly two options and both hurt. `RawTarget` is Deserialize-only, and `examples/mint.toml` is a heavily commented teaching document that is `include_str!`-embedded as the bootstrap template. Either (a) derive `Serialize` + `skip_serializing_if = "Option::is_none"` on 22 fields and accept that every Save destroys the user's comments and field order, or (b) drive `toml_edit` and mutate in place, preserving comments but introducing a second hand-rolled GUI-state→TOML-key mapping that can silently drift from `RawTarget` — precisely the multi-site-checklist failure CLAUDE.md exists to prevent. There is no third option. Pick `toml_edit`, add a round-trip property test, and add the GUI to the 'new target field' checklist.
- egui's version and MSRV treadmill is imposed on the whole workspace. Verified on this machine: `eframe 0.36.1` fails to build on rustc 1.92.0 because ecolor/eframe/egui/epaint all require 1.95. Adopting egui means every dev box and both CI workflows follow egui's toolchain schedule, not yours — and the policy is only 'at least two minor releases behind latest'.
- Bus factor. emilk has 3,018 commits; the next contributor (lucasmerlin) has 189. Development is funded by Rerun, a VC-backed startup that employs several of the top contributors. MIT/Apache-2.0 means a fork is always legal, but forking a GUI toolkit of this size is not a maintenance plan for a small team. If Rerun pivots, the cadence collapses.
- wgpu is the default backend in 0.36 and can fail hard rather than slowly: an old Windows studio box with a broken DX12/Vulkan driver, or a headless/RDP/VM session, gives a black window. Keep `glow` behind a `--renderer` flag as a fallback (it also saves the 3.6 MiB I measured) and actually test over RDP — engineers do run render boxes headless.
- No native macOS menu bar. egui draws its menus inside the window; there is no `NSMenu`, so Cmd+Q, Cmd+, and the Services menu need hand-wiring or a `muda` dependency with its own winit-version coupling. On the platform most mastering engineers use, this is the loudest 'not a real app' tell.
- Installer + signing is entirely bolt-on. `cargo-packager` and `cargo-bundle` both produce bundles and neither signs or notarizes; you write the `codesign`/`notarytool`/`stapler` and `signtool` steps yourself, with an Apple Developer account and a Windows OV/EV cert or SmartScreen flags every download. 2–4 days of one-time CI work that Tauri would have given you.
- Live validation calls `Config::resolve_target` per frame, but `Config::from_path` is the only public constructor and `validate()` is private — so validating an in-memory edit buffer currently requires writing a temp file. One-line repo fix (`Config::from_str`, `pub fn validate`), but it is a repo change and it belongs in the prototype, not the retrospective.
- Wayland fractional scaling produces soft text at 125%/150%. Ecosystem-wide rather than egui's fault, but the Linux user will blame your app.
- `egui_kittest`'s wgpu snapshot tests need a GPU or lavapipe on the Linux runner, so visual regression coverage either adds CI setup or silently only runs on developer laptops.
- No hot reload and no designer tooling at all. Every visual decision is a Rust literal recompiled at 3–6 minutes cold. The theming pass — the exact work that determines whether an engineer calls this elegant — has the slowest possible iteration loop and can only be done by a Rust developer.

### The strongest argument for

**The GUI does not re-describe mint's rules; it executes them.** mint's central correctness anxiety, spelled out repeatedly in CLAUDE.md, is that the derived chain already has three separate descriptions (`pipeline.rs`, `ResolvedTarget::describe()`, `estimated_output_bytes()`) that drift silently, and that the 4-layer merge has a load-bearing exception (`format`/`rate`/`bwf` judged against the target's own layer) that a naive reimplementation gets wrong — the exception is precisely what makes `preset = "sacd"` work and what stops a `[defaults] format = "s24"` from breaking every DSD target. With egui, the form's inheritance preview and its validation errors are `Config::resolve_target` called in the render function, sixty times a second: the same code, the same exception, the same `anyhow` string, no schema, no version skew, no fourth description to keep in sync. `describe()` already returns `Vec<String>` and `estimated_output_bytes()` a `u64`, so the dry-run panel is a `for` loop over data mint computes today. Every out-of-process stack adds a fourth description of the merge and a serialization boundary in front of `ProcessingReport`; for this codebase, that is not a cost of convenience, it is a new instance of the exact bug class the project is organized to prevent. And the ancillary wins are all free: libsoxr links exactly as it does today (the existing `release.yml` already produces static soxr on five targets), a future spectrogram reads the same `Vec<Vec<f64>>` the DSP just wrote with zero copies, and the whole thing ships as one ~10–14 MiB binary with no runtime dependency.

### The strongest argument against

**egui asks you to co-locate the window with a DSP core that was designed on the assumption it can crash.** mint deliberately spends 500–700 MB of RAM and a full core per job, holds the entire master in one `Vec<Vec<f64>>`, links a C library over FFI, contains 36 non-test `unwrap`/`expect`/`panic!` sites, and offers neither cancellation nor progress — all of which are perfectly reasonable choices *for a process that exits after one batch*. egui gives you no supervision, no sandbox, no restart, and no way to reclaim memory a panicked job leaked; a libsoxr segfault is not even catchable. Eight parallel jobs at the CLI's own default worker count is 4–5 GB in the process that owns your unsaved config. A sidecar architecture makes every one of those a red row in a job list and a "Retry" button; egui makes them a crash report. This is a process-isolation argument, not a rendering argument, and it is the one thing in-process immediate mode structurally cannot answer. The secondary blow is the one the brief actually asked about: "elegant" is reachable — Rerun proves it — but only by hand-painting widgets in Rust with no hot reload, no styling file and no possibility of a designer contributing, and the default state you start from is a gray debug menu that an audio engineer will judge in three seconds.

---

## Iced

**Full stack as evaluated:** Iced (Elm architecture, retained-mode, pure Rust, wgpu + tiny-skia) — with `iced_aw` 0.14.1, `iced_audio` 0.15.0, `rfd` 0.17.2, `sipper` 0.1.0, and `cargo-packager`/`dist` for bundling.

**Version researched:** iced **0.14.0**, published 2025-12-07 (crates.io API; latest as of 2026-08-13 — **no 0.14.x patch release in the 8 months since**). Deps as published: winit ^0.30 (upstream), wgpu ^27.0, cryoglyph ^0.1 (iced's own glyphon fork), rust-version 1.88, edition 2024. Repo is alive: last push 2026-08-12, 31,229 stars, 478 open issues, MIT, ~708 commits in the trailing 52 weeks. **master is already incompatible with the release**: `iced-rs/iced@master` Cargo.toml pins `winit` to a *git fork* (`github.com/iced-rs/winit`, rev 05b8ff1), wgpu 29, rust-version 1.92 — that is 0.15-dev. Prior cadence: 0.10 (2023-07-28) → 0.12 (2024-02-15) → 0.13.0/0.13.1 (2024-09-18/19) → 0.14.0 (2025-12-07). Ecosystem has caught up to 0.14: iced_aw 0.14.1 (2026-04-27), iced_audio 0.15.0 (2026-07-29, targets iced ^0.14). Sniffnet 1.5.1 ships on plain crates.io `iced = "0.14.0"`; Halloy 2026.8 (2026-07-26) does **not** — it `[patch.crates-io]`-pins its own fork `squidowl/iced` at rev 8d458d2 on `0.15.0-dev`.

**Self-assessed verdict:** `viable-with-caveats`

Iced is the best structural fit for mint's shape: one Rust binary, `pipeline::process` called directly, libsoxr linked exactly as it is today, and no serialization boundary between the DSP and the UI. Its Task/subscription model is a genuinely good answer to the progress-and-cancellation problem mint doesn't have yet — `Task::run(stream).abortable()` is the whole pattern in three lines. Against that: iced has **zero accessibility** (issue #552 open since Oct 2020; the most complete AccessKit PR, #3281, was closed unmerged 12 hours after it opened in March 2026, blocked on iced pinning a winit fork that `accesskit_winit` can't semver-match), the 0.14 `table` widget is a layout helper with no sorting/selection/virtualization, and the release cadence (15 months between minors, zero patch releases) means a blocking bug puts you in Halloy's position — forking and pinning. mint's real UI is a 22-field target editor plus a batch table, and those are precisely iced's two weakest widget areas; the future waveform/spectrum work is precisely its strongest (canvas + the `shader` widget + iced_audio's meters/knobs). Recommend only if nobody in the buyer chain has an accessibility requirement.

| Criterion | Score |
|---|:--:|
| Cross-platform parity | 3 / 5 |
| Native-dep integration | 5 / 5 |
| Packaging & signing | 4 / 5 |
| UI ceiling | 3 / 5 |
| Dev velocity | 3 / 5 |
| Longevity | 3 / 5 |
| Accessibility | 1 / 5 |

### Integration with the mint core

**In-process crate, one binary — no sidecar.** Add `mint-gui` as a second `[[bin]]` (or a workspace member) with `mint = { path = ".." }`. `src/lib.rs` already exposes everything, so `pipeline::process(&Path, &ResolvedTarget, Option<u64>) -> Result<ProcessingReport>` is a plain function call. `ProcessingReport` is `Debug + Clone` and its sub-structs (`LoudnessAnalysis`, `DsdReport`, `TaskFailure`) are plain data, so they cross an mpsc channel unmodified — **`json_report.rs` is not needed by the GUI at all** and stays a CLI-only DTO layer.

**Threading.** iced 0.14's default features include `thread-pool` (futures' ThreadPool); `tokio` and `smol` are opt-in. mint needs no async runtime. The pattern, verbatim from the 0.14 API:

```rust
Message::Run => {
    let (tx, rx) = futures::channel::mpsc::channel(64);
    std::thread::spawn(move || {
        pool.install(|| jobs.par_iter().for_each(|j| {
            let r = std::panic::catch_unwind(|| pipeline::process(j.input, j.target, seed));
            let _ = tx.clone().try_send(JobEvent::Done(j.id, r));
        }))
    });
    let (task, handle) = Task::run(rx, Message::Job).abortable(); // Task::abortable() -> (Task<T>, Handle)
    self.abort = Some(handle);
    task
}
```
`catch_unwind` is sound here: mint has no cross-job shared mutable state (`tests/soxr.rs` guards that specifically), and the one process-wide `Mutex` — `INIT_LOCK` in `ops::resample::create_soxr` — already de-poisons itself with `.unwrap_or_else(|e| e.into_inner())`. Without `catch_unwind`, a DSP panic or a libsoxr FFI abort takes the whole window down, which a CLI-sidecar design would degrade to "one failed job" instead. That is the strongest argument for *not* going in-process.

**Progress.** `grep` confirms mint has zero `AtomicBool` and zero progress hooks. Two honest tiers:
- *Free today, zero mint changes:* per-**job** granularity. 4 files × 3 targets = 12 discrete ticks, plus a plan preview built from `ResolvedTarget::describe()` and `estimated_output_bytes()` (both pure, instant, and already the `--dry-run` code path).
- *Real intra-file progress:* a `JobCtx { progress: &dyn Fn(Stage, f32), cancel: &AtomicBool }` third parameter on `process`. The three streaming stages already have block loops to hook — `ops::dsd::encode_dsf`, `ops::limiter::limit_channels`, `ops::resample::apply` — but `loudness::analyze`, `measure_true_peak_dbtp` and `bitdepth::apply` are whole-buffer passes with no loop, so those stay coarse stage-boundary ticks. This work is identical under any GUI stack; it is a mint change, not an iced change.

**Cancellation.** `Task::abortable`'s `Handle` aborts the *stream consumer*, not the OS thread. `pipeline::process` is blocking with no yield points, so a Stop button without the `&AtomicBool` above means "stop reporting" while eight rayon workers keep grinding a 461 MB buffer. Mitigating asset already present: every writer stages through `audio::atomic_write` (sibling + rename), so an abandoned job leaves a staging file, never a half-written deliverable.

**What mint has to change.** (1) `RawTarget` is Deserialize-only — writing edits back needs either `#[derive(Serialize)]` (which destroys the comments in the `include_str!`-embedded `examples/mint.toml` template) or `toml_edit` surgical edits (preserves them, more code). (2) **Do not reimplement the 4-layer merge in the GUI.** Call `config::resolve_target` and render *its* output as an "effective values" column beside the raw fields — otherwise you will silently get the load-bearing exception wrong (`format`/`rate`/`bwf` judged against the target's own layer *without* `[defaults]`, so `preset = "sacd"` works). (3) `probe_inputs` and `plan_outputs` are private fns in `cli.rs`; only `config::check_planned_outputs` is `pub`. Lift the batch pre-flight into a `batch` module so the GUI's collision checking is literally the CLI's. Small refactor, high value. (4) `env_logger`/`log` output goes to stderr; per-job warnings already ride in `ProcessingReport.warnings`, so the loss is mostly cosmetic.

**Memory.** One address space: the GUI's ~70 MB idle plus mint's measured 485–700 MB per concurrent job. Eight parallel jobs on a 96 kHz master is ~4–5.5 GB RSS in the same process as the window. That is not an iced problem, but the GUI must expose a worker-count control (mint's `--jobs`) prominently, because the CLI's default of `available_parallelism()` is much scarier when the same process owns your UI.

### Packaging & distribution

**Shipped artifact: one self-contained executable.** No webview, no runtime, no sidecar, no helper processes — a single Mach-O / PE / ELF that statically links libsoxr and contains the whole DSP. This is the easiest possible thing to codesign and notarize (contrast Tauri, which needs WebView2 on Windows and WebKitGTK on Linux, or Electron). Ship the CLI as a second `[[bin]]` in the same bundle: `Mint.app/Contents/MacOS/mint` next to `mint-gui`, with an optional installer step symlinking it into `/usr/local/bin`. GUI user gets one app; CLI/CI user gets the same binary they have now.

**Size.** Real numbers from the flagship iced app, Halloy 2026.8: `halloy.dmg` 38.4 MB, `halloy-installer.exe` 15.6 MB, `halloy-x86_64-linux.tar.gz` 21.7 MB. The dmg is fat because it is a universal binary. Expect mint-gui in the same 15–40 MB band. Older "iced counter = 6 MB" figures circulating online are 0.4-era and no longer apply (wgpu 27 + cosmic-text/cryoglyph + resvg dominate).

**libsoxr — already solved in this repo.** `.github/workflows/release.yml` builds soxr 0.1.3 statically from the Debian orig tarball on Linux/macOS (`-DBUILD_SHARED_LIBS=OFF -DWITH_OPENMP=OFF -DCMAKE_POLICY_VERSION_MINIMUM=3.5`) and uses vcpkg's `x64-windows-static-md` triplet on Windows, hand-writing the `soxr.pc` that upstream's CMake refuses to generate on WIN32 (because `libsoxr-sys` 0.1.4's build.rs probes *exclusively* via pkg-config with no fallback — I read it). Five targets: x86_64/aarch64 Linux, x86_64/aarch64 macOS, x86_64 MSVC. The GUI adds a binary to that existing matrix and inherits all of it. **This is the single biggest de-risking factor and it is already done.** Note the residual: `libsoxr-sys`'s hard pkg-config dependency means every *developer* machine also needs pkg-config + soxr, which is a worse first-run experience for a GUI contributor than for a CLI one.

**What is missing.** iced has no first-party bundler — unlike Tauri, there is no `iced build`. You assemble it:
- `cargo-packager` (CrabNebula, actively maintained) covers .app/.dmg/.deb/.AppImage/pacman/NSIS/WiX-MSI from a `[package.metadata.packager]` block. It explicitly does **not** sign or notarize.
- macOS signing/notarization is DIY, but Halloy ships a ~45-line `scripts/sign-macos.sh` you can copy almost verbatim: import a base64 cert into a temp keychain → `codesign --force -s ... --options runtime` (hardened runtime) → zip → `xcrun notarytool submit --wait` → `xcrun stapler staple`. Requires an Apple Developer Program membership ($99/yr) and 4 CI secrets. Budget one day the first time, then it is a solved problem.
- Windows: Halloy uses Inno Setup via choco plus **SignPath** (`signpath/github-action-submit-signing-request@v2`), which is free for OSS — a genuinely good answer to the "we don't want to buy an EV cert" problem.
- Linux: no signing; tar.gz is what Halloy actually ships. AppImage via cargo-packager works but you must confirm the Vulkan/mesa loader story; the tiny-skia fallback (see below) is your safety net.
- `dist` (ex-cargo-dist) 0.32.0, 2026-05-21, still maintained, is the alternative if you want the whole release orchestrated rather than hand-rolled — but mint's existing release.yml already does 80% of what it would generate.

**Robustness win worth naming.** iced 0.14's *default* features include **both** `wgpu` and `tiny-skia`; it tries wgpu first and silently falls back to CPU software rasterization if GPU init fails (also forceable with `ICED_BACKEND=tiny-skia`). That means the app survives Windows RDP sessions, headless VMs, ancient Intel Macs, and Linux boxes with a broken Vulkan ICD — a real advantage over a GPU-only stack for a tool that mastering engineers may run on a studio machine over remote desktop.

**CI cross-build feasibility: proven.** The repo already cross-builds 5 targets on GitHub-hosted runners including `macos-15-intel` (free Intel-mac label, supported until Aug 2027) and `ubuntu-24.04-arm`. Adding a GUI needs Linux dev packages on the Linux legs (`libxkbcommon-dev`, `libwayland-dev`, `libxcb`/`libX11` headers) and a `lipo` step for the macOS universal binary. Cold build time roughly triples: iced pulls ~400–500 crates.

### UI capability ceiling

**Split verdict: excellent at the meters/waveform half, weak at the forms/tables half — and mint's primary screen is the second half.**

*Dense forms — the weak spot.* iced 0.14's widget set (verified from the 0.14 docs index) is: button, checkbox, combo_box, container, float, grid, image, keyed, markdown, pane_grid, pick_list, progress_bar, qr_code, radio, row, rule, scrollable, selector, sensor, shader, slider, space, svg, table, text, text_editor, text_input, toggler, tooltip, vertical_slider, plus `lazy`, `responsive`, `hover`, `mouse_area`, `stack`, `themer`, `pin`. **No numeric spinner, no tabs, no menu bar, no tree view, no form/validation abstraction.** For mint's ~22 fields you write, per field: a widget call, a `Message` variant, an `update` arm, and for every numeric field (`lufs`, `ceiling_dbtp`, `warn_limiting_db`, `max_limiting_db`, `mp3_bitrate`, `rate`) a `text_input` plus your own parse/validate/error-display. `iced_aw` 0.14.1 (2026-04-27) supplies `number_input`, `tabs`, `menu`, `card`, `date_picker`, closing part of the gap. Realistically ~500–900 lines for the target editor, mostly mechanical, and mostly compressible into 4–5 helper functions (`labelled_enum::<T>`, `labelled_f64`, `optional_f64`) once the enums implement `Display + PartialEq` for `pick_list`.

*Tables — worse than the headline suggests.* `table` is new in 0.14 and sounds like the answer, but reading `widget/src/table.rs`: it builds **all rows eagerly** through a three-pass column-sizing layout, and supports **no sorting, no selection, no resizable columns, no sticky headers, no virtualization**. Signature is `table(columns, rows)` with `column(header, view_fn)` — it is a smart layout container, not a data grid. Fine for 12–50 (input × target) rows, which is mint's realistic batch. Questionable at 2,000 rows (500-file library × 4 targets); you'd fall back to `scrollable` + `lazy` + manual windowing. Nothing in the ecosystem fills this — the HN thread on the 0.14 release names "treeviews and datagrids" as the top missing pieces versus Qt.

*Live meters, waveform, spectrum — the strong spot, and genuinely a 5.* Three escalating tools: (1) `canvas` with `Program::draw` and a `Cache` that stores tessellated `Geometry` so an unchanged meter costs nothing per frame — ideal for LUFS/true-peak bars fed from `ProcessingReport.delivered`. (2) `iced_audio` **0.15.0, released 2026-07-29, targeting iced ^0.14** — knobs, sliders, XY pads, ramps, range inputs, built by the RustAudio crowd for exactly this domain; it is not abandonware, it tracked 0.14 within 8 months. (3) the `shader` widget — a raw wgpu render pass inside a widget, which makes a GPU-side spectrogram or a 5-minute waveform overview a real option rather than a wish. `plotters-iced` exists for conventional charts. This is a better future-waveform story than any HTML-based stack and better than most native toolkits.

*Theming and elegance.* iced 0.14 has built-in `Theme` palettes, a per-widget `Catalog`/`StyleFn` system, and `themer` for subtree overrides; 0.14 also added system-theme reaction and ships `crisp` and `linux-theme-detection` as default features (the former appears aimed at the long-standing "text looks washed out on non-HiDPI" complaint, issue open since Feb 2024). The ceiling is high — Halloy is a genuinely handsome application and COSMIC's settings/launcher/text-editor prove it scales to a whole desktop — but COSMIC uses **libcosmic, a fork**, so that proof point is weaker than it looks. Styling is code-only: no CSS, no hot design iteration, no designer handoff. The recurring community complaint is exactly this ("difficulty recreating the style to the same high bar I expected from websites using CSS").

*HiDPI and text.* winit scale-factor handling is solid on all three; cosmic-text/cryoglyph does proper shaping. 0.14 added input-method (IME) support (#2777) — CJK/dead-key input works, which was a real gap before. Known text issue: on Linux, iced enumerates all system fonts at startup, which is the cause of the "slow startup on Linux" bug (open since 2024-06, 12 reactions) — a ~2 s cold start is commonly reported.

*Drag-and-drop.* `window::Event::{FileHovered, FileDropped, FilesHoveredLeft}` carry `PathBuf`s and fire once per file — exactly what mint's glob-driven input list wants. **But iced 0.14's own docs say all three are "Not implemented on Wayland."** winit 0.30.13's FEATURES.md is more optimistic (marks DnD "mostly works, known bugs" on Wayland too, tracking issue #720), so these two sources disagree; I could not resolve it without building. Treat "drag a folder of masters onto the window on Fedora/Ubuntu Wayland" as unproven and test it in the prototype. The `rfd` 0.17.2 (2026-01-12, 25M downloads) "Add files…" dialog uses the XDG portal and works everywhere, so it is a working fallback, not a workaround for a missing feature.

*Native file dialogs.* Not built into iced; `rfd::AsyncFileDialog` + `Task::perform` is the standard, healthy answer.

### Development velocity & maintenance

**Learning curve: low for this team specifically.** The Elm loop is `State / Message / update / view`, and a team that already maintains three deliberately exhaustive `match target.codec` arms as a correctness device will find `match message` congenial. `Task` composes monadically (`then`, `map`, `batch`, `abortable`); `Subscription` handles timers and global keyboard. There is no hidden runtime, no macro DSL, no second language.

**Boilerplate is the real cost, and it is front-loaded.** The 22-field editor is the bulk of it (see uiCeiling). Beyond that, iced 0.14 adds real productivity features: `Task::sip` with `sipper` 0.1.0 (2026-06-16, by hecrj) gives you a compiler-checked "stream N progress items, then produce exactly one `ProcessingReport`" type, which is a better fit for mint's job model than raw streams and eliminates the usual `enum Progress { Tick, Done(T) }` juggling. 0.14 also shipped **first-class end-to-end testing** (`tester` feature) and **headless testing** — a genuinely unusual capability for a Rust GUI, and one that matters here because mint's culture is measurement harnesses with pinned numbers; you can assert on rendered UI in CI rather than eyeballing.

**Hot reload exists but do not plan around it.** #3000 integrates Dioxus's `subsecond` runtime hotpatching via a `cargo-hot` drop-in for `cargo run`. Maintainer's own caveats: "very experimental and may crash your OS", only root-crate changes trigger reload, and any change to `State` or `Message` needs a cold restart — which in a form-heavy app is most of your edits. Compared to Slint's live LSP preview or web hot reload, this is not competitive yet.

**No designer tooling at all.** No visual editor, no Figma path, no CSS. Everything is Rust.

**Documentation and LLM support are a live drag in 2026.** The book is incomplete; the examples directory is the real documentation; and the model-training corpus is dominated by 0.12-era code (`Sandbox`, `Command`, the `Application` trait), all of which was removed. Expect an assistant to confidently hand you API that hasn't existed since Sept 2024 — a measurable tax if the team leans on AI-assisted coding.

**Maintenance is where velocity bites.** You will sit on 0.14.0 for roughly a year with **no patch stream** (zero 0.14.x releases in 8 months). If you hit a blocking bug, your options are: work around it, or do what Halloy does — fork iced, pin `[patch.crates-io]` to your rev, and cherry-pick from master. That is a real, recurring, unbudgeted maintenance line item. Then 0.15 lands with edition/MSRV bumps (master is already at Rust 1.92, wgpu 29, forked winit) and you pay a migration of roughly the size of 0.13→0.14: edition 2024, `Widget::update` taking `&Event`, `Overlay::is_over` removed, `color!` shorthand removed — a day or two for a small app, more if you wrote custom widgets.

**Build ergonomics.** ~400–500 extra crates, several-minute cold release builds, and Linux contributors now need Wayland/X11/xkbcommon dev headers on top of the existing `libsoxr-dev` + `pkg-config`. Incremental debug builds stay acceptable.

### Risks

- Accessibility is absent, not merely immature, and there is no landed code after six years. Issue #552 has been open since 2020-10-05. Three attempts exist: #1849 (WIP, stale), #3111 (draft feasibility, Button and Text only, macOS VoiceOver), and #3281 (comprehensive, by dhedlund, opened 2026-03-14 and closed unmerged 2026-03-14T19:26 — twelve hours). #3281's own body names the blocker: iced pins a fork of winit while accesskit_winit depends on upstream winit, a semver conflict it says must be resolved before 0.15. I confirmed the fork in iced master's Cargo.toml (git = https://github.com/iced-rs/winit.git, rev 05b8ff1). hecrj said on SE Radio 713 (2026-03-25) that accessibility is his main focus for upcoming releases — but 0.15 has no date and he explicitly declined to commit to a 1.0 timeline. If any customer, employer or public-sector buyer has a WCAG/EN 301 549/Section 508 requirement, this is a hard disqualifier with no mitigation short of shipping the CLI as the accessible path.
- Release cadence forces a fork-and-pin posture. Zero patch releases for 0.14 in 8 months; 15 months between 0.13.1 and 0.14.0. The most visible iced desktop app, Halloy, does not use released iced — its Cargo.toml [patch.crates-io] pins github.com/squidowl/iced at rev 8d458d2 on 0.15.0-dev, i.e. a maintained fork. Budget for the possibility that mint-gui ends up in the same place. Sniffnet 1.5.1 on plain iced 0.14.0 is the counter-example, so it is not inevitable — but plan the escape hatch.
- Drag-and-drop of audio files may not work on Wayland, which is the default on current Fedora/Ubuntu. iced 0.14's own docs state window::Event::FileHovered, FileDropped and FilesHoveredLeft are 'Not implemented on Wayland'; winit 0.30.13's FEATURES.md contradicts this (marks DnD 'mostly works, known bugs' on Wayland, tracking #720). Unresolved without building. rfd 0.17.2's XDG-portal file dialog is a working fallback, but 'drag your masters onto the window' is a stated requirement and may simply not exist on a third of Linux desktops.
- No true cancellation without changing mint. Task::abortable's Handle aborts the stream consumer, not the OS thread; pipeline::process is a blocking call with zero yield points and zero AtomicBool (verified by grep). A Stop button today means 'stop showing you results' while eight rayon workers keep saturating the CPU and holding ~4-5 GB. Fixing it means threading a cancel token through process() and the three block loops (encode_dsf, limit_channels, resample::apply) — and the whole-buffer passes (loudness::analyze, measure_true_peak_dbtp, bitdepth::apply) have no loop to hook, so they stay uninterruptible chunks of seconds.
- In-process means a DSP panic kills the window. mint's resolve_target unwrap()s are safe by the builtin_fallback() invariant, but any panic in DSP or an abort inside libsoxr's FFI takes the GUI down with it, losing the batch's progress. Mitigation is std::panic::catch_unwind per job — sound here because mint has no cross-job shared mutable state and ops::resample::create_soxr's INIT_LOCK already de-poisons with unwrap_or_else(|e| e.into_inner()) — but catch_unwind cannot catch an abort() from the C library, and a soxr FFI abort is unrecoverable in-process. A sidecar-CLI design degrades this to one failed job.
- Bus factor of approximately one. hecrj authored ~18 of the last 20 commits (Jul 27 – Aug 12, 2026). No governance structure, no co-maintainer with merge rights in evidence. The README still credits 'the Cryptowatch team at Kraken.com' as sponsor — Kraken sunset Cryptowatch in 2023 — so the current funding position is stale-documented and unclear. The project self-describes as 'currently experimental software' in its own README as of Aug 2026.
- The 0.14 table widget is not a data grid. Reading widget/src/table.rs: eager three-pass layout over all rows, no virtualization, no sorting, no selection, no sticky headers, no column resize. Fine for a 12-50 row batch view; a 500-file library × 4 targets (2,000 rows) will need manual windowing you write yourself. Nothing in iced_aw or the wider ecosystem fills this gap.
- Config round-trip is a design decision the GUI forces and can get subtly wrong. RawTarget is Deserialize-only with deny_unknown_fields, and examples/mint.toml is include_str!-embedded as the bootstrap template and heavily commented. Deriving Serialize destroys those comments on first save; toml_edit preserves them at the cost of real code. Worse, any GUI that reimplements the four-layer merge will get the load-bearing exception wrong — format/rate/bwf are judged against the target's own layer without [defaults]. The mitigation is to never reimplement it: call config::resolve_target and display its output.
- Platform bug tail is nontrivial and slow-moving with no patch releases. Currently open, with reaction counts: slow startup on Linux from enumerating all system fonts (~2 s cold start, open since 2024-06); window transparency broken on Windows; Win11 lag during window resize (opened 2025-11); crashes on the Vulkan backend after a few seconds; Hyprland 'surface already has a syncobj attached'; incorrect initial window size on Linux; XMapRaised on X11; cannot disable dynamic linking on musl. Several trace back to winit 0.30.x, which has itself been on 0.30.x since 2024 with 0.31 perpetually pending.
- Memory pressure becomes a UI problem once the DSP shares the process. mint's measured 485-700 MB peak per job × available_parallelism() workers now lives in the same address space as the renderer. On an 8-core machine that is 4-5.5 GB; an OOM kill now loses the whole session rather than one CLI invocation. The GUI must surface and default the worker count conservatively, which the CLI never had to.
- No first-party packaging. Unlike Tauri there is no 'iced build'. You own cargo-packager (or dist) configuration, the macOS codesign/notarytool/stapler script, an Apple Developer membership, and Windows signing (SignPath is free for OSS but is a third-party dependency in your release path). Halloy's scripts are a good template but they are copy-paste, not a supported product.

### The strongest argument for

The DSP does not have to leave the process, and the Elm loop is the right shape for what mint actually is: a queue of long, blocking, CPU-bound jobs that produce one structured report each. `Task::run(rx, Message::Job).abortable()` plus a `std::thread::spawn` that calls `pipeline::process` directly is the entire concurrency design — no IPC, no JSON boundary, no second binary to version-lock, no serialization of a `ProcessingReport` that is already `Clone`. mint's libsoxr static-linking problem — the one genuinely hard packaging constraint — is already solved by `.github/workflows/release.yml` across five targets, and iced adds a binary to that matrix rather than a new native-dependency story. And the future the project clearly wants (waveform overviews, spectrum, metering) is iced's best area: `canvas` with cached geometry, the `shader` widget for a raw wgpu pass, and `iced_audio` 0.15.0 released July 2026 against iced 0.14 with knobs, meters and XY pads built by people who ship audio software.

### The strongest argument against

A mastering engineer's daily-driver tool built on iced is, today, unusable by anyone who needs a screen reader — and this is not a "not yet prioritized" gap but a six-year-old open issue whose most complete solution (PR #3281, an AccessKit integration with broad widget coverage) was closed unmerged twelve hours after it was opened, blocked on iced pinning a winit fork that `accesskit_winit` cannot semver-match. Compounding it: the two widget families mint's main screen is made of — a 22-field settings form and a batch results table — are precisely iced's weakest, with no numeric input, no validation layer, and a `table` that is an eager layout helper with no sorting, selection or virtualization. You would be adopting a framework that is strongest at the parts of mint that don't exist yet and weakest at the parts you have to ship first, while accepting a release cadence (zero patch releases in 8 months, 15 months between minors) that pushed the flagship iced app into maintaining its own fork.

---

## Slint

**Full stack as evaluated:** Slint (slint-ui/slint) — declarative `.slint` DSL compiled ahead-of-time to Rust by `slint-build` in `build.rs`, rendered by FemtoVG (default), Skia, or the software renderer over a winit (or Qt) backend. Evaluated as an in-process GUI crate for `mint`, in a Cargo workspace: `mint` (Apache-2.0 lib + CLI, no Slint dep) + `mint-gui` (bin, `slint` + path-dep on `mint`).

**Version researched:** **slint 1.17.1, published 2026-07-07** (crates.io); 1.17.0 was 2026-06-24, 1.16.1 2026-04-23, 1.15.1 2026-02-12. `1.18.0` is unreleased on `master` (adds `FlexboxLayout`). MSRV is **Rust 1.92** as of 1.17. `slint-build`, `slint-lsp`, `slint-viewer` all track 1.17.1. Repo: 23,449 stars, 951 forks, 850 open issues, last push 2026-08-12 (i.e. yesterday). crates.io license string: `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`. 1.0 shipped April 2023 — **three years and seventeen minor releases with no 2.0**, and a documented `compat-1-2` cargo-feature mechanism for adding default features without breaking. The pre-1.0 language churn (`:=` → `component`, `in/out/in-out property`) is from a Nov 2022 blog post and is long settled.

**Self-assessed verdict:** `recommend`

Slint is the stack with the least impedance against mint's actual shape: mint is a Rust library whose most valuable product — `ResolvedTarget::describe()` + `estimated_output_bytes()` recomputed per keystroke over a 22-field form — becomes a direct function call with zero marshalling, and `libsoxr`, rayon and the 700 MB `AudioBuffer` stay exactly where they are today. The existing `release.yml` already static-links soxr 0.1.3 across five targets, so the GUI inherits a dependency-free binary for free; that single fact removes the biggest packaging risk before it exists. The cost is paid in two places: Slint is not available under any OSI-approved permissive license, which collides with mint's Apache-2.0 `LICENSE` and forces a deliberate, permanent decision about what the shipped binary is licensed as; and the widget set is genuinely lean — there is no decimal `SpinBox` (source-verified: `in-out property <int> value`), no editable table, no modal dialog, and external file drag-and-drop from Finder/Explorer is only reachable through the `unstable-winit-030` escape hatch that Slint documents as removable in a minor release. Pro-audio precedent exists and is not hypothetical: WesAudio's DAW plugin UIs, Viiri Audio's Aava convolution plugin, and Zeedle (a music player with spectrum visualization) are all on Slint's own showcase. Recommend, with the licensing decision made in writing before the first `cargo add slint`.

| Criterion | Score |
|---|:--:|
| Cross-platform parity | 4 / 5 |
| Native-dep integration | 5 / 5 |
| Packaging & signing | 3 / 5 |
| UI ceiling | 4 / 5 |
| Dev velocity | 4 / 5 |
| Longevity | 4 / 5 |
| Accessibility | 4 / 5 |

### Integration with the mint core

**In-process crate, no sidecar, no IPC, no serialization boundary.** `mint-gui` takes a path dependency on `mint` and calls `mint::pipeline::process(input, &ResolvedTarget, seed) -> Result<ProcessingReport>` directly. One `cargo build`, one linker invocation, one binary. This is the dimension where Slint is strictly better than any web-shell or sidecar architecture: `ProcessingReport` (with its nested `LoudnessAnalysis` and `Option<DsdReport>`) moves across the thread boundary as a Rust value — no JSON round-trip, no schema drift between `json_report.rs` and a TypeScript type.

**Threading.** Slint's event loop must own the main thread and all components must be created on it. The pattern is `ui.as_weak()` → `std::thread::spawn(move || { ...; weak.upgrade_in_event_loop(|ui| ui.set_progress(..)) })`. mint's existing rayon fan-out lives *inside* that worker thread; rayon and Slint never touch. `slint::Timer` on the UI thread is the right tool for meter/progress repaints at 30 Hz rather than pushing on every callback. `slint::spawn_local` exists for futures but mint has no async surface, so it is not needed.

**Progress.** There is none today and Slint cannot invent it. `pipeline::process` is a single blocking call; the honest v1 is per-*job* granularity — with N inputs × M targets you get a real progress bar for free from the batch, and a per-job indeterminate spinner. Sub-job progress requires mint changes: the natural hook points are the three streaming stages (`ops::dsd::encode_dsf`'s block loop, `ops::limiter::limit_channels`, `ops::resample::apply`) plus the adaptive dither's whole-file STFT analysis pass. Threading an `&dyn Fn(f32)` through `process` is invasive but mechanical; a `crossbeam` channel or an `AtomicU64` frame counter polled by a `slint::Timer` is less invasive and good enough for a bar.

**Cancellation.** Also absent, and worse than progress — mint has no cancellation points at all. An `AtomicBool` checked at the same block-loop boundaries is the only realistic design. Until then the honest GUI behaviour is "stop button = detach and discard", which is *safe* precisely because `audio::atomic_write` stages to a sibling and renames: an abandoned job leaves no half-written deliverable. That property is worth exploiting rather than fighting.

**What mint would have to change** (all of it stack-independent except the last):
1. **`RawTarget` is `Deserialize`-only.** The GUI must hand-write a TOML emitter, and decide about comment preservation in the heavily-commented `examples/mint.toml`. Adding `Serialize` gets you a serializer that discards every comment; `toml_edit` preserving the document is the better answer but is real work.
2. **`Config::from_path` has no `from_str` sibling.** A GUI validating an in-memory edit must currently write a temp file. Three-line fix.
3. **The batch pre-flight is private.** Only `Args` and `run()` are `pub` in `cli.rs`; `probe_inputs`, `plan_outputs` and `print_dry_run` are not. `config::check_planned_outputs` and `PlannedOutput` *are* `pub`, and `audio::io_read::probe_source` is `pub`, so the GUI can rebuild the pre-flight in ~80 lines — but it will be a second implementation of the collision logic that drifts.
4. **Worker count.** `cli.rs` defaults to `available_parallelism()`. Ten concurrent jobs × the measured 485–700 MB peak RSS is 5–7 GB on an M-series laptop. In a CLI that's a conscious `--jobs`; in a GUI it is an OOM the user never chose. The GUI must cap concurrency (probably 2–3) or derive it from available RAM.
5. **Licensing/workspace split** — see below. This one *is* Slint-specific.

### Packaging & distribution

**Slint ships no packaging tooling.** There is no `slint bundle`, no equivalent of `tauri build`. You bring `cargo-packager` (CrabNebula, 0.11.x — it has a documented Slint example and a Slint auto-updater guide) or `cargo-bundle`. That is the honest ceiling on this dimension: a well-trodden but entirely DIY path.

**The good news is mint-specific and large.** `.github/workflows/release.yml` already builds five targets (`x86_64`/`aarch64-linux-gnu`, `x86_64`/`aarch64-apple-darwin`, `x86_64-pc-windows-msvc`) and **static-links libsoxr 0.1.3** — built from Debian's orig tarball with `-DBUILD_SHARED_LIBS=OFF` on Linux/macOS, and vcpkg's `x64-windows-static-md` triplet on Windows (static soxr, dynamic CRT, matching Rust's MSVC default). There is no `soxr.dll`/`libsoxr.0.dylib` to relocate into a bundle, no `install_name_tool` dance, no rpath surgery. The Slint GUI binary inherits all of it verbatim. This is the single biggest packaging de-risker and it already exists.

**Renderer choice drives size and CI cost.** Slint's default features are `renderer-femtovg` + `renderer-software` + `accessibility` + `system-tray`. FemtoVG is OpenGL-only and adds roughly 3–6 MB; against mint's current 5.2 MB release binary (aarch64-darwin, unstripped, no LTO) that lands the GUI around 12–18 MB. Skia (`renderer-skia`) is what you want for text quality — Slint's own docs concede FemtoVG's "text and path rendering quality sometimes sub-optimal" — but costs ~20–25 MB (reported ~23 MB overhead with prebuilt Skia) and **requires Visual Studio 2022 on Windows** plus a `skia-bindings` download/build in CI. Both are present on GitHub runners; the cost is minutes and cache size, not feasibility. Recommendation: build with `renderer-femtovg` *and* `renderer-skia` compiled in and let `SLINT_BACKEND=winit-skia` be the default via `BackendSelector`, so you can fall back at runtime on a machine with a broken GL/Metal stack. Also disable the default `system-tray` feature — on Linux it pulls `ksni` and mint has no tray use case.

**Signing/notarization is net-new work.** Today mint ships bare `.tar.gz`/`.zip` with no signing at all — a CLI can get away with that, a GUI cannot. Needed: an Apple Developer ID Application cert + hardened runtime + `notarytool` submit/staple → `.dmg`; an Authenticode cert (or SmartScreen reputation purgatory) → `.msi`/NSIS; and for Linux an AppImage or `.deb`. `cargo-packager` covers `.dmg`/`.app`, `.msi`/NSIS and `.deb`/AppImage from one `[package.metadata.packager]` block. Budget 3–5 days plus certificate procurement lead time.

**Gotchas specific to this repo.** (a) `release.yml` builds *without* `--features mp3`; if the GUI exposes `codec = "mp3"` the release matrix must add it, which means `cc` compiling vendored libmp3lame on all five targets — works, but it is a new C build in the release path. (b) `cmake -DCMAKE_POLICY_VERSION_MINIMUM=3.5` is already needed for soxr under cmake 4.x; that hack is load-bearing and will need re-checking as runners move. (c) Windows GUI apps need `#![windows_subsystem = "windows"]` or a console window appears — Slint's desktop docs call this out explicitly. (d) Linux runtime needs Wayland-or-X11, glibc and **d-bus**; Slint's stated Linux support is "most recent LTS or newer at the time of a Slint release", which is narrower than mint's current static CLI.

### UI capability ceiling

**The widget set is lean — 19 exported std widgets.** Verified from `internal/compiler/widgets/fluent/std-widgets.slint`: `AboutSlint, StandardButton, Button, ScrollView, Palette, StyleMetrics, CheckBox, ComboBox, GroupBox, LineEdit, RadioGroup, ListView, StandardListView, ProgressIndicator, Slider, SpinBox, Spinner, TabWidget, Switch, TextEdit, TimePickerPopup, DatePickerPopup, StandardTableView` plus `VerticalBox/HorizontalBox/GridBox`. Language-level additions in 1.17: `Tooltip`, `DragArea`/`DropArea`, `SystemTrayIcon`, `MenuBar`/`ContextMenuArea` (native via `muda` on Windows and macOS since 1.10), `StyledText` with markdown (1.16), `KeyBinding`/`@keys(...)` (1.16).

**Four concrete gaps for mint's forms.** (1) **`SpinBox` is integer-only** — I read the source: `in-out property <int> value; in property <int> step-size`. mint's `lufs` (−14.0), `ceiling_dbtp` (−1.0), `warn_limiting_db` (1.0) and `max_limiting_db` (8.0) are all `f64` with decimals. You write your own `FloatSpinBox` from `TextInput` + two `Button`s. Half a day, but it is your widget to maintain forever. (2) **`StandardTableView` cells are `StandardListViewItem` — text only, no inline editing, no per-cell widgets.** A table of targets with editable cells means hand-rolling rows inside a `ListView`. 1.17's two-way bindings on model rows (#2013) makes that materially less painful than it was six months ago. (3) **No splitter/resizable pane.** (4) **No OS-modal dialog** — issue #6607 is open and the maintainer note is that winit only supports owner-windows on Windows. In-window overlay modality (a full-bleed `Rectangle` + a `TouchArea` that swallows clicks) is the standard workaround and is what most modern apps do anyway; `rfd` file dialogs *are* genuinely OS-modal.

**Live meters, waveform and spectrum: genuinely good.** Three real routes. (a) `SharedPixelBuffer<Rgba8Pixel>` + `Image::from_rgba8` — render the spectrogram/waveform yourself in Rust into a pixel buffer and hand it over; this is the documented pattern and it is exactly right for a spectrum view fed by `rustfft`, which mint already depends on. (b) The `Path` element with `commands` bound to an SVG path string for a vector waveform that scales cleanly on HiDPI. (c) WGPU texture import (`Image::to_wgpu_29_texture`, `FemtoVGWGPURenderer`) if you ever want a GPU-side scope. Property updates driven by a `slint::Timer` at 30–60 Hz are what the reactive property system is built for; LU/LUFS bars and true-peak indicators are trivial. **Zeedle**, a music player with spectrum visualization, is on Slint's own showcase.

**Elegance ceiling is set by how much you build, not by the toolkit.** The DSL is a real scene graph: layouts, `states`, `transitions`, `animate` with easings, custom `component`s, GPU-composited drop-shadows and (1.17) inner-shadows, conic/radial gradients with `at <x> <y>`. Theming: `Palette` plus fluent (default since 1.16), cupertino, material, and native/Qt styles, with automatic dark/light following the system. But **you cannot deeply re-skin the built-in widgets** — they are compiled in from the chosen style. A distinctive look means writing your own widget layer from primitives, which is exactly what `coop` and `SurrealismUI` on the showcase are. HiDPI is handled properly: Slint's `px` is a logical unit scaled by the window's device-pixel-ratio, with `phx` available when you need physical pixels; Skia added subpixel glyph positioning in 1.16 and text rasterization moved to `swash`.

**Direct pro-audio precedent** on Slint's showcase: WesAudio (commercial, "complete redesign of their DAW plugin user interfaces"), Viiri Audio's Aava convolution plugin, Clockworks (live-event timecode distribution), Accidental Synthesizer (macOS four-oscillator synth, Apache-2.0), Chiptrack (Game Boy sound-chip sequencer). Plus LibrePCB 2.0/2.1 (Jan/May 2026) — a large CAD app that migrated its Qt GUI to Slint and added a light theme in 2.1, which is the best available evidence that Slint scales to a dense professional desktop tool.

### Development velocity & maintenance

**The live-preview tooling is the real differentiator and it is not marketing.** The VS Code extension (`Slint.slint`) and standalone `slint-viewer` reload a `.slint` file on save with no recompile — layout, colors, animations, property values update as you type. Design Mode goes further: drag-and-drop of elements, visual drop zones inside layouts, interactive property editing, all written *back* to the `.slint` source. Separately, the `live-preview` cargo feature plus `SLINT_LIVE_PREVIEW=1` reloads `.slint` inside your actual running application, so you can iterate on the target-editor layout while a real 96 kHz render is in flight. Nothing else in the native-Rust space has that loop.

**But the boilerplate for ~22 fields is real and Slint offers no reflection.** Each field needs: a property in `.slint`, a generated Rust setter/getter or a callback, a TOML read path, and a TOML write path. Times 22, times "and again inside a repeated model row for the target list". Realistic first-cut estimate for a Rust-fluent pair: ~1 week for a single-target editor with live `describe()`, ~2 weeks more for the multi-target list + batch runner + report table, ~1 week for packaging/signing. Call it 4–6 weeks to a defensible v1. Maintenance afterwards is *low* — one binary, one language, an API that has not broken in three years.

**Three specific velocity taxes.** (1) **The DSL is a second language with a hard ceiling.** No loops beyond `for x in model`, no general expressions, no user functions until you escape to Rust. Everything non-trivial crosses the boundary as a callback, and a Rust-fluent team hits that wall daily at first. Slint's own FAQ argues the DSL is no harder than learning a toolkit API, which is fair — but it is not *free*. (2) **The TOML emitter is yours to write** and it is the highest-risk piece of glue in the project, because a GUI that silently rewrites a hand-commented `mint.toml` and drops the comments will be used exactly once. `toml_edit` preserving the document, not `serde` round-tripping, is the design. (3) **Number formatting is a trap here specifically.** Slint 1.17 changed float↔string conversion to use *the locale's* decimal separator (`Platform.decimal-separator`, #10857). On a `de_DE` machine — which this repo's author is on — a `Text { text: root.lufs; }` renders `-14,0`. Format every number in Rust and pass `SharedString` into the UI; never let `.slint` stringify a value that will reach TOML.

**Designer tooling:** a Figma plugin exists (since 1.10) but it is an import aid, not a round-trip. Widget ecosystem outside the std set is thin — `coop`, `SurrealismUI`, `vivi` — nothing you would bet a product on.

### Risks

- LICENSING IS A ONE-WAY DOOR AND MINT'S OWN LICENSE FILE IS UNFINISHED. mint ships an unmodified Apache-2.0 LICENSE whose copyright line is still the literal placeholder `Copyright [yyyy] [name of copyright owner]` (line 189), and Cargo.toml has NO `license` field at all. Slint is `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` — none of those is OSI-approved except GPLv3. Choosing GPLv3 makes the shipped GUI binary GPLv3 as a whole (Apache-2.0 is one-way compatible with GPLv3, so mint's own files can stay Apache-2.0 — but the combined work cannot) and permanently forecloses the Mac App Store. Choosing the Royalty-free 2.0 license is free and permits closed distribution on desktop, but it is a field-of-use-restricted, non-OSI license that Debian/Fedora/nixpkgs will not accept, so any distro package must be rebuilt under GPLv3 by the packager. Decide this in writing, fill in the copyright placeholder, add `license =` to both crates' Cargo.toml, and split the workspace so the `mint` CLI never depends on Slint — BEFORE the first `cargo add slint`.
- EXTERNAL FILE DRAG-AND-DROP — the single interaction an audio engineer will judge the app on in the first ten seconds — is not a supported Slint feature as of 1.17.1. `DragArea`/`DropArea` (new in 1.17, 2026-06-24) are explicitly in-window only; Slint's own release post says "dragging to other applications and receiving drops from them is in development upstream in winit". The only route today is the `unstable-winit-030` cargo feature + `WinitWindowAccessor::on_winit_window_event()` (PR #5531, merged 2024-07-04) to intercept the raw winit drag events — and that module is documented as "behind a feature flag and may be removed or changed in future minor releases, as new major Winit releases become available". Compounding it: winit recently REPLACED `DroppedFile`/`HoveredFile` with `DragEntered`/`DragMoved`/`DragDropped`/`DragLeft`, so the event names you write against will change, and Wayland support for it is recent. You get file paths but no integration with Slint's scene graph, so hover highlighting is hand-rolled. Prototype this on day 4, on all three OSes including Wayland, before committing.
- NO SUB-JOB PROGRESS AND NO CANCELLATION EXIST IN MINT, AND THE MEMORY MODEL MAKES THAT VISIBLE. `pipeline::process` is one blocking call over a whole-file `Vec<Vec<f64>>`; a five-minute 96 kHz master is 461 MB of audio at 485-700 MB peak RSS. A GUI user who drops a 45-minute live master and gets a frozen indeterminate spinner for four minutes with no Stop button will conclude the app is broken. Worse: `cli.rs` defaults worker count to `available_parallelism()`, so ten parallel jobs is 5-7 GB — a conscious `--jobs` choice on a CLI, an unexplained OOM in a GUI. The GUI must cap concurrency and mint must grow cancellation checks at the streaming-stage block boundaries (`encode_dsf`, `limit_channels`, `resample::apply`). Mitigating factor worth exploiting: `audio::atomic_write` stages to a sibling and renames, so an abandoned job leaves no partial deliverable on disk.
- THE TOML WRITE-BACK PATH IS THE HIGHEST-RISK GLUE IN THE PROJECT AND SLINT DOES NOT HELP. `RawTarget` derives `Deserialize` only, has ~22 `Option` fields, and `#[serde(deny_unknown_fields)]` makes any emitter bug a hard load error rather than a warning. `examples/mint.toml` is heavily commented AND `include_str!`-embedded in `cli.rs` as the bootstrap template — a GUI that rewrites a user's config with serde and silently drops every comment will be used once. Use `toml_edit` document-preserving edits, not `#[derive(Serialize)]`. Also note `Config` exposes only `from_path`, no `from_str`, so validating an in-memory edit currently requires a temp file.
- SINGLE-VENDOR CONCENTRATION. SixtyFPS GmbH is a small German company (Brandenburg, three co-founders — Goffart, Hausmann, Jana, EU co-funded) and Slint has no foundation, no independent governance, and a CLA that assigns rights to the company. The commitment to keep offering the Royalty-free license lives in that CLA, i.e. it is a company promise, not a charter. If the company is acquired or pivots harder toward embedded (which is where the per-device royalty revenue is), desktop maintenance is the thing that gets cut. 850 open issues. Counterweights: 23.4k stars, commits landing daily as of 2026-08-12, a 3-year unbroken 1.x API, and a paying customer base including WesAudio and OTIV — but this is still a bet on one vendor.
- DESKTOP IS A RECENT PRIORITY, NOT A FOUNDING ONE. Slint's own blog post "Making Slint Desktop-Ready" is dated 2025-10-09 and opens by conceding they "often encounter questions or concerns about whether Slint is a viable option for creating full-featured desktop applications". It listed eight roadmap items; by 1.17 (2026-06-24) five to six shipped (keyboard shortcuts, tray, tooltips, two-way model bindings, in-app D&D, rich-text first step) but item 3 (global cross-application D&D) and item 4 (modal windows, issue #6607) have not. The crate description on crates.io still reads "for embedded devices and desktop applications" — in that order. You are adopting a toolkit ~10 months into its desktop push.
- NO DECIMAL SPINBOX, NO EDITABLE TABLE — you will write and maintain your own widget layer. Source-verified: `SpinBoxBase` is `in-out property <int> value`. mint has four decimal-valued form fields (`lufs`, `ceiling_dbtp`, `warn_limiting_db`, `max_limiting_db`). `StandardTableView` cells are text-only `StandardListViewItem`s with no inline editing. Neither is hard, both are permanent maintenance surface, and every custom widget you write also loses AccessKit's automatic accessibility annotations and needs manual `accessible-role`/`accessible-value`/`accessible-action-*`.
- SKIA vs FEMTOVG IS A FORCED TRADE BETWEEN TEXT QUALITY AND BUNDLE SIZE. Slint's default is FemtoVG, and Slint's own docs admit its "text and path rendering quality sometimes sub-optimal" — in a dense, text-heavy mastering UI on a 5K display that is the first thing a designer will complain about. Skia fixes it at ~20-25 MB and a `skia-bindings` build that needs Visual Studio 2022 on Windows. Neither is a blocker; you just cannot have both small and beautiful.
- MSRV 1.92 AND A FAST-MOVING DEP TREE. Slint 1.17 raised MSRV to Rust 1.92 and the 1.17 changelog shows churn in pinned upstreams (wgpu 27→28→29 with renamed cargo features, fontique/parley 0.9→0.10 with a renamed feature, accesskit updates, winit 0.30). mint today is 135 crates in Cargo.lock; adding Slint with winit+femtovg+accesskit roughly triples that. Nothing here is broken, but `cargo update` becomes a thing you schedule rather than a thing you run.

### The strongest argument for

mint's highest-value UI feature already exists as a pure function and Slint is the only evaluated stack where wiring it costs nothing. `ResolvedTarget::describe(source_rate) -> Vec<String>` and `estimated_output_bytes()` are exactly the "what will this actually do, and how big will it be" answer an engineer wants recomputed on every keystroke of a 22-field form — and in Slint that is a direct Rust call in a callback handler, not a JSON round-trip across a process boundary or an FFI layer. The same property holds for the hard part: `libsoxr` links exactly as it does today (the existing `release.yml` recipe that static-links soxr 0.1.3 across five targets carries over unchanged, so there is no DLL/dylib to relocate into a bundle), rayon fans out inside a worker thread that Slint never touches, the 700 MB `AudioBuffer` is never copied because there is nothing to copy it *to*, and `ProcessingReport` arrives at the UI as a Rust value via `Weak::upgrade_in_event_loop`. One binary, one linker line, one language. And the pro-audio precedent is not theoretical — WesAudio's DAW plugin UIs, Viiri Audio's Aava convolution plugin and Zeedle's spectrum view are all shipping on Slint today.

### The strongest argument against

You are permanently attaching a non-OSI, field-of-use-restricted license to a project whose own `LICENSE` is Apache-2.0 with the copyright line still unfilled — and Slint offers no permissive escape. The three options are all lossy: GPLv3 relicenses the entire shipped GUI as copyleft (legal, since Apache-2.0 is one-way GPLv3-compatible, but it closes the Mac App Store and makes every downstream integrator's lawyer read the whole tree); Royalty-free 2.0 is free and permits closed distribution on desktop but is not OSI-approved, so Debian, Fedora and nixpkgs cannot ship it and must rebuild under GPLv3 themselves; or you pay SixtyFPS GmbH. The `AboutSlint` attribution obligation itself is trivial — five lines under a Help menu Slint already renders natively — but the structural asterisk on the license is not, and it never goes away. That is a permanent constraint bought for a toolkit whose standard widget set does not contain a spin box that can hold `-14.0`, cannot show a modal dialog, and cannot yet accept a WAV file dragged out of Finder without an explicitly-unstable feature flag.

---

## Dioxus

**Full stack as evaluated:** Dioxus 0.7 (RSX + signals) with `dioxus-desktop` (wry/tao system webview) as the shipping renderer, `mint` linked as an in-workspace path dependency; Tailwind v4 via the built-in `dx` watcher; `dioxus-native`/Blitz treated as a future migration target, not a 2026 option.

**Version researched:** Stable: **dioxus 0.7.10, released 2026-07-30** (0.7.0 was 2025-10-31). `dioxus-desktop` 0.7.10, depending on **wry ^0.53.5** and **tao ^0.34**. `dioxus-native` also publishes 0.7.10 but sits on **Blitz, which is still self-described as alpha** ("ready for experimentation and early adoption, but not ready for production usage"; its own roadmap targeted beta by end of 2025 and has slipped). Pre-release line: **0.8.0-alpha.1 (2026-07-31)**, 0.8.0-alpha.0 (2026-05-19); no 0.8 release date announced. License MIT OR Apache-2.0. ~38.6k GitHub stars.

**Self-assessed verdict:** `viable-with-caveats`

For mint specifically, Dioxus's decisive advantage is real: the GUI is the same process and the same crate graph as the DSP, so `mint::pipeline::process(&path, &resolved, seed)` is a plain function call and `ProcessingReport`, `LoudnessAnalysis` and `RawTarget` never need a `Serialize` impl they don't have. That single fact removes the largest chunk of Tauri's cost for this codebase. The rendering, however, is still the system webview (wry 0.53/tao 0.34) — `dioxus-native`/Blitz is alpha and the 0.8 roadmap still lists "native widget support" as future work — so mint inherits Tauri's exact Linux webkit2gtk problems (DMABUF blank-window, distro-version drift) with none of Tauri's packaging maturity. Packaging is where this choice actually bleeds: `dx bundle` produces .app/.dmg/.msi/.deb/.rpm/.appimage but the docs explicitly say Dioxus "doesn't provide built-in utilities" for signing/notarization and point you at Tauri's docs, and mint's release binary hard-links `/opt/homebrew/opt/libsoxr/lib/libsoxr.0.dylib` (verified with `otool -L`), which no bundler here relocates for you. The UI ceiling — 22-field forms, live LUFS/GR meters, future waveform views, theming — is the best available to a Rust team, and hot-patching plus zero-config Tailwind make iteration genuinely fast. The reason this is "with caveats" and not "recommend" is governance: a 3-person company on a 2023 $520K seed, one dominant maintainer, no 1.0, and a 0.8 that openly plans "a substantial overhaul of platform APIs".

| Criterion | Score |
|---|:--:|
| Cross-platform parity | 3 / 5 |
| Native-dep integration | 4 / 5 |
| Packaging & signing | 3 / 5 |
| UI ceiling | 5 / 5 |
| Dev velocity | 4 / 5 |
| Longevity | 2 / 5 |
| Accessibility | 4 / 5 |

### Integration with the mint core

**In-process crate, no sidecar.** Cargo workspace: `mint` (existing lib, unchanged) + `mint-gui` with `mint = { path = ".." }` and `dioxus = { version = "0.7", features = ["desktop"] }`. Everything mint exposes from `src/lib.rs` (audio, cli, config, dither, json_report, ops, pipeline) is directly callable from an event handler.

**Is "no IPC serialization" actually true for dioxus-desktop? Partly — and the part that's true is the part mint needs.** There *is* a boundary: dioxus-desktop renders into a wry webview and ships DOM mutations across it using a sledgehammer_bindgen binary protocol (since 0.5; ~1/5 the time and 1/2 the latency of the old JSON path), with DOM events coming back through wry's IPC as serialized `PlatformEventData`. What *never* crosses is your application data. `RawTarget`, `ResolvedTarget`, `ProcessingReport`, `DsdReport`, `LoudnessAnalysis` live in Rust `Signal`s and are rendered by Rust code into RSX. Contrast with Tauri, where every one of those types needs a `Serialize` impl plus a hand-maintained TypeScript mirror — and `config.rs` currently derives `Deserialize` only, for all 22 `RawTarget` fields and every enum (`DitherMode`, `OnClipPolicy`, `LimiterCharacter`, `DsdRate`, …). With Dioxus you add zero derives. That is a concrete, countable saving of maybe 400 lines of DTO plus a permanent drift hazard. (Note the 0.8 roadmap lists "proper `event.target` access across webview boundaries" as *planned* work — so the boundary is real enough to have ergonomic bugs today.)

**Threading.** The docs are blunt: "This will block the main thread and make the UI unresponsive. Do not do this!" and recommend `std::thread::spawn` for expensive work. So the pattern is: keep mint's rayon pool (`cli.rs:147` already builds one), spawn the whole batch on a non-UI thread, and communicate back through `use_signal_sync` / `SyncSignal<T>` (`Signal<T, SyncStorage>` is `Send + Sync + Copy`; there is an official `packages/signals/examples/send.rs`). Caveat I could not confirm from the docs: the async page does **not** document whether a signal write from a foreign thread wakes the tao event loop and schedules a re-render. It does work (that's what the example is for), but this is under-documented and is the #1 thing the prototype must prove.

**Progress + cancellation: mint has neither, and Dioxus does not help.** `pipeline::process` is one blocking call with no callback and no cancellation token. Options, cheapest first: (a) job-level granularity only — with N inputs × M targets fanned out over rayon you get a real progress bar for free at (input × target) resolution, and that is probably enough for a mastering batch; (b) add an `Option<&dyn Fn(Stage, f32)>` progress sink to `process()` and call it at the seven chain boundaries (loudness → resample → verify → ceiling → quantize/encode) — a small, honest change; (c) cooperative cancellation needs an `&AtomicBool` checked inside the block loops (`ops::dsd::encode_dsf`, `ops::limiter::limit_channels`, `ops::resample::apply` are already block-structured, so the check points exist). Note that killing a job mid-flight is *safe* today because every writer stages through `audio::atomic_write` — no partial file lands on the final path. That is a nice property to lean on.

**Memory.** Whole-file `Vec<Vec<f64>>` at 485–700 MB peak, and a webview process alongside it. Not a problem in itself, but two jobs in parallel is already >1 GB and the webview adds 100–300 MB; the GUI should surface/limit worker count rather than default to `num_cpus`.

**What mint would have to change:** nothing to *start*. To ship well: (1) a `Serialize`-equivalent TOML writer — this is required regardless of GUI stack, since `RawTarget` has no `Serialize` and `examples/mint.toml` is heavily commented (use `toml_edit` to preserve comments; a plain `toml::to_string` round-trip would destroy the template that `cli.rs` `include_str!`s); (2) the progress sink above; (3) possibly promote `probe_source`/`plan_outputs`/`check_planned_outputs` to a stable public API so the GUI's live dry-run panel isn't reaching into `cli.rs` internals.

### Packaging & distribution

**What `dx bundle` gives you.** macOS `.app` + `.dmg`; Windows `.msi` (WiX candle/light) and `.exe`; Linux `.deb`, `.rpm`, `.appimage`. Selected with repeated `--package-types`. **Cross-compilation is explicitly unsupported** — the docs say "you can't build macOS apps from Windows, Linux apps from Mac, etc." and recommend a GitHub Actions matrix. mint's `.github/workflows/ci.yml` already runs a three-OS matrix with per-OS libsoxr provisioning (including the hand-written `soxr.pc` for vcpkg on Windows), so this costs almost nothing — you add `libwebkit2gtk-4.1-dev` + `libgtk-3-dev` to the Linux job and `dx` to all three.

**Signing/notarization: not provided.** The Dioxus bundle docs say verbatim that "Dioxus doesn't provide built-in utilities for this, so you'll need to figure out signing by reading 3rd-party documentation," and link Tauri's docs. So macOS is a manual `codesign --deep --options runtime` + `xcrun notarytool submit --wait` + `xcrun stapler staple` chain in CI (or `rcodesign` for a Linux-hosted signer), and Windows is your own `signtool`/Azure Trusted Signing step over the `.msi`. Tauri ships this; Dioxus does not. Budget 1–2 days plus the Apple Developer / EV-cert acquisition lead time.

**libsoxr is the real packaging problem, and it is mint-specific.** Verified locally: `otool -L target/release/mint` shows `/opt/homebrew/opt/libsoxr/lib/libsoxr.0.dylib` as an absolute load command. Homebrew ships **no `libsoxr.a`** (only `.dylib` symlinks into the Cellar), and `libsoxr-sys` 0.1.4's build.rs is a bare `pkg_config::probe_library("soxr")` with no vendored/static option. So a `.app` on a machine without Homebrew will die at launch. Dioxus 0.7 did add automatic dylib/framework bundling — but the release notes describe it as "intercepts `-framework` flags in the linker"; soxr's `.pc` emits `-L… -lsoxr`, **not** `-framework`, so I would not expect it to be caught (unverified — worth 30 minutes of the prototype). The fix is manual and identical under any GUI stack: copy `libsoxr.0.dylib` into `Contents/Frameworks/`, `install_name_tool -change` the load command to `@executable_path/../Frameworks/`, **then** re-sign (signing must come last), then notarize. A universal binary needs libsoxr built from source for both arches or `lipo`'d, because Homebrew gives you host-arch only. Linux `.deb`/`.rpm` can just `Depends: libsoxr0`; AppImage must bundle it. Windows must ship `soxr.dll` next to the exe inside the MSI. Long-term the clean answer is a vendored `soxr-src`-style crate that CMake-builds and static-links — that removes the problem from every platform at once and I'd score packaging a point higher if it existed.

**Windows WebView2.** Preinstalled on Windows 11, *not* on Windows 10. `Dioxus.toml` exposes `bundle.windows.webview_install_mode` (`EmbedBootstrapper` / `OfflineInstaller` / `FixedVersion`), inherited from the tauri-bundler lineage, but issue #1321 reports the offline/fixed modes not actually being embedded. Verify on a clean Win10 VM.

**Known open bundler bugs.** Discussion #4356 ("Desktop bundle not working (Windows – Linux)", reproduced on 0.6.3, 0.7.0-alpha.2 and 0.7.1, still **unresolved**): the bundled `.deb` launches with **no CSS and no icons**, partially worked around by building with `-r`; the `.msi` case reported a broken native (SQLite) dependency. For mint that maps directly onto "the Tailwind stylesheet and the app icon may silently not be in the shipped bundle." This is exactly the class of bug that only shows up after you stop using `dx serve`.

**Size (estimate, not measured).** mint's release binary is 5.4 MB today. Expect a Dioxus desktop binary in the **15–25 MB** range and a `.dmg`/`.msi` in the **8–20 MB** range, since the webview engine is the OS's. Roughly 5–10× smaller than the Electron equivalent, comparable to Tauri, larger than egui.

### UI capability ceiling

**The highest ceiling available to a Rust team, with one specific hole.**

*Dense forms.* 22 `Option` fields per target, four merge layers, N targets. HTML/CSS is the right tool: a two-column grid, per-field "inherited from `[defaults]`" / "from preset `sacd`" / "explicit" provenance badges, a strikethrough-and-tooltip for the deliberate `[defaults]`-exclusion on `format`/`rate`/`bwf`. In egui or iced this is a week of layout fighting; in Tailwind it's a `grid grid-cols-[1fr_auto] gap-x-4` and done. Table of targets × inputs with the predicted output paths and sizes from `estimated_output_bytes()`: trivial. Long batch log with virtualized scroll: trivial.

*Live meters.* `ProcessingReport` today is end-of-job only, so "live" means either job-granularity or the progress-sink change described above. Either way, rendering an LUFS/true-peak/GR meter as a CSS-transform bar at 30–60 fps is cheap; each frame is a signal write and a small DOM mutation over the binary protocol. Fine.

*Waveform / spectrum — this is the hole.* dioxus-desktop has no Rust-side canvas. You either (a) precompute peak/RMS envelopes in Rust (mint already has the whole file in RAM, so min/max per pixel column is nearly free) and emit an SVG `<path>` or a set of `<rect>`s — perfectly good for an overview and a loudness-over-time strip, and this is what I'd do; or (b) for a real scrolling spectrogram, push Float32 data to a JS `<canvas>`/WebGL via `document::eval` and its `dioxus.send`/`dioxus.recv` channel, which works but is the one place you'll be writing JavaScript in a Rust-only project, and it re-serializes across the boundary. `rustfft` is already a dependency, so the DSP side of a spectrum view is free; only the blitting is awkward.

*Theming/HiDPI.* Best in class. CSS variables give you light/dark and a studio-dark theme for free; HiDPI is the webview's problem and it solves it correctly on all three OSes, including per-monitor DPI changes on Windows — which is a genuine pain point in immediate-mode Rust GUIs.

*Native file dialogs.* Two options and both are fine: `<input type="file">` opens the real OS chooser, or (better, and what I'd use) call `rfd` directly from the event handler — it's in-process, native, and returns `PathBuf`s straight into your `Signal`. Issue #3439 ("File dialog issues on Linux desktop") suggests the HTML route has Linux rough edges; `rfd` sidesteps them.

*Drag-and-drop of audio files.* The weakest UI item. wry installs a native file-drop handler, and on Windows that handler **suppresses the HTML5 drag events** — PR #3046 works around it by re-synthesizing HTML events via eval, and issues #986/#1921 track `DragEvent` not carrying file data. Assume drag-and-drop needs per-OS testing and possibly a `use_wry_event_handler` fallback rather than the pretty `ondrop` you'd write first.

*Component kit.* The official `dioxus-primitives` / `dioxus-components` (shadcn-style, WAI-ARIA APG, Playwright-tested) exists on GitHub — but **crates.io shows `dioxus-primitives` at version 0.0.0, a 12-line placeholder published 2025-04-13 with 1,573 downloads**. So today you either git-dependency it or hand-roll number inputs, selects, comboboxes and validation display. For an 11-preset dropdown and 22 fields that's real but bounded work.

### Development velocity & maintenance

**Strong, and the strongest part is genuinely differentiating.**

*Hot reload.* 0.7's headline is subsecond hot-*patching* of Rust, not just RSX — you edit a component body and the running app updates without losing state. For a config editor where you're tuning layout against a 22-field form, that removes the single biggest tax of Rust GUI work (30–90 s rebuild cycles). Documented limits: patching happens at explicit `subsecond::call()` integration points rather than implicitly, and **adding or removing a struct field between patches does not migrate state** — so when you change `RawTarget`'s shape you restart. Layout/CSS/RSX iteration is where it pays; type-level changes it doesn't help with.

*Tailwind.* Zero-config in 0.7: `dx` detects a root `tailwind.css` and runs the watcher itself, auto-detecting v3 vs v4. No node build step to maintain. This matters more than it sounds for a small team with no frontend person.

*One language, one debugger, one test runner.* You can write `#[test]`s that call `resolve_target()` and assert on what the form should show, in the same `cargo test` run as the DSP tests. With Tauri you'd be running vitest+cargo and keeping TS types in sync.

*Where it costs.* (1) No published component library, so ~22 form controls and their validation UI are hand-written the first time. (2) RSX macro errors can be cryptic; a mismatched brace inside `rsx!` produces bad spans. (3) `dx` is an opinionated cargo wrapper that intercepts the linker for hot-patching — that's fine until it meets a pkg-config-linked C library, which is unproven here. (4) Compile times: adding dioxus-desktop to a workspace that already pulls symphonia, rustfft, flacenc and rayon will push cold builds well past a minute; keep `mint-gui` a thin crate so incremental rebuilds stay small.

*Maintenance.* The honest cost is upgrade churn, not day-to-day. 0.4→0.5 was a signals rewrite, 0.5→0.6 and 0.6→0.7 were both large, and 0.8 is announced as "a substantial overhaul of platform APIs" plus a windowing swap (tao → winit, waiting on winit 0.31). Budget a multi-day migration per minor release, roughly annually. There is a published migration guide each time, which helps.

Realistic effort for a Rust-fluent pair: **3–5 weeks** to a good v1 (project list, target editor with live derived-chain panel, batch run with per-job progress, report view with the DSD sub-report), plus **1–2 weeks** for packaging/signing/notarization across three OSes. The packaging fortnight is not optional and is the part teams underestimate.

### Risks

- Governance/bus factor. Dioxus Labs is a ~3-person YC company (founded 2021, Panama) whose last disclosed funding is a $520K seed from July 2023; jkelleyrtp is the dominant contributor. The framework is not 1.0, and 0.8 is explicitly scoped as 'a substantial overhaul of platform APIs' including a tao→winit migration. MIT/Apache-2.0 means you can always fork, but forking a webview framework + a browser engine is not a real option for an audio team. This is the single biggest reason not to pick Dioxus.
- Linux webkit2gtk, inherited whole from Tauri with no mitigation of your own. wry 0.53 uses webkit2gtk-4.1; WebKitGTK's DMABUF renderer fails to initialize on NVIDIA proprietary drivers and several Wayland compositors and silently renders nothing — the ecosystem-wide fix is to export WEBKIT_DISABLE_DMABUF_RENDERER=1 before the webview initializes (Tauri documents this; Wails, Yaak and others carry the same bug). You will ship a launcher wrapper or a std::env::set_var at the top of main(), and you will still get 'blank window' bug reports from Linux users.
- libsoxr in the shipped bundle. Verified: target/release/mint links /opt/homebrew/opt/libsoxr/lib/libsoxr.0.dylib absolutely; Homebrew ships no static .a; libsoxr-sys 0.1.4's build.rs is a bare pkg_config probe with no vendoring. dx's 0.7 auto-bundling is documented as intercepting -framework flags, which soxr's .pc does not emit. Manual install_name_tool + Contents/Frameworks + re-sign is required, and a universal macOS binary needs libsoxr built from source for both arches. This is stack-independent but it lands squarely in the dx bundle step and nothing in the toolchain warns you.
- Unresolved dx bundle asset bugs. Discussion #4356, reproduced on 0.6.3 / 0.7.0-alpha.2 / 0.7.1 and still open: the Linux .deb ships with no CSS and no icons (partially fixed by building with -r), and the Windows .msi broke a native dependency. Translation for mint: your Tailwind output and app icon may be missing from the artifact even though dx serve looks perfect. Gate every release on launching the actual bundle in a clean VM, not on dx serve.
- No signing or notarization support. The Dioxus bundle docs state outright that Dioxus 'doesn't provide built-in utilities' for signing and refer you to Tauri's documentation. You own the entire codesign/notarytool/stapler and signtool pipeline. Tauri would have given you this; Dioxus does not.
- Blitz/dioxus-native is not a 2026 escape hatch. Blitz still self-describes as alpha and 'not ready for production usage'; its own beta target of end-2025 has slipped; the 0.7 release notes say 'Bear in mind that Blitz is still considered a work in progress. We have not focused on performance,' and 0.8 still lists 'native widget support in Dioxus Native' as future work. Do not choose Dioxus on the promise that you can drop the webview later — choose it on whether the webview is acceptable today.
- Cross-thread signal updates are under-documented. The async guide says to use std::thread::spawn for blocking work but never explains whether writing a SyncSignal from that thread wakes the event loop and schedules a re-render. use_signal_sync/SyncSignal exist and there is an upstream send.rs example, so it works — but for mint this is load-bearing (a 5-minute 96 kHz job is tens of seconds of blocking DSP) and it is the first thing the prototype must prove empirically.
- Three different browser engines, not one. WebView2 on Windows is Chromium; WKWebView on macOS and WebKitGTK on Linux are WebKit, and WebKitGTK's version varies by distro. You get Safari-vs-Chrome CSS divergence for free, on machines you don't control, and you cannot pin the engine the way Electron can. Budget CSS QA on all three, and avoid bleeding-edge CSS.
- Memory: 485–700 MB per in-flight job plus a webview process (100–300 MB). Two parallel jobs already exceed 1 GB. The GUI must expose and default the worker count conservatively rather than inheriting cli.rs's pool sizing, or a 3-target batch on an 8-core laptop will swap.
- Drag-and-drop of audio files is quirky per-OS. wry's native file-drop handler suppresses HTML5 drag events on Windows (PR #3046 works around it with synthesized events via eval); DragEvent historically didn't carry file data (#986, #1921). Plan to test on all three OSes and to fall back to use_wry_event_handler.
- No published component library. dioxus-primitives is version 0.0.0 on crates.io — a 12-line placeholder from April 2025 with 1,573 downloads. The real components live in the DioxusLabs/dioxus-components git repo. You'll be pinning a git rev for a dependency that carries your entire form UI, or hand-rolling.
- TOML round-tripping is a mint problem the GUI forces you to solve. RawTarget derives Deserialize only, and examples/mint.toml is heavily commented and include_str!-embedded in cli.rs as the bootstrap template. A naive toml::to_string write-back would strip every comment from the user's project file. Use toml_edit and treat comment preservation as a requirement, not a nicety.

### The strongest argument for

mint's types were never designed to leave the process, and with Dioxus they never have to. `Config`, `RawTarget` and all 12 of its enums derive `Deserialize` only; `ProcessingReport`, `DsdReport` and `LoudnessAnalysis` derive nothing serializable at all — `json_report.rs` exists precisely as a hand-written DTO layer because the internal structs aren't a stable wire schema. Every out-of-process GUI (Tauri, an Electron shell, a CLI-driving app) makes you build and then permanently maintain a second such layer, plus TypeScript mirrors, for a *22-field* config type with a *four-layer merge* whose subtleties (the `[defaults]`-excluded `format`/`rate`/`bwf` own-layer rule) are exactly the kind of thing that drifts silently across a serialization boundary. With dioxus-desktop the form binds a `Signal<RawTarget>`, calls `resolve_target()` on every keystroke, and renders `describe()` and `estimated_output_bytes()` live — the dry-run *is* the UI, sharing the identical code path the CLI uses, so the two can never disagree. And you get that while keeping HTML/CSS/Tailwind, which is the only toolkit in the Rust world where a dense mastering-console UI can actually look good.

### The strongest argument against

You take on all of Tauri's risk and none of its safety net. The renderer is the same wry/webkit2gtk stack, so you inherit the Linux blank-window class of bug and three divergent browser engines verbatim — but Tauri is governed by a foundation with a large maintainer base and a bundler that does signing and notarization, whereas Dioxus is a 3-person company on a 2023 $520K seed whose bundle docs say "Dioxus doesn't provide built-in utilities for this, so you'll need to figure out signing by reading 3rd-party documentation" and whose bundler has an *open, unresolved* bug where the Linux .deb ships with no CSS and no icons. Meanwhile the thing that would justify the risk — Blitz/dioxus-native escaping the webview — is still alpha, has already slipped a year past its own beta target, and is described in the 0.7 release notes with "we have not focused on performance." So the choice is: pay Tauri's costs, forgo Tauri's maturity, and bank on a promise that a small team will finish writing a browser engine.

---

## Foreign UI + mint-as-sidecar

**Full stack as evaluated:** Foreign-UI + mint-as-sidecar: a GUI in a non-Rust stack (Electron/TS+React, Flutter/Dart, or Avalonia/C#) that spawns the existing `mint` binary as a child process and talks to it over argv + stdout JSON. Recommended member if the family is chosen: **Electron 43 + TypeScript/React**. Flutter is the runner-up for pure visual elegance but loses on accessibility; Avalonia is the best fit if the team already has .NET fluency. The mint binary ships *inside* the app bundle (5.4 MB measured for the current release build), so the user installs one thing.

**Version researched:** Electron 43.4.0, released 2026-08-11 (Chromium 150.0.7871.224, Node 24.18.1); the 43.x line opened 2026-06-30. Only the last three majors (41/42/43) are supported, 8-week major cadence. Flutter 3.44.8 stable (July 2026, Dart 3.12.2); 3.44.0 landed 2026-05-18. Avalonia 12.0, released 2026-04-07 (MIT), on .NET 10 LTS (2025-11-11, supported to 2028-11-10). flutter_rust_bridge 2.12.0 (2026-03-29) for the in-process Flutter variant. Qt 6.11 (2026-03-23) for the sidebar comparison. Verified against endoflife.date/electron, releases.electronjs.org, avaloniaui.net/blog/avalonia-12, docs.rs, and doc.qt.io.

**Self-assessed verdict:** `viable-with-caveats`

The process boundary solves mint's two hardest GUI problems for free: cancellation of a synchronous, non-interruptible DSP core becomes `kill()`, and the 485–700 MB-per-job whole-file `AudioBuffer` lives in a short-lived child whose RSS the OS reclaims completely, instead of becoming a permanent high-water mark in a long-lived UI process. mint's `release.yml` already builds the exact sidecar artifact for five targets with statically-linked libsoxr, so the libsoxr/pkg-config constraint the brief flags is almost entirely already solved for this family — the GUI toolchain never sees a C library. But the sidecar *contract* does not exist yet: I measured an 11-target batch of a 10-second file emitting literally nothing on stdout for 4.37 s and then the whole JSON document at once (a 5-minute master would be ~2 minutes of a mute spinner), there is not a single `log::info!` call anywhere in `src/`, and `--dry-run --json` silently prints human text because `cli.rs:99` returns from `print_dry_run` before the `--json` branch at line 168. The killer structural cost is that mint's real domain model — 23 `RawTarget` fields, 11 presets, the 4-layer merge with its deliberate own-layer exception for `format`/`rate`/`bwf`, and ~15 validation rules — would have to be mirrored in TypeScript/Dart/C#, in a codebase whose whole ethos is making a forgotten step a compile error. Adding `--dry-run --json` plus a `mint schema --json` (both small, both derived from data mint already computes) collapses that duplication to zero and is the difference between this family being a good choice and a slow-motion drift bug.

| Criterion | Score |
|---|:--:|
| Cross-platform parity | 4 / 5 |
| Native-dep integration | 5 / 5 |
| Packaging & signing | 3 / 5 |
| UI ceiling | 5 / 5 |
| Dev velocity | 3 / 5 |
| Longevity | 3 / 5 |
| Accessibility | 4 / 5 |

### Integration with the mint core

**Shape.** Out-of-process only. `pipeline::process` is never called; the GUI spawns `mint --config <project.toml> --json --jobs N [--target ...] <inputs...>` with an explicit `cwd`, reads stdout, and reaps the exit code. Nothing links Rust. (The flutter_rust_bridge variant inverts this — see Risks; it trades every advantage listed here for a Dart-FFI cdylib that must link libsoxr on 5 targets.)

**What works today, measured.** Process startup is ~15–20 ms warm on macOS for the 5.4 MB release binary, so a keystroke-debounced spawn-per-validation loop is entirely practical — the sidecar does not need to cache or mirror anything if mint will answer questions. `--json` output is a clean, stable DTO (`json_report.rs`) with `null` for non-finite meters and `dsd: null` explicitly present on PCM tasks, which is exactly what a typed client wants.

**What does not work today.**
1. *No incremental anything.* I timestamped stdout on `mint --config mint.toml --json --force tone.wav` (10 s / 96 kHz / stereo, 11 targets): first byte at 4.37 s, i.e. at completion. stderr carried one line. `grep` for `log::info!`/`debug!` across `src/` returns zero hits — env_logger is initialized and nothing ever logs, so there is nothing to tail either.
2. *`--dry-run --json` is not JSON.* Verified by running it: it prints the human chain listing. This is the single most damaging gap, because the live derived-chain + output-path + predicted-size preview *is* the GUI's reason to exist.
3. *Exit code 1 means "≥1 task failed", not "the run failed".* My batch exited 1 with 10/11 tasks succeeded and a perfectly valid JSON document on stdout. The client must parse stdout regardless of exit status and distinguish "exit 1 + valid JSON" from "exit 1 + garbage".

**What mint would have to add, by value/effort.**
- **NDJSON progress (highest value, ~40 lines).** `json_report.rs` already has `TaskJson<'a>` as the per-task DTO; add `pub fn render_task(&ProcessingReport) -> Result<String>` and replace the rayon `.par_iter().map(...).collect()` in `cli.rs` with a `for_each` that prints one line per completed task under a stdout lock. Prefix with a `{"kind":"plan", tasks:[...]}` preamble built from the pre-flight that already runs (`probe_inputs` → `plan_outputs`), so the GUI has a determinate denominator before work starts. This alone turns "unusable" into "good", and it also means a *cancelled* batch still leaves an accurate record of what completed.
- **`--dry-run --json` (~half a day).** A `PlanJson` DTO over what `print_dry_run` already computes: output path, `symlink_metadata` exists flag, `SizeEstimate` + its qualifier, the `describe()` step strings, and any `delivery_blocker()` reason. Removes essentially all domain-logic duplication from the GUI.
- **`mint schema --json` (~a day, optional but transformative).** Emit field names, types, enum variants, `builtin_fallback()` defaults, and the `PRESET_NAMES` table from the Rust definitions; codegen TS/Dart/C# types from it in CI. This is what makes a missed checklist step a *build failure* again, restoring the property the codebase is built around.
- **Intra-file progress (don't).** `pipeline::process` is straight-line with no callback seam. The three streaming stages (`ops::dsd::encode_dsf`, `ops::limiter::limit_channels`, `ops::resample::apply`) have block loops you could tick from, but the loudness analysis passes and the writers do not, and any sink must be `Sync` because of rayon. Stage-level events are a day; percentages are a week and touch the DSP modules the project is most protective of. **You don't need it:** `estimated_output_bytes` is byte-*exact* for WAV and DSF (deterministic length contracts end-to-end), and `atomic_write` writes to a documented, pid-stamped sibling path — so the GUI can `stat` `.<name>.<pid>.<n>.mint-tmp` and derive a true percentage for the long DSD renders with **zero mint changes**. That is a codebase-specific freebie worth exploiting.

**Cancellation and kill safety (the requested check on `audio::atomic_write`).** Every writer stages: `io_write.rs` routes through `atomic_write::write`/`Staged` at lines 29, 37, 56, 322, 409; `io_write_dsd.rs:187` uses `atomic_write::create` and commits at `:315`. `Drop for Staged` removes the temp file — but Drop does not run on SIGTERM/SIGKILL/TerminateProcess, and I confirmed there is **no signal handler anywhere in `src/`** (no `ctrlc`, no `signal_hook`, no SIGINT handling). So:
- Killing the child is **safe for the deliverable**. The final path holds either the previous good render or the new one, never a truncated hybrid. This is the property that makes a Cancel button acceptable at all.
- It is **not clean**: the staging file leaks and nothing sweeps it. The module doc says so verbatim. Scale matters — a 5-minute DSD512 stereo render is ~1.7 GB of staging file per cancelled job, times `--jobs` (default = CPU thread count).
- **The GUI must sweep, and can do so exactly.** The name is `.<final name>.<pid>.<n>.mint-tmp`; the GUI owns the child's pid and knows every output dir from the plan, so `glob(dir + "/.*." + pid + ".*.mint-tmp")` is a precise sweep with no risk of touching another process's staging file. (`cli::is_supported_input` already rejects `.mint-tmp` so leftovers can't re-enter a glob — but they still occupy disk.)
- **Honesty caveat:** I could not verify this empirically. `target/release/mint` (2026-08-06 11:41) predates `src/audio/atomic_write.rs` (12:48) and contains no `mint-tmp` string. SIGKILLing that stale binary mid-DSD-render left a truncated 3.95 MB `.dsf` at the *final* path (complete = 7,061,596 B) — precisely the failure mode `atomic_write` was written to close. I did not rebuild (read-only brief). **The prototype must re-run this against a fresh build; it is the highest-stakes assumption in the design.**

**Per-member process management.** Electron: `child_process.spawn(bin, args, {cwd})` + `readline` over stdout; `child.kill('SIGTERM')` (POSIX) / TerminateProcess (Windows, no graceful path — irrelevant here since mint has no cleanup). Dart: `Process.start` + `stdout.transform(utf8.decoder).transform(LineSplitter())`, `process.kill(ProcessSignal.sigterm)` — the cleanest of the three. C#: `Process` with `RedirectStandardOutput`; drain *both* pipes asynchronously or you can deadlock when stderr fills (low risk with `--json`, but it's the classic .NET footgun).

**Two sharp spawn gotchas specific to mint.** (a) `dir` defaults to `"./out"` (`config.rs:925`) and `check_planned_outputs` reads `std::env::current_dir()` (`config.rs:1028`) — an app launched from Finder/Dock has CWD `/`, so without an explicit `cwd` mint tries to write `/out/cd/tone.wav`. (b) **Never omit `--config`**: `cli::resolve_config_path` *writes* `./mint.toml` from the `include_str!`-embedded template when the flag is absent and the file is missing, so a forgetful GUI litters (or fails at `/`).

**Concurrency hazard the GUI inherits.** `config::check_planned_outputs` sees only one process's jobs, and `atomic_write` documents that it has no TOCTOU protection ("two mint processes racing on one path are a user error"). If the GUI lets two renders run at once against a shared `dir`, the batch collision detection is silently gone. Serialize child processes, or replicate the plan check across them.

**Memory.** Each in-flight job holds a whole-file f64 `AudioBuffer`; CLAUDE.md's 485–700 MB figure is *per conversion*, and mint defaults `--jobs` to `available_parallelism()`. On a 16-thread laptop that is potentially >8 GB. The GUI must expose and clamp `--jobs`. The upside of the process boundary is real: that RSS is transient and fully reclaimed on child exit, whereas an in-process design would hold the high-water mark in the UI process where the allocator may never return the pages.

### Packaging & distribution

**The shipped artifact is one app containing the CLI.** `mint`/`mint.exe` (5.4 MB measured; CI static-soxr builds are comparable) rides inside the bundle as an app resource. A nice product move for this audience: offer "Install command-line tool…" (VS Code style) that symlinks the embedded binary onto PATH — one installer, two front ends, one DSP, and `--json` stays the QC-automation surface it was designed as.

**libsoxr is a non-issue for this family.** The GUI build never sees pkg-config or a C library. `release.yml` already static-links soxr 0.1.3 (Debian orig tarball via cmake on Linux/macOS; vcpkg `x64-windows-static-md` on Windows, with the hand-written `soxr.pc` because upstream gates `.pc` generation behind `NOT WIN32`) for x86_64-linux-gnu, aarch64-linux-gnu, x86_64-darwin, aarch64-darwin, x86_64-pc-windows-msvc. **The one gap is aarch64-pc-windows-msvc** — Windows-on-ARM is now a real segment and would need a sixth matrix entry plus a vcpkg ARM64 static triplet.

**CI shape.** Two graphs that must be fused: mint's existing 5-target release matrix, and the GUI's 3-platform build. The GUI job downloads the mint artifact for its target(s) and drops it into `extraResources`/the Runner copy phase before signing. Universal macOS needs `lipo`-ing the x86_64 + aarch64 mint binaries into a fat Mach-O first (Electron's `universal` target expects this). Tractable, but it means a release is a two-stage pipeline with an artifact handoff, not one `cargo build`.

**macOS: you sign two things and notarize one.** Notarization requires hardened runtime and rejects *any* unsigned Mach-O inside the bundle, so the embedded `mint` must be `codesign`ed with the same Team ID and a `--options runtime` before the outer bundle is sealed. In electron-builder that's `extraResources` + an `afterSign` hook that signs the binary and then submits to `notarytool`; Flutter needs a Runner "Copy Files" build phase plus your own signing script (Flutter has no first-party notarization step); Avalonia via Velopack generates and signs a `.pkg` for you. **Mac App Store is effectively off the table** for the sidecar design: the sandbox hands the app a security-scoped bookmark for a user-picked file, and you cannot hand that to a child process as a plain path string. Direct download + Developer ID + notarization only. Even outside the sandbox, TCC (Documents/Desktop/Downloads, Full Disk Access) is attributed to the responsible process — the child normally inherits, which is why "run the notarized .app from Finder and drag a file in from ~/Music" must be an actual prototype step, not an assumption.

**Windows.** Sign the installer and the embedded `mint.exe`. As of 2026 the reputation game got worse, not better: EV certificates stopped granting immediate SmartScreen bypass in 2024, and Azure Trusted Signing's intermediate-CA rotation on 2026-03-26 (Microsoft ID Verified CS AOC CA 03 / EOC CA 04) caused freshly-signed binaries to be flagged "unrecognized app" again. Budget for a SmartScreen reputation grind on the first release regardless of certificate class. Azure Artifact Signing (renamed from Trusted Signing) is the cheapest credible route.

**Linux.** Three artifacts, no way around it: AppImage (easiest for the sidecar — everything is already in one blob), .deb, and ideally Flatpak. Flatpak is the awkward one: the child process runs inside the same sandbox, so `--filesystem=host` or per-directory portals are needed for arbitrary master locations, and portal-granted paths are not always ordinary paths from the child's view.

**Sizes.** Electron: ~80–150 MB installer, ~90–160 MB installed, plus 6 MB mint (these are widely-cited community figures, not something I measured). Flutter desktop: ~45–60 MB. Avalonia self-contained + NativeAOT: ~26–80 MB depending on trimming (and Avalonia's reflection-based bindings make NativeAOT trimming genuinely fiddly); framework-dependent is ~15 MB but adds a .NET 10 runtime prerequisite, which I would not inflict on a mastering engineer.

**Tooling verdict per member.** electron-builder is the most mature packaging/signing/notarization/auto-update tool in the desktop world and handles the embedded-binary case as a documented pattern — it is the family's strongest asset. Velopack (Rust-written, Win/macOS/Linux, auto-update, signs a .pkg on macOS) makes Avalonia a close second. Flutter is a clear third: no first-party installer maker, so it's `msix`/Inno Setup/`flutter_distributor`/hand-rolled AppImage, and the notarization script is yours to write.

### UI capability ceiling

This is the family's reason to exist, and specifically Electron's. The honest way to quantify it is against the six components this app actually needs, not against marketing.

**1. The 22-field target editor.** Needs conditional visibility (dither fields vanish for DSD and MP3; `bwf` is WAV-only; `dsd` is DSF-only; `mp3_bitrate` is MP3-only), inline validation, and — the feature that would make this GUI genuinely better than editing TOML — a per-field provenance badge showing *which of the four layers* supplied the effective value ("inherited from `[defaults]`" / "from preset `apple-music`" / "explicit"), including the own-layer exception for `format`/`rate`/`bwf`. React with `react-hook-form` + `zod` + a schema-array-driven `<Field>` renderer: ~300–500 LOC total, and the provenance data can come straight from a `mint --dry-run --json`. Flutter: ~800–1200 LOC, because every field is a widget tree with an explicit controller and validator. Avalonia: ~400 LOC of XAML plus a ViewModel, and it gets real two-way binding with `INotifyDataErrorInfo` validation and `CommunityToolkit.Mvvm` source generators nearly for free — Avalonia is genuinely competitive here and beats Flutter.

**2. Dense tables (the render-task list).** Web wins outright: TanStack Table + a virtualizer gives sorting, grouping by target, column resize and 10k-row virtualization off the shelf. Avalonia has a real `DataGrid` with virtualization (its strongest suit, and the only one of the three with a WPF-grade grid). Flutter is weakest — `DataTable`/`DataTable2` are serviceable but you will fight them.

**3. Live meters.** A 60 fps LUFS / gain-reduction / true-peak meter is ~100 LOC of Canvas2D in Electron. Flutter's `CustomPainter` on Impeller is arguably the *best renderer* of the three and will beat Canvas2D on a 4K spectrogram — but you write everything yourself. Avalonia gives you Skia via `DrawingContext`, competent and unglamorous. Caveat for all three: today mint reports meters only at task completion, so "live" means animating the delivered figures as tasks land, not a real-time meter — a true realtime meter would need mint to stream frames, which is out of scope.

**4. Future waveform/spectrum.** Decisive for Electron: wavesurfer.js v7, peaks.js, and WebGL spectrogram shaders are mature, free, and battle-tested in exactly this domain. Flutter has no equivalent — you build it. Avalonia has essentially no audio-viz ecosystem.

**5. Theming, HiDPI, drag-and-drop, native dialogs.** Electron: CSS custom properties give light/dark/high-contrast/reduced-motion for free; HiDPI is Chromium's problem, not yours; native dialogs via `dialog.showOpenDialog`. One sharp gotcha for an audio app whose primary gesture is dragging WAVs in: **`File.path` was removed in Electron 32** — you must use `webUtils.getPathForFile(file)` to get a filesystem path from a dropped file. Flutter needs the community `file_selector` + `desktop_drop` packages, and Linux DnD has historically been the flaky one. Avalonia 12 has built-in DnD and `StorageProvider` dialogs; Wayland is where its rough edges remain.

**6. Elegance.** Electron's advantage is not rendering quality, it's the **ecosystem of pre-solved, accessibility-audited primitives** — Radix/shadcn give you a design system with correct focus management and ARIA on day one, and Figma→React codegen is the best designer handoff of any desktop stack. Flutter produces the most visually distinctive results if you have a designer and are willing to draw things; Avalonia produces a competent, slightly WPF-flavoured application that will never look like a boutique mastering tool without real effort (and the good third-party control suites — Actipro, Syncfusion — are paid).

Net: Electron 5, Flutter 4–5, Avalonia 4. Family: 5.

### Development velocity & maintenance

**Build.** Electron is the fastest path in the industry for this shape of app: Vite HMR under 100 ms, React DevTools, and the six components above mostly assembled rather than written. A functional first version — project list, target editor, dry-run preview, run with progress, cancel — is realistically 3–4 weeks for one competent TS developer, assuming mint gains NDJSON and `--dry-run --json` first (those are ~1.5 Rust days combined). Flutter's stateful hot reload on desktop is arguably a *better* inner loop than web HMR, but you write 2–3× more code for forms and tables, so call it 5–6 weeks. Avalonia: XAML hot reload via `dotnet watch` is the weakest of the three, and the first week is spent on MVVM plumbing, but after that it moves well; 4–6 weeks with existing .NET fluency, considerably worse without.

**Maintain — this is where the family loses.** CLAUDE.md documents cross-file checklists that are the project's actual safety mechanism, e.g. "A new target field: `RawTarget` → `overlay()` → `builtin_fallback()` → `ResolvedTarget` → `resolve_target()` → `validate_fields()` → `describe()` → README → `examples/mint.toml`", explicitly noting that "missing any of the first five is a compile error." A foreign GUI adds 3–5 more steps in a language the maintainer doesn't live in, and **none of them is a compile error.** The codebase has spent real effort structurally eliminating exactly this class of drift — three exhaustive `match target.codec` sites kept exhaustive on purpose, with a regression test in `tests/dsd_pipeline.rs` for the catch-all failure mode. A hand-maintained TypeScript mirror of `OutputCodec`, `DitherMode`, `PRESET_NAMES` and the 4-layer merge is that same bug, reintroduced by architecture.

**The mitigation is specific and it works.** Make mint the single source of truth: `--dry-run --json` (so the GUI never *derives* a chain, path, size, or blocker) plus `mint schema --json` (so field lists, enum variants, defaults and the preset table are generated into TS/Dart/C# in CI). With both, the GUI models zero domain logic and a missed step is a codegen diff / build failure. Without them, this is a slow drift bug with a two-year fuse. That's the single largest swing factor in the whole evaluation — it's the difference between velocity 3 and velocity 2.

**The one duplication you cannot codegen away: writing TOML back.** `Config`/`RawTarget` are `Deserialize`-only, and `examples/mint.toml` is 93 lines of which 36 are comments — the bootstrap template users will inherit and then annotate. Rust's answer is `toml_edit` (format-preserving, the crate `toml` 0.8 is built on) and it is excellent; there is **no comparably maintained format-preserving TOML editor in TypeScript, Dart, or C#** (`smol-toml`, `@iarna/toml`, `Tomlyn` all round-trip values, not comments). Your three options are all unattractive: destroy the comments, hand-roll a comment-preserving writer, or add a `mint config set --json` subcommand so the writes happen in Rust — which is the right answer and which quietly makes mint a config-server as well as a renderer.

**Designer tooling and boilerplate.** Figma→React is the best handoff available; Figma→Flutter is decent; Figma→XAML is essentially nonexistent. For the 22-field form specifically, a schema-driven renderer keeps Electron near-zero-boilerplate; Flutter and Avalonia both want a widget/control per field unless you build your own generic renderer first.

**Team fit is the honest deduction.** For a Rust-fluent maintainer, every GUI bug is now debugged in a stack they're less fluent in, with a second dependency ecosystem (npm advisories, Electron's ~6 majors/year security treadmill) and a second CI graph. That tax never goes away.

### Risks

- Kill safety is asserted but unverified against the current source. `src/audio/atomic_write.rs` is UNTRACKED in git (added 2026-08-06 12:48) and the only built binary predates it (11:41, contains no `mint-tmp` string). SIGKILLing that stale binary mid-DSD-render left a truncated 3.95 MB .dsf at the FINAL path where a complete file is 7,061,596 bytes — exactly the failure mode atomic_write exists to close. If the new staging path has any hole in it, the GUI's Cancel button destroys the previous good render. Verify against a fresh build before anything else.
- The sidecar contract does not exist yet. Measured: an 11-target batch of a 10-second file emitted nothing on stdout for 4.37 s and then the entire document; scaled to a 5-minute master that is ~2 minutes of a mute spinner. There are zero `log::info!`/`debug!` calls in all of `src/`, so stderr carries nothing to tail either. Without NDJSON, the GUI is strictly worse than a terminal.
- `--dry-run --json` silently emits human text, not JSON (`cli.rs` returns from `print_dry_run` at line ~99, before the `--json` branch at ~168) — verified by running it. The GUI's single most valuable feature, the live derived-chain + output-path + byte-size preview, is available only as text to be regex-scraped, or must be reimplemented in TS/Dart/C#.
- Domain-model duplication with no compile-time enforcement: 23 `RawTarget` fields, 11 presets, the 4-layer merge, the deliberate own-layer exception for `format`/`rate`/`bwf`, ~15 validation rules and the input-dependent `delivery_blocker()` rules would live in two languages. This is precisely the drift the codebase's three exhaustive `match target.codec` sites were designed to make impossible.
- Cancelled DSD jobs leak gigabytes. A killed process leaves `.<name>.<pid>.<n>.mint-tmp` behind and nothing sweeps it; a 5-minute DSD512 stereo staging file is ~1.7 GB, times `--jobs` in-flight (defaults to CPU thread count). The GUI must sweep by pid glob or the app becomes a disk-filling machine. Mitigable, but must be built deliberately.
- Killing also destroys the report. Files committed by already-finished jobs remain on disk, but stdout produced nothing, so a cancelled batch leaves the GUI unable to say what completed without stat-ing the filesystem. NDJSON per-task events fix this too — it is the same fix twice.
- Writing config back destroys comments. `examples/mint.toml` is 93 lines with 36 comment lines and it is the `include_str!` bootstrap template every fresh install inherits. There is no maintained format-preserving TOML editor in TypeScript, Dart, or C# equivalent to Rust's `toml_edit`.
- Cross-process collision detection is gone. `config::check_planned_outputs` sees only one process's jobs and `atomic_write` documents that it has no TOCTOU protection. If the GUI permits two concurrent renders against a shared `dir`, the batch-level guard that `cli.rs` was built to provide silently does not apply.
- Memory: `--jobs` defaults to CPU thread count and each in-flight job holds a whole-file f64 buffer (485–700 MB measured for a 5-min 96 kHz stereo master). A 16-thread machine can exceed 8 GB. The GUI must clamp `--jobs` and surface it, or users will OOM on a batch of hi-res masters.
- Two macOS spawn traps: an app launched from Finder has CWD `/` while `dir` defaults to `"./out"` (`config.rs:925`) and `check_planned_outputs` reads `current_dir()` (`config.rs:1028`); and omitting `--config` makes `cli::resolve_config_path` WRITE a `mint.toml` from the embedded template into the CWD.
- Mac App Store is closed to this design: the sandbox grants file access via security-scoped bookmarks, which cannot be handed to a child process as a path string. Direct-download + Developer ID + notarization only. Flatpak has an analogous portal-vs-child-process problem on Linux.
- Windows SmartScreen in 2026 is worse than folklore suggests: EV certs stopped bypassing it in 2024, and Azure Trusted Signing's 2026-03-26 intermediate-CA rotation flagged freshly-signed binaries as 'unrecognized app' again. Budget for a reputation grind on first release regardless of cert class.
- Electron's release treadmill: 8-week majors, only the last three supported, so roughly six upgrades a year to stay on supported Chromium. Individually small, cumulatively a real maintenance line item for a small team.
- Flutter-specific longevity: Google laid off ~200 across Flutter/Dart/Python in 2026 and handed desktop stewardship to Canonical at I/O 2026. Canonical is a credible steward with real Flutter-desktop history, but this is a single commercial company with a Linux-first agenda now owning the roadmap for the platform you'd be shipping on. Material/Cupertino also moved out of the core SDK in 3.44.
- Flutter-specific accessibility: everything is canvas-drawn and semantics are bridged; there are documented failures with NVDA on Windows (semantics tree not fully exposed to UI Automation), Tab-focus problems, and Orca on Linux is the worst case. If accessibility matters, choosing Flutter takes this family from 4 to 2 on that axis.
- aarch64-pc-windows-msvc is missing from `release.yml`'s five-target matrix. Windows-on-ARM users would get an x64 sidecar under emulation (slow for a DSP core) or nothing.
- The flutter_rust_bridge variant (fRB 2.12.0, 2026-03-29) forfeits every advantage of this family: you go back to linking libsoxr into a Dart FFI cdylib on 5 targets via cargokit, you lose free cancellation (a cancel token must be threaded through `src/ops/`), and the 700 MB-per-job buffer now lives in the long-lived UI process. If you want in-process, evaluate a Rust GUI instead.

### The strongest argument for

The process boundary hands you, for free, the two things that are genuinely hard about wrapping *this* core: cancellation of a synchronous, whole-file, non-interruptible DSP pipeline becomes `kill()` instead of threading a `Sync` cancel token through every op in `src/ops/`, and the 485–700 MB-per-job `AudioBuffer` becomes a transient child-process RSS the OS reclaims entirely rather than a permanent high-water mark inside a long-lived UI process. Kill is safe for the deliverable because `audio::atomic_write` stages every writer and renames — you lose a temp file, never the previous good render. On top of that, `release.yml` already builds exactly the artifact you'd embed, statically linked against soxr, for five targets — so the "unconditional libsoxr + pkg-config" constraint that dominates every other option's packaging story costs this family precisely one `cp` into `extraResources`. And because the contract is a CLI, the UI framework becomes a *replaceable* component: if Electron's treadmill or Flutter's governance turns sour in three years, you rewrite the front end and the DSP, the config format and the QC JSON all survive untouched.

### The strongest argument against

mint's config semantics *are* the product — 23 fields, 11 presets, a 4-layer merge with a deliberate own-layer exception for `format`/`rate`/`bwf`, ~15 validation rules and input-dependent delivery blockers — and today the only way a TypeScript/Dart/C# GUI can render, validate or explain them is to reimplement them, in a repo whose stated design ethos is that "a forgotten arm is a compile error rather than a wrongly-encoded file." You would be hand-porting the exact invariants the author built three exhaustive `match` sites and a regression test to protect, into a language where nothing checks them. And the contract that would rescue you doesn't exist: mint emits *nothing* until the entire batch ends (measured: 4.37 s of silence, then the whole document), there is not one `log::info!` call in the codebase to tail, `--dry-run --json` silently isn't JSON, and writing an edited target back to TOML destroys the 36 comment lines in the shipped template because no TS/Dart/C# equivalent of `toml_edit` exists. Every one of those is fixable in Rust in about two days — but until they are, you're building a GUI on a contract you've assumed rather than one that ships.

---

# Part III — UX and information architecture

Produced independently of any toolkit choice, and deliberately so: it answers *what should the app be*, and its capability checklist is what the judge panels scored the stacks against. It is the reason Iced was eliminated and the reason Slint placed third.

*Read: `README.md` (709 ll.), `examples/mint.toml`, `src/config.rs` (1271 ll.), `src/cli.rs`, `src/pipeline.rs`, `src/json_report.rs`, `report.md` §§7–8. Corrections to your premises are in §8.*

---

## 1. The central design question

### The honest test

> Would an engineer *fluent in the TOML* still open this app every day?

If the answer is "only because they forgot the field names," the GUI has failed. mint's config is 92 commented lines, `deny_unknown_fields` gives precise instant errors, and VS Code + a JSON schema beats any form ever built. **A GUI that competes with the text editor loses.**

### What the CLI structurally cannot do

Four things, and only two of them are about editing at all:

| # | Gap | Why the CLI can't close it |
|---|---|---|
| 1 | **Provenance** — "why is this target 44.1 kHz?" | There is no `--explain`. `--dry-run` prints the derived *chain*, never the *field origin*. With 4 layers × 11 presets × 3 fields on a narrower stack, answering it means reading `config.rs`. A CLI answer would be a wall of text per field; a GUI answer is a badge. |
| 2 | **A feedback loop over the plan** | Today: edit → save → re-run `--dry-run` → diff 200 lines of text by eye. `describe()` + `estimated_output_bytes()` are pure and cost microseconds; recomputing a 30-input × 10-target matrix per keystroke is free. The CLI throws that away every invocation. |
| 3 | **All problems at once** | `Config::validate` (config.rs:729) and `check_planned_outputs` (config.rs:1024) both `bail!` on the *first* error. Fix one, re-run, hit the next. A compiler-style problem list is a pure GUI affordance. |
| 4 | **Durable, comparable evidence** | `--json` exists, but the report dies with the process. Nobody diffs two `mint --json` runs by hand. |

### The answer

**The mint GUI is a plan-and-evidence workbench, not a config editor.**

The primary object on screen is the **plan** — the (input × target) matrix with derived chain, output path, predicted bytes, and blockers — and, after a run, the **evidence** — delivered LUFS/dBTP against what was declared. Editing happens by poking at the plan and watching it re-derive. The TOML is the persistence format, not the subject.

That reframing survives the honest test: the plan matrix and the report are things the engineer cannot get anywhere else, and they are the things they'd use daily *even while continuing to edit the TOML in vim*.

**Corollary, and it is load-bearing: the GUI must assume the config is co-edited by a text editor and treat that as the normal case.** File-watch, hot-reload, never auto-format, never fight the editor. Most GUIs treat external edits as a conflict to resolve; this one treats them as the expected input.

### Ranking your four candidates

1. **(b) Provenance — the winner.** Highest unique value, literally impossible in a CLI, and it sits exactly where mint's complexity lives. §3 is the detailed design.
2. **(a) Live derived-chain preview — the *loop*, not a feature.** On its own it's an auto-refreshing dry-run; `entr` + a terminal gets you 70%. Its value is that it makes (b) and (c) interactive.
3. **(c) Batch pre-flight — best value per unit of effort.** All the machinery is already public (`probe_source`, `render_output_path`, `estimated_output_bytes`, `delivery_blocker`, `check_planned_outputs`). Fixes the one-error-at-a-time defect for free.
4. **(d) QC report — last of the four, but the one used daily once the config settles.** The CLI already *has* the capability; the GUI adds persistence, sorting, and comparison. Not capability — legibility.

These aren't four features. They're two halves of one loop: **Plan** (a+b+c) and **Evidence** (d).

### One thing that changes the presets question

`report.md` §7 argues mint's streaming presets encode a mistaken model (no platform asks for 16-bit or a pre-converted rate; the published LUFS figures are *playback* ceilings, not delivery targets). The GUI must not paper over that with a confident dropdown. Preset UI should show the preset's four fields inline, and the `hires`/`spotify`/`apple-music` rows should carry the same "sensible starting point, not gospel" caveat the README already prints. **A GUI that makes a wrong preset look authoritative is worse than a CLI that makes you type it.**

---

## 2. Screen inventory

One window, three modes, two drawers. Not ten screens — a mastering tool with ten screens is a tool nobody learns.

| # | Name | Job | Shows |
|---|---|---|---|
| 1 | **Project bar** *(persistent)* | Orient + act | Config path + dirty/external-change indicator, `[meta].name`, "N inputs × M targets = K jobs", run-scoped flags (`seed`, `force`, `jobs`, `fail_fast`), primary action (Dry-run ⇄ Render ⇄ Cancel), **"Copy as CLI command"** |
| 2 | **Sources pane** *(left top)* | The input set | Globs as chips showing what they expanded to; per file: rate, channels, frames→duration, size. Unreadable files flagged inline (`probe: Err` is per-file, not fatal — matches `ProbedInput`). Drop target. |
| 3 | **Targets pane** *(left bottom)* | The target set | One row per `[target.NAME]`: name, one-line resolved summary (`44.1k · s16 · −12 LUFS · −0.1 dBTP · wav`), status dot (ok / blocked for *n* inputs / config error), preset chip |
| 4 | **Target Inspector** *(right)* | **Provenance.** The centrepiece | 21 fields in 4 groups + the preset selector. Per field: effective value, layer badge, effect badge, expandable "why". §3. |
| 5 | **Plan matrix** *(centre, Plan mode)* | The pre-flight | inputs × targets grid. Cell = output filename + predicted size (with `~`/`≤` qualifier) + status glyph. Column headers = targets, row headers = inputs. |
| 6 | **Chain preview** *(centre bottom)* | Live `describe()` | The derived steps for the selected cell, re-rendered on every edit. Plus the `!` `delivery_blocker` line and the `(exists — will fail without --force)` marker. |
| 7 | **Run view** *(centre, Run mode)* | Watch it happen | Job list: state, elapsed, target, input, output. Live stderr log pane (append-only, pin/auto-scroll, filter). Running totals: done/failed/remaining, bytes written. Cancel. **Honest about progress — see below.** |
| 8 | **Report view** *(centre, Report mode)* | The evidence | Sortable table, one row per task: target, input, delivered LUFS vs declared (with ±0.1 LU pass/fail), delivered dBTP vs ceiling, peak/avg GR + active %, LRA, PLR/PSR, DSD peak modulation + overloads, warnings count. Detail card per row = source→delivered side by side. Failures as a separate, non-collapsible section. Run history with A/B diff. |
| 9 | **Config drawer** *(bottom, collapsible)* | The escape hatch | The actual TOML, editable, TOML syntax highlighting, error squiggles at the offending span, current selection highlighted. Bidirectional. **Non-negotiable.** |
| 10 | **Problems panel** *(bottom, tab beside 9)* | Everything wrong, at once | Every resolution error, every target collision, every batch collision, every per-input blocker — as a list, each clicking through to the field or cell. This is the item that fixes the `bail!`-on-first ergonomics. |

### The run view's honesty problem

`pipeline::process(input, target, seed)` is a single blocking call with no callbacks and no cancellation token. So per-job progress is **binary** (queued / running / done) unless the library changes.

Design accordingly:
- Show job state, elapsed time, and predicted output bytes. Show *batch* progress as jobs-completed/total (a real number).
- **Do not fake a per-job percentage from a timer.** A DSD512 encode and a same-rate WAV transcode differ by 100×; a lying bar is worse than none.
- Cancel = "stop launching new jobs, let in-flight ones finish." Honest, and achievable today. Atomic writes (`audio::atomic_write`) mean in-flight jobs killed hard leave no corrupt deliverable — but also no partial credit.

**Minimum library change to unlock real progress** (name it in the proposal, don't smuggle it in):

```rust
pub fn process(
    input: &Path,
    target: &ResolvedTarget,
    seed: Option<u64>,
    progress: Option<&dyn Fn(Stage, f32)>,   // Stage = Loudness|Resample|Verify|Quantize|Encode
    cancel: Option<&AtomicBool>,
) -> Result<ProcessingReport>
```
Checked at stage boundaries (5 coarse ticks) plus inside `encode_dsf`'s block loop, where it is nearly free — that loop already iterates once per container block and is the only stage that runs for hours.

---

## 3. The provenance problem

### The model: two orthogonal axes, not one

Everyone designing this reaches for "which of 4 layers won." That's half of it. Reading `resolve_target` (config.rs:628–726), a value has **two** independent properties:

**Axis 1 — origin** (4 states): `B` builtin_fallback · `D` `[defaults]` · `P` preset *name* · `E` explicit on this target.

**Axis 2 — fate** (4 states), the one nobody expects:

| Glyph | Fate | Mechanism | Examples |
|---|---|---|---|
| *(none)* | **applies** | merged value reaches the audio | most fields |
| `~` | **inert** | carried into `ResolvedTarget` but no code path reads it | `format` on DSD/MP3 (`format_placeholder()` returns `"1bit"`); `dither*` on DSD/MP3; `mp3_bitrate` off MP3; `dither_strength`/`dither_correlation` on non-adaptive modes (report.md #9) |
| `/` | **dropped** | the narrow stack discards an inherited value | `rate` on DSD (`if dsd.is_some() { own.rate }`, l.698); `bwf` off WAV (`merged.bwf.unwrap() && codec == Wav`, l.717) |
| `!` | **would error** | legal inherited, illegal if stated here | `format` on DSD (l.671); `bwf` off WAV (l.685) |

Axis 2 is where the load-bearing exception lives, and it is **invisible in every existing surface** — not in `--dry-run`, not in `--json`, not in the error messages. This is the single most valuable thing the GUI can make visible, and it is exactly what you flagged.

### The killer demo config

One target exercising all four fates at once:

```toml
[defaults]
dir = "./out"; quality = "vhq"; format = "s24"; rate = 48000
bwf = true;    dither = "psychoacoustic"

[target.sacd]
preset = "sacd"
lufs   = -16.0
```

### Inspector mockup

```
┌─ TARGET  sacd ───────────────────────────────────── source: master.wav @ 96000 Hz ─┐
│ preset [ sacd            ▾ ]  sets 3 of 21:  codec=dsf  dsd=dsd64  ceiling=-1.0    │
│                               deliberately sets NO format and NO rate      [why?]  │
│                                                                                     │
│ origin  B builtin   D [defaults]   P preset   E explicit here                       │
│ fate    · applies   ~ inert here   / dropped here   ! illegal if stated here        │
├─────────────────────────────────────────────────────────────────────────────────────┤
│ DELIVERABLE                                                                         │
│   codec              dsf                       P sacd     ·                         │
│   dsd                dsd64                     P sacd     ·                         │
│   rate               88200 Hz  (derived)       — none     /  D rate=48000 dropped   │
│   format             — (1-bit, no bit depth)   D s24      ~! inert; error if stated │
│   lufs               -16.0 LUFS                E here     ·                         │
│   ceiling_dbtp       -1.0 dBTP                 P sacd     ·                         │
│   bwf                false                     D true     /  dropped: bext is WAV   │
│   mp3_bitrate        320                       B builtin  ~  inert: codec is dsf    │
│ POLICY                                                                              │
│   on_clip            limit                     B builtin  ·                         │
│   limiter_character  balanced                  B builtin  ·                         │
│   limiter_soft_clip  false                     B builtin  ·                         │
│   limiter_link_chan  true                      B builtin  ·                         │
│   warn_limiting_db   1.0                       B builtin  ·                         │
│   max_limiting_db    8.0                       B builtin  ~  inert: on_clip=limit   │
│ QUALITY                                                                             │
│   quality            vhq                       D          ·  feeds the ×32 interp.  │
│   dither             psychoacoustic            D          ~  inert: modulator is    │
│                                                              the quantizer          │
│   dither_strength    normal                    B builtin  ~  inert                  │
│   dither_correlation decorrelated              B builtin  ~  inert                  │
│ DESTINATION                                                                         │
│   dir                ./out                     D          ·                         │
│   naming             {target}/{stem}.{cext}    B builtin  ·                         │
│   overwrite          false                     B builtin  ·  (--force overrides)    │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

### The expanded "why" — the money shot

Click any row. This is where the exception is *taught*, at the exact moment of confusion:

```
┌─ why is  rate = 88200 Hz (derived)? ──────────────────────────────────────┐
│                                                                            │
│   E  this target       rate = —          not set                           │
│   P  preset 'sacd'     rate = —          not set, deliberately             │
│   D  [defaults]        rate = 48000      ✗ NOT READ                        │
│   B  builtin           rate = —          not set                           │
│  ────────────────────────────────────────────────────────────────────────  │
│   ⇒ unset  ⇒  DERIVED from the source:                                     │
│        min( nearest 44.1-family rate , 2 822 400 / 32 )                    │
│      = min( 88200 , 88200 )  =  88200 Hz     · source is 96 000 Hz         │
│      → then interpolated ×32 into the modulator                            │
│                                                                            │
│   ⚠  On a DSD target, `rate` reads  preset + explicit  only — never        │
│      [defaults]. Your  [defaults] rate = 48000  is dropped here.           │
│      Were it read, this target would be a load-time ERROR: 48000 Hz        │
│      cannot reach 2 822 400 Hz by a power of two.                          │
│      That is the whole reason for the narrower stack.                      │
│                                                                            │
│      README › "Overriding presets"        src/config.rs:698                │
│                                                                            │
│      [ set rate on this target ]   [ open [defaults] ]   [ dismiss ]       │
└────────────────────────────────────────────────────────────────────────────┘
```

And the counterfactual for `format`, which is the nastier one because *stating* it changes the outcome:

```
┌─ why is  format  shown as "— (1-bit)"? ───────────────────────────────────┐
│   D  [defaults]  format = s24   → inherited, carried, but never read.     │
│                                   The DSD branch of pipeline.rs has no    │
│                                   bit-depth stage at all.                 │
│   !  If you set format on THIS target it becomes a load-time error, not   │
│      a warning. Same value, different fate — because [defaults] is a      │
│      baseline for a heterogeneous set of targets, not a description of    │
│      this one.                                                            │
│      This is also why no preset except `sacd` works with codec = "dsf":   │
│      every PCM preset sets `format`, and a preset counts as this          │
│      target's own layer.                                                  │
└────────────────────────────────────────────────────────────────────────────┘
```

### Two extras this model gets for free

**A real bug becomes visible.** `[defaults] preset = "..."` is parsed (it's a `RawTarget` field) but never applied — `resolve_target` reads `raw.preset`, not `merged.preset` (report.md defect #14). Under this model the GUI renders it struck through: *"ignored — `preset` in `[defaults]` is parsed but never applied."* The GUI should surface the bug, not reproduce it.

**Compact form for the matrix.** In the plan matrix and the target list, collapse to a count: `apple-music +2 overrides`, `4 inherited · 2 inert · 1 dropped`. Full badges only in the inspector.

### The preset picker is a diff, not a dropdown

Selecting a candidate preset shows which *resolved* values would change before you commit. Costs one extra `resolve_target` call:

```
  preset:  cd  →  apple-music
    lufs           -12.0  (E, kept)      unchanged — your explicit value wins
    ceiling_dbtp   -0.1   →  -1.0        preset
    rate           44100  →  44100       unchanged
    format         s16    →  s16         unchanged
  1 field changes.
```

---

## 4. File-format politics

**Verdict: `mint.toml` IS the project file. The GUI owns no config format. Non-negotiable.**

### Why

1. **Two sources of truth is fatal for *this* tool specifically.** mint's entire proposition is a *derived* pipeline you must trust. A GUI-owned format means a class of bug where "the GUI said X, the CLI did Y" — and the user has no way to adjudicate. `Config::from_path` must read the same bytes.
2. **Version control.** Engineers keep `mint.toml` beside the masters, in git, reviewed in PRs. A JSON/binary sidecar breaks `git diff`.
3. **Mixed CLI/GUI is the expected mode**, not an edge case: CI runs `mint --json` for QC; the engineer iterates in the GUI. Both must be first-class.
4. **The comments are the documentation.** `examples/mint.toml` is `include_str!`-embedded as the bootstrap template. Users inherit those 40 lines of comments in every fresh project. Destroying them is destroying the onboarding.

### How to write back: `toml_edit`, surgically

`RawTarget` is `Deserialize`-only. Adding `Serialize` is the wrong fix twice over:
- serde would emit all 22 fields unless every one gets `skip_serializing_if`, and
- **round-tripping through the struct erases the layering.** You'd write *resolved* values — `preset = "cd"` becomes 22 explicit fields — permanently destroying the thing the tool is built around. This is the #1 anti-feature (§5).

Use format-preserving surgical edits instead. **Concrete and free:** `toml_edit 0.22.27` is *already in `Cargo.lock`*, transitively via `toml 0.8.23`. Adding it as a direct dependency at `0.22` costs **zero new crates**. (Current release is `0.25.13+spec-1.1.0`, 14 Jul 2026; taking it would duplicate the tree unless mint also bumps `toml` to `1.1.4+spec-1.1.0`, 28 Jul 2026. Recommendation: match the lockfile, revisit if mint upgrades.)

Editing discipline, enforced by review:
- Touch only the key that changed. Never re-serialize `DocumentMut` wholesale.
- Never reorder tables or keys. Never normalize whitespace or quote style.
- Removing a value = removing the key, never writing the default explicitly.
- Setting a field that a preset already sets = adding an override line, never rewriting the preset.

### The gap nobody has noticed: mint.toml cannot hold a project

**`mint.toml` has no field for inputs.** Inputs come from `argv` (`#[arg(required = true)]`). So a "rendering project" in the GUI sense — *config + input set* — is **not expressible in the contractual format.**

Options, and my call:

| Option | Verdict |
|---|---|
| Add `[inputs]` to the schema | **No.** Breaks CLI parity (inputs are a required positional), widens the contract, and `deny_unknown_fields` means older mint binaries would then hard-error on newer configs. |
| Sneak GUI state into `[meta]` | **No.** `MetaConfig` has exactly `name`, with `deny_unknown_fields`. Adding an escape hatch weakens the property that a typo'd key is a hard error — which is a stated design value. |
| **Sidecar** | **Yes.** `mint.toml` stays byte-pure and CLI-owned. |

**Sidecar spec:** `.mint-gui/project.toml` next to the config (git-ignorable, or committed if the team wants shared input globs), holding *only* things that cannot change the audio: input globs and their expansion cache, selected targets, window/pane layout, run history index, last-used `seed`/`jobs`. Run reports go to `.mint-gui/runs/<iso8601>.json` in **exactly the `--json` schema** so `jq` still works on them.

**The bridge affordance:** a persistent "Copy as CLI command" button rendering the current state as
`mint --config mint.toml --target sacd --target cd --seed 42 --json "masters/*.wav"`.
Small to build, and it is what keeps a mixed CLI/GUI team reconciled. It also makes the GUI self-documenting for anyone learning the CLI.

### External edits

File-watch `mint.toml`. On external change: re-parse, re-derive, update everything, show a non-modal "reloaded" toast. If the GUI has unsaved edits, that is a genuine conflict — show both, never silently pick. **The GUI must never hold the config hostage**; if it crashes, the TOML is intact and the CLI still runs.

---

## 5. What the GUI must not do

Anti-features, ranked by how badly each betrays the design:

1. **Per-step DSP toggles.** No `[ ] resample  [ ] dither  [ ] limit`. Derivation *is* the product. The moment a user can force a step, `describe()`, `estimated_output_bytes()` and the pipeline stop being three views of one truth.
2. **Writing resolved values back.** Never expand `preset = "cd"` into 22 fields. Never write a value equal to the default. The layering is user intent; flattening it is data loss.
3. **Reformatting the user's TOML.** No reordering, no comment stripping, no quote normalization, no "tidy on save." A GUI that eats the comments in `examples/mint.toml` has eaten the documentation.
4. **A wizard that hides derivation.** "Step 3 of 5: choose your quality" with the chain revealed only at the end inverts the whole model. The chain must be visible *while* choosing.
5. **GUI-only state that changes the audio.** Every value that affects a sample must live in `mint.toml`. A UI checkbox not in the TOML is a rendering the CLI cannot reproduce. (`--force`, `--seed`, `--jobs` are legitimately run-scoped — but must appear in the copyable CLI line.)
6. **GUI-only presets.** Adding "Deezer −15" in the UI produces a config the CLI can't load. If report.md §7's missing presets are wanted, they go in `preset()` + `PRESET_NAMES` + README. Same checklist as always.
7. **An "album mode" toggle.** mint normalizes per track (report.md §7.3). A checkbox promising album normalization would be a lie until the library does it.
8. **Silent auto-fix of collisions.** Don't append `_1` to a colliding output path. mint deliberately errors and names both ways out; the GUI should offer the *same two* fixes as buttons, not invent a third.
9. **A progress bar that lies.** Covered in §2. Also: no fake "estimated time remaining" from a linear extrapolation across heterogeneous jobs.
10. **Collapsing warnings into a green checkmark.** "Heavy limiting: 4.2 dB", "loudness target not met", "sigma-delta overload on 118 samples" *are the product*. A run with warnings is not a success.
11. **Audio preview / playback / waveform / spectrum / live meters.** Scope creep into a DAW. mint has no playback path, and "the buffer is the budget" forbids the second copy a preview engine wants.
12. **Owning the only copy of a report.** Persist in the `--json` schema, always.
13. **Auto-saving the TOML on every keystroke while a render is running from it.**
14. **Hiding the failure list behind a "details" disclosure.**

---

## 6. Wireframes

### A — Plan mode (the default; ~85% of time spent here)

```
┌────────────────────────────────────────────────────────────────────────────────────────────┐
│ mint   album-masters          mint.toml ●edited          [Plan] Run  Report      ⚙ ⓘ       │
│ 3 sources × 4 targets = 12 jobs · 1.31 GB      seed 42  jobs 10  ☐force   [ Render ▸ ]     │
├──────────────────────┬─────────────────────────────────────────────────────────────────────┤
│ SOURCES         + ⌕  │ PLAN                              output = ./out    ⚠ 2 problems    │
│ ▾ masters/*.wav  (3) │        │ streaming   │ cd          │ flac        │ sacd            │
│   ●01 lead.wav       │────────┼─────────────┼─────────────┼─────────────┼─────────────────│
│      96k · 2ch · 5:12│ 01 lead│ ✓ 52.9 MB   │ ✓ 52.9 MB   │ ✓ ≤173.3 MB │ ✓ 211.7 MB      │
│   ●02 bridge.wav     │ 02 brid│ ✓ 41.2 MB   │ ⟳ 41.2 MB   │ ✓ ≤134.9 MB │ ✓ 164.9 MB      │
│      96k · 2ch · 4:03│ 03 outr│ ✓ 22.1 MB   │ ✓ 22.1 MB   │ ! >96 kHz   │ ✓  88.4 MB      │
│   ●03 outro.aif      │        │             │             │             │                 │
│     192k · 2ch · 2:10│  ✓ ok   ⟳ replaces existing   ! target will fail   ✗ collision      │
│                      ├─────────────────────────────────────────────────────────────────────│
│ TARGETS          + ⌕ │ CHAIN   03 outro.aif → [sacd] → ./out/sacd/03 outro.dsf   88.4 MB   │
│  ● streaming    P ap.│   1  transcode (no loudness normalization);                         │
│    44.1k s16 -16 wav │        ceiling -1.0 dBTP enforced (limit; balanced; linked)         │
│  ● cd           P cd │   2  resample 192000 → 88200 Hz (vhq, derived)                      │
│    44.1k s16 -12 +sh │   3  re-check true peak against the ceiling                         │
│  ▲ flac         P hi.│   4  interpolate ×32 → 5644800 Hz (dsd64, vhq), streaming           │
│    96k s24 -16 flac  │   5  sigma-delta modulate to 1 bit (order 7; 0 dBFS → 50 %)         │
│  ● sacd         P sa.│   6  write DSF (planar 4096-byte blocks, LSB first)                 │
│    dsd64 -1.0 dBTP   │                                                                     │
├──────────────────────┴─────────────────────────────────────────────────────────────────────┤
│ PROBLEMS (2)   ⌄ mint.toml   ⌄                                                             │
│ ! [flac] cannot deliver 192000 Hz for "03 outro.aif" — encoder tops out at 96000 Hz.       │
│      → [ set rate = 96000 on flac ]   [ switch flac to codec = "wav" ]                     │
│ ⟳ [cd] "./out/cd/02 bridge.wav" exists — will fail without --force.  [ enable force ]      │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

Notes: the target list shows `P <preset>` and a resolved one-liner. Chain is live — it re-renders as you type in the inspector or the TOML drawer. Problems is a *list*, not a first-`bail!`. Every problem carries the same two fixes mint's own error prose names.

### B — Run mode

```
┌────────────────────────────────────────────────────────────────────────────────────────────┐
│ mint   album-masters                                     Plan  [Run]  Report                │
│ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░  7 / 12 done · 1 failed · 10 workers   [ Cancel ⏹ ]   │
├────────────────────────────────────────────────────────┬───────────────────────────────────┤
│ JOB                                        STATE   TIME│ LOG                    ⌕  ⏸ pin  │
│ ✓ 01 lead      → streaming   52.9 MB       done    4.1s│ [INFO] probe 03 outro.aif 192000  │
│ ✓ 01 lead      → cd          52.9 MB       done    4.4s│ [INFO] resample 192000→88200 vhq  │
│ ✓ 01 lead      → flac       171.0 MB       done   11.2s│ [WARN] [cd] heavy limiting: 2.3   │
│ ⟳ 01 lead      → sacd       211.7 MB    running   2m18s│        dB of peak gain reduction  │
│ ✓ 02 bridge    → streaming   41.2 MB       done    3.2s│        to reach -12.0 LUFS under  │
│ ✓ 02 bridge    → cd          41.2 MB       done    3.5s│        the -0.1 dBTP ceiling      │
│ ✓ 02 bridge    → flac       133.1 MB       done    8.9s│        (source true peak -0.4)    │
│ ⟳ 02 bridge    → sacd       164.9 MB    running   1m47s│ [INFO] modulate dsd64 order 7 ×32 │
│ ✗ 03 outro     → flac             —      failed    0.0s│ [ERR ] target 'flac': codec=flac  │
│ ⧗ 03 outro     → streaming   22.1 MB     queued       —│        cannot deliver 192000 Hz…  │
│ ⧗ 03 outro     → cd          22.1 MB     queued       —│                                   │
│ ⧗ 03 outro     → sacd        88.4 MB     queued       —│                                   │
└────────────────────────────────────────────────────────┴───────────────────────────────────┘
```

No per-job percentage. Elapsed time and predicted size only — both honest. Batch bar counts *jobs*, which is a real quantity. Warnings surface in the log the moment they're produced, not only at the end.

### C — Report mode

```
┌────────────────────────────────────────────────────────────────────────────────────────────┐
│ mint   album-masters       run 2026-08-13 14:02   11 ok · 1 failed   Plan Run [Report]     │
│ [ Export JSON ]  [ Compare with ▾ 2026-08-12 18:44 ]                    ⚠ 3 warnings       │
├────────────────────────────────────────────────────────────────────────────────────────────┤
│ TASK                    LUFS ▾           dBTP        GR (peak/avg/%)   LRA  PLR   ⚠         │
│ 01 lead   → streaming   -16.0  ✓ ±0.0    -1.0 ✓      —                 8.1  15.0            │
│ 01 lead   → cd          -12.0  ✓ ±0.0    -0.1 ✓      -2.3 / -0.6 / 14% 6.9  11.9  ⚠1        │
│ 01 lead   → flac        -16.0  ✓ ±0.0    -0.1 ✓      —                 8.1  15.9            │
│ 01 lead   → sacd        -18.4    n/a     -1.0 ✓      —                 8.4  17.4            │
│ 02 bridge → cd          -12.4  ✗ -0.4    -0.1 ✓      -6.1 / -2.2 / 61% 4.2   7.7  ⚠2        │
│ …                                                                                           │
├────────────────────────────────────────────────────────────────────────────────────────────┤
│ DETAIL   02 bridge → [cd]  →  ./out/cd/02 bridge.wav                                       │
│                        source            delivered        declared     verdict             │
│   integrated       -18.9 LUFS        →   -12.4 LUFS       -12.0        ✗  -0.4 LU          │
│   true peak         -0.4 dBTP        →    -0.1 dBTP       ≤ -0.1       ✓                   │
│   LRA               11.3 LU          →     4.2 LU          —           −7.1 LU spent       │
│   PLR / PSR         18.5 / 12.1      →     7.7 /  5.3      —                               │
│   limiting          peak -6.1 dB · avg -2.2 dB over 61 % of samples                        │
│   quantize          s16 + tpdf + noise-shaping (gentle) · seed 42                          │
│                                                                                             │
│   ⚠ heavy limiting: 6.1 dB of peak gain reduction to reach -12.0 LUFS under the            │
│     -0.1 dBTP ceiling (source true peak -0.4 dBTP); consider a lower target or             │
│     more source headroom                                                                    │
│   ⚠ loudness target not met within +/-0.1 LU (target -12.00, delivered -12.40)             │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

The verdict column is the point: mint already computes it (`LUFS_TOLERANCE`, pipeline.rs:316) and buries it in a warning string. `LRA -7.1 LU spent` is the number no existing surface shows, and it is the one that tells a mastering engineer what the loudness target cost.

---

## 7. Toolkit capability checklist

Score stacks against this. **M** = mandatory (design breaks without it), **S** = strongly wanted, **O** = optional.

| # | Capability | Why *this* design needs it | Pri |
|---|---|---|---|
| 1 | **Virtualized data grid**, per-cell custom content + selection | Plan matrix: an album batch (20 inputs × 8 targets) = 160 cells; a library run can hit thousands | **M** |
| 2 | **Sortable multi-column table** with per-cell status glyphs | Report view | **M** |
| 3 | **Text editor widget: TOML syntax highlighting, error spans, undo** | The config drawer is the non-negotiable escape hatch. **This is the requirement most likely to eliminate a stack** — most Rust GUI toolkits have no production-grade code editor | **M** |
| 4 | **Live re-render at keystroke rate** (partial invalidation, no full relayout) | The whole plan re-derives on every edit | **M** |
| 5 | **Background thread → UI channel** | The rayon pool must never touch the UI thread | **M** |
| 6 | **Cancellation / cooperative shutdown plumbing** | Cancel button; also required for clean app exit mid-render | **M** |
| 7 | **Native modal file/folder pickers**, multi-select, on all 3 OSes | Choosing masters, output dirs, config | **M** |
| 8 | **Append-only log view**, 10⁴+ lines, auto-scroll + pin + filter | Run view stderr | **M** |
| 9 | **Monospace-aligned mixed content in table cells** (badges + text) | Provenance badges, chain steps | **M** |
| 10 | **Tooltips / hover popovers** with rich content | The "why is rate = …" panel | **M** |
| 11 | **Status encoded by glyph, not colour alone** | Accessibility, and mastering rooms are dark | **M** |
| 12 | **Dark mode / system theme following** | Non-negotiable for this audience | **M** |
| 13 | **Tree / list with disclosure + grouping** | Sources (glob → files), inspector field groups | **S** |
| 14 | **Drag & drop from OS file manager** onto the window | Adding masters; the expected gesture | **S** |
| 15 | **Determinate + indeterminate per-row progress** | Job rows (indeterminate today, determinate after the library change) | **S** |
| 16 | **Context menus** (right-click) | "Reveal output in Finder/Explorer", "copy path" | **S** |
| 17 | **Shell integration**: reveal-in-file-manager, open-with, cross-platform | Post-render workflow | **S** |
| 18 | **Clipboard** (text) | "Copy as CLI command", copy JSON | **S** |
| 19 | **Keyboard-first navigation**, full tab order, shortcuts | Engineers live on the keyboard | **S** |
| 20 | **Non-modal toasts** + real modal dialogs | External-edit reload; destructive confirms | **S** |
| 21 | **HiDPI / per-monitor scaling** | Windows multi-monitor especially | **S** |
| 22 | **Filesystem watcher** (may be a crate, not the toolkit) | Config hot-reload | **S** |
| 23 | **Persisted window/pane geometry** | Sidecar-backed | **O** |
| 24 | **Second window** (report while rendering) | Nice; single window is the right default | **O** |
| 25 | **2D canvas / vector drawing** | **Deliberately not required.** No waveform, no spectrum, no meters. Only a GR sparkline in the report would want it — drop it if the stack is weak here | **O** |
| 26 | **Rich text / inline links in prose** | Clickable `config.rs:698` and README anchors in the why-panel | **O** |

**Negative requirements worth scoring** (they widen the field): no audio I/O, no realtime anything, no video, no 3D, no embedded web view required, no plugin hosting, no MIDI.

---

## 8. Corrections and gaps in the brief

Your premises held up; five additions, one of which changes the plan.

1. **✗ Blocker: the provenance screen is impossible with today's public API.** `overlay()`, `builtin_fallback()`, `preset()`, `validate()`, `check_collisions()` and every `*_tag()` helper except `format_tag` are **private** (`src/config.rs`). `resolve_target` returns only the flattened `ResolvedTarget` — the layer each value came from is discarded. The GUI would have to fork the merge, and CLAUDE.md warns about exactly that drift repeatedly ("`describe()` and the pipeline are separate code paths and drift silently"). **Needed:** a provenance-aware resolve in `config.rs`, e.g. `resolve_target_explained(&self, name) -> Result<(ResolvedTarget, FieldOrigins)>` where `FieldOrigins` records `(Layer, Fate)` per field — computed *inside* `overlay`/`resolve_target`, never re-derived. Roughly a day's work, and it belongs in the library regardless: it would also let the CLI grow `mint --explain <target>`.

2. **✗ No `Config::from_str`.** `from_path` reads from disk and is the only entry point, and `validate`/`check_collisions` are private. A GUI validating in-memory TOML must round-trip through a temp file or reimplement validation. **Needed:** `Config::from_str(&str) -> Result<Config>` running the identical `validate()`.

3. **✗ No enum→string, no variant enumeration.** `ResampleQuality`, `OnClipPolicy`, `LimiterCharacter`, `DitherMode`, `DitherStrength`, `DitherCorrelation`, `BitDepthFormat`, `OutputCodec`, `DsdRate` are `Deserialize`-only with no `Display` and no way to list variants. Every GUI dropdown would hardcode a list that goes stale — the precise failure `PRESET_NAMES` was introduced to fix (the valid-name list was "a third hand-maintained copy inside the error string, and adding a preset silently left it stale"). **Needed:** make the `*_tag` helpers `pub`, plus a `const VARIANTS` per enum.

4. **⚠ `mint.toml` cannot express a project.** There is no input field; inputs are `argv`. A GUI "project" (config + input set) has no home in the contractual format. Drives the sidecar decision in §4 — this is the most consequential thing not in your brief.

5. **Minor corrections.** `RawTarget` has exactly **22** fields (21 deliverable/policy/quality/destination + `preset`), so "~22" is right. `toml_edit 0.22.27` is **already in `Cargo.lock`** transitively via `toml 0.8.23` — format-preserving writeback costs zero new dependencies. And `report.md` §7 documents that several built-in presets encode a mistaken model of streaming delivery; the GUI should present them with the README's own "starting points, not gospel" caveat rather than as authoritative.

**Sources:** [toml_edit on crates.io](https://crates.io/crates/toml_edit) (0.25.13+spec-1.1.0, 14 Jul 2026) · [toml on crates.io](https://crates.io/crates/toml) (1.1.4+spec-1.1.0, 28 Jul 2026) · [toml-rs/toml on GitHub](https://github.com/toml-rs/toml) · [toml_edit docs.rs](https://docs.rs/toml_edit/)

---

# Part IV — Integration-surface audit

What a GUI can stand on in the mint core as it exists today: what is public, what is private, what is missing, and the minimal least-invasive change list to close the gaps.

Read: `src/lib.rs`, `src/config.rs` (1271 l), `src/cli.rs` (529 l), `src/pipeline.rs` (467 l), `src/json_report.rs`, `src/audio/{mod,buffer,io_read,io_write,io_write_dsd,atomic_write}.rs`, `src/ops/{mod,resample,loudness,limiter,bitdepth}.rs`, `src/ops/dsd/mod.rs`, `examples/mint.toml`, `README.md`, `CLAUDE.md`, `.github/workflows/{ci,release}.yml`, plus the vendored sources of `toml 0.8.23` / `toml_edit 0.22.27` / `serde 1.0.228` / `libsoxr-sys 0.1.4`.

## 0. Corrections to the established facts

Nearly all hold. Five refinements, each load-bearing:

1. **"`src/lib.rs` exposes every module publicly" is true but misleading.** Module visibility ≠ item visibility. The *entire batch layer* of `cli.rs` is private (`probe_inputs` :265, `plan_outputs` :282, `print_dry_run` :301, `resolve_inputs` :456, `is_supported_input` :509, `fmt_bytes` :363, `resolve_selected_targets` :220), and so is *whole-config* validation (`Config::validate` config.rs:729, `check_collisions` :1130, `ResolvedTarget::validate_fields` :746) and **every label function except one** (`quality_tag` :1173, `on_clip_tag` :1182, `limiter_character_tag` :1191, `dither_tag` :1223, `dither_strength_tag` :1233, `effective_dither_tag` :1243 — only `format_tag` :1164 is `pub`). Details in §3.
2. **`ResolvedTarget` is fully constructible from outside the crate** (config.rs:299, all 22 fields `pub`). A GUI can bypass TOML entirely and call `pipeline::process` with a hand-built target — but `validate_fields()` is private, so nothing checks it. Biggest single opportunity *and* biggest footgun on the surface.
3. **`--dry-run --json` silently ignores `--json`.** cli.rs:101-103 returns `print_dry_run` before the JSON branch at :167, and `print_dry_run` uses `println!` text. A sidecar GUI has no structured preview at all.
4. **The ~485–700 MB peak-RSS figure has two exceptions.** `loudness::apply_maximize` does `let pristine = buffer.channels.clone()` (loudness.rs:165) — a genuine second full copy, live for the whole stage, so `on_clip = "maximize"` roughly doubles the budget. `bitdepth::mono_mix` (bitdepth.rs:70) adds a 1-channel copy for `dither = "adaptive"`, and `io_write::write_flac` builds an interleaved `Vec<i32>` of the whole buffer (+0.5×) plus the output `ByteSink`. Also: `report.md` §8 defect #3 (8.58 GB) is **stale** — it predates the streaming limiter.
5. **The native-dependency constraint is a *builder* problem, not a *user* problem.** `.github/workflows/release.yml` already static-links soxr for five targets (Linux x86_64/aarch64, macOS x86_64/aarch64, Windows MSVC; lines 94-125, `-DBUILD_SHARED_LIBS=OFF` / vcpkg `x64-windows-static-md`). A sidecar GUI can ship those artifacts with zero runtime deps. Caveat: mint is Apache-2.0 (`LICENSE`), libsoxr is LGPL-2.1, and `libsoxr-sys` itself is LGPL-2.0 — statically linking LGPL into a distributed binary carries a relinking obligation (not legal advice, but it is the one licensing item a shipped desktop product must actually resolve; dynamic linking sidesteps it).

---

## 1. In-process vs sidecar

### 1.1 What already works when linking mint as a crate

| Surface | Location | Verdict |
|---|---|---|
| `pipeline::process(&Path, &ResolvedTarget, Option<u64>) -> Result<ProcessingReport>` | pipeline.rs:162 | Clean. No hidden globals, no I/O to stdout, `Send`-safe args. |
| Header-only probe | `io_read::probe_source` io_read.rs:33, `SourceProbe` :20 (pub fields) | Exactly what a GUI needs (§3). |
| Preview | `describe` config.rs:486, `estimated_output_bytes` :403, `delivery_blocker` :463, `render_output_path` :373 | All `pub` (§4). |
| JSON DTO | `json_report::render(Option<&str>, &[ProcessingReport], &[TaskFailure])` json_report.rs:137 | `pub` — an in-process GUI can render the *same* document per job by passing a 1-element slice. |
| Report structs | `ProcessingReport` pipeline.rs:15, `DsdReport` :62, `TaskFailure` :84, `LoudnessAnalysis` loudness.rs:318 | All pub fields, `Clone`. |
| Batch collision rule | `config::check_planned_outputs` :1024, `PlannedOutput` :1005 (pub fields) | Callable; the GUI builds the `Vec` itself via `render_output_path`. |

**No library code writes to stdout or stderr.** The only `eprintln!` outside `cli.rs` is `main.rs:3`. `env_logger` is initialised only in the private `cli::init_logging` (:518). Verified by grep.

### 1.2 What breaks or is missing in-process

**a) `process()` is one opaque blocking call.** No progress, no cancellation, no intermediate results (§5). For a 5-minute 96 kHz → DSD512 job that is minutes of frozen UI unless run on a worker thread — which works, but leaves the user with a spinner and no Cancel.

**b) There is no logging at all.** `log` and `env_logger` are declared in `Cargo.toml` but have **zero call sites** in `src/` (grep for `log::`, `warn!(`, `info!(` returns nothing). So even a `log` subscriber buys the GUI nothing. Diagnostics exist only as `ProcessingReport.warnings: Vec<String>` (pipeline.rs:52), delivered at the end.

**c) Global state, in practice benign.** Two statics: `atomic_write::SEQ` (AtomicU64, harmless) and `resample::create_soxr`'s `static INIT_LOCK: Mutex<()>` (resample.rs:114-121). The lock is poison-tolerant (`unwrap_or_else(|e| e.into_inner())`) and only wraps `Soxr::create`, which is cheap. **But `create_soxr` is `pub(crate)`** — a GUI cannot construct a resampler for its own preview/audition path, and if it did so via another crate it would race the same libsoxr lazy tables the lock exists to protect. If the GUI ever wants to resample for playback, it must go through `resample::apply` or `ops::dsd::interpolate::SoxrStream` (both `pub`).

**d) Rayon pool ownership is a non-issue.** `cli.rs:147` builds a *scoped* `ThreadPoolBuilder`, never the global pool, and nothing in `ops/` uses rayon (`flacenc` is pinned `default-features = false` precisely to keep its `par` feature off). The GUI owns its own concurrency. Ops are free of shared mutable state by design (CLAUDE.md; `tests/soxr.rs` guards it).

**e) Panics are contained but real.** No `panic = "abort"` profile, so `catch_unwind` works, and `ResolvedTarget`/`&Path` are `RefUnwindSafe`. Live panic sites reachable in principle: `io_write.rs:71,134,138,145` (`unreachable!`), `limiter.rs:625` (`expect`), `io_write_dsd.rs:309`, plus the 17 `unwrap()`s in `resolve_target` (config.rs:655-721) that are safe only while `builtin_fallback()` stays complete. On unwind, `atomic_write::Staged::drop` (atomic_write.rs:144) removes the staging file, so **a panicked or cancelled job leaves no partial deliverable** — a genuinely nice property for the in-process design.

**f) Memory is the strongest argument against in-process.** One job is 485–700 MB (2× with `maximize`). A GUI fanning 8 jobs across cores holds 4–6 GB **in the UI process**; an OOM kills the editor and the user's unsaved project, not a subprocess.

**g) `process()` always re-reads the file.** It calls `io_read::read_audio` itself (pipeline.rs:185). A GUI that has already decoded the master for a waveform display cannot hand the buffer over — it either decodes twice or holds two copies. There is no `process_buffer(&AudioBuffer, …)` entry point.

**h) MP3 is a silent capability hole.** Without `--features mp3`, `codec = "mp3"` resolves, validates, passes `delivery_blocker`, runs the *entire* DSP chain, and fails at `io_write::write_mp3` (io_write.rs:438-441). There is no `pub const MP3_AVAILABLE` for a GUI to grey out the option.

### 1.3 Sidecar: what it buys and what it costs

**Buys:** memory and panic isolation; a hard Cancel (kill the process — and `Staged::drop` won't run on SIGKILL, leaving `.name.<pid>.<n>.mint-tmp` litter, which is documented and glob-safe per atomic_write.rs:31-34); no libsoxr/pkg-config burden on GUI contributors; the ability to ship the prebuilt static binaries release.yml already produces; and a decoupled release cadence.

**Loses, concretely:**

- **All incremental feedback.** `json_report::render` is called exactly once, at cli.rs:169, *after the whole batch completes*. There is no NDJSON-per-task mode. A 40-file album gives one document at the end.
- **The dry-run preview.** Text only (§1, correction 3). Parsing `"  [cd] -> out/cd/song.wav  (52.9 MB)  (exists — will fail without --force)"` and the `"      - "` step lines is brittle by construction.
- **Actionable errors.** `TaskFailure.error` is `format!("{e:#}")` (cli.rs:126) — an anyhow chain flattened to a string. The GUI cannot distinguish "output exists → offer Force" from "decode failed" without substring matching on English prose.
- **Batch-level rejections produce no document at all** (README:385-390): unreadable config, unknown `--target`, unprobeable input, or a planned-output collision → stderr + exit ≠ 0 + empty stdout. Worse: **a mid-batch panic looks identical** (empty stdout, non-zero exit) *even though N files were already written*, which breaks the documented "empty stdout = nothing ran" contract.
- **Two side effects to defend against.** Omitting `--config` *writes* `./mint.toml` from the embedded template (cli.rs:191, 205) — always pass `--config`. And `resolve_inputs` `eprintln!`s "skipping … already named by another argument" (cli.rs:477) even under `--json`.
- **`--jobs` is the only concurrency control**; there is no way to reprioritise or pause.

### 1.4 Is the `--json` schema sufficient to drive a GUI?

Sufficient as a **post-run QC panel**; insufficient as the GUI's data model. Full per-task shape (json_report.rs:20-43):

```rust
target, input, output,
sample_rate: { "in": u32, "out": u32 },
codec, format,            // "s16"|"s24"|"f32" | "<N> kbps" | "1-bit"
bwf: bool,
dither: { applied: bool, noise_shaping: Option<&str> },
dsd: Option<{ rate, rate_hz, intermediate_pcm_rate_hz, interpolation_factor,
              modulator_order, max_modulation_percent, overload_samples,
              loop_resets, samples_per_channel, file_bytes }>,
limiter_gain_reduction_db: Option<f64>,
limiter_avg_gain_reduction_db: Option<f64>,
limiter_active_percent: Option<f64>,
source:    { integrated_lufs, loudness_range_lu, max_momentary_lufs,
             max_short_term_lufs, true_peak_dbtp, plr_db, psr_db },  // all Option<f64>
delivered: { …same… },
warnings: [String]
```

Missing, in rough order of how much a GUI wants them:

| Gap | Why it hurts |
|---|---|
| **No schema/version field** — top level is only `{ config, tasks, failures }` (:13-18) | A GUI pinned to a mint binary it doesn't ship cannot detect drift. Cheapest high-value addition anywhere in this audit. |
| **No `file_bytes` except for DSD** | The one number every render UI shows. GUI must `stat()` (fine locally, wrong for remote). |
| **No channel count, no duration/frames** | Not in `ProcessingReport` either. The GUI already has it from its own probe, but the *report* cannot stand alone. |
| **No applied gain** | `LoudnessApplyResult.gain_db` (loudness.rs:30) is computed and **discarded** — never copied into `ProcessingReport`. This is report.md §9 item 6. |
| **No dither mode/strength/seed, no SRC quality tier** | `dither: {applied, noise_shaping}` cannot distinguish `tpdf` from `adaptive`-degraded-to-tpdf. Reproducibility is unreportable. |
| **Warnings are free-form prose** | No code, no severity. A GUI cannot badge "heavy limiting" differently from "dsd512 player support". |
| **Failures are free-form prose** | No error kind → no "Retry with Force" button. |
| **No elapsed time per task** | Standard in a render queue UI. |

---

## 2. Config round-trip

`RawTarget` (config.rs:41-83) is `Deserialize, Debug, Clone, Default` — **no `Serialize`** — with 22 `Option` fields, under `#[serde(deny_unknown_fields)]`.

### Option A — derive `Serialize`

Mechanically required, not optional:

1. `#[serde(skip_serializing_if = "Option::is_none")]` on **all 22 fields**. Verified necessary: `toml_edit`'s serializer returns `Error::UnsupportedNone` for a `None` value (`toml_edit-0.22.27/src/ser/mod.rs:114,140`), so a single unskipped `None` fails the whole write.
2. `Serialize` on the 8 enums (`ResampleQuality`, `OnClipPolicy`, `LimiterCharacter`, `DitherStrength`, `DitherCorrelation`, `DitherMode`, `BitDepthFormat`, `OutputCodec`) plus `DsdRate` (`ops/dsd/mod.rs:30`). The existing `rename_all` attributes then produce the right wire strings for free — including `DitherCorrelation::MidSide`'s explicit `#[serde(rename = "mid_side")]` (:152).
3. `Config` and `MetaConfig` too. Structurally the emitted document is valid: every `Config` field is a table and every `RawTarget` field is a scalar, so the toml serializer's "values before tables" rule is satisfied.

**Failure modes:**

- **Comment annihilation.** `examples/mint.toml` is 92 lines of which ~55 are pedagogy, and it is `include_str!`-embedded at cli.rs:191 as the bootstrap template. A GUI Save wipes all of it. Unacceptable for the file mint hands new users.
- **Target reordering.** `Config.targets` is a `BTreeMap<String, RawTarget>` (config.rs:27). Round-tripping *alphabetises* the user's targets. `[target.streaming]` written before `[target.cd]` comes back reversed. A silent, visible, un-undoable diff on every save.
- **Formatting loss.** The aligned `dir     = "./out"` style throughout `examples/mint.toml` becomes canonical single-space output.
- **`deny_unknown_fields` cuts the other way on write:** anything the GUI's version of mint doesn't know is dropped from the file, not preserved. A newer mint's config opened in an older GUI loses fields on save.
- **Non-UTF-8 `dir`.** `serde`'s `impl Serialize for Path` errors with `"path contains invalid UTF-8 characters"` (`serde-1.0.228/src/core/ser/impls.rs:908-917`). A native folder picker on macOS/Linux can return one. *Note this is inherent to TOML being UTF-8, so `toml_edit` has the same limit — the GUI must reject such paths at the picker, either way.*

### Option B — `toml_edit` (format-preserving)

Latest stable: **`toml_edit 0.25.13+spec-1.1.0`, published 2026-07-14** (crates.io API). `toml 0.8.23` already pulls in `toml_edit 0.22.27` transitively (`Cargo.lock:1101`), and the current `toml` crate is at **1.1.4+spec-1.1.0 (2026-07-28)**.

Preserves comments, key order, whitespace and unknown keys; edits are surgical (`doc["target"]["cd"]["lufs"] = value(-12.0)`).

**Failure modes:**

- **No type safety.** `doc["target"]["cd"]["dither"] = value("shapd")` compiles happily and produces a config that fails to load. Every write is a stringly-typed poke, and the enum→string mapping must be hand-written *because the tag functions are private* (config.rs:1173-1271). Two spellings of every enum then exist in the tree.
- **Version split.** Adding `toml_edit ^0.25` while `toml 0.8` carries `0.22.27` puts two copies of `toml_edit`+`toml_datetime` in the tree — compiles, but `toml::Value` and `toml_edit::Value` become unrelated types. Clean fix: bump mint to `toml 1.x`, which pairs with `toml_edit 0.25`. Note that `toml 0.9` changed integer handling (validated normalised strings rather than direct `i64` parsing) — behaviourally invisible for mint's small scalar surface, but it is a real bump, not a patch.
- **Creating a target from scratch is verbose** — you assemble an `InlineTable`/`Table` field by field, which is where a Serialize impl would have been one line.

### Recommendation

**`toml_edit` for writes, `serde`/`Deserialize` for reads — and add `Serialize` anyway, for a different job.**

The two are not exclusive and they answer different questions. `toml_edit` is the only correct answer for *"save the user's file"*: `examples/mint.toml` is a document, not a serialisation, and a Save that eats the comments and re-sorts the targets will be reported as data loss. `Serialize` is the right answer for *"send this target to the preview pane / to a sidecar / to an undo snapshot / to a test fixture"*, where no file is involved.

The hybrid that keeps both honest: derive `Serialize` on `RawTarget`, then implement the writer as *serialize one target to a `toml_edit::Table`, then splice that table into the existing `DocumentMut`*, replacing only keys whose values actually changed. That gives one enum→string mapping (serde's), no hand-written spelling table, comments preserved on every untouched line, and order preserved. Cost: `skip_serializing_if` × 22 + `Serialize` × 9 enums + ~80 lines of splice logic.

---

## 3. Live validation — exact reachability

**Reachable from outside the crate today:**

| Item | Location |
|---|---|
| `Config` (`Deserialize`), `Config::from_path`, `target_names`, `resolve_target` | config.rs:20, 613, 623, 628 |
| `check_planned_outputs`, `PlannedOutput` | :1024, :1005 |
| `PRESET_NAMES` (11), `MP3_BITRATES`, `format_tag`, `SizeEstimate` | :940, :248, :1164, :271 |
| `ResolvedTarget` + all fields + `pcm_stage_rate` / `delivery_rate` / `format_placeholder` / `render_output_path` / `estimated_output_bytes` / `delivery_blocker` / `describe` | :299, 344, 355, 364, 373, 403, 463, 486 |
| `io_read::probe_source`, `SourceProbe` | io_read.rs:33, :20 |
| `io_write::FLAC_MAX_SAMPLE_RATE`, `io_write_dsd::MAX_CHANNELS` | io_write.rs:276, io_write_dsd.rs:109 |

**Private — the gaps:**

| Item | Location | Consequence for a GUI |
|---|---|---|
| `Config::validate` | :729 | **No whole-config validation from in-memory text.** `from_path` is the only door and it takes a `Path`. |
| `check_collisions` | :1130 | Two targets resolving to one output path is undetectable without reimplementing the probe-stem trick. |
| `ResolvedTarget::validate_fields` | :746 | A hand-built `ResolvedTarget` (which the GUI *can* build) is unvalidated. |
| `preset()`, `builtin_fallback()`, `overlay()`, `resolve_dsd()` | :959, :901, :873, :851 | The GUI cannot show "this value came from the preset / from `[defaults]` / from you" — the single most valuable thing a config UI can display for a 4-layer merge, and it is exactly what `overlay` knows. Nor can it show what a preset *contains* before applying it. |
| all `*_tag` fns except `format_tag` | :1173-1271 | No CLI-identical labels for dropdowns; `effective_dither_tag` (:1243) in particular encodes the s16-only downgrade rule. |
| `pipeline::guard_output_path`, `same_file` | pipeline.rs:102, :135 | The GUI cannot ask "will this be refused?" — the `(dev, ino)` hard-link comparison is the subtle part it would have to clone. |
| `cli::is_supported_input` | cli.rs:509 | The `wav|wave|aif|aiff` list must be duplicated in the file picker. |

**Practical answer for "validate as the user types":** yes, mostly, with one ugly workaround.

```
GUI text ──toml::from_str::<Config>()──►  parse + deny_unknown_fields errors
                                          (toml::de::Error::span() → byte range;
                                           confirmed at toml-0.8.23/src/de.rs:70)
        └─for each name──► Config::resolve_target(name)  ──► per-target errors
                                          (merge conflicts, format-on-DSD, ceiling>0,
                                           FLAC float/rate, DSD power-of-two, mp3_bitrate,
                                           missing {stem})
        └─cross-target──►  check_collisions  ✗ PRIVATE  (reimplement via render_output_path
                                                          with a probe stem, or write a
                                                          temp file and call from_path)
```

The span support is genuinely good news: `toml::de::Error::span()` gives a `Range<usize>` for parse *and* `deny_unknown_fields` errors, so a GUI editor can underline the offending key. `anyhow` preserves it through `Config::from_path`'s `with_context` — `err.downcast_ref::<toml::de::Error>()` recovers it. **Everything raised by `bail!` in `validate_fields`/`resolve_target` has no span and no structure**, only prose naming the target.

**Input rate + channels, cheaply:** `io_read::probe_source` (io_read.rs:33) is exactly the header-only probe you asked about. One `File::open` + a symphonia container probe; no packet is decoded. Returns `sample_rate: u32` (hard-required), `frames: Option<u64>`, `channels: Option<usize>`. It already encodes two quirks worth not re-deriving: the AIFF SSND 8-byte overcount correction (:87-99) and the `0xFFFFFFFF` streamed-WAV cap against file size (:100-112). Sub-millisecond per file — a GUI can call it on every file-picker selection and cache by (path, mtime). That feeds `delivery_blocker(source_rate, channels)` (the FLAC >96 kHz and DSD >5.1 rules) and `estimated_output_bytes(rate, frames, channels)` directly.

---

## 4. Preview surface

`describe(source_rate) -> Vec<String>` (config.rs:486-578) is **presentation-formatted, not structured**. Representative output:

```
loudness -> -14.0 LUFS (ceiling -1.0 dBTP, limit; balanced release; linked; soft_clip=false)
resample 96000 -> 44100 Hz (vhq)
re-check true peak against the ceiling and re-verify loudness (corrected within the headroom)
quantize -> s16 + tpdf + noise-shaping (psychoacoustic) (normal)
write WAV
```

What a GUI preview pane wants and cannot get from this:

- **Per-step identity**, to highlight the step the user just changed (change `lufs` → flash the loudness row) or to grey out skipped steps. Requires substring matching today.
- **Per-step parameters as data**, for a two-column "step / detail" layout, tooltips, or an icon per stage kind.
- **The conditional/unconditional distinction**, which the code knows and the string only hints at: the recheck steps are "unconditional *steps* of a resampling chain" that are "conditional at run time" (comment at :527-531).
- **The warning channel is fused into the list**: `"note: dsd512 has limited player support"` (:606) arrives as just another step, and `delivery_blocker`'s output is a *separate* call the caller must remember to make (cli.rs:352).

**Cost of a structured version:** introduce `pub enum ChainStep { Loudness{target_lufs, ceiling, policy, character, soft_clip, linked, max_limiting_db}, Transcode{…}, Resample{from, to, quality, derived: bool}, PostSrcVerify{loudness: bool}, Quantize{format, dither: DitherMode, effective_shaping: Option<&'static str>, strength}, EncodeMp3{kbps}, Interpolate{factor, to_hz, rate_tag, quality}, Modulate{order, input_scale}, Write{container}, Note(String) }` plus `pub fn chain(&self, source_rate: u32) -> Vec<ChainStep>`, then reimplement `describe()` as a `Display`/format pass over `chain()`. **~100-140 lines, no DSP touched, zero runtime cost.**

Two constraints on doing it:

1. CLAUDE.md's rule that `describe()` and the `match target.codec` in `pipeline.rs` must both stay exhaustive and mirror each other. Making `chain()` the single producer and `describe()` a pure renderer *improves* that (two representations become one + a formatter); adding `chain()` *alongside* `describe()` makes it worse (three).
2. **~15 existing assertions read `describe()` strings** — `tests/config_parse.rs:74,132,242,247,268,275,298,305,328,337,383,421,489,510` and `tests/dsd_pipeline.rs:389,416`, all substring matches like `.contains("noise-shaping (psychoacoustic)")`. The renderer must reproduce the text byte-for-byte or those change too.

`estimated_output_bytes` (:403) needs nothing: it already returns `Option<SizeEstimate>` with `Exact`/`About`/`AtMost` (:271) and a `qualifier()` (`""`/`"~"`/`"<= "`) — precisely the three-way honesty a GUI size column needs. Only `cli::fmt_bytes` (:363, private) has to be duplicated.

---

## 5. Progress + cancellation — minimal change

**Design:** one new borrowed control struct, threaded down; `process()` kept as a wrapper so the CLI and every test are untouched.

```rust
// new, e.g. src/progress.rs
pub enum Stage { Probe, Decode, AnalyzeSource, Loudness, Resample, PostSrcVerify,
                 Ceiling, AnalyzeDelivered, Quantize, Encode }

#[derive(Default, Clone, Copy)]
pub struct JobControl<'a> {
    pub cancel:   Option<&'a AtomicBool>,
    pub progress: Option<&'a (dyn Fn(Stage, f64) + Sync)>,   // fraction 0..=1 within the stage
}
impl JobControl<'_> {
    #[inline] fn check(&self) -> Result<()> { /* Relaxed load; bail!("cancelled") */ }
    #[inline] fn tick(&self, s: Stage, done: usize, total: usize) { /* … */ }
}

pub fn process(i: &Path, t: &ResolvedTarget, seed: Option<u64>) -> Result<ProcessingReport>
    { process_with(i, t, seed, JobControl::default()) }
pub fn process_with(i: &Path, t: &ResolvedTarget, seed: Option<u64>,
                    ctl: JobControl<'_>) -> Result<ProcessingReport>
```

**Why this respects "the buffer is the budget":** an `&AtomicBool` and a `&dyn Fn` are two pointers. Nothing is allocated, nothing is sized in frames, no audio is copied. The only per-block cost is one `Relaxed` atomic load and one non-inlined call per *block* — at limiter `BLOCK_FRAMES = 4096` (limiter.rs:70) that is ~24 calls/second of audio at 96 kHz; at resample `BLOCK_FRAMES = 16_384` (resample.rs:11) ~6/second. With `JobControl::default()` both are a `None` branch the optimiser folds out, so **the CLI's codegen is unchanged**.

**Natural yield points, in order of value:**

| Stage | Loop | Note |
|---|---|---|
| `ops::dsd::encode_dsf` | `while pos < frames` dsd/mod.rs:182 | **The one that matters.** Longest stage by far; `step` is 64–1024 frames (:177), so granularity is already excellent. Cancel here also stops a multi-GB write. |
| `ops::resample::apply` | `while pos < in_frames` resample.rs:67 | 16 k frames/block. |
| `ops::limiter::limit_channels` | `while start < frames` limiter.rs:439 | 4 k frames/block. Called repeatedly by `limit_and_verify`'s convergence loop and once per `apply_maximize` pass — pass the control through both so progress doesn't stall. |
| `ops::loudness::analyze` | `while pos < frames` loudness.rs:363 | Already hops in 100 ms slabs for the momentary meter. Free. |
| `io_read::read_audio` | `loop { format.next_packet() }` io_read.rs:165 | Per-packet. Needed: decoding a 461 MB master is seconds of un-cancellable work today. |
| `bitdepth::quantize_in_place` | per-channel sample loops, bitdepth.rs:97 | Coarse per-channel checks suffice. |

**Blind spots that stay blind without more work:**

- `measure_lufs` (loudness.rs:271) and `measure_true_peak_dbtp` (:283) feed the **whole buffer** in one `add_frames_planar_f64` call. Neither can be interrupted or reported on. Fix is trivial and matches `analyze`: chunk the feed. (Watch out — `limit_and_verify` and `apply_maximize` call `measure_lufs` once per pass, up to `MAX_MAKEUP_PASSES` times.)
- `io_write::write_flac` / `write_mp3` / `write_wav` are single blocking calls into encoder crates; a `flacenc` `MemSource` encode of a full album is one un-yielding call. Leave it — report "encoding" as an indeterminate stage.
- The adaptive-dither STFT analysis (`dither/psychoacoustic.rs`, `adaptive.rs`) is a whole-file pass with its own internal frame loop; add a tick there only if `dither = "adaptive"` proves slow enough to notice.

**Cancellation semantics come out right for free:** `ctl.check()?` propagates as an `Err` through the same `?` path every other error uses, unwinding past `Staged::drop` (atomic_write.rs:144) which deletes the staging file. **A cancelled job leaves the previous render intact and no partial file.** That is worth stating in the API doc, because it is the behaviour a mastering engineer will assume and would be furious to find absent.

**Optional bonus, still within budget:** a waveform min/max envelope decimated 1:512 costs `frames/512 × 2 × 8 B` ≈ 0.4 % of the buffer (~1.8 MB for a 5-minute 96 kHz stereo master). Computing it during the decode loop and returning it on `ProcessingReport` would give the GUI a before/after overview display without a second decode. This does *not* violate the invariant — it is not sized in frames at 1:1.

---

## 6. Ranked change list

Least invasive first. "Risk" is risk to shipped audio output unless stated otherwise.

| # | Change | Where | Risk |
|---|---|---|---|
| 1 | `PartialEq` on `ResampleQuality`, `OnClipPolicy`, `LimiterCharacter`, `DitherMode`, `BitDepthFormat` | config.rs:85, 94, 107, 162, 205 | None. Currently blocks every toolkit's dropdown widget (egui `selectable_value`, iced `PickList` both require it). |
| 2 | `pub` on `quality_tag`, `on_clip_tag`, `limiter_character_tag`, `dither_tag`, `dither_strength_tag`, `effective_dither_tag` (+ `Display` impls) | config.rs:1173-1271 | None. Removes a whole duplicated spelling table from the GUI. |
| 3 | `ALL: [T; N]` consts per enum, mirroring `DsdRate::ALL` | config.rs / dsd/mod.rs:65 | None. Nothing enumerates variants today except `PRESET_NAMES`. |
| 4 | `pub const MP3_AVAILABLE: bool = cfg!(feature = "mp3")` (or a `capabilities()`) | config.rs or lib.rs | None. Today an MP3 target runs the full chain then fails at io_write.rs:438. |
| 5 | `"schema_version"` in the JSON document | json_report.rs:13-18 | None; additive. Highest value-per-line on this list for a sidecar. |
| 6 | `pub fn Config::from_str(&str) -> Result<Config>` (parse + `validate`), or just `pub fn validate` | config.rs:613, 729 | None. Removes the temp-file workaround for live validation. |
| 7 | `pub` on `check_collisions` and `ResolvedTarget::validate_fields` | config.rs:1130, 746 | None. Stops the GUI from reimplementing two rules that must not drift. |
| 8 | `pub` on `cli::is_supported_input`, `fmt_bytes`, `fmt_db` (or move to a `util` module) | cli.rs:509, 363, 376 | None. Three duplications avoided; the extension list is a correctness surface. |
| 9 | `pub` on `pipeline::guard_output_path` (or a `pub fn output_conflict(input,output,overwrite) -> Option<Conflict>`) | pipeline.rs:102 | Low. Must stay a *query*; CLAUDE.md is explicit the policy must not move into a writer. |
| 10 | `Serialize` + `skip_serializing_if` on `RawTarget`/`Config`/9 enums | config.rs:41, 20, 85-225 | Low, but it creates a second contract: from then on every new field must be added to the serialize path or it silently vanishes on save. Add it to the CLAUDE.md "new target field" checklist. |
| 11 | Report the figures already computed and thrown away: `gain_db`, dither mode/strength/seed, SRC tier, output `file_bytes`, channels, frames, elapsed | pipeline.rs:15-53 + json_report.rs + cli.rs:380 | Low; touches the 3-place report checklist (struct → DTO → console line). This is report.md §9 item 6. |
| 12 | Structured warnings/failures (`enum WarningKind` / `enum FailureKind` alongside the prose) | pipeline.rs:52, 84 | Low-medium. Every warning site (~12 in pipeline.rs + loudness.rs) needs a kind; prose must stay for the CLI. |
| 13 | `ChainStep` + `chain()`, with `describe()` reimplemented as its renderer | config.rs:486-609 | Medium — **not to audio, to the dry-run's truthfulness**. ~15 test assertions read the strings; the exhaustive-codec-match discipline must survive the refactor. Do it as *one* producer + a formatter, never as a parallel third representation. |
| 14 | `JobControl` + `process_with`, threaded to the 6 loops in §5 | pipeline.rs:162 + resample.rs:67, limiter.rs:439, dsd/mod.rs:182, loudness.rs:363, io_read.rs:165, bitdepth.rs:97 | Medium. Touches DSP call signatures but not DSP *math*. The bit-equality reference tests (`ops::limiter`, `ops::resample` `#[cfg(test)] mod reference`) are the proof it changed nothing — run them. Do **not** let the callback see or copy audio. |
| 15 | Chunk the feed in `measure_lufs` / `measure_true_peak_dbtp` | loudness.rs:271, 283 | Medium. ebur128 is stateful; chunking must be verified bit-identical (`tests/loudness.rs` pins the channel-map behaviour both ways). |
| 16 | `pub fn process_buffer(&mut AudioBuffer, …)` so a GUI can hand over already-decoded audio | pipeline.rs:162 | Medium. Splits the function around the `read_audio` at :185 and must preserve the "nothing costs a decode before it is known possible" ordering — the probe/`delivery_blocker`/`guard_output_path` prologue (:169-183) has to stay in front. |
| 17 | Per-task incremental JSON (NDJSON) mode for the sidecar | cli.rs:167-172 | Medium. New CLI surface + a documented second schema; the "empty stdout = nothing ran" contract (README:385) has to be restated. |
| 18 | `--dry-run --json` | cli.rs:101 + a DTO over `chain()`/`estimated_output_bytes` | Medium; depends on #13. |
| 19 | Bump `toml 0.8.23 → 1.x` so the GUI can share one `toml_edit 0.25` | Cargo.toml | Medium. A major bump (integer handling changed in 0.9); `tests/config_parse.rs` is the safety net. Only needed if the GUI takes the `toml_edit` route (it should). |
| 20 | `pub` on `resample::create_soxr` | resample.rs:108 | **Highest risk on this list, for its size.** The `INIT_LOCK` is only sound because construction is funnelled through one site (CLAUDE.md: "Every Soxr instance in the codebase must be built here"). Publishing it invites a GUI audition path to race libsoxr's lazy tables. Prefer exposing `SoxrStream` (already `pub`) instead. |

### Bottom line

**Sidecar for execution, in-process for everything else** is the shape this codebase is actually built for. The read-only half of the surface — `probe_source`, `resolve_target`, `delivery_blocker`, `describe`, `estimated_output_bytes`, `render_output_path` — is already public, cheap, side-effect-free and precisely the set a live config editor needs; linking mint as a crate *just for those* costs the GUI nothing but the libsoxr build dependency and gives it real validation instead of a reimplementation that will drift. The execution half is where in-process hurts: 485 MB–1.4 GB per job inside the UI process, no cancel, no progress, and a panic that takes the editor with it — against a sidecar whose only real cost is that `--json` arrives once at the end and `--dry-run` isn't machine-readable at all (#17, #18).

If only three things get built: **#5 (schema version)**, **#14 (JobControl)**, **#13 (ChainStep)**. The first makes a sidecar safe to depend on, the second makes any GUI feel like a desktop app rather than a wrapper around a frozen window, and the third is the difference between a preview pane and a `<pre>` block.

Sources: [crates.io toml_edit](https://crates.io/crates/toml_edit) · [crates.io toml](https://crates.io/crates/toml) · [toml v0.9 release notes](https://epage.github.io/blog/2025/07/toml-09/) · [toml_edit CHANGELOG](https://github.com/toml-rs/toml/blob/main/crates/toml_edit/CHANGELOG.md)

---

# Part V — Packaging and toolchain audit

The reality of shipping a Rust binary with a system C dependency as a signed, installable desktop app on three platforms. Much of this applies to the existing CLI releases already, independently of whether a GUI is ever built.

**Audit date:** 2026-08-13 · **Host used for measurements:** macOS 15 (Darwin 25.6.0), Apple Silicon (arm64), rustc 1.92.0
**Repo state:** `main` @ `e61fe91`, mint 0.1.0

> **Verification note.** Everything labelled *measured* was run on macOS arm64 in a scratch directory outside the repo. **No file in `/Users/thomas.mahler/code/mint` was created, modified or deleted.** Claims I could not execute (x86_64 engine selection, MSVC linking, cross-builds) are labelled *unverified* explicitly.

## Corrections to your established facts

Your brief is accurate on all checkable points. Confirmations and two refinements:

| Claim | Status |
|---|---|
| `src/lib.rs` exposes all modules publicly | **Confirmed** — `audio, cli, config, dither, json_report, ops, pipeline`; `main.rs` is a 6-line shim over `mint::cli::run()` |
| `pipeline::process(&Path, &ResolvedTarget, Option<u64>) -> Result<ProcessingReport>` | **Confirmed verbatim** (`src/pipeline.rs:162`) |
| `Config`/`RawTarget` are Deserialize-only | **Confirmed** — every `#[derive]` in `config.rs` is `Deserialize`, no `Serialize` anywhere |
| 11 presets in `PRESET_NAMES` | **Confirmed** (`config.rs:940`) |
| libsoxr unconditional, via pkg-config | **Confirmed, and worse than stated** — see §1 |
| `mp3` vendors libmp3lame via `cc` | **Refinement:** only on a *Windows host*. On Unix hosts it uses **autotools**, not `cc`. This is a cross-compilation landmine — see §2.3 |
| Release binaries need a system libsoxr | **Refinement:** `release.yml` already static-links soxr. Your packaging problem is half-solved; the unsolved halves are LGPL compliance and cross-building |

---

## 1. libsoxr — the central packaging constraint

### 1.1 `libsoxr-sys` strictly requires a preinstalled system library

`libsoxr 0.2.9` → `libsoxr-sys 0.1.4`. The entire build script is a pkg-config probe with **no vendoring, no fallback, no `static`/`vendored` feature**:

```rust
// libsoxr-sys-0.1.4/build.rs — complete logic
if std::env::var("DOCS_RS").is_err() {
    if let Err(e) = pkg_config::probe_library("soxr") { /* panic */ }
}
```

`links = "soxr"`, declared license **`LGPL-2.0`**.

**Both crates are abandoned.** Last commits: `libsoxr-sys` **2023-04-12**, `libsoxr-rs` **2023-04-12** (6 and 13 commits total respectively). `libsoxr-rs` has an *open* issue since 2023-04-07: *"split channels are not working, only interleaved"* — directly adjacent to mint's planar `Vec<Vec<f64>>` buffer.

**The obvious alternative is not one.** The `soxr` crate (0.6.0, haileys, 2024-01-25) is a nicer safe wrapper but its `Cargo.toml` reads `libsoxr-sys = "0.1"` — **the same pkg-config-only sys crate**. Switching wrappers buys nothing for packaging.

### 1.2 There *is* a vendoring crate — and it silently degrades mint's top quality tier

`soxr-sys 0.1.3` (2026-04-02, from **LiveKit**'s `livekit/rust-sdks`, declared Apache-2.0) **vendors the full soxr 0.1.3 C tree and compiles it with `cc`** — no pkg-config, no CMake, no system library. 500 KB crate, `cargo add soxr-sys` and you are done on every platform.

That looked like the answer. It is not, unmodified.

Its `build.rs` compiles a **reduced file list** (`// TODO(theomonnom): Add SIMD support`), and its `src/soxr-config.h` hard-codes:

```c
#define WITH_CR32 1
#define WITH_CR32S 0
#define WITH_CR64 0      /* <-- double-precision engine OFF */
#define WITH_CR64S 0
#define WITH_HI_PREC_CLOCK 0   /* <-- upstream default is ON */
```

Upstream `CMakeLists.txt` defaults **all** of `WITH_CR64`, `WITH_CR32S`, `WITH_CR64S`, `WITH_HI_PREC_CLOCK` to **ON**.

Why this bites mint specifically — soxr's engine dispatch (`src/soxr.c:433-451`):

```c
|| !(WITH_CR64 || WITH_CR64S) || (p->q_spec.precision <= 20 && ...)
```

With both 64-bit engines compiled out, that first disjunct is *unconditionally true*, so **every** request takes the single-precision `float` branch — including `SOXR_VHQ` (precision 28), which is what `ResampleQuality::Vhq` maps to in `src/ops/resample.rs:135`. No error, no warning.

**Measured** (two separate binaries — see the methodology warning in §1.3 — querying `soxr_engine()`, 96 kHz → 44.1 kHz):

| Build | HQ (prec 20) | **VHQ (prec 28)** |
|---|---|---|
| LiveKit `soxr-sys` 0.1.3 (vendored, default) | `cr32` | **`cr32`** ← silent downgrade |
| Homebrew `libsoxr` 0.1.3 arm64 (what mint uses today) | `cr32` | **`cr64`** |
| Same vendored sources, `WITH_CR64 1` + `cr64.c` added | `cr32` | **`cr64`** |

Deviation from the system library on a 3-tone test signal (997 Hz / 8 kHz / 19 kHz):

| Vendored config | max &#124;diff&#124; at VHQ |
|---|---|
| LiveKit default | `4.35e-07` (**−127.2 dBFS**) — above the 24-bit LSB (−144.5 dBFS) |
| With `cr64` enabled | `2.22e-16` (**−313 dBFS**) = **1 ULP of f64** |

The broadband in-band noise floor is *unchanged* (−157 dBFS both ways) — the error is signal-correlated single-precision arithmetic, not added noise. So this is not an audible catastrophe. But it silently invalidates the `vhq` tier README.md:437 sells as *"reference-grade multi-stage SRC with a ~150 dB stopband"*, and mint's `#[cfg(test)] mod reference` blocks assert **bit equality via `to_bits()`** — a culture this quietly undermines.

**The fix is ~4 lines.** The crate already ships `cr64.c`, `cr64s.c`, `cr32s.c`, `pffft*.c` — LiveKit just doesn't compile them. Vendor the crate (or fork it), add `cr64.c` to the file list and flip two defines in `soxr-config.h`. I did exactly that and got the 1-ULP result above.

### 1.3 Methodology warning worth recording

My first comparison linked *both* `soxr-sys` and `libsoxr-sys` into one binary and reported "bit-identical". **That was an artifact.** Both export the same C symbols (`soxr_create`, `soxr_quality_spec`, …); the static archive shadowed the dylib, so I was comparing the vendored build against itself. Re-running as two separate binaries produced the real (differing) result.

This is a live hazard for mint too: if a GUI ever links a second soxr consumer (an audio-playback crate, a preview engine), the ODR violation is **silent** on macOS/Linux.

### 1.4 The finding that outranks the packaging question

`mint` is **already not reproducible across platforms**, today, before any GUI exists.

**Measured** — engine name strings in real distributed binaries:

- Homebrew arm64 `libsoxr.0.1.2.dylib` → contains `cr32`, `cr64`, `vr32` (**no SIMD**; `FindSIMD64.cmake` probes for AVX, absent on arm64)
- Debian bookworm `libsoxr0_0.1.3-4_amd64.deb` → contains `cr32`, **`cr64s`** (AVX), and *not* `cr64`

So VHQ resolves to `cr64` on macOS-arm64, `cr64s` on Debian-amd64, and `cr32` under LiveKit's vendored build. Three different arithmetic paths. `README.md:571` advertises bit-for-bit reproducible output "useful for reproducible builds and A/B testing" — that guarantee covers the dither RNG seed, but the **resampler silently breaks it across platforms**, and nothing in the test suite can see it because every reference test compares mint-to-mint on one machine.

**Vendoring actually fixes this**, and that is the strongest argument for it — one engine everywhere, chosen by you, pinned in your repo. Recommendation: vendor with `WITH_CR64=1` and SIMD **off**. Uniform, correct, and the throughput cost is negligible for this workload (**measured**: 282 ms vs 266 ms for 300 s of mono 96→44.1 kHz VHQ, ~6%, i.e. >1000× realtime either way; the file is in RAM and rayon is already saturating cores across jobs).

### 1.5 Options ranked

| Option | Verdict |
|---|---|
| **Fork/vendor LiveKit `soxr-sys` + enable `cr64`** | **Recommended.** Kills pkg-config, vcpkg, the hand-written `.pc` hack, and cross-compile breakage in one move; fixes cross-platform reproducibility. Cost: you own ~50 C files and the LGPL obligation becomes unavoidable and explicit (§2). |
| Keep current `release.yml` (CMake static build) | Works today and is already written. But needs CMake + a network fetch on every runner, keeps the `soxr.pc` forgery on Windows, and leaves the platform-divergence bug in place. |
| Switch to the `soxr` crate | **Pointless** — same `libsoxr-sys` underneath. |
| `media-soxr-sys` 0.1.0 (2025-10-18, LGPL-2.1) | Not investigated in depth; 545 downloads, one release. Too thin to bet a product on. |
| Return to `rubato` (5.0.0, 2026-08-10, MIT/Apache-2.0, pure Rust, 9.1M downloads) | Erases the entire native-dependency problem and is *far* healthier than libsoxr's bindings. But CLAUDE.md records this backend as deliberately removed; it's a DSP-quality decision, not a packaging one. Worth re-examining **only** if signing/packaging cost dominates — and it would need its own verification harness. |

### 1.6 Windows/MSVC specifics

Your current `release.yml` already picks the right triplet, and the reasoning in its header comment is correct:

- **`x64-windows-static-md`** = static soxr + **dynamic CRT**, matching Rust's MSVC default (`/MD`). This is the correct choice.
- The failure mode you avoided: `x64-windows-static` (full `/MT`) against Rust's default `/MD` produces `LNK2038: mismatch detected for 'RuntimeLibrary'`, or — worse — links and then corrupts the heap when a CRT allocation crosses the boundary. Forcing `-C target-feature=+crt-static` to match makes soxr work but breaks other things and changes the DLL story.
- `x64-windows` (shared) works but means shipping `soxr.dll` next to `mint.exe` and getting it into the MSI payload — an extra thing to sign.
- The **`soxr.pc` forgery** is the fragile part: upstream gates `.pc` generation behind `NOT WIN32`, `vcpkg_fixup_pkgconfig` only rewrites files that exist, and `libsoxr-sys` has no non-pkg-config path. You are hand-writing a file to satisfy a probe. Vendoring (§1.2) deletes this entire step, plus the `choco install pkgconfiglite` dependency.

**Unverified:** I could not test MSVC linking from this host. `cc`-based vendoring on MSVC is the well-trodden path (`mp3lame-sys` already does it), so I rate the risk low, but it must be proven on a Windows runner.

---

## 2. The `mp3` feature, and the licensing problem that is bigger than MP3

### 2.1 Patents: a non-issue

MP3 patent licensing ended **23 April 2017** when Fraunhofer/Technicolor terminated the program after the last core patents (incl. US6009399) expired. No royalty, no patent risk in 2026.

### 2.2 Copyleft: a real issue, and it is **not** limited to MP3

This is the finding with the most legal exposure, and it applies to the **CLI you ship today**, not just a future GUI.

| Component | License | Linkage in `release.yml` |
|---|---|---|
| **mint** | **Apache-2.0** (verified: `LICENSE` is the 201-line Apache 2.0 text) | — |
| **libsoxr** | **LGPL-2.1-or-later** (verified in soxr's own `LICENCE`; Homebrew agrees) | **static, unconditional** |
| **libmp3lame** via `mp3lame-sys` | crate declares **LGPL-3.0**, and the crate ships the **LGPL v3 text** as its `LICENSE` | static, opt-in |
| flacenc / symphonia / ebur128 / rustfft | Apache-2.0 / MIT | — |

**Two concrete gaps:**

1. **`release.yml:149` and `:157` package only `README.md` and `LICENSE`** (Apache-2.0). Every published mint release therefore ships **statically-linked LGPL-2.1 code with no copy of the LGPL, no copyright notice, and no relinking offer**. LGPL-2.1 §6 requires, for a statically linked combined work, that you supply the LGPL text, the copyright notice, *and* a means to relink (object files, or a written offer). This is a straightforward compliance bug in the workflow as it stands — and vendoring soxr (§1.2) makes it more visible, not less.

2. **A GUI bundle makes it structurally harder.** A `.app`/`.msi` is a single signed blob; "here are the object files so you can relink" is awkward but still satisfiable *because mint is Apache-2.0 and the build is public*. Practical discharge: ship a `THIRD-PARTY-LICENSES` file in the bundle, and point at the tagged CI build that reproduces it.

**Apache-2.0 ↔ LGPL-2.1 (not legal advice).** Apache-2.0 is famously one-way-incompatible with **GPLv2** because of its patent/indemnity terms. For **LGPL-2.1 §6 linking** the combined work may be distributed under your own terms provided the LGPL obligations are met, so this is generally treated as workable — but note the interaction is with §6, not §3, and mixing in **LGPL-3.0** LAME adds a *second*, differently-worded regime to the same bundle. **Get this in front of ista legal before shipping a branded binary.** I am flagging structure, not rendering an opinion.

**Mac App Store is off the table** while LGPL code is statically linked — the classic VLC/App Store conflict (App Store terms vs LGPL §6 relinking). Direct **Developer ID** distribution is unaffected, which is the right channel for this audience anyway.

**Cheapest de-risking:** keep `mp3` opt-in and **off** in shipped GUI builds until someone actually asks. That removes LGPL-3.0 entirely and leaves one LGPL-2.1 obligation. AAC is already excluded for licensing reasons (`README.md:326`), so this is consistent with existing policy.

### 2.3 `mp3lame-sys` cannot be cross-compiled to Windows

Its `build.rs` selects the build path with **`#[cfg(unix)]` / `#[cfg(windows)]`**. Build scripts compile for the **host**, so these are *host* predicates, not target predicates:

- host Windows → `cc::Build` with `configMS.h` (works, incl. `clang-cl`)
- host Unix → **`autotools::Config`** (`./configure`, `make`)

Cross-compiling `--target x86_64-pc-windows-msvc` from Linux/macOS therefore runs **autotools targeting MSVC**, which autotools does not support. It will fail, or produce the wrong artifact. The crate's own `MP3LAME_SYS_OVERRIDE_HOST` escape hatch is aimed at Android/Apple, not MSVC.

**Consequence: with `--features mp3`, Windows builds must run on a Windows runner.** No cargo-xwin, no `cross`.

---

## 3. Cross-building in GitHub Actions with a C library in the loop

### 3.1 Short answer: use native runners. You already do, and you should keep doing it.

The three popular cross-compilation shortcuts all break on mint's dependency graph:

| Tool | Verdict for mint |
|---|---|
| **`cargo-zigbuild` 0.23.0** / **`cross`** | **Blocked by pkg-config.** The `pkg-config` crate (0.3.33 in your lock) refuses to probe when host ≠ target, returning `Error::CrossCompilation`: *"pkg-config has not been configured to support cross-compilation."* `libsoxr-sys`'s `build.rs` matches only `Error::Failure` for its friendly message, so this surfaces as a raw panic. Workaround requires `PKG_CONFIG_ALLOW_CROSS=1` **plus** a real target sysroot containing a cross-built soxr — at which point you are maintaining a sysroot per target. |
| **`cargo-xwin` 0.23.0** | Handles the *Rust* side and auto-generates a CMake toolchain file, and needs clang + ninja + `llvm-tools`. But it hits the same pkg-config wall for soxr, and **cannot** build `mp3lame-sys` (§2.3). |
| **Native runners** | **Recommended.** Already implemented in `release.yml`. |

**Vendoring soxr (§1.2) changes this materially.** With `cc`-compiled soxr and no pkg-config, `cargo-zigbuild` becomes viable for the *Linux* targets (glibc version pinning is zig's actual strength: `--target x86_64-unknown-linux-gnu.2.28`), which is the cleanest known answer to the AppImage/glibc problem in §3.4. Windows would still need a native runner if `mp3` is on.

### 3.2 macOS: `.app` / `.dmg` / universal

Your matrix builds `x86_64-apple-darwin` on `macos-15-intel` and `aarch64-apple-darwin` on `macos-latest`. Two things:

- **`macos-15-intel` is the last x86_64 image; GitHub ends x86_64 macOS support in August 2027** (the comment at `release.yml:46` is correct). Plan for `lipo`-based universal binaries produced *on arm64* using `--target x86_64-apple-darwin` cross-compilation — Apple's toolchain handles this natively, and it is the only path that survives 2027.
- For a **universal `.app`** you need both slices *in one job* to run `lipo -create`. Today the two slices are in different jobs and different artifacts. Either add a `lipo` merge job, or cross-compile both slices on the arm64 runner. **With vendored soxr this is easy** (`cc` respects `CARGO_CFG_TARGET_ARCH`); with pkg-config it means two soxr builds and two `PKG_CONFIG_PATH`s in one job.
- Signing must happen **after** `lipo`, and `codesign --deep` is deprecated — sign inner binaries/frameworks first, then the bundle.

### 3.3 Windows: `.msi` / `.exe`

`cargo-dist` MSI generation needs the **WiX v3** toolchain on Windows. `cargo-packager` supports NSIS **and** MSI (WiX). Tauri bundles both natively. Signing needs `signtool` on a Windows runner (or Azure Artifact Signing's GitHub Action, §4.2).

### 3.4 Linux: AppImage / .deb / Flatpak

Sequenced by pain, least to most:

- **`.deb`/`.rpm`** — easy. `cargo-packager`/`cargo-dist` handle it. With static soxr there is no runtime dependency to declare at all.
- **AppImage** — the binding constraint is **glibc**, not soxr. You must build on the **oldest glibc you intend to support**; building on 22.04 to run on 20.04 is reported not to work. `cargo-zigbuild` with a pinned glibc is the cleaner fix.
- **Flatpak** — hardest, and its difficulty is almost entirely **offline builds**: Flathub builds with no network, so every crate must be pre-vendored into a manifest (`flatpak-cargo-generator.py`). A **vendored** soxr helps here too — no `curl` of a Debian tarball mid-build, which your current `release.yml:98` does and which Flathub would reject.

**If you choose Tauri, Linux gets materially worse** — see §5.2.

---

## 4. Signing

### 4.1 macOS

- **Apple Developer Program: $99/year.** Notarization is included at no extra cost. Individual/sole-proprietor enrollment is available.
- **Required chain:** Developer ID Application cert → `codesign --options runtime --timestamp` (hardened runtime is **mandatory** for notarization) → `xcrun notarytool submit --wait` → `xcrun stapler staple`. `altool` is dead; `notarytool` only.
- **Reliability caveat:** notarization is a hard external dependency in your release path. There are reports from **February 2026** of submissions stuck "In Progress" for 16+ hours while the service reported "operational". Do not put `notarytool --wait` on a critical timed path; make releases resumable.
- **Entitlements — this is where GUI stack choice bites.** A pure-Rust GUI (egui/iced/slint) needs essentially nothing beyond hardened runtime. **Webview-based stacks (Tauri/wry) typically need JIT-related entitlements** because WKWebView runs a JavaScript JIT: `com.apple.security.cs.allow-jit`, and often `allow-unsigned-executable-memory` / `disable-library-validation`. Each is an attack-surface concession you would not otherwise make. For a tool whose value is deterministic DSP, this is a real, if modest, argument against a webview.
- File-access: mastering engineers point the app at arbitrary folders. Outside the App Store you are **not sandboxed**, so no `com.apple.security.files.user-selected.read-write` needed — but macOS will still prompt for Desktop/Documents/Downloads via TCC. Budget for a clear first-run explanation, and note `NSDesktopFolderUsageDescription` etc. in `Info.plist`.

### 4.2 Windows

The 2024 rule change is the important one: **as of August 2024 Microsoft no longer distinguishes OV from EV for SmartScreen.** EV's instant-reputation bypass is gone; EV-signed files build reputation exactly like OV. **Paying $500+/yr for EV now buys you nothing for SmartScreen.**

Since June 2023 all code-signing keys must live in FIPS 140-2 Level 2 hardware (HSM/token), which is why the cloud services exist:

| Option | Cost | Catch |
|---|---|---|
| **Azure Trusted Signing** (rebranded **Azure Artifact Signing** in 2026) | **$9.99/mo** (Basic, 5 000 signatures); $99.99/mo Premium | **Individual sign-up is US/Canada only.** Org path historically required **3+ years** of incorporation (Microsoft has been relaxing this). |
| Traditional OV cert + HSM | ~$200-400/yr | Token logistics, manual renewal |
| EV cert | $500+/yr | **No longer worth it** |

**Directly relevant to you:** you are at `Thomas.Mahler@ista.com` — Germany. **The individual Azure path is unavailable.** Realistic routes are (a) sign under the **ista** legal entity via Azure Artifact Signing (easily clears the 3-year bar; needs internal sponsorship of an Azure subscription), or (b) a conventional OV certificate.

**Is unsigned survivable for this audience?** Partly, and asymmetrically:

- **Windows:** an unsigned installer shows *"Windows protected your PC"* with the Run-anyway path hidden behind "More info". Painful but survivable for a CLI aimed at engineers; **not** acceptable for a paid/branded GUI. Reputation builds within days of real installs once signed.
- **macOS:** unsigned/un-notarized is effectively **not survivable** for a GUI. Gatekeeper's right-click-Open bypass has been progressively tightened; on recent macOS users must go through System Settings → Privacy & Security per app. For a `.dmg` you hand to a mastering studio, this reads as broken.

**Bottom line:** $99/yr Apple is non-negotiable and cheap. Windows: ~$120/yr via ista's Azure tenant, or ship the Windows build unsigned initially and accept the SmartScreen friction while the macOS build is signed properly.

---

## 5. Bundle-size budget per stack family

### 5.1 Measured on this machine (macOS arm64, `strip=symbols`, `lto=thin`)

| Artifact | Size | Provenance |
|---|---|---|
| **`mint` CLI, existing release build** | **5.2 MB** | `target/release/mint` in the repo |
| **`eframe` 0.36.1 hello-world** | **8.2 MB** | built in scratch, same profile |
| Implied **mint + egui single binary** | **~11-14 MB** | estimate — shared std/deps mean it is not additive |

### 5.2 Family comparison

| Family | Realistic app bundle | Notes for mint |
|---|---|---|
| **Pure-Rust GPU** (egui/eframe 0.36.1, iced 0.14.0, slint 1.17.1) | **~12-20 MB** | One binary, zero runtime deps, no webview entitlements, trivially the same build pipeline you already have. **Slint's license needs care**: `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0` — a commercial/royalty-free option exists but it is a *decision*, unlike egui/iced (MIT/Apache-2.0). |
| **Webview** (Tauri 2.11.5, 2026-07-01) | **Tauri claims "less than 600 KB" minimal**; realistically **~5-15 MB** *plus* mint's own ~5 MB | Smallest download, but see below. |
| **Bundled runtime** (Electron 34.x / Flutter) | Electron commonly cited at **~85 MB** hello-world, 80-150 MB real apps; Flutter desktop typically ~20-40 MB | **Source-quality caveat:** the Electron/Tauri comparison figures circulating for 2026 come from SEO content farms, not primary benchmarks. Treat the *order of magnitude* (tens vs hundreds of MB) as sound and the precise numbers as unverified. Flutter figures are my estimate and **unverified**. |

### 5.3 Size is the wrong axis here

Every option is 5-100 MB. **A single 96 kHz stereo master is 461 MB, and mint's peak RSS is 485-700 MB.** Nobody in this audience will notice 8 MB vs 85 MB.

The decision axes that actually matter for mint:

1. **Linux distribution risk.** Tauri's `webkit2gtk-4.1` dependency is a documented, ongoing source of pain — AppImages missing `libwebkit2gtkinjectedbundle.so`, no way to force WebKit2GTK-4.1 onto target machines, the 4.1 migration existing *specifically* to enable Flatpak. A pure-Rust GPU stack has none of this: one static binary.
2. **macOS entitlements.** Webview ⇒ JIT entitlements (§4.1). Pure-Rust ⇒ none.
3. **Windows runtime.** Tauri needs WebView2 — preinstalled on Win11, needs the Evergreen bootstrapper on older Win10. One more installer branch.
4. **Toolchain count.** Tauri adds Node/npm to a build that is currently pure Cargo + one C library. That is a second dependency-supply-chain and a second lockfile in a project whose entire ethos is pinned, verified determinism.

**Recommendation: `egui`/`eframe`** (or `iced`) — a single Rust binary, no webview, no Node, no extra entitlements, and it reuses `release.yml` almost verbatim. mint's GUI is a form editor plus a job list plus meter readouts; it does not need a browser engine. If the design demands rich/animated layout, `iced` 0.14 is the more layout-capable pure-Rust option; reach for Tauri only if you want to hire web designers.

---

## 6. Repo shape: workspace with a path dependency — yes

**Recommendation: single repo, Cargo workspace, `gui/` depending on `mint` by path.** Arguments specific to this codebase:

**For the workspace:**

1. **The GUI cannot be a read-only consumer.** It needs at least three changes *inside* mint: `Serialize` on `RawTarget`/`Config` (currently Deserialize-only), **progress callbacks + cancellation** on `pipeline::process` (a blocking call with no hooks today), and probably a partial-result channel. Every one of those is a mint API change. A separate repo turns each into a publish-and-bump cycle for a crate that isn't on crates.io.
2. **CLAUDE.md is the invariant registry, and the GUI becomes a consumer of those invariants.** The "Adding things" checklists are explicit that a new codec must touch `match target.codec` in `pipeline.rs`, `describe()` *and* `estimated_output_bytes()`, and that their exhaustiveness is *"the only reason a forgotten arm is a compile error rather than a wrongly-encoded file."* A GUI adds a **fourth** exhaustive match (codec picker, per-codec fields). In one workspace, forgetting it is a **compile error**, which is exactly the enforcement philosophy already in place. Across repos it becomes a stale GUI that silently omits a format.
3. **One CI matrix.** The libsoxr install dance (§1) is per-OS and fiddly. Solving it once in `ci.yml` for both crates beats duplicating the vcpkg/`soxr.pc` workaround into a second repo.
4. **`--dry-run` is a GUI feature already.** `describe()` + `estimated_output_bytes()` are precisely what a config editor's live preview pane wants. Same repo keeps preview and engine from drifting — CLAUDE.md already warns those "are separate code paths and drift silently."
5. Low mechanical cost: `Cargo.toml` has no explicit `[lib]`/`[[bin]]` sections, so adding `[workspace] members = ["mint", "gui"]` is clean.

**Real costs, and how to contain them:**

- **Feature unification is a licensing hazard.** If `gui/Cargo.toml` declares `mint = { path = "../mint", features = ["mp3"] }`, then workspace-wide builds pull LGPL-3.0 LAME into everything, quietly undoing the deliberate opt-in at `Cargo.toml:38`. **Mitigation:** re-export `mp3` as a GUI feature that is **off by default**, and add a CI job asserting the default GUI build links no LAME. Edition 2024 implies resolver 3, so per-target features are not unified the way resolver 1 did — but `--workspace` builds still activate the union.
- **`cargo test` gets slower**, and mint's suite is already heavy (DSP harnesses, `#[ignore]`d probes). Use `-p mint` in the DSP loop; let CI run the workspace.
- **Release tagging.** `release.yml:66` greps `^version` from the *root* `Cargo.toml`. A workspace move **breaks that guard silently** if the root becomes a virtual manifest — it would read no version, or the wrong one. This check must be updated in the same commit as the workspace conversion.

**When a separate repo would win:** only if the GUI were closed-source or commercially licensed while mint stays Apache-2.0. Given the LGPL analysis in §2, a *proprietary* GUI statically linking LGPL soxr is a materially harder legal position than an Apache-2.0 one. If ista ever wants a commercial GUI, revisit **§1 option "rubato"** first — dropping the LGPL dependency entirely is worth more than any repo layout.

---

## Prioritized actions

| # | Action | Why |
|---|---|---|
| **1** | Add LGPL-2.1 text + soxr copyright notice (and LAME's LGPL-3.0 when `mp3` is on) to the release archives — `release.yml:149,157` | Every published release is currently non-compliant. Cheap, immediate. |
| **2** | Decide the soxr strategy. If vendoring: fork LiveKit `soxr-sys` 0.1.3 **with `WITH_CR64=1`, `WITH_HI_PREC_CLOCK=1`, `cr64.c` added** | Adopting it unmodified silently downgrades `vhq` to a single-precision engine (§1.2) |
| **3** | Add a test pinning the selected soxr engine per quality tier (`soxr_engine()` is a one-line FFI call) | Would have caught #2, and catches the cross-platform divergence in §1.4 |
| **4** | Decide whether `vhq` must be bit-reproducible across platforms; if yes, vendoring is mandatory | Today macOS=`cr64`, Debian=`cr64s`. Contradicts `README.md:571`. |
| **5** | Get the Apple Developer Program ($99/yr) started now | Enrollment + first notarization has latency; nothing else unblocks a macOS GUI |
| **6** | Resolve Windows signing via ista's Azure tenant | Individual Azure path is closed to Germany (§4.2) |
| **7** | Pick `egui`/`iced` over Tauri unless web-design skills dominate | Avoids webkit2gtk Linux breakage, JIT entitlements, and a Node toolchain |
| **8** | Convert to a workspace — **and fix the version guard at `release.yml:66` in the same commit** | §6 |
| **9** | Keep `mp3` **off** in shipped GUI builds until requested | Removes LGPL-3.0 from the bundle entirely (§2.2) |
| **10** | Before any GUI work: add progress/cancellation to `pipeline::process` | A blocking call with no hooks cannot back a responsive UI on 461 MB files |

---

## Sources

- [libsoxr-sys — crates.io](https://crates.io/crates/libsoxr-sys) · [build.rs](https://github.com/lrbalt/libsoxr-sys/blob/main/build.rs) · [libsoxr-rs](https://github.com/lrbalt/libsoxr-rs) (last commits 2023-04-12)
- [soxr crate (haileys/soxr-rs)](https://github.com/haileys/soxr-rs) — depends on `libsoxr-sys`
- [soxr-sys 0.1.3 build.rs (LiveKit)](https://docs.rs/crate/soxr-sys/latest/source/build.rs) · [livekit/rust-sdks](https://github.com/livekit/rust-sdks)
- [Homebrew libsoxr formula](https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/lib/libsoxr.rb) (LGPL-2.1-or-later) · Debian `libsoxr0_0.1.3-4_amd64.deb`
- [rubato — crates.io](https://crates.io/crates/rubato) (5.0.0, 2026-08-10, MIT/Apache-2.0)
- [mp3lame-sys](https://github.com/DoumanAsh/mp3lame-sys) (0.1.11, LGPL-3.0) · [pkg-config cross-compilation guard](https://docs.rs/pkg-config)
- [Fraunhofer: mp3 software, patents and licenses](https://www.audioblog.iis.fraunhofer.com/mp3-software-patents-licenses) · [Billboard: Fraunhofer terminates MP3 license](https://www.billboard.com/pro/mp3-license-terminated-fraunhofer-iis/)
- [Notarizing macOS software before distribution — Apple](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution) · [Apple Developer Program enrollment](https://developer.apple.com/help/account/membership/program-enrollment/) · [macOS notarization guide](https://www.frr.dev/posts/macos-notarization-guide-linter/)
- [Code signing options for Windows app developers — Microsoft Learn](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options) · [Azure Artifact Signing quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart) · [Trusted Signing for individual developers](https://techcommunity.microsoft.com/blog/microsoft-security-blog/trusted-signing-is-now-open-for-individual-developers-to-sign-up-in-public-previ/4273554) · [Code signing on Windows with Azure Trusted Signing — Melatonin](https://melatonin.dev/blog/code-signing-on-windows-with-azure-trusted-signing/)
- [Tauri v2 — Start](https://v2.tauri.app/start/) ("less than 600KB") · [Tauri Linux bundling discussion #10026](https://github.com/tauri-apps/tauri/discussions/10026) · [AppImage missing libwebkit2gtkinjectedbundle.so #12463](https://github.com/tauri-apps/tauri/issues/12463) · [Packaging a Tauri v2 app for Flatpak](https://vincent.jousse.org/blog/en/packaging-tauri-v2-flatpak-snapcraft-elm/)
- [cargo-xwin](https://github.com/rust-cross/cargo-xwin) · [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) · [cargo-dist MSI docs](https://github.com/axodotdev/cargo-dist/blob/main/book/src/installers/msi.md)
- [GitHub Actions macOS 13 runner closing down](https://github.blog/changelog/2025-09-19-github-actions-macos-13-runner-image-is-closing-down/) · [macos-15-intel image announcement #13045](https://github.com/actions/runner-images/issues/13045) (x86_64 ends Aug 2027)
- Electron/Tauri size comparisons ([pkgpulse](https://www.pkgpulse.com/guides/electron-vs-tauri-2026), [rustify](https://rustify.rs/articles/rust-tauri-vs-electron-2026)) — **secondary/SEO sources, orders of magnitude only**

---

# Part VI — Judge panels

Three independent panels, each given the complete dossier — both ground-truth audits, the UX analysis and all six candidate evaluations — and each explicitly tasked with attacking the claims in them. Two ranked egui first; the product-design panel ranked it fourth. Part I §3 adjudicates that disagreement rather than averaging it away.

## Release engineering

*Lens:* Release engineering — repeatable build, sign, notarize and ship to three OSes from CI by a small team, with a system C library (libsoxr) already in the dependency graph. Ranked on shipping risk above all else; "works on my Mac" scores zero.

**Ranking:** egui / eframe (pure Rust, single binary) › Slint (pure Rust, single binary) › Iced (pure Rust, single binary) › Foreign-UI + mint-as-sidecar (Electron/TS recommended member) › Tauri 2.x (system webview) › Dioxus 0.7 (wry/tao system webview)

**Winner:** egui / eframe (pure Rust, single binary) · **Runner-up:** Slint (pure Rust, single binary)

### Reasoning

The dominant term under this lens is how many new, unpinnable failure surfaces the GUI adds to a pipeline that is already fragile because `libsoxr-sys` 0.1.4 probes exclusively through pkg-config with no fallback — I read `release.yml` and it survives that only by building soxr from Debian's orig tarball with `-DBUILD_SHARED_LIBS=OFF` and hand-forging a `soxr.pc` on Windows. egui adds literally nothing to that: it is `cargo build`, one binary, no second toolchain, no second lockfile, no runtime engine on the user's machine, and both `glow` and `wgpu` can be compiled in so a bad driver or an RDP session degrades instead of black-screening. Slint is the same shape with better multi-year stability (17 minors, no 2.0, a documented `compat-1-2` mechanism) and native accessibility, but it puts a non-OSI-or-GPLv3 legal decision in the critical path of a first release, and legal review is schedule risk I cannot compress. Iced is structurally identical and has the best automatic software-render fallback (tiny-skia in default features), but zero patch releases in eight months and a flagship app (Halloy) running a pinned fork means an upstream bug can block my release with no patch stream — for a two-person team, "become a GUI toolkit maintainer" is a worse tail risk than egui's five breaking minors. The sidecar family is genuinely attractive because the GUI toolchain never sees a C library and electron-builder is the most battle-tested signing/notarization tool in the desktop world with a fully pinned Chromium on Linux, but it doubles the CI graph with an artifact handoff, adds Node plus six Electron majors a year, and walks straight into the embedded-binary notarization trap (Apple rejects any unsigned Mach-O inside a hardened bundle — tauri#11992 is the canonical instance). Tauri's Rust-native build is a real asset and its bundler signs and notarizes first-class, but I have been burned by exactly this: a Linux `.deb` with a hard runtime dependency on `libwebkit2gtk-4.1-0`, or a 70 MB AppImage that still needs `WEBKIT_DISABLE_DMABUF_RENDERER=1` injected by a launcher wrapper, whose failure mode is a blank window on a user's NVIDIA box and a perfect render on mine — I confirmed that guidance is still live in Tauri's own docs in August 2026. Dioxus is Tauri's Linux exposure with none of the compensation: no signing or notarization support in `dx bundle` (the docs point you at Tauri's), an open unresolved bug where the Linux `.deb` ships with no CSS and no icons, and a three-person vendor planning a platform-API overhaul in 0.8 — that is last, and not close. Two findings outrank the stack choice entirely: the Linux release binaries are built on ubuntu-24.04/glibc 2.39 and therefore already do not run on Debian 12, RHEL 9 or Ubuntu 22.04, and every published archive statically links LGPL-2.1 soxr while shipping only the Apache-2.0 `LICENSE` — fix both before adding a GUI to the pipeline.

### Claims this panel judged inflated, wrong, or constraint-blind

- WRONG, and it appears in three reports (egui, Iced, Slint) as a scored packaging penalty: 'cargo-packager does not sign or notarize.' I checked the config API directly. `cargo_packager::config::MacOsConfig` has `signing_identity`, `entitlements`, `provider_short_name` ('Provider short name for notarization'), and `notarization_credentials` backed by a `MacOsNotarizationCredentials` enum; `WindowsConfig` has `certificate_thumbprint`, `digest_algorithm`, `timestamp_url`, `tsp` and `sign_command`. cargo-packager descends from tauri-bundler and inherited the signing path. This error costs all three pure-Rust stacks a point they should have, and hands Tauri a differentiator ('signing is first-class, unlike the alternatives') that it does not actually have.
- OVERSTATED ACROSS THE WHOLE DOSSIER: 'libsoxr is already solved / release.yml de-risks packaging.' It is solved for the CLI tarball only, and only inside release.yml's environment. I ran `otool -L target/release/mint` on this machine: it links `/opt/homebrew/opt/libsoxr/lib/libsoxr.0.dylib` absolutely, and Homebrew ships no `libsoxr.a` — only dylibs. So the static link exists nowhere except in a CI job that exports `PKG_CONFIG_PATH` at a locally-built prefix. Any bundler run on a dev box, or any new GUI packaging job that forgets to replicate that step, silently produces a dynamically-linked app that dies at launch on a machine without Homebrew. That is the textbook 'works on my Mac' failure and the dossier treats the whole area as closed.
- NOBODY FLAGGED THE GLIBC FLOOR, and it is a live defect today. release.yml builds Linux on `ubuntu-latest` and `ubuntu-24.04-arm` — glibc 2.39. Those binaries will not start on Debian 12 (2.36), RHEL/Rocky 9 (2.34), or Ubuntu 22.04 (2.35). The packaging audit mentions glibc only inside the AppImage subsection while simultaneously asserting for .deb/.rpm that 'with static soxr there is no runtime dependency to declare at all' — glibc *is* the runtime dependency, and it is the one that actually fragments. Static soxr bought portability from the package manager, not from the libc.
- Tauri report, 'the existing release.yml carries over verbatim because tauri build just shells out to cargo build; PKG_CONFIG_PATH is inherited.' The pkg-config half is true (the soxr step writes it to GITHUB_ENV). The framing is not: the Linux legs additionally need `libwebkit2gtk-4.1-dev` at build time, and the shipped `.deb` acquires a hard runtime dependency on `libwebkit2gtk-4.1-0`. The entire portability benefit that static soxr purchased is handed back on Linux by the GUI layer. The report concedes this in its size paragraph but the summary and integration sections read as if packaging is unchanged.
- Slint report, 'GPLv3 permanently forecloses the Mac App Store' presented as a headline cost. True in isolation, non-additive here: mint already cannot ship to the Mac App Store because it statically links LGPL-2.1 libsoxr, and LGPL §6's relinking obligation is the classic App Store conflict — the packaging audit says so itself. Slint's real license cost is distro packaging (Debian/Fedora/nixpkgs cannot take the royalty-free variant) plus a corporate legal review in the release critical path. Note also that GPL-3.0 is a free, immediately available option and Apache-2.0 is one-way compatible with it, so the 'must pay or must decide' framing is softer than presented.
- egui report, 'adopting egui means every dev box and both CI workflows follow egui's toolchain schedule.' The MSRV fact checks out — I verified eframe 0.36.1 requires rustc 1.95 (0.35.0 was 1.92) and this machine is on 1.92.0. The CI half is wrong: both `ci.yml` and `release.yml` use `dtolnay/rust-toolchain@stable` and there is no `rust-toolchain.toml` anywhere in the repo, so CI already tracks latest stable and needs zero change. This is a `rustup update`, not a pipeline constraint.
- Dioxus report treats the `otool -L` finding (dx auto-bundling intercepts `-framework`, soxr emits `-lsoxr`, so the dylib won't be relocated) as an open packaging problem. The observation is the single most valuable one in the dossier, but the conclusion is inverted: in CI soxr is linked *statically*, so there is no dylib to relocate and `install_name_tool` is never needed. The real lesson is the one the report doesn't draw — that the dev-box and CI builds have different linkage, so any bundling verified locally proves nothing about the artifact.
- Sidecar report's killerArgumentFor: 'the libsoxr constraint costs this family precisely one cp into extraResources.' Its own packaging section is more honest than its summary. Notarization requires hardened runtime and rejects any unsigned Mach-O inside the bundle, so the embedded `mint` must be codesigned with the same Team ID *before* the outer bundle is sealed, in an afterSign hook, and re-signing order matters. tauri#11992 exists precisely because people get this wrong. It is a day of CI work and a recurring failure mode, not a `cp`.
- Two universal-binary gaps that every report glosses. release.yml builds `x86_64-apple-darwin` on `macos-15-intel` and `aarch64-apple-darwin` on `macos-latest` as *separate jobs on separate runners*, with no `lipo` merge step — so a universal `.app` (needed by Electron's universal target, and wanted by anyone shipping one DMG) has no producer today, for any stack. And there is no `aarch64-pc-windows-msvc` leg at all; only the sidecar report noticed. Windows-on-ARM users get x64 under emulation for a CPU-bound DSP core, or nothing.
- Universal, guaranteed break that only the packaging audit caught and no candidate report repeated: `release.yml` gates on `grep -m1 '^version' Cargo.toml`. Every stack here requires converting the repo to a Cargo workspace, and a virtual root manifest has no `version` key — the guard then reads empty and the tag/version check silently stops guarding. This must land in the same commit as the workspace conversion, whichever GUI wins.
- Understated compliance defect, stack-independent: the Package steps copy only `README.md` and `LICENSE` (Apache-2.0) into every archive, while the binary statically links LGPL-2.1 libsoxr. LGPL §6 wants the license text, the copyright notice and a relinking means. Compounding it, `Cargo.toml` has no `license` field at all and `LICENSE` still carries the literal `Copyright [yyyy] [name of copyright owner]` placeholder — I checked both. A signed, notarized single-file GUI bundle makes this harder to discharge, not easier.
- One thing worth crediting rather than deflating: release.yml does *not* pass `--features mp3`, so the LGPL-3.0 LAME exposure the packaging audit worries about is currently zero. Keep it that way for GUI builds — enabling it adds an autotools/cc C build to five legs and, per the audit's own finding on `mp3lame-sys`'s host-`cfg` build script, cannot be cross-compiled to MSVC from a Unix host at all.

### Dealbreakers under this lens

- Dioxus 0.7 — DISQUALIFIED. `dx bundle` has no signing or notarization support (its docs explicitly refer you to Tauri's), so you own the entire codesign/notarytool/stapler and signtool pipeline with no framework help, while simultaneously carrying Tauri's full webkit2gtk Linux exposure. On top of that, an open unresolved bundler bug ships the Linux `.deb` with no CSS and no icons — i.e. the artifact differs from `dx serve` in a way that only surfaces after release. Governance (3-person vendor, no 1.0, 0.8 scoped as a platform-API overhaul plus a tao→winit swap) means this gets worse before better.
- Tauri 2.x — DISQUALIFIED on Linux, not on tooling. Verified still-live in August 2026: WebKitGTK's DMABUF renderer fails on NVIDIA proprietary drivers and several Wayland compositors and the failure mode is a blank window, with the ecosystem fix being an injected `WEBKIT_DISABLE_DMABUF_RENDERER=1`. You cannot pin the engine (2.36 on 22.04 through 2.52.x on 26.04), the `.deb` hard-depends on `libwebkit2gtk-4.1-0`, and the AppImage that bundles it costs 70+ MB and still needs the env-var wrapper. A defect class I cannot reproduce, test, or regression-pin inside my own repo is the definition of unshippable under this lens. Everything else about Tauri (Rust-native build, first-class notarization) is genuinely good.
- Foreign-UI sidecar / Electron — no dealbreaker, but two structural costs that keep it below the pure-Rust options: a two-stage CI pipeline with a cross-job artifact handoff (the classic source of 'we shipped a stale engine'), and Electron's 8-week major cadence with only three supported lines, which a two-person team will fall behind on and end up shipping unsupported Chromium. The embedded-binary notarization coupling is a known trap but is solvable once and stays solved. Its compensating strength is real and underrated: Chromium is pinned, so it is the only stack here whose Linux rendering I can actually reproduce.
- Iced — no hard dealbreaker; the disqualifying *risk* is release availability. Zero patch releases in the eight months since 0.14.0, 15 months between minors, and the flagship desktop app pins a fork at `0.15.0-dev`. If a release-blocking bug lands, the remediation path is to fork and maintain iced, which a small team cannot absorb. Its automatic wgpu→tiny-skia software fallback is the best driver-robustness story of any candidate and I nearly ranked it second for that alone.
- Slint — no build or CI dealbreaker; it is a legal gate in the critical path. Verified license string is `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0`, with no permissive OSI option, against a repo whose `Cargo.toml` has no `license` field and whose `LICENSE` still says `Copyright [yyyy] [name of copyright owner]`. GPLv3 relicenses the shipped bundle; royalty-free is free and permits closed desktop distribution but blocks Debian/Fedora/nixpkgs. Decide it in writing before the first `cargo add slint`, and split the workspace so the CLI never depends on Slint. Secondary: choosing Skia for text quality pulls skia-bindings and a Visual Studio 2022 requirement onto the Windows leg — stay on the default femtovg and that cost disappears.
- egui / eframe — no dealbreaker under this lens. Pure cargo, one self-contained binary, no second toolchain, no second lockfile, no runtime engine on the user's machine, MIT/Apache-2.0, and cargo-packager covers .app/.dmg/MSI/NSIS/AppImage/.deb including macOS notarization credentials and Windows Authenticode. Residual costs are real but bounded and one-time: add winit's Linux dev headers (`libxkbcommon-dev`, `libwayland-dev`, X11) to the Linux legs, compile both `glow` and `wgpu` so a broken driver or an RDP session falls back instead of black-screening, and budget half a day per egui minor for the migration treadmill.
- APPLIES TO ALL SIX, fix before choosing: the Linux glibc floor (2.39 from ubuntu-24.04 runners; use zig or an older container to reach 2.28 if you want Debian 12/RHEL 9), the missing macOS `lipo` merge job for a universal bundle, the missing `aarch64-pc-windows-msvc` leg, the `grep '^version' Cargo.toml` release guard that a workspace conversion silently defeats, and the LGPL-2.1 notice/relinking obligation absent from every archive shipped so far.

---

## Product design for professional audio software

*Lens:* Product design for professional audio software — can this stack produce something a mastering engineer respects, weighted on typography, layout control, live-updating dense information, and future waveform/meter views, with the cost-to-elegance honestly priced.

**Ranking:** Tauri 2.x (Rust backend + system webview, mint linked in-process) › Foreign-UI sidecar — specifically the Electron 43 + TypeScript/React member (NOT the family average) › Slint 1.17.1 › egui / eframe 0.36.1 › Iced 0.14.0 › Dioxus 0.7 (dioxus-desktop / wry)

**Winner:** Tauri 2.x (Rust backend + system webview, mint linked in-process) · **Runner-up:** Foreign-UI sidecar — Electron 43 + TypeScript/React member

### Reasoning

Every screen in the UX proposal is a numeric telemetry surface — LUFS, dBTP, GR, byte counts, provenance badges, a 160-cell plan matrix — and that is a typography problem before it is a widget problem, which is where the native-Rust set has a hard, unfixable ceiling: none of egui, iced or Slint exposes OpenType feature control, so you cannot turn on tabular figures, you get no baseline alignment, no subgrid, no optical decimal alignment, and no `text-wrap: balance`; you must pick a font whose default figures happen to be tabular and live with it. CSS gives all of that in one line, which is why Tauri and Electron are the only two stacks where the ceiling is set by the designer rather than by the toolkit. Tauri wins over Electron because mint links in-process, so the IA doc's §5 freebie — a 1:512 min/max envelope computed inside the existing decode loop for 0.4% of the buffer — reaches an `ArrayBuffer` with no second decoder and no new CLI surface, whereas the sidecar has literally no peak-data path today and would need a duplicate of symphonia's AIFF frame-count quirk in Node; Tauri also ships a bundler that signs and notarizes, and 12 MB reads as a tool while 120 MB reads as bloat to this audience. I am giving Electron the runner-up over every native option, and I want to be explicit that Electron has one advantage Tauri never concedes: a single pinned Chromium means your design is pixel-identical everywhere, while Tauri means tuning in DevTools and then discovering WKWebView smooths text differently and WebKitGTK 2.36 is four CSS years behind. Among the native-Rust three, Slint is the only one whose *aesthetic* primitives are in the right class — a retained scene graph with declarative states, transitions, easings, conic gradients, inner and drop shadows, Skia text, and a real native macOS menu bar via muda — and LibrePCB's Qt-to-Slint migration is the one credible dense-professional proof point on offer; the cost is that you build the data grid, the float spin box, the modal system and (see below) any editable syntax-highlighted text from scratch. egui ranks above iced despite looking worse by default purely because it *passes the checklist*: `egui_table` gives real virtualized custom cells, `TextEdit::layouter` + `LayoutJob` gives per-range colour, background and underline strokes so the TOML editor with error squiggles is genuinely buildable, and `Painter` + a wgpu `PaintCallback` is the strongest waveform/spectrogram story of any candidate. Iced lands fifth because its two weakest widget families are exactly the two the primary screens are made of — there is no numeric input at all, and `table` is an eager three-pass layout helper with no sorting, selection or virtualization, so the Plan matrix and the Report table are both hand-rolled before design begins. Dioxus is last under this lens and it is not close: it takes on every webview cost (three engines, WebKitGTK fragility, an open bundler bug where the Linux .deb ships with no CSS and no icons) while forfeiting the only reason to accept those costs, because `dioxus-primitives` is version 0.0.0 on crates.io and none of CodeMirror, TanStack Table, Radix or wavesurfer.js comes along for the ride in RSX.

### Claims this panel judged inflated, wrong, or constraint-blind

- egui report: 'Theming pass to get out of default-gray: 3-5 days.' Off by roughly an order of magnitude for anything the brief would call elegant. Three to five days buys you a nicer grey. Reaching 'designed' means writing custom widgets — a numeric field with unit suffix and drag behaviour, a combo box, table headers, provenance badges — because there is no styling cascade (egui#3284, 'CSS-like styling', still open), no baseline alignment, and one global Style blob you push and pop. The same report contradicts itself two sentences later: 'past themed egui you are hand-painting with Painter in Rust.' Budget 3-6 weeks of a designer-developer, with no hot reload, and accept a ceiling below the web stacks.
- egui report treats the TOML editor as a likely dealbreaker ('if you ever want an embedded TOML source editor with syntax highlighting, it becomes one') and never mentions `TextEdit::layouter`. That hook returns a `LayoutJob` whose per-section `TextFormat` carries colour, background and an underline `Stroke` — egui ships its own `code_editor` demo built on exactly this, and the documented caveat is only 'cache the highlighter, the layouter runs every frame.' For a 92-line mint.toml this is days, not a blocker. Correction in egui's favour; it is also why egui outranks iced here.
- egui report: Rerun cited as the elegance existence proof, then scored uiCeiling 3. The footnote matters more than the citation — Rerun funds egui, employs several of its top contributors, and can land upstream commits to unblock its own design. It is proof of a ceiling reachable with a paid design team and commit access, not a template a small team inherits. And Rerun still looks like a bespoke instrument, not like a Mac app.
- Iced report scores uiCeiling 3 on the strength of Halloy and COSMIC. Neither runs released iced: Halloy 2026.8 pins `[patch.crates-io]` to its own fork (squidowl/iced @ 8d458d2, 0.15.0-dev) and COSMIC uses libcosmic, also a fork. The one flagship on plain 0.14.0 is Sniffnet, which is competent, not beautiful. Meanwhile the report's own finding — `table` builds all rows eagerly with no sorting, selection, resize, sticky headers or virtualization — is a direct failure of mandatory checklist items 1 and 2, i.e. the Plan matrix and Report view, which the IA doc names as the primary object on screen. That should have pulled the score below 3, not sustained it.
- Iced report omits its single best design asset. iced 0.14's `TextEditor` has built-in `.highlight()` backed by syntect via `iced_highlighter` (0.14 added more syntaxes and swapped onig for fancy-regex), plus a public `Highlighter` trait you can implement to paint error spans from `toml::de::Error::span()`. There is even a third-party `iced-code-editor` crate with line numbers. That is the best native-Rust answer to the one requirement the IA doc calls 'most likely to eliminate a stack,' and the report never mentions it. Correction in iced's favour — it does not rescue the tables.
- Slint report scores uiCeiling 4 and never addresses the TOML editor at all. Verified: a Slint maintainer's answer to 'how to show text in the TextEdit widget with syntax highlight' is 'I am sorry, that is not supported yet' (slint-ui/slint discussion #3401, pointing at #2723), and the underlying gap — no text document model for undo/redo or rich formatting — is issue #1325, open since June 2022. `StyledText` (1.16) is read-only markdown display, not editing. So the config drawer is a from-scratch text-editing engine, the most expensive widget there is. Stacked on the missing decimal SpinBox, the text-only `StandardTableView` cells and the absent modal dialogs, that is four checklist items you build before you design. Survivable by redesigning the drawer as a read-only highlighted view plus 'open in $EDITOR' and a file watch — which is arguably the better product anyway — but the report should have priced it, not skipped it.
- Slint report leans on WesAudio and Viiri Audio as pro-audio precedent. Those are plugin UIs: small, fully custom-drawn, no data grid, no text editing, no file browsing, no dense numeric tables. They demonstrate the easy case for a scene-graph toolkit. LibrePCB 2.0/2.1 is the real proof point for a dense professional desktop tool and the report buries it.
- Dioxus report scores uiCeiling 5. No — that score belongs to a stack where the ceiling is set by the designer. Dioxus gives you the typography and layout half (CSS/Tailwind, genuinely excellent) and withholds the components half: `dioxus-primitives` is version 0.0.0 on crates.io, a 12-line placeholder from April 2025 with ~1.5k downloads, so the real components are a pinned git rev. The report itself concedes you hand-roll number inputs, selects, comboboxes and validation display, and that a spectrogram is 'the one place you'll be writing JavaScript in a Rust-only project.' TanStack Table's value is its framework adapter; CodeMirror, Radix and wavesurfer.js are all React/DOM integrations you would re-bridge by hand. Call it 3.
- Tauri report: 'HiDPI: genuinely excellent on all three... one of the clearest advantages over pixel-scaling native toolkits.' True and incomplete. The flipside it never states is that three engines means three text rasterizers and three CSS feature sets — WKWebView's font smoothing, Chromium's, and WebKitGTK 2.36-to-2.52 — so a design tuned in DevTools is not pixel-identical anywhere else. Under an elegance lens Electron's single pinned Chromium is a real advantage over Tauri, and neither report says so.
- Tauri report buries its most design-relevant Linux finding as a caveat. WebKitGTK masks `WEBGL_debug_renderer_info`, so a WebGL2 spectrogram can succeed while silently backed by a software rasterizer with no way to feature-detect it. That is a 5 fps failure on a tester's machine that reports nothing wrong. Promote it from a caveat to a design constraint: Canvas2D is the floor, WebGL is a Windows/macOS-only enhancement.
- Sidecar report scores the whole family uiCeiling 5. That score is carried entirely by Electron. Flutter draws every glyph itself so text never matches the OS, has no CodeMirror equivalent, no wavesurfer/peaks.js equivalent, and documented NVDA/UI-Automation semantics failures; Avalonia is the literal 'settings dialog from 2009' risk this brief asks about — a competent WPF-flavoured app whose route to distinctive runs through paid control suites (Actipro, Syncfusion), with essentially no audio-visualisation ecosystem and no Figma handoff. Averaging those into a 5 hides the fact that two of three family members are disqualified under this lens.
- Sidecar report misses a design-blocking data gap: mint's `--json` carries no frames, no channel count and no envelope, so there is no waveform path at all. A peaks view in the sidecar shape means either a second audio decoder in Node/Dart/C# — which would have to re-derive symphonia's AIFF SSND overcount correction and the 0xFFFFFFFF streamed-WAV cap that `io_read::probe_source` already encodes — or a new mint subcommand. In-process stacks get the IA doc's §5 envelope for ~0.4% of the buffer inside the decode loop they already run.
- Levelling correction that applies to all six and that three reports quietly claim credit for: the provenance inspector — the IA doc's single highest-value screen — is impossible in every stack today. `overlay()`, `builtin_fallback()`, `preset()`, `validate()`, `check_collisions()` and every `*_tag` helper except `format_tag` are private, and `resolve_target` discards which layer supplied each value. `resolve_target_explained` is required regardless of toolkit. The egui, Slint and Dioxus 'we call resolve_target directly so validation is free' argument is true for *error strings* and false for *provenance*, which is the screen a designer would actually be hired to make.
- Levelling correction #2: 'live re-render at keystroke rate' is not a differentiator and should not be scored as one. `resolve_target` is microseconds; a React form re-render is sub-millisecond; a Tauri IPC hop over 22 scalars is well inside a frame. The only shape that is meaningfully slower is the sidecar's measured 15-20 ms process spawn, and a 150 ms debounce makes that invisible. Any report treating a 60 Hz resolve loop as an elegance advantage is arguing a point the user cannot perceive.

### Dealbreakers under this lens

- Dioxus 0.7 — DISQUALIFIED. The only justification for accepting a webview is the component and tooling ecosystem, and RSX does not give you it: `dioxus-primitives` is 0.0.0 on crates.io, so CodeMirror, TanStack Table/Virtual, Radix and wavesurfer.js are all hand-written bridges, while you still pay three engines, the WebKitGTK blank-window class, and an open, reproduced bundler bug (discussion #4356) where the shipped Linux .deb has no CSS and no icons. You would ship the least elegant webview app possible.
- Iced 0.14 — DISQUALIFIED as specified. There is no numeric input widget for the six decimal fields, and `table` is an eager three-pass layout helper with no sorting, selection, resizing, sticky headers or virtualization. The Plan matrix and the Report view — the two things the IA doc says are the product — must both be hand-built from `scrollable` + `lazy` before any design work starts, and 15 months between minors with zero patch releases means a blocking widget bug puts you on a fork. Its genuine strengths (canvas geometry caching, the `shader` widget, iced_audio 0.15) serve the features that do not exist yet.
- egui 0.36 — not disqualified, ceiling-capped. It passes all twelve mandatory items, but no OpenType feature access means no `tnum` toggle (tabular figures come only from font choice), there is no baseline alignment and no styling cascade, and the macOS menu bar is drawn inside the window with no NSMenu — an immediate 'not a real app' tell for a Mac-first audience. Correctly chosen only if the team accepts an instrument aesthetic rather than an elegant one, and budgets 3-6 weeks, not 3-5 days, to escape default grey with no hot reload and no possibility of a designer contributing.
- Slint 1.17.1 — not disqualified, but one mandatory item is a from-scratch text engine. Editable syntax highlighting is maintainer-confirmed unsupported (discussion #3401) with no text document model since issue #1325 opened in June 2022, so the 'non-negotiable' config drawer must be redesigned as a read-only highlighted view plus external editor and file watch. Add the missing decimal SpinBox, text-only StandardTableView cells and no modal dialogs, and you build four widgets before you design one screen. The non-OSI licence (GPL-3.0-only OR two LicenseRef terms) is a product-strategy asterisk against an Apache-2.0 core, not a design one, but it never goes away.
- Tauri 2.x — one conditional dealbreaker: Linux. WebKitGTK is unpinnable across 2.36 to 2.52.3, its signature failure is a blank window rather than a degraded one, and Tauri's own current docs answer it with a four-step env-var ladder ending in `WEBKIT_DISABLE_DMABUF_RENDERER=1`. WebGL is additionally undetectably software-rasterized there. Acceptable if Linux is tier-2 and Canvas2D is the visualization floor; disqualifying if Linux is a first-class target.
- Foreign-UI sidecar — the family is only viable through its Electron member. Flutter is disqualified (canvas-drawn text that never matches the OS, no code-editor or waveform ecosystem, documented NVDA/UIA semantics failures) and Avalonia is disqualified (no route to a distinctive look without paid control suites, no audio-visualisation ecosystem, no designer handoff). Even the Electron member carries two design costs the report understates: a 100-150 MB download that reads as bloat to an audience used to 20 MB plugins, and no waveform data path at all until mint grows one — plus the whole live half of the design is blocked on `--dry-run --json`, NDJSON per-task events, and a schema command that do not exist yet.

---

## Maintainer of mint in 2031: total lifetime cost

*Lens:* Maintainer of mint in 2031: total lifetime cost — toolchain count, whether validation is shared or duplicated, framework churn history, how much the GUI forces the core to change, bus factor, and the cost of abandonment. Accidental complexity is the enemy.

**Ranking:** egui / eframe (pure Rust, in-process) › Slint (Rust + .slint DSL, in-process) › Iced (pure Rust, in-process) › Tauri 2.x (Rust backend + TS/JS webview frontend) › Foreign-UI sidecar (Electron/Flutter/Avalonia driving the mint CLI) › Dioxus 0.7 (RSX + wry webview)

**Winner:** egui / eframe (pure Rust, in-process) · **Runner-up:** Slint (Rust + .slint DSL, in-process)

### Reasoning

CLAUDE.md's central anxiety is that the derived chain already has three parallel representations (pipeline.rs, describe(), estimated_output_bytes()) that drift silently, and the whole codebase is organised so a forgotten arm is a compile error; the only question that matters under my lens is which stack adds a fourth representation and in what language. I verified the decisive fact myself: config.rs:724 shows the public `resolve_target` already calls `validate_fields()`, so every per-target rule — the own-layer format/rate/bwf exception included — is reachable today from outside the crate as a plain function call, and only `check_collisions` (cross-target) is private, a one-line `pub`. egui turns that into the whole validation layer: the form's error text is mint's own anyhow string, recomputed per frame, with no DTO, no wire schema, no TS mirror, no codegen step, and no second lockfile. Its cost is paid in aesthetics and in ~5 mechanical breaking minors a year plus an MSRV treadmill (I accept that: half a day each, bounded, and freezable), whereas Slint's better churn record (three years of 1.x) is bought with a DSL that has exactly one implementation, a non-OSI licence, a build.rs codegen step, and hand-written widgets — including a decimal spin box, because SpinBox is `int`-only and mint has four f64 fields. Iced is egui's equal on language count and validation sharing but its flagship app pins a fork because there has been no patch release in eight months, and "maintain your own fork of a GUI toolkit" is the single worst line item a solo maintainer can sign up for. Tauri and Dioxus both bet a third of the platform matrix on WebKitGTK, a defect class whose current official answer is a four-step env-var ladder and which I can never fix in my own repo; Tauri at least has real governance, Dioxus is a three-person company on a 2023 seed shipping "a substantial overhaul of platform APIs" in the next minor. The foreign sidecar ranks last-but-one despite having the best individual component longevity, because its escape from duplication is to make mint grow and forever version three or four new machine-readable contracts (NDJSON, --dry-run --json, schema --json, config set --json) — that is not avoiding a fourth code path, it is building four of them and calling them an API. One thing every report undersells: the cheapest progress mechanism in the dossier needs no DSP surgery at all, because estimated_output_bytes is byte-exact for WAV and DSF and atomic_write stages to a known sibling path, so stat-ing the staging file yields a true percentage — which removes most of the "the core must change" argument being used to justify the process boundary.

### Claims this panel judged inflated, wrong, or constraint-blind

- Dioxus report frames libsoxr as an unsolved shipping problem, budgeting install_name_tool + Contents/Frameworks + re-sign work, on the basis of `otool -L` against a local Homebrew dev build. This directly contradicts the packaging audit's correction #5: release.yml already static-links soxr 0.1.3 on all five release targets. The dev-machine link is irrelevant to the shipped artifact, and none of that work is on the CI path.
- Both the egui and Iced reports cite 'a libsoxr FFI segfault/abort is uncatchable and takes the whole window down' as a live in-process risk. libsoxr is pure computation with no I/O, every Soxr instance is constructed through one lock-guarded site (ops::resample::create_soxr), and tests/soxr.rs guards the binding under concurrency. This is FUD and it inflates the case for process isolation across three separate reports.
- The egui report's own killerArgumentAgainst — 'an OOM kills the editor and the user's unsaved project' — over-weights lost state. The UX design explicitly assumes mint.toml is co-edited in a text editor and is the source of truth on disk; a crash costs a spinner, not work. And the mitigation is one integer (cap --jobs), which every stack including the sidecar needs anyway, since cli.rs defaults to available_parallelism().
- Sidecar report: '`mint schema --json` (~a day, optional but transformative)'. A schema emitter that stays truthful about the 4-layer merge with its deliberate own-layer exception, the input-dependent delivery_blocker rules, and the ~15 bail! sites is not a day — and it is a permanent additional representation of exactly the invariants CLAUDE.md says already drift across three code paths. The same report's `mint config set --json` proposal quietly turns mint into a config server, which is a large unbudgeted expansion of a contract that can then never be broken.
- Slint report's stability framing ('three years and seventeen minor releases with no 2.0') is undercut by its own risk list: the single gesture this app is judged on — dragging a WAV out of Finder — is reachable only through `unstable-winit-030`, which Slint documents as removable in a minor release; and 1.17 changed float-to-string to use the locale decimal separator, so a de_DE machine renders `-14,0`. A silent behaviour break plus a load-bearing explicitly-unstable feature is churn, whatever the semver number says.
- Iced's crossPlatform score of 3 is generous given its own finding that iced 0.14's docs state FileDropped/FileHovered/FilesHoveredLeft are 'not implemented on Wayland'. Wayland is the default on current Fedora and Ubuntu, and drag-and-drop is the primary input gesture for this app.
- Tauri's crossPlatform 3 should be 2 on its own evidence: a blank-window failure mode, a four-step env-var ladder in current official docs, and an engine spread from WebKitGTK 2.36 to 2.52.3 across supported distros.
- The integration-surface audit calls the fact that ResolvedTarget is fully constructible from outside the crate the 'biggest single opportunity and biggest footgun'. Under this lens it is not an opportunity at all — a hand-built ResolvedTarget bypasses validate_fields and instantly becomes the fourth parallel code path. The GUI should never construct one; it should only ever receive them from resolve_target.
- The UX report lists ResolvedTarget::validate_fields as a validation gap for a live editor. I checked: config.rs:724 shows resolve_target calls validate_fields itself, so the public API already gives full per-target validation. The genuine gap is narrower than stated — only cross-target check_collisions and whole-config validate are unreachable.
- Several reports treat per-job progress as blocked on invasive DSP surgery, and the sidecar report uses that to argue for the process boundary. But its own best finding applies equally in-process: estimated_output_bytes is byte-exact for WAV and DSF and atomic_write stages to a documented `.<name>.<pid>.<n>.mint-tmp` sibling, so a true percentage is a stat() call away with zero changes to ops/.

### Dealbreakers under this lens

- egui / eframe: none disqualifying under this lens. The nearest thing is the in-window menu bar (no NSMenu) — only a dealbreaker if 'must feel like a native Mac app' is ever a hard requirement rather than a preference.
- Slint: the licence is a permanent structural asterisk — GPL-3.0 / non-OSI royalty-free / paid, with the royalty-free option resting on a company promise in a CLA. For this Apache-2.0 repo the free correct answer exists (workspace split, GPLv3 GUI binary, Apache-2.0 core), so it is not disqualifying today; it becomes one the moment a proprietary GUI, a Mac App Store listing, or a Debian/Fedora package is required. The deeper cost is DSL lock-in: five years out, changing the UI requires a compiler with exactly one implementation.
- Iced: zero accessibility, six years open (#552), with the most complete AccessKit PR closed unmerged twelve hours after opening because iced pins a winit fork. Hard disqualifier if any WCAG/EN 301 549/Section 508 requirement ever appears. Independently near-disqualifying for a solo maintainer: zero patch releases in eight months means the flagship app forks and pins, so 'maintain a fork of your GUI toolkit' is the expected steady state.
- Tauri 2.x: two full toolchains and two supply chains in a repo whose entire value proposition is verifiable, reproducible, bit-exact output — plus WebKitGTK, a defect class on one of three platforms that I can never fix, test around, or regression-pin inside my own repository.
- Foreign-UI sidecar: the domain model (23 fields, 11 presets, the 4-layer merge with its own-layer exception, ~15 validation rules, input-dependent delivery blockers) must either be reimplemented in a second language with nothing checking it, or mint must grow and forever version three to four new machine-readable contracts. Both outcomes are the exact failure mode the three exhaustive `match target.codec` sites exist to prevent. Aggravating: I confirmed there is not one log:: call site outside cli.rs and no signal handler anywhere in src/, so the streaming contract the whole design assumes does not exist yet.
- Dioxus 0.7: pre-1.0 with an announced 'substantial overhaul of platform APIs' plus a tao→winit swap in the very next minor, on top of large breaks at 0.4→0.5, 0.5→0.6 and 0.6→0.7; a three-person company on a $520K 2023 seed; an unresolved bundler bug shipping Linux .deb packages with no CSS and no icons across three versions; no signing support; dioxus-primitives published to crates.io as a 0.0.0 placeholder. It carries Tauri's webview risk with none of Tauri's governance, and its one real advantage — rendering Rust types without a TS mirror — is available for free from any pure-Rust stack without a browser engine at all.

---

*End of document.*

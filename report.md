# mint vs. reference-class delivery tools — a measured capability report

*Audit date: 2026-07-27. Subject: `mint` @ `f13d071`, built with default features (libsoxr backend), macOS 15 / M1 Pro (10 cores).*

## 0. Method

Everything below was checked against the implementation, not the README. Specifically: I read every DSP and I/O module; ran the full test suite (107 tests, all pass); ran the project's own measurement harnesses in release mode; wrote a temporary probe that drove `pipeline::process` end-to-end and re-measured the delivered files independently (16× true-peak estimator, an independent FLAC decoder written for this audit); benchmarked CPU and peak RSS; and cross-checked the competitor claims against vendor manuals and standards documents rather than folklore. An 18-agent code audit and a 14-agent vendor-documentation study, each adversarially verified, fed into this; every number stated below was either measured directly or traced to a primary source. The probe file was deleted after the run; the repo is clean.

**Short verdict.** mint's *resampler* is reference-grade (it is libsoxr, driven correctly). Its *loudness engine* is more accurate and more honest than most batch tools, and its meter-verified ceiling loop is something none of the reference tools do. Its *dither* is competitive with published fixed curves and, on one objective, measurably better. But as a *delivery* tool it is not in the same class: format coverage, metadata, CD/DDP, album handling, and scaling are far behind, the streaming presets encode a mistaken model of what platforms want, and there are two defects that silently ship broken or wrong deliverables.

---

## 1. Head-to-head summary

| Area | mint | WaveLab Pro | Audition | Weiss Saracon | Ozone/RX | Verdict |
|---|---|---|---|---|---|---|
| **SRC engine** | libsoxr VHQ (28-bit ≈ 168 dB design rejection), linear phase only | "Resampler", 4 undocumented tiers | one unlabelled quality slider | own linear-phase FIR, DSD | RX: steepness + pre-ringing knobs | **mint ≥ parity**, fewer knobs |
| **SRC controls** | 4 tiers, compile-time backend | 4 tiers | 1 slider + pre/post filter | fixed | continuous filter design | **behind RX** |
| **Loudness metering** | I, LRA, M, S, TP (ebur128) | full R128 + Global Analysis | Match Loudness + Amplitude Statistics | none | RX Loudness Control (discontinued 2022) | **parity** |
| **Loudness accuracy** | measured ±0.03 LU end-to-end | ±0.2 LU class | ±0.1 LU class | — | — | **mint best-in-class** |
| **Album/multi-file loudness** | **none** | yes (Montage) | Match Loudness across files | — | — | **mint far behind** |
| **Limiter** | 1 band, 3 presets, zero latency, meter-verified ceiling | 5 limiters incl. Raiser | Hard Limiter (sample peak only) | DS1-MK3 Type 2 | Maximizer IRC I–IV | **mint behind on flexibility, ahead on verification** |
| **True-peak accuracy** | 4× detector, verified by 4× meter | 4×-class "Detect Intersample Clipping" | true peak only in Match Loudness | 4× | 4× + soft clip | **parity with the mid-field, behind Pro-L 2 (up to 32×)** |
| **Dither modes** | 5, **16-bit only** | 2 plug-ins, 8/16/24-bit, 7 shapers | 4 pdfs × 5 shaping families, adaptive mode | POW-r 1/2/3 | MBIT+ | **mint behind on depth range and control** |
| **Dither quality** | Lipshitz +9.9 dB ATH-weighted; adaptive +4.05 dB NMR | LINpro (MAAT) | documented curves | POW-r | MBIT+ | **parity on fixed curves; adaptive is real** |
| **Formats in** | WAV/AIFF PCM only | ~20 formats | ~20 formats | PCM+DSD | RX: 12+ | **mint far behind** |
| **Formats out** | WAV/BWF, FLAC, MP3 (opt-in) | WAV/RF64/W64/AIFF/FLAC/MP3/AAC/Ogg/Opus/WavPack | most | PCM/DSD | 5 | **mint far behind** |
| **Metadata** | **none in, synthetic bext out** | BWF/iXML/CART/ID3 | BWF/ID3/XMP | — | preserve non-audio data | **mint far behind** |
| **CD/DDP** | none | full DDP 2.0, PQ, ISRC, CD-Text | none | none | none | **behind WaveLab; parity with Audition/RX** |
| **Batch/automation** | native CLI, TOML, JSON report, rayon | GUI batch processor | GUI batch panel | GUI + CLI | GUI only | **mint's clear win** |
| **Long-file scaling** | whole file in RAM, 8.6 GB peak for a 5-min 96k track | streams | streams | streams | streams | **mint far behind** |

---

## 2. Formats and file I/O — the weakest area

### What actually decodes

`Cargo.toml` enables symphonia's `wav`, `aiff`, `pcm` features **without disabling default features**, so FLAC, Ogg/Vorbis, MKV and ADPCM decoders are compiled in. But `cli.rs:372` gates inputs on the extension whitelist `wav | wave | aif | aiff`, and non-matching files are **silently dropped from the batch, not reported as errors**. So `.flac`, `.aifc`, `.caf`, `.w64`, `.rf64` inputs simply vanish. (A further quirk: symphonia 0.5.5's `Probe::format` ignores the `Hint`, so detection is by start-of-stream marker — a FLAC renamed `.wav` actually decodes.)

Supported input sample formats are broad and handled at full precision: PCM 8/16/24/32 int, f32/f64, A-law/µ-law, AIFF/AIFC big-endian and `sowt`, all decoded into a planar `Vec<Vec<f64>>`. f64 input is bit-exact through to f64 output. That part is genuinely good — most tools truncate to f32 at the input boundary.

Against reference tools this is the biggest single gap. WaveLab Pro 12 reads and writes WAV, Wave64, RF64, AIFF, FLAC, MP3 (Fraunhofer *or* LAME, CBR/VBR), AAC/MP4, Ogg Vorbis, Opus, WavPack, WMA, Ambisonics up to 7th order, raw PCM and more, with per-format encoder option dialogs.

### Output findings (measured)

**1. FLAC output does not play on Apple platforms.** This is the most serious finding in the report and it is fully characterised.

- `afinfo` parses mint's FLAC correctly (2 ch, 96 kHz, 24-bit, right frame count), but CoreAudio's `ExtAudioFileRead` fails with error −50 on **both** 16-bit and 24-bit mint FLACs.
- Control: `afconvert` encoded the *identical* PCM to its own FLAC and decoded it back fine — so this is not an afconvert limitation.
- An independent FLAC decoder written for this audit (bit reader, frame CRC-8/CRC-16 validation, all four subframe types, rice + escape partitions, stereo decorrelation) decodes mint's file **perfectly and losslessly** — 0 of 245,760 samples differ from the source WAV, all CRCs valid. So the bitstream is valid and the audio is correct.
- Root cause, isolated: flacenc writes `min_blocksize = 512`, `max_blocksize = 4096` in STREAMINFO. Per the FLAC spec, `min != max` declares a *variable*-blocksize stream, but every frame uses fixed-blocksize (frame-number) coding (sync `0xfff8`). CoreAudio rejects that combination. **Patching exactly two bytes** (min_blocksize → 4096) makes the file decode in CoreAudio, bit-identical to the source.
- Nothing in mint catches this: `tests/output_formats.rs` asserts only the four-byte `fLaC` magic, and mint cannot read FLAC back (symphonia's flac feature isn't wired to the reader path).

**2. FLAC compression is poor to inverted at 24-bit** (measured, same PCM in each case):

| content | raw PCM | mint FLAC | CoreAudio FLAC |
|---|---|---|---|
| 1 kHz sine, 96k/24 | 17.3 MB | 3.5 MB (20.5 %) | — |
| programme, 44.1k/16 | 5.29 MB | 4.14 MB (78.3 %) | — |
| programme, 96k/24 | 17.3 MB | **25.8 MB (149.5 %)** | 14.2 MB (82 %) |

A conformant encoder never exceeds raw — libFLAC falls back to VERBATIM subframes. mint's "lossless" hi-res FLAC is 1.8× the size of CoreAudio's for identical audio.

**3. FLAC above 96 kHz fails — after the whole chain has run.** `codec = "flac"` with `rate = 176400` or `192000` is accepted at config load and then errors at write time: `flac encode failed: Config(VerifyError { components: ["sample_rate"], reason: "must be less than or equal to 96000" })`. Tidal HiRes FLAC and Amazon Ultra HD both accept up to 24/192, so this blocks a real deliverable.

**4. Integer round trips are not bit-exact** (measured). symphonia decodes i16 as `s/32768.0` and i24 as `s/8388608.0`; mint quantises and writes with the symmetric grid `×32767` / `×8388607`. The two conventions disagree, so codes in the upper half of the range shift by 1 LSB even with `dither = "none"`:

```
code   16383 -> 16383 (exact)     code   20000 -> 19999 (SHIFTED)
code   16384 -> 16384 (exact)     code   32767 -> 32766 (SHIFTED)
```

End to end, a pure s16 → s16 transcode changed 52,180 of 441,000 samples (RMS 0.344 LSB) with dither off, and 154,491 samples (RMS 0.592 LSB, up to 2 LSB) with the default `tpdf`. The audible effect is nil (it is a −0.00027 dB scaling plus rounding), but it means a WAV→FLAC "lossless transcode" is not lossless and a null test against the source fails — which is the first thing a mastering engineer checks.

**5. Compounding that: the pipeline never knows the source bit depth.** `pipeline.rs:193` hardcodes `let mut current = OutputSampleFormat::F32`, so `is_reduction` is *always* true for an integer target and every PCM output is re-dithered — including 16→16 and 24→24. Measured: an already-on-grid 16-bit signal picks up 0.501 LSB RMS with `tpdf`, 1.225 with `shaped`, 2.039 with `psychoacoustic`. Sonoris's policy is the right model, and WaveLab documents the same discipline (dither only when depth actually drops or processing occurred).

**6. Zero metadata.** `MetadataOptions::default()` is passed to the probe and `probed.metadata` is never read. No RIFF INFO, ID3, iXML, source bext, cue/markers, loop points, or ISRC is read or written. The one thing written is a synthesised BWF `bext` v2 chunk — and that part is done well: the 602-byte layout and the ×100 `i16` loudness scaling are EBU Tech 3285-conformant, with `0x7FFF` for non-finite, and the values are the real measured EBU R128 figures for the delivery. That is genuinely nicer than most tools, which make you type them in. But `TimeReference`, `UMID`, `OriginationDate/Time` are hardcoded zero/blank, and `CodingHistory` says `M=stereo` for anything with ≥2 channels, so a 5.1 file is mislabelled.

**7. Other I/O gaps:** no atomic writes (`WavWriter::create` and `fs::write` truncate in place — an error mid-write destroys the previous good deliverable); `overwrite = false` only guards `output_path == input` by raw `PathBuf` equality, so pre-existing output files are always clobbered and the input guard is defeated by path spelling; two inputs with the same stem in different directories race onto one output path; >4 GB WAV silently corrupts on the hound path (only the f64 writer has a size guard); no RF64/W64/BW64. WaveLab switches to RF64 automatically above 2 GB.

### Performance and scaling (measured, M1 Pro, 10 cores)

| workload | result |
|---|---|
| 5-min 96k/24 stereo → 4 targets, `--jobs 1` | 71.6 s |
| same, all cores | 28.7 s |
| **1 target, 1 job, peak RSS** | **8.58 GB** (audio payload: 461 MB → 18.6×) |
| same with `on_clip = "warn"` (no gain curve) | 3.72 GB |

The limiter builds five to six full-length 4×-oversampled `f64` arrays (~160–192 bytes per input frame). Extrapolated, a 74-minute album master at 96k/24 would need ~127 GB. Every reference tool streams in blocks and handles multi-hour files in a few hundred MB. This is a hard ceiling on what mint can process, and it is invisible until it fails.

---

## 3. Sample-rate conversion — the strongest area

### What it actually is

`quality = lq|mq|hq|vhq` maps to `SOXR_LQ(1) / SOXR_MQ(2) / SOXR_HQ = SOXR_20_BITQ(4) / SOXR_VHQ = SOXR_28_BITQ(6)`, plus `ROLLOFF_SMALL` at every tier and `HI_PREC_CLOCK` only at vhq. Since it never sets `phase_response`, it is **always linear phase** (soxr default 50). Passband ends at soxr's default 0.913 × Nyquist for hq/vhq; LQ is a much narrower 0.6575. Two details worth knowing: soxr overrides `ROLLOFF_SMALL` to `ROLLOFF_MEDIUM` for lq/mq, so the flag only bites at hq/vhq; and `precision × 6.0206 dB` (120 dB at hq, 169 dB at vhq) is the library's internal filter-design rejection target, not a measured end-to-end stopband — it should not be quoted as a measurement.

I/O is `Float64I` in and out, which provably prevents soxr's internal TPDF dither from engaging (it only applies to INT16 output), preserving the "quantise exactly once" invariant. libsoxr's group delay is queried and the priming frames dropped, and the output is forced to exactly `round(in × ratio)`.

### Measured

| test | result |
|---|---|
| 48k → 44.1k impulse alignment | **+0.000 samples** |
| 96k → 44.1k impulse alignment | **+0.000 samples** |
| 44.1k → 48k impulse alignment | +0.128 samples (+2.7 µs) |
| output length | exactly `round(in × ratio)` |
| 30 kHz tone, 96k → 44.1k, stopband leakage | < −120 dBFS (repo test bound) |
| concurrent use under rayon | 32/32 outputs clean |

### Versus the references

The most useful comparison is against published Infinite Wave measurements (96 → 44.1 kHz), because Weiss Saracon is the usual "reference" claim:

- **Saracon 1.6**: −0.1 dB @ 20.9 kHz, −3 dB @ 21.74 kHz, and still only **−6.5 dB at Nyquist** — with a visible folded-back alias trace in the sweep spectrogram. Weiss acknowledges this as a deliberate trade: some aliasing in 18–22 kHz in exchange for less ringing.
- **SoX/libsoxr VHQ linear phase** (which is what mint runs): −0.1 dB @ 21.76 kHz, −3 dB @ 21.85 kHz, −60 dB @ 22.00 kHz, curve gone before Nyquist, **no measurable alias trace**.
- Passband flatness to 20 kHz is a tie among all of them (±0.02 dB reading resolution).

So on the standard benchmark, mint's SRC is not "approaching" reference class — on aliasing it measures *better* than Saracon, and identically to SoX because it is the same engine. WaveLab and Audition publish no numbers at all for their converters ("exceptional transparency"; an unlabelled quality slider).

### What's missing

No phase choice (SoX exposes `-M/-I/-L` minimum/intermediate/linear; RX exposes continuous pre-ringing 0–1; mint's wrapper's closed `QualityRecipe` enum makes soxr's phase and `STEEP_FILTER` options unreachable). No bandwidth/aliasing trade knobs (`-b`, `-a`, `-s`). No DSD. Backend is compile-time, not per-target. The SRC tier isn't recorded in the JSON report. The rubato fallback has no numeric test coverage at all, and for inputs under ~131 frames it emits pure digital silence. And libsoxr itself has had no release since 2018.

---

## 4. Loudness — accurate core, structural gaps

### What it does

ebur128 0.1.10 (pure-Rust libebur128 port). Integrated with BS.1770 gating (−70 LUFS absolute, −10 LU relative), LRA, max momentary and max short-term polled on 100 ms hops, true peak, plus derived PLR and PSR. Normalisation is a single static gain, then the ceiling is enforced by a loop that re-limits with a progressively lowered internal ceiling until the **BS.1770 meter itself** agrees (≤6 passes, +0.05 dB tolerance). `on_clip = "maximize"` adds a makeup-gain convergence loop that restarts from a pristine copy each pass, so exactly one limiter generation survives.

### Measured accuracy (end-to-end, 96k/24 source → delivered file, re-measured independently)

| target | delivered | error |
|---|---|---|
| spotify preset (−14 LUFS, 96k→44.1k SRC, s16) | −14.03 LUFS | **−0.03 LU** |
| same-rate −14 LUFS | −14.00 LUFS | **−0.00 LU** |
| hires (−16 LUFS, no SRC) | −16.00 LUFS | −0.00 LU |
| maximize to −9 LUFS, cap 6 dB | −9.16 LUFS | capped, **warned correctly** |

That is better than the ±0.1–0.2 LU that reference tools promise, and the maximize convergence loop is a real differentiator: Audition's Match Loudness and WaveLab's Loudness Normalizer compute one gain and let the limiter eat loudness, which is exactly the undershoot mint solves.

### Gaps that matter

- **No album/multi-file normalisation.** Every job is independent. This is the single most consequential functional gap, because Spotify, Apple Sound Check and Tidal all normalise *per album*, and AES TD1008 gives distinct values for track-normalised (−16 LUFS) versus album-loudest-track (−14 LUFS). Per-track normalisation to a fixed LUFS actively destroys the inter-track relationships an album master is built on. WaveLab does this in the Montage.
- **Mono is measured 3.01 LU quieter than the same signal as dual mono** (measured: −15.04 vs −12.03 LUFS). mint never calls `set_channel_map`, so a 1-channel file is metered as a single `Left` channel. A mono track normalised to −14 LUFS plays back through two speakers at ≈−11 LUFS. libebur128 has a `DualMono` channel type for exactly this.
- **7.1 rear channels are dropped from the measurement** — `default_channel_map` marks channels 7+ as `Unused` (weight 0). 5.1 in SMPTE order is handled correctly (Ls/Rs +1.5 dB, LFE excluded).
- **The loudness check happens before the resampler.** The ±0.1 LU "target not met" warning compares the *pre-SRC* figure (`pipeline.rs:121`), and loudness is never re-verified afterwards. For band-limited programme the drift is −0.03 LU (harmless); with an ultrasonic-heavy 96 kHz source the measured result was **−2.54 LU delivered error with no warning at all**. AES TD1008 is explicit that true-peak limiting belongs *after* the SRC; the same logic applies to the loudness verification.
- **Digitally silent input**: accepted, gain is `+inf`, output is all zeros, delivered loudness reported as NaN, **no warning, exit code 0**.
- No measure-only mode (metering requires rendering a file). No sample-peak, no clipped-sample count, no DC offset, no per-channel figures. The applied gain is computed and then discarded — it never reaches the report, the JSON, or the bext chunk. `on_clip = "warn"` leaves supra-full-scale samples in the buffer and the quantiser then hard-clips them silently and uncounted.
- PSR is defined as global true peak minus max short-term, not the conventional per-window short-term crest.

---

## 5. Limiting

### Architecture

Single-band, single-stage, offline look-ahead brickwall. The detector is a self-generated 4× polyphase interpolator with 12 taps per phase — structurally the same size as BS.1770 Annex 2's order-48, 4-phase FIR, but with Blackman-windowed-sinc coefficients rather than the ITU table. Gain curve: instantaneous requirement → O(n) monotonic-deque sliding minimum over the look-ahead → adaptive linear-in-dB release → causal normalised-Hann convolution so the attack is a raised-cosine ramp that reaches full attenuation exactly on the peak. Three characters: balanced (1.2 ms look-ahead, 18→42 dB/s), transient (0.6 ms, 30→75), dense (2.0 ms, 7→28).

### Real strengths (measured)

- **Zero latency, exactly sample-aligned.** Cross-correlation of input against limited output peaks at lag 0. Samples are only multiplied in place.
- **Bit-exact transparent below the ceiling.** Max sample difference 0.000e0 on a −10 dBFS sine with a −1 dBTP ceiling. That is a genuine delivery property, not just an optimisation.
- **The ceiling is verified against the BS.1770 meter, not just the limiter's own detector.** No reference limiter does this — Pro-L 2, ISL 2 and Ozone all trust their own detector.
- Provable no-overshoot argument for the smoothing stage, encoded as a test.

### Where it sits against the references

BS.1770's own Attachment 1 tabulates the worst-case under-read of an oversampled true-peak estimator: 4× is 0.554–0.688 dB optimistic; you need ≥12× to stay under 0.1 dB. mint detects at 4× and verifies at 4× (ebur128 uses 4× below 96 kHz, 2× to 192 kHz, and no oversampling at/above 192 kHz — which is standard-conformant, since BS.1770 scales the requirement with input rate).

Measured consequence, end to end: on a pure-transcode target with a −1.00 dBTP ceiling, the delivered file measured **−1.00 dBTP at 4× and −0.92 dBTP at 16× — 0.08 dB over the configured ceiling**. Other targets came in at −1.02/−1.02. So the practical error is small but real, and it is one-sided. Pro-L 2 offers up to 32× oversampling and documents 4× + ≥0.1 ms lookahead as its "true peaks within 0.1 dB" working point; NUGEN ISL 2 lets you pick between the BS.1770 algorithm and Apple's `afclip` algorithm and power-compensates the ceiling when dither is engaged.

### Gaps

Single band (no L3-style multiband). Three fixed characters with no numeric attack/release/lookahead controls — Pro-L 2 has 8 styles plus continuous controls, WaveLab ships five different limiters, Ozone has five IRC modes with a character knob. No oversampled output rendering, so the limiter's own output isn't band-limited after gain application (the iterative meter check is the mitigation). No delta/listen mode, no GR history, no per-channel ceilings, no DC-offset filter. `max_limiting_db` bounds only `maximize` — the default `on_clip = "limit"` has *no* bound on how hard it will squash. The reported "peak gain reduction" is an estimate (`predicted_tp − ceiling`), not the measured maximum, and understates GR whenever the verify loop needs more than one pass; the post-SRC and pure-transcode limiter runs report no GR at all. `limiter_soft_clip` is mathematically an *expander* in the knee (it raises samples in the top 0.97–1.0 of the ceiling by up to +0.028 dB) and it runs once per limiter pass. The post-SRC re-limit ignores `on_clip` entirely, so `warn` and `reduce_gain` targets get brickwalled whenever a resample happens — it does emit a warning, but README:340 says these options are ignored unless `on_clip` is limit/maximize, which is wrong.

---

## 6. Dither and noise shaping — the most interesting area

### What's actually implemented (measured, from the project's own harness)

| curve | peak dB | total noise dB | notch | ATH-weighted improvement |
|---|---|---|---|---|
| Gentle `(1−z⁻¹)²` | +12.04 | +7.78 | DC | +5.98 dB |
| Lipshitz psychoacoustic | +19.43 | +12.21 | **−27.2 dB @ 4048 Hz** | +9.78 dB (analytic) / +9.88 (empirical) |
| adaptive (production path) | — | — | per-frame | +5.05 idle / **+4.05 dB NMR** |
| adaptive, quiet-passage safeguard | — | — | — | **+10.35 dB** (vs Lipshitz +9.88) |

The adaptive path is real work: 1024-point/512-hop Hann STFT → 24 Bark bands → Schroeder spreading → SFM tonality → Terhardt ATH → per-frame cepstral minimal-phase FIR design (order 5/7/9 by strength) satisfying the Gerzon–Craven constraint, linearly crossfaded per sample, with a per-frame choice between the masking-adaptive curve and a fixed ATH-optimal floor so it can never be worse than a fixed shaper in exposed passages. That is the same *class* of idea as MBIT+ and POW-r 2/3, independently implemented, and the harness proves it wins the noise-to-mask objective (+4.05 dB vs the best fixed curve's +0.14 dB) while the safeguard keeps it ahead on the idle floor too.

Two caveats on the theory, found in the audit and worth fixing: the ATH implementation computes `exp(−0.36(f−3.3)²)` instead of the published Terhardt `exp(−0.6(f−3.3)²)` (an operator-precedence slip — `-(0.6*(f-3.3)).powi(2)`), giving a notch ~1.67× too wide; and `FLOOR_CLAMP_DB = 16` saturates an ATH target whose true dynamic range is ~240 dB, so the "ATH-optimal floor" is effectively a high-shelf near 15 kHz rather than a 4 kHz notch.

### Where it falls short of the references

- **Shaping is 16-bit only.** At s24 all three shaped modes silently degrade to flat TPDF. Pro-L 2 dithers at 24/22/20/18/16; Waves IDR at 24/22/20/18/16; ISL 2 at 24/20/18/16; MAAT LINpro (shipped in WaveLab as Lin Pro Dither) at 8/16/24 with seven shaping orders; Audition offers 4 pdfs × 5 noise-shaping families with crossover, strength and an adaptive mode. mint offers three curves at one depth.
- **`dither_strength` and `dither_correlation` are parsed, defaulted, merged, printed in the dry-run — and ignored** by `tpdf`, `shaped` and `psychoacoustic`. They only reach the adaptive+s16 path. Neither appears anywhere in README.md or examples/mint.toml.
- **No auto-blanking on the three non-adaptive modes.** A silent lead-in renders as ±1 LSB TPDF, or ~2.0 LSB RMS with the psychoacoustic shaper. UV22HR ("Auto Black"), Waves IDR Type 2 and MAAT LINpro all blank. Only the adaptive path blanks — and there it mutes real sub-half-LSB programme material to digital zero, is not defeatable, and is never reported.
- **The adaptive path uses a hardcoded RNG seed** (`0xDEADBEEFCAFEF00D`) when `--seed` is absent, so every file and every target in a batch gets the identical dither noise stream. Conversely the other modes seed from entropy and record nothing, so a delivered file can never be reproduced — the report and JSON carry neither the dither mode, strength, correlation, nor the seed.
- **`dither = "adaptive"` produces a digitally silent output file at 176.4 kHz and 192 kHz** (measured, s16 target, peak 0.0000, no error, and the report still claims adaptive shaping ran). Cause: the ATH's `1e-3·f⁴` term overflows to `+inf` above ~50 kHz, NaN-poisoning the FIR design. 44.1k and 96k are fine.
- Context on the fixed curves: SoX/FFmpeg offer eight named shaping filters (lipshitz, f-weighted, three e-weighted variants, gesemann, shibata family) but they are rate-tied with a 5 % tolerance and nothing above 48 kHz, so mint's two fixed curves are not unusually thin by open-source standards — they are thin by *commercial* standards. mint's psychoacoustic curve is likewise tuned for 44.1 kHz and applied unchanged at any s16 rate.

---

## 7. The presets encode a mistaken model of streaming delivery

This is a design-level finding, not a bug, and it is well sourced.

| preset | mint | what the platform actually publishes |
|---|---|---|
| spotify | 44100/s16/−14/−1.0 | FLAC or WAV, **44.1 kHz or higher, 24-bit** for native masters; −14 LUFS is the *playback normalisation* level and −1 dBTP is a **ceiling** (−2 if louder than −14); normalises **per album** |
| apple-music | 44100/s16/−16/−1.0 | Apple Digital Masters requires **24-bit at native rate**; up-sampling/bit-padding "not allowed" — a 44.1/16 file cannot be badged. Apple publishes **no stereo LUFS target**; −16 is a third-party report of Sound Check |
| youtube | 48000/s16/−14/−1.0 | 48 kHz and **24-bit recommended**, FLAC/PCM. YouTube publishes **no LUFS figure anywhere** |
| tidal | 44100/s16/−14/−1.0 | Tidal publishes **nothing**; the −14 figure is 2016–17 third-party measurement. FLAC-only, HiRes up to 24/192 — a 16/44.1 delivery forfeits HiRes badging |
| amazon-music | 44100/s16/−14/−2.0 | Amazon publishes **no LUFS and no dBTP**; the −2 is untraceable. 44.1/16 caps you at HD and forfeits Ultra HD |
| soundcloud | 44100/s16/−14/−1.0 | **The one preset that matches published guidance** — though it misses the conditional −2 dBTP rule |
| cd | 44100/s16/—/−0.1 | correct as a *file*, but a CD deliverable needs DDP 2.0, PQ codes, ISRC, UPC, CD-Text, gaps |
| broadcast-ebu-r128 | 48000/s24/−23/−1.0 | **correct** vs R128 (2023). R128 itself specifies no rate or depth; short-form (R128 s1) additionally caps max short-term at −18.0 LUFS |
| hires | 96000/s24/−16/−0.1 | **no hi-res destination specifies any loudness target.** Bandcamp applies no normalisation at all and asks for 0.5 dB headroom, so −0.1 dBTP is too hot. (96k/24 *does* qualify for Apple Hi-Res Lossless, which requires ≥88.2 kHz) |

Three structural points behind that table:

1. **No platform asks for 16-bit, and none asks you to pre-convert the rate.** They all want the native, highest-resolution master and do their own SRC and word-length reduction — several actively penalise a pre-downsampled delivery. Every streaming preset in mint does the opposite.
2. **The published LUFS numbers are playback ceilings, not delivery targets.** Spotify applies *positive* gain to soft masters. Pushing a −18 LUFS master up to −14 with `on_clip = "limit"` (the default) permanently spends dynamic range to pre-empt a gain change the platform would have made losslessly. mint already ships `reduce_gain`, which would be the safer default for these presets.
3. **All of them normalise per album; mint normalises per track.** Rendering an album through mint flattens the inter-track loudness relationships that album normalisation would have preserved.

Missing but standard: Deezer (−15), ATSC A/85 (−24 LKFS, −2 dBTP), Bandcamp (no normalisation, 24-bit, 0.5 dB headroom), Apple Digital Masters proper, R128 s1 short-form, and AES TD1008's speech/podcast values.

---

## 8. Defects found, ranked

| # | Severity | Finding |
|---|---|---|
| 1 | **critical** | FLAC output fails to decode in Apple CoreAudio (16- and 24-bit) — valid, lossless bitstream, but STREAMINFO declares variable blocksize while frames are fixed. 2-byte fix verified |
| 2 | **critical** | `dither = "adaptive"` at 176.4/192 kHz silently writes a digitally silent file (ATH `f⁴` overflow → NaN) |
| 3 | **high** | Whole-file-in-RAM + 4× oversampled limiter arrays: 8.58 GB peak for one 5-minute 96k track; long-form/album masters are simply not processable |
| 4 | high | `overwrite = false` doesn't protect pre-existing output files; no atomic writes; same-stem inputs race onto one path |
| 5 | high | Mono deliverables normalised 3.01 LU too loud relative to dual-mono playback |
| 6 | high | Loudness never re-verified after SRC; the ±0.1 LU warning uses the pre-SRC value (measured −2.54 LU silent error on ultrasonic-heavy input) |
| 7 | high | FLAC at >96 kHz fails at write time, after the full DSP chain |
| 8 | medium | No integer round-trip transparency (decode ÷32768 vs encode ×32767) + unconditional re-dither because source depth is never tracked |
| 9 | medium | `dither_strength` / `dither_correlation` silently inert for all non-adaptive modes; both undocumented |
| 10 | medium | 24-bit FLAC expands to 149 % of raw PCM |
| 11 | medium | Silent/NaN input → all-zero deliverable, NaN in the report, no warning, exit 0 |
| 12 | medium | `on_clip = "warn"` output is hard-clipped by the quantiser, uncounted and unreported |
| 13 | medium | Adaptive dither uses a constant seed; no mode/seed recorded in any report |
| 14 | low | `[defaults] preset = "..."` is parsed but never applied |
| 15 | low | ATH Terhardt operator-precedence slip; `FLOOR_CLAMP_DB` saturation |
| 16 | low | README: default `naming` documented as `{target}/{stem}.wav`, code uses `{cext}`; README:340 wrong about limiter options and the post-SRC pass; `examples/distribution.toml` referenced but absent |

---

## 9. What would move it toward reference class

Ranked by benefit per unit of work:

1. **Fix the two shipping defects** (#1, #2). Both are small: set `min_blocksize == max_blocksize` (or file it upstream at flacenc), and clamp the ATH frequency argument.
2. **Track the source bit depth and adopt a dither policy** — dither only on an actual reduction or after gain/SRC, and align the decode/encode integer grids. That buys bit-transparent transcodes and removes a whole class of "why isn't this a null?" complaints.
3. **Stream the limiter** (block-process the gain curve) or at minimum reuse buffers. Removes the memory wall and the album-length limit.
4. **Album mode**: one gain per group of inputs. This is the biggest functional gap against WaveLab and against how every platform actually works.
5. **Rebuild the presets around what platforms publish**: 24-bit, native rate, `reduce_gain` as the default policy for streaming, conditional −2 dBTP, and drop the loudness target from `hires` entirely. Cite the source for each.
6. **Report what you did**: applied gain, dither mode/strength/seed, SRC tier, sample peak, clipped-sample count, GR from every limiter pass. All of it exists internally and is thrown away.
7. Then the depth items: shaping above 16-bit, wiring strength/correlation, a mono/dual-mono switch, 8× or 16× true-peak detection, metadata passthrough, and — if CD is really a target — DDP.

The core DSP is in better shape than the surrounding delivery machinery. The resampler, the loudness convergence, the zero-latency bit-transparent limiter and the meter-verified ceiling are all things worth defending against the commercial field. The parts that decide whether a deliverable is *usable* — container correctness, metadata, bit-exactness, scale, and preset semantics — are where the distance is.

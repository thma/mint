# MBIT+ Implementation Plan: Auf echtes Top-of-Class-Niveau

> Begleitdokument zu `# MBIT+ Dither.md` (Zielbild) — dieses Dokument beschreibt,
> **was konkret im Code geändert werden muss**, um vom heutigen heuristischen
> `mbit_plus`-Pfad auf das Niveau von Ozone / WaveLab (UV22HR) / Weiss Saracon
> (POW-r) / iZotope MBIT+ zu kommen.

## Ausgangslage (Stand der Analyse)

Der heutige `quantize_mbit_plus` (`src/dither.rs`) trägt das Vokabular von MBIT+,
implementiert aber ein zeitvariantes Heuristik-Verfahren mit mehreren **Regressionen
gegenüber einfachem TPDF**:

- **Signalabhängige Dither-Amplitude** (0.40–1.60 LSB, `dither.rs:293-300`) →
  Noise-Modulation + unvollständige Linearisierung. Genau der Artefakt, den gute
  Tools vermeiden.
- **Zeitvariante Feedback-Koeffizienten pro Sample** (`dither.rs:172, 258-291`) →
  keine definierte/stabile NTF, Distortion durch `clamp`.
- **„3-Band-Split" ist faktisch keiner**: der LF-Tiefpass hat ~13 Hz Grenzfrequenz
  (`dither.rs:114-122`), LF/MF/HF sind nahezu kollinear und amplitudengetrieben.
- **Per-Sample-Moduswechsel** (`select_mode`, `dither.rs:143, 226-234`) → Zipper-/
  Klick-Artefakte (kein Crossfade).
- **„Pre-Masking" ist Steigungs-Extrapolation**, kein Look-ahead (`dither.rs:147`).
- **Fehlbenannte Größen**: `lufs_proxy` (keine K-Gewichtung), `dyn_range` (real
  Crest-Faktor) (`dither.rs:141-142`).
- **Keine Wirksamkeits-Tests**: `tests/noise_shaping.rs` prüft für `mbit_plus` nur
  Determinismus/Gating/Stereo — nicht, dass der Modus In-Band-Rauschen senkt.

Ironie: Der bereits vorhandene `DitherMode::Psychoacoustic` (Lipshitz/Vanderkooy/
Wannamaker, `dither.rs:345`) ist näher an Top-of-Class als der neue `mbit_plus`-Modus.

## Schlüssel-Vorteil von mint

mint arbeitet **offline/batch** — die komplette Datei liegt im `AudioBuffer` im RAM.
Damit kosten **echtes Look-ahead (Pre-Masking)** und eine **separate Analyse-Vorab-
Pass null Latenz**. Dieser Vorteil wird heute verschenkt und ist der Hebel für ein
korrektes, sogar über-statisches Verfahren.

---

## Strategische Weiche

„Echtes MBIT+-Level" zerfällt in zwei unterschiedlich teure Ziele:

| Ziel | Bedeutung | Aufwand/Risiko |
|---|---|---|
| **A — Static best-in-class** | Feste, hoch-optimierte, gehörgewichtete Shaping-Kurven (mehrere Stärken) + korrektes Dither + Auto-Blanking + Rate-Awareness | mittel, planbar |
| **B — Adaptiv (Doc-Ambition)** | Zusätzlich echte FFT/Bark-Maskierung, blockweise Filter-Neuberechnung, perceptual noise allocation | hoch, forschungs-nah |

**Einordnung:** Ozone, WaveLab/UV22HR, Weiss Saracon/POW-r sind im Kern **statische**
Verfahren mit fest optimierten Kurven. iZotope MBIT+ selbst ist nach allem öffentlich
Bekannten sophisticated *statisches* Shaping mit wählbaren Stärken + gutem Dither +
Auto-Blank — **kein** per-Block-FFT-Allocator.

➡️ **Phase A allein erreicht „MBIT+-Level".** Phase B ist „darüber hinaus / experimentell"
mit abnehmendem hörbarem Grenznutzen.

**Empfehlung:** Ziel A umsetzen (Phasen 1–3), Ziel B (Phase 4) bewusst zurückstellen.

---

## Zielarchitektur

```
AudioBuffer (ganze Datei im RAM)
   │
   ├─ PASS 1: Analyse (nur Phase B)
   │     STFT → Bark/ERB-Energie → Spreading → ATH
   │     → globale Maskierungsschwelle pro Block
   │     → minimalphasige Shaping-Koeffizienten pro Block (+ Interpolation)
   │
   └─ PASS 2: Quantisierung (sequentiell pro Kanal)
         konstantes TPDF  →  fester/blockweiser Error-Feedback-Shaper
         →  Auto-Blank-Gate  →  Rundung  →  Clamp
```

---

## Phase 1 — Korrektheit: nicht schlechter als TPDF

Ziel: die Regressionen entfernen, die `mbit_plus` heute *unter* Lehrbuch-TPDF drücken.
Nicht verhandelbar.

**1.1 Konstantes Dither** (`src/dither.rs`)
- `dither_amplitude_lsb` (Z. 293-300) durch **konstant 1.0 LSB TPDF** (2 LSB pp)
  ersetzen. Eliminiert Noise-Modulation, linearisiert den Quantisierer vollständig.
  → Wichtigster Einzelschritt.

**1.2 Feste Shaping-Koeffizienten statt per-Sample-Adaption** (`dither.rs:172, 258-291`)
- `adaptive_feedback` als sample-variant ersetzen; Koeffizienten über den Block
  (Phase 1: über die ganze Datei) konstant halten.
- `select_mode` per-Sample (Z. 143, 226-234) entfernen.

**1.3 Auto-Blanking**
- Lauf von `|x| < ~0.5 LSB` über > ~50 ms → Dither + Feedback aus, History
  auf 0 zurückklingen lassen. Verhindert ewigen Rauschteppich in Stille.

**1.4 Größen ehrlich machen**
- ~13-Hz-Pseudosplit (Z. 102-132) entfernen oder klar als Pegel-Proxy benennen;
  `lufs_proxy`/`dyn_range` umbenennen. Dürfen in Phase 1 keine Koeffizienten steuern.

**Ergebnis:** sauberer, fester Error-Feedback-Shaper mit korrektem Dither —
funktional ≈ vorhandener `psychoacoustic`-Modus, aber als Basis für Phase 2/3.

---

## Phase 2 — Static best-in-class: die Kurve, die zählt

Der eigentliche Qualitätssprung. Kern: eine **richtig entworfene, minimalphasige,
gehörgewichtete Shaping-Kurve** statt ad-hoc Koeffizienten.

**2.1 Filter-Design (Herzstück, kritischer Pfad)**
- Ziel-NTF: `|1 − H(e^jω)|` folgt dem **inversen ATH-/Equal-Loudness-Verlauf**
  (tief bei 2-4 kHz, hoch zu DC und Nyquist).
- Verfahren: Ziel-Magnitudengang aus ATH-/F-Gewichtung → **minimalphasige
  Spektralfaktorisierung** (Cepstrum-Methode) → FIR-Koeffizienten. Minimalphasig
  ist Pflicht: garantiert Stabilität der Rückkopplung **und** minimiert die zugefügte
  Gesamt-Rauschleistung für die gegebene NTF-Form (Noise-Shaping-Theorem).
- **3 Stärken** analog POW-r 1/2/3 bzw. MBIT+ low/normal/high — z. B. 3-Tap (sanft),
  5-7-Tap (normal), 9-Tap (aggressiv). Vorhandene Lipshitz-5-Tap als „normal"-Referenz.
- Liefer-Form: Koeffizienten **offline berechnen und als Konstanten einchecken**
  (kein Design-Code im Hot-Path) + ein dokumentiertes Design-Skript/Test, das sie
  reproduziert.

**2.2 Rate-Awareness** (heute fehlend — Lipshitz sitzt nur bei 44.1 k richtig)
- Pro Standardrate (44.1 / 48 k) eigene Koeffizientensätze; für 88.2/96 k eigene
  Sätze oder Design-on-the-fly aus der ATH. Notch immer bei ~3.5-4 kHz, ratenunabhängig.

**2.3 Config/API** (`src/config.rs`)
- `mbit_plus` mit Stärke-Stufen: neue Modi (`mbit_plus_low/normal/high`) **oder**
  Begleitparameter `dither_strength`. Default = „normal".
- Dry-run/Report (`effective_dither_tag`, Z. 610) muss echte Kurve + Stärke + Rate
  ausweisen.

**Ergebnis:** Niveau POW-r/UV22HR/MBIT+ (statisch). Realistisches Ziel „echtes
MBIT+-Level".

---

## Phase 3 — Validierung & Messung (Pflicht)

Heute belegt kein Test, dass `mbit_plus` etwas verbessert.

- **FFT-NTF-Messung**: Senke real bei ~3-4 kHz, Tiefe pro Stärke korrekt (statt der
  No-FFT-Proxies in `tests/noise_shaping.rs:57-79`).
- **Noise-Modulation-Test**: Rauschvarianz in Stille vs. unter Ton ≈ konstant
  (beweist 1.1).
- **Auto-Blank-Test**: digitale Stille → Output exakt 0.
- **Gesamt-Rauschleistung vs. In-Band-Energie** pro Stärke.
- **Rate-Test**: Notch-Lage bei 44.1 vs. 48 k.
- Abhängigkeit: `realfft`/`rustfft` als **dev-dependency** (Tests/Design), nicht im
  Runtime-Pfad.

---

## Phase 4 — Adaptiv (optional, „über MBIT+ hinaus")

Nur bei Ziel B. Löst das Design-Doc wirklich ein.

**4.1 Analyse-Pass** (`src/dither/psychoacoustic.rs`, neu)
- STFT (`realfft`, Frame 1024 @ 50-75 % Overlap, Hann) → **Bark/ERB-Bandenergien**
  → **Spreading-Function** (Schroeder) → Tonalitäts-Maß (Spectral Flatness) für
  Maskierungs-Offset (NMT ~5 dB / TMN ~25 dB, MPEG-Psymodell-1) → **globale
  Maskierungsschwelle = max(spread+offset, ATH)**.

**4.2 Per-Block-Filterdesign mit Crossfade**
- Aus der Maskierungsschwelle pro Frame eine minimalphasige Shaping-Kurve
  (begrenzte Ordnung) entwerfen, Koeffizienten **zwischen Blöcken interpolieren** →
  kein Zipper. Niemals per-Sample.

**4.3 Echtes Pre-Masking ohne Latenz**
- Look-ahead = ein Frame, gratis (alles im RAM). Ersetzt die Steigungs-Extrapolation
  (`dither.rs:147`).

**4.4 Stereo-Dither korrekt**
- Default **dekorreliert pro Kanal** (Standard). Korrelation nur optional mit
  ehrlicher Doku; für >2 Kanäle sinnvolles Schema statt „alle gegen L"
  (`dither.rs:164-170`).

**Ergebnis:** Über den statischen Tools — aber hoher Aufwand, schwer messbarer
Mehrwert ggü. Phase 2. Ehrlich optional.

---

## Aufwand & Reihenfolge

| Phase | Liefert | Aufwand |
|---|---|---|
| 1 Korrektheit | Nicht schlechter als TPDF | klein |
| 2 Static curve | **MBIT+-Level erreicht** | mittel (Filter-Design = Knackpunkt) |
| 3 Validierung | Beweis + Regressionsschutz | klein-mittel |
| 4 Adaptiv | Über die Tools hinaus | groß |

**Empfohlene Reihenfolge:** 1 → 3 (Tests früh) → 2 → 3 erweitern → (optional) 4.

**Kritischer Pfad:** minimalphasiges Filter-Design (2.1). Daran hängt die Qualität.

**Größte Risiken:** (a) Spektralfaktorisierung korrekt + stabil; (b) Phase 4
Modulationsartefakte bei unsauberer Block-Interpolation/Crossfade.

---

## Betroffene Dateien (Übersicht)

- `src/dither.rs` — Kern: Dither-Amplitude, Shaper, Koeffizienten, Auto-Blank.
- `src/ops/bitdepth.rs` — Integration/Gating (`is_mbit_plus`, `quantize_in_place`).
- `src/config.rs` — `DitherMode`/Stärke-Param, `effective_dither_tag`.
- `tests/noise_shaping.rs` — neue FFT-/Modulations-/Auto-Blank-/Rate-Tests.
- `Cargo.toml` — `realfft`/`rustfft` als dev-dependency.
- ggf. `src/dither/psychoacoustic.rs` (neu, nur Phase B).
- `README.md` — Doku an tatsächliches Verhalten anpassen.

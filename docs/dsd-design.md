# DSD-Ausgabe für mint — Design & Plan

Status: **Entwurf, erste Runde durchgesprochen**. Nichts davon ist implementiert.

### Getroffene Entscheidungen (Runde 1)

| Frage | Entscheidung |
|---|---|
| Vorgehen | Durchgehend nach Plan, kein vorgeschalteter Go/No-Go-Spike |
| Scope erster Wurf | **Alle DSD-Raten (64/128/256/512), Container nur DSF**; DFF vorerst raus |
| Interpolation | **Komplett auf libsoxr** — rubato gilt als obsolet, Bit-Parität zwischen den Build-Varianten ist damit kein Designziel mehr |
| rubato-Backend | **Bereits entfernt** (eigener Commit, vor der DSD-Arbeit): kein `soxr`-Feature, kein `--no-default-features`-Pfad, kein zweiter CI-Job mehr |

Offen: die Zwischenraten-Regel (§ 2.1) — unten ausführlich hergeleitet.

---

## 0. Kurzfazit

Machbar, aber es ist kein „noch ein Codec"-Feature. Drei Dinge unterscheiden DSD von
FLAC/MP3:

1. **Es gibt keine Bibliothek, auf die wir aufsetzen können.** Den Sigma-Delta-Modulator
   (PCM → 1 Bit) müssen wir selbst schreiben. Das ist der eigentliche Aufwand und das
   einzige echte Risiko.
2. **Der f64-Vollpuffer-Ansatz von mint bricht.** DSD64 Stereo, 5 Minuten, wären als f64
   im RAM ~13,5 GB (DSD256: ~54 GB). Die DSD-Stufe muss blockweise streamen.
3. **Die Container (DSF/DFF) sind trivial** — je ~100–150 Zeilen, keine Abhängigkeit nötig.

Aufwandsschätzung: **~7–9 Personentage** für DSD64/128/256/512 in DSF inkl. Messharness;
davon entfallen 3–4 Tage auf Modulator + Verifikation. Der Rest ist mechanische
Integration entlang der bestehenden Checklisten in `CLAUDE.md`.

Entschieden ist, durchgehend nach Plan zu arbeiten statt den Modulator vorher als
isolierten Spike abzusichern. Die Risikoreihenfolge bleibt trotzdem erhalten: Phase 1 und
2 (Interpolation, Modulator + Messharness) kommen zuerst, weil sie das einzige echte
Unbekannte enthalten — nur eben ohne formales Go/No-Go-Tor dahinter.

---

## 1. Bibliothekslage (recherchiert, Stand 08/2026)

### Was es gibt

| Projekt | Sprache | Kann | Für uns brauchbar |
|---|---|---|---|
| [`dsf`](https://crates.io/crates/dsf) | Rust | DSF **lesen**/parsen, ID3 | nur als Test-Oracle |
| [`dsd-reader`](https://lib.rs/crates/dsd-reader) | Rust | DSF/DFF/raw **lesen** | nur als Test-Oracle |
| [`dsd2dxd`](https://github.com/clone206/dsd2dxd) | C++ | DSD → PCM (Dezimation) | Referenz für unseren Test-Dezimator |
| [`JustDSD`](https://github.com/drogatkin/JustDSD) | Java | DSD → PCM | Referenz |
| [`dsenc`](https://github.com/dcherednik/dsenc) | C | PCM → DSD64, **2. Ordnung** | nur als Anschauung; Qualität reicht nicht |
| ffmpeg | C | DSD **de**kodieren (`dsd_lsbf_planar`) | Test-Oracle, kein Encoder |
| [Delta-Sigma Toolbox](https://www.eecis.udel.edu/~vsaxena/courses/ece697A/docs/DSToolbox.pdf) (Schreier) | MATLAB/Python | NTF-**Design** | ja — offline zur Koeffizientenberechnung |

### Was es nicht gibt

Keinen produktionsreifen Open-Source-PCM→DSD-Encoder, in keiner Sprache. Die Werkzeuge,
die das ernsthaft können, sind alle kommerziell (HQPlayer/Signalyst, Weiss Saracon,
Merging Pyramix, KORG AudioGate) und veröffentlichen ihre Modulatoren nicht. `dsenc` ist
ein 2.-Ordnung-Experiment und sagt das selbst.

**Konsequenz:** Wir schreiben den Modulator selbst. Das ist weniger dramatisch als es
klingt — die Theorie ist seit 30 Jahren publiziert, die Koeffizienten kommen aus
Schreiers `synthesizeNTF`, und mint hat mit `dither/psychoacoustic.rs` +
`dither/adaptive.rs` bereits deutlich anspruchsvollere DSP im Repo. Der Modulator ist
strukturell ein naher Verwandter des vorhandenen `NoiseShaper`: Fehler-Rückkopplung um
einen Quantisierer, nur mit 1-Bit-Quantisierer, höherer Ordnung und
Stabilitätsüberwachung.

Für die Container brauchen wir gar nichts: DSF ist ein Little-Endian-Chunk-Format mit
3 Chunks, DFF ein Big-Endian-IFF/FORM-Format. Beide schreiben wir direkt — genau wie
`encode_wav_f64` und der `bext`-Chunk heute schon von Hand geschrieben werden.

---

## 2. Fachliche Grundlagen (das, was das Design bestimmt)

### 2.1 Raten — deine Annahme stimmt, mit einer Präzisierung

DSD ist an die 44,1-kHz-Familie gebunden:

| Name | Rate | = 44 100 × | Stereo-Datenrate | 5 min Stereo |
|---|---|---|---|---|
| DSD64 | 2 822 400 Hz | 64 | 0,71 MB/s | ~212 MB |
| DSD128 | 5 644 800 Hz | 128 | 1,41 MB/s | ~423 MB |
| DSD256 | 11 289 600 Hz | 256 | 2,82 MB/s | ~847 MB |
| DSD512 | 22 579 200 Hz | 512 | 5,64 MB/s | ~1,69 GB |

Die Präzisierung: der eigentliche Grund für „vorher auf 44,1/88,2/176,4 runter" ist nicht
DSD selbst, sondern dass **das Interpolationsverhältnis eine Zweierpotenz sein soll**.
2 822 400 / 176 400 = 16, / 88 200 = 32, / 44 100 = 64 — alles exakt. Aus der
48-kHz-Familie (48/96/192 kHz) wird das Verhältnis irrational (2 822 400 / 192 000 =
14,7), was einen teuren, unsauberen SRC direkt vor dem empfindlichsten Glied der Kette
erzwingen würde. Also: erst in die 44,1-Familie, dann rein binär hochinterpolieren.

Das kann mint schon — Schritt 2 der bestehenden Kette (`resample`, libsoxr) macht
192 k → 176,4 k mit `vhq` sauber. Es kommt also kein neuer SRC dazu, nur eine
Ratenableitung.

#### Die Regel im Detail

```
zwischenrate = min( nächste_441_rate(quellrate),  dsd_rate / 32 )
```

Zwei unabhängige Bedingungen, die aus verschiedenen Gründen da sind.

**Bedingung A — „nächste Rate der 44,1-Familie".** Sorgt dafür, dass das
Interpolationsverhältnis eine Zweierpotenz ist (siehe oben). Für alle üblichen Quellraten
ist die nächstgelegene 44,1-Rate zugleich die nächstniedrigere, weil die 44,1-Familie
jeweils knapp unter der 48er liegt: 192→176,4; 96→88,2; 48→44,1. Raten der 44,1-Familie
bleiben unverändert.

Einziger Fall, in dem dabei echte Bandbreite verlorengeht: **48 → 44,1 kHz** kappt das
Band von 24 auf 22,05 kHz. Das ist derselbe Handel, den mint für jedes 44,1-Deliverable
(`cd`, `spotify`, …) längst macht; die Alternative wäre ein Hochtasten auf 88,2 kHz, das
keine Information hinzufügt und nur Rechenzeit kostet. Ich würde es dabei belassen.

**Bedingung B — der Deckel `dsd_rate / 32`.** Das ist der eigentliche inhaltliche Teil,
und er ist im Kern **ein Tiefpass, den wir gratis bekommen**.

Ein 1-Bit-Quantisierer hat eine gewaltige Gesamtrauschleistung; Noise Shaping verschiebt
sie nur. Bei DSD64 heißt das konkret: unterhalb ~20 kHz ist der Rauschteppich sehr tief,
ab ~20–25 kHz steigt er steil an und ist bei 100 kHz riesig. Alles oberhalb von grob
30 kHz ist in einer DSD64-Datei also **Rauschen, nicht Signal**. Passend dazu verlangt die
SACD-Spezifikation von Playern einen analogen Tiefpass um 50 kHz, gerade damit dieses
Ultraschallrauschen nicht in den Verstärker läuft.

Was passiert nun, wenn wir dem Modulator trotzdem Programminhalt bis 88 kHz vorlegen
(DSD64 aus 176,4 kHz Quelle)?

1. **Kein Gegenwert.** Der Inhalt landet in einem Bereich, den am Ausgang ohnehin das
   geformte Rauschen dominiert und den der Player wegfiltert.
2. **Er kostet Stabilitätsreserve.** Die Stabilität eines 1-Bit-Modulators hängt am
   *momentanen* Pegel am Quantisiererknoten, nicht am hörbaren Pegel. Ultraschallenergie
   erhöht Spitzen, ohne hörbar beizutragen — sie frisst Aussteuerungsreserve, die wir
   lieber für die Musik hätten.
3. **Sie provoziert Intermodulation.** Der 1-Bit-Quantisierer ist stark nichtlinear;
   viel Ultraschallenergie in einem Modulator hoher Ordnung ist ein bekannter Weg,
   IMD-Produkte zurück ins Audioband zu falten und im Grenzfall Instabilität auszulösen.

Man könnte das mit einem expliziten Tiefpass vor dem Modulator lösen. Der Deckel macht
dasselbe *ohne zusätzlichen Filter*: begrenzt man die Zwischenrate, übernimmt das
Interpolationsfilter — das sowieso läuft und sowieso auf die Eingangs-Nyquistfrequenz
bandbegrenzt — die Aufgabe umsonst. Das ist das ganze Argument für den Deckel.

Warum ausgerechnet `/32`? Damit liegt die Eingangs-Nyquistfrequenz bei `dsd_rate/64` —
bei DSD64 also 44,1 kHz, knapp unter dem 50-kHz-Player-Filter und deutlich oberhalb des
Bereichs, in dem noch Nutzsignal zu erwarten ist. Eine runde, verteidigbare Zahl, kein
Naturgesetz: `/64` (DSD64 bekäme dann 44,1 kHz Eingang, Band bis 22,05 kHz) wäre die
konservative Variante und käme dem am nächsten, was eine SACD real liefert.

#### Was dabei herauskommt

Zwischenrate (kHz) und Interpolationsverhältnis; **fett** = der Deckel greift:

| Quelle | DSD64 | DSD128 | DSD256 | DSD512 |
|---|---|---|---|---|
| 44,1 | 44,1 ×64 | 44,1 ×128 | 44,1 ×256 | 44,1 ×512 |
| 48 | 44,1 ×64 | 44,1 ×128 | 44,1 ×256 | 44,1 ×512 |
| 88,2 | 88,2 ×32 | 88,2 ×64 | 88,2 ×128 | 88,2 ×256 |
| 96 | 88,2 ×32 | 88,2 ×64 | 88,2 ×128 | 88,2 ×256 |
| 176,4 | **88,2 ×32** | 176,4 ×32 | 176,4 ×64 | 176,4 ×128 |
| 192 | **88,2 ×32** | 176,4 ×32 | 176,4 ×64 | 176,4 ×128 |
| 352,8 (DXD) | **88,2 ×32** | **176,4 ×32** | 352,8 ×32 | 352,8 ×64 |

Zwei Konsequenzen für die Implementierung:

- Das Verhältnis liegt zwischen **32 und 512**. Der Smoke-Test in Phase 1 muss also den
  ganzen Bereich abdecken, nicht nur bis 128 — ×512 (44,1 kHz → DSD512) ist ein legaler,
  wenn auch unsinniger Fall und darf nicht umkippen.
- Der Deckel greift genau dort, wo es wehtun würde: hochauflösende Quellen auf niedrige
  DSD-Raten. Ein DXD-Master (352,8 kHz) nach DSD64 wird also bewusst auf 88,2 kHz
  heruntergerechnet, bevor moduliert wird.

**Fluchtweg.** Die Regel ist nur der Default. Ein explizites `rate` im Target übersteuert
sie vollständig — wer für ein Archivformat bewusst Ultraschall in einer DSD64-Datei
behalten will, schreibt `rate = 176400` und bekommt das. Validiert wird nur, dass die
Rate die DSD-Rate ganzzahlig als Zweierpotenz teilt; eine Rate außerhalb der 44,1-Familie
ist bei DSD-Codecs ein harter Konfigfehler.

### 2.2 Pegel — der Punkt, der am ehesten übersehen wird

Bei DSD ist „0 dB" **nicht** die volle Modulation des Bitstreams. Die Scarlet-Book-/
SACD-Konvention:

- **0 dB SACD = 50 % Modulationsindex** ⇒ entspricht −6 dBFS in der PCM-Welt.
- **Maximum = 71,43 % (20/28-Regel)** = **+3,1 dB** über der Referenz.
- Darüber wird der 1-Bit-Modulator instabil (Overload, massive Verzerrung — auch im DAC
  bei der Wiedergabe).

Praktisch heißt das: der Modulator bekommt das PCM-Signal mit **Faktor 0,5** skaliert.
Das ist *nicht* eine Abschwächung um 6 dB im Ergebnis — DACs interpretieren 50 %
Modulation als ihren 0-dBFS-Bezug, die Wiedergabelautstärke bleibt also identisch.

Angenehme Folge für uns: die bestehende `ceiling_dbtp`-Semantik trägt unverändert. Ein
Target mit `ceiling_dbtp = -1.0` landet bei −1 dB relativ zur SACD-Referenz, also bei
~44,6 % Modulation — komfortabel unter dem 71,43-%-Limit. Neu ist nur, dass der Report
den **tatsächlich erreichten Spitzen-Modulationsindex** ausweisen und oberhalb von
71,43 % warnen sollte.

### 2.3 Modulator

Stand der Technik für DSD64: **Rückkopplungsmodulator 5. bis 7. Ordnung, 1 Bit**, mit
im Audioband optimierten NTF-Nullstellen und begrenztem Out-of-Band-Gain.

- NTF-Design: `synthesizeNTF(order=5, osr=64, opt=1, H_inf=1.5)` aus Schreiers Toolbox.
  **Lee-Kriterium**: `H_inf < 2` ⇒ mit 1-Bit-Quantisierer stabil. 1,5 ist der übliche,
  konservative Wert.
- Realisierung als CIFB/CRFB-Struktur (Integratorkette mit Resonator-Rückkopplungen für
  die Nullstellen), **nicht** als reine FIR-Fehlerrückkopplung: nur in der Zustandsform
  kann man die Integratorzustände einzeln begrenzen, und genau das ist die
  Stabilitätsversicherung.
- **Quantisierer-Dither**: kleines TPDF (Größenordnung 1e-3 … 1e-2) vor dem Komparator
  gegen Idle Tones und Grenzzyklen. mint reicht bereits einen `seed` durch die Pipeline
  ⇒ reproduzierbar.
- Koeffizienten kommen als Konstantentabelle in den Code (eine NTF pro DSD-Rate, damit
  die In-Band-Nullstellen jeweils in 0–20 kHz liegen), berechnet mit einem einmaligen
  Offline-Skript (Python `deltasigma`/scipy) — dokumentiert im Modulkopf, Präzedenzfall
  ist `dither/static_fir.rs`.
- Erwartungswert: theoretisch ~167 dB SNR (Ordnung 5, OSR 64, Vollaussteuerung),
  realistisch **110–125 dB im Band 20 Hz – 20 kHz** bei 50 % Modulation. Was es wirklich
  wird, sagt uns die Messung in Phase 0 — nicht die Formel.

---

## 3. Architektur

### 3.1 Wo DSD in die abgeleitete Kette passt

Die kanonische Reihenfolge aus `pipeline.rs` bleibt unangetastet. DSD ist ein
**zweiter „lossy-artiger" Zweig neben MP3**: es überspringt die Bit-Tiefen-/Dither-Stufe
komplett, weil der Modulator selbst der Quantisierer ist.

```
loudness (wenn lufs)
  → resample  (jetzt: Zielrate wird bei DSD abgeleitet statt frei gewählt)
  → ceiling enforcement
  → ┬─ PCM:  quantize genau einmal (bitdepth) → WAV/FLAC
    ├─ MP3:  encode direkt aus f64
    └─ DSD:  interpolate ×2^k → sigma-delta-modulate → pack → DSF/DFF   ← neu
```

Damit bleibt die zentrale Invariante („`ops/bitdepth` ist die *einzige* Stelle, die den
f64-Puffer auf ein Ganzzahlraster abbildet") formal erhalten — DSD bildet nicht auf ein
Ganzzahlraster ab, sondern auf einen 1-Bit-Strom, und tut das genau einmal. Der Modulator
muss zwingend den **ungequantisierten** f64-Puffer sehen; ein vorgeschalteter
`bitdepth::apply` wäre ein echter Fehler und gehört als Assertion abgesichert.

### 3.2 Das Speicherproblem und die Antwort darauf

`AudioBuffer` ist planar f64, ganze Datei im RAM. Für PCM ist das eine bewusste, gute
Entscheidung (offline-Analysen, Nulllatenz). Für DSD kippt sie:

| | f64 im RAM | gepackte Bytes auf Platte |
|---|---|---|
| DSD64, 5 min, Stereo | 13,5 GB | 212 MB |
| DSD256, 5 min, Stereo | 54 GB | 847 MB |

Antwort: **die DSD-Stufe materialisiert nie einen f64-Puffer bei DSD-Rate.** Sie ist eine
Blockschleife:

```
für jeden Block von N PCM-Frames:
    interpolate  → N·2^k f64-Samples (Scratch, wiederverwendet)
    modulate     → N·2^k Bits
    pack         → Bytes
    write        → direkt in die Datei
Header am Ende zurückpatchen (Dateigröße, Sample-Count) via seek
```

Blockgröße fällt aus dem DSF-Format: ein DSF-Block ist **4096 Byte pro Kanal** =
32 768 DSD-Samples. Bei ×16 sind das genau 2048 PCM-Frames pro Block. Spitzen-RAM der
Stufe: ~0,5 MB Scratch pro Kanal. Konstant, unabhängig von Datei- und DSD-Rate.

### 3.3 Interpolation — libsoxr im Streaming-Modus

**Entschieden: komplett libsoxr.** Ursprünglich hatte ich eine eigene Halfband-Kaskade
empfohlen, im Wesentlichen mit dem Argument, dass soxr- und rubato-Build dann
bit-identische DSD-Dateien liefern. Da rubato faktisch obsolet ist, ist dieses Argument
gegenstandslos — und gegen selbstgebaute Filter spricht schlicht, dass libsoxr die
mehrstufige Zerlegung (Halfband-Kaskade + Polyphase/DFT) für eine Zweierpotenz ohnehin
optimal selbst herleitet und referenzqualitativ ist.

Zu bauen ist deshalb kein Filter, sondern ein **Streaming-Wrapper**. Der bestehende
`resample::apply` ist One-Shot und würde hier den 13,5-GB-Puffer erzeugen; die
Soxr-Instanz kann aber inkrementell gefüttert werden (`process(Some(block))` wiederholt,
am Ende `process(None)` zum Ausspülen — dieselbe API, die `apply_soxr` heute schon für
den Flush-Tail benutzt). Der Wrapper hält also eine Soxr-Instanz über die gesamte
Blockschleife am Leben:

```rust
// Skizze — eine Instanz pro Datei, nicht pro Block.
let mut src = SoxrStream::new(pcm_rate, dsd_rate, channels, Vhq)?;
for block in pcm_blocks {
    let up = src.process(block)?;   // ~block.len() * ratio Frames, Scratch wiederverwendet
    modulate_and_write(up)?;
}
let tail = src.flush()?;
```

Punkte, die der Wrapper korrekt treffen muss und die im PCM-Pfad heute pauschal gelöst
sind:

- **Latenz**: `soxr.delay()` liefert die Priming-Frames. Im One-Shot-Pfad werden sie
  hinten weggeschnitten; im Streaming-Pfad müssen sie *am Anfang des ersten Blocks*
  verworfen werden, sonst ist der DSD-Strom gegen die Quelle versetzt.
- **Exakte Länge**: `out_frames == round(in_frames * ratio)` muss über die Blockgrenzen
  hinweg in Summe gelten, nicht pro Block — soxr gibt pro Aufruf schwankende Mengen
  zurück. Umgesetzt als laufender Deckel: nie mehr ausgeben, als der Vertrag für die
  bisher verfütterten Eingangsframes hergibt, Überschuss intern parken, im `finish`
  auffüllen. *Abweichung vom Entwurf:* ausgegeben werden **variabel lange** Blöcke, keine
  festen. Feste Blöcke bringen nichts, weil der Bit-Packer ohnehin einen eigenen
  Akkumulator auf DSF-Blockgrenze braucht — die Schleife wird stattdessen in DSD-Samples
  getaktet (32 768 = ein DSF-Block), woraus sich die PCM-Blockgröße ergibt.
- **Thread-Sicherheit**: der `INIT_LOCK` aus `apply_soxr` gilt weiter — `Soxr::create`
  rennt unter rayon sonst in die bekannte Tabellen-Race. Der Wrapper übernimmt das Muster
  unverändert.
#### Messergebnis (Phase 1 abgeschlossen)

`tests/soxr_dsd_probe.rs`, Apple Silicon, `--release`, stereo, ein Kern, `vhq`, jeweils
4 Mio. Ausgangsframes, Bestwert aus 5 Läufen. `PCM-Block` ist die Eingangsblockgröße, die
sich ergibt, wenn man die Schleife in **DSD-Samples** taktet (32 768 = ein DSF-Block):

| PCM kHz | DSD MHz | Ratio | PCM-Block | Setup ms | 1-Shot ms | Stream ms | xRT | Resident |
|---|---|---|---|---|---|---|---|---|
| 44,1 | 2,8224 | ×64 | 512 | 0,9 | 55,1 | 46,4 | 30,6 | 512 kB |
| 88,2 | 2,8224 | ×32 | 1024 | 0,4 | 53,0 | 45,2 | 31,4 | 512 kB |
| 44,1 | 5,6448 | ×128 | 256 | 1,4 | 50,1 | 45,5 | 15,6 | 512 kB |
| 88,2 | 5,6448 | ×64 | 512 | 0,7 | 50,4 | 46,7 | 15,2 | 512 kB |
| 176,4 | 5,6448 | ×32 | 1024 | 0,4 | 52,6 | 45,2 | 15,7 | 512 kB |
| 88,2 | 11,2896 | ×128 | 256 | 1,4 | 46,7 | 45,1 | 7,9 | 512 kB |
| 176,4 | 11,2896 | ×64 | 512 | 0,7 | 47,5 | 46,4 | 7,6 | 512 kB |
| 352,8 | 11,2896 | ×32 | 1024 | 0,4 | 47,3 | 45,8 | 7,7 | 512 kB |
| 44,1 | 22,5792 | **×512** | 64 | 6,5 | 53,3 | 48,8 | 3,6 | 512 kB |
| 176,4 | 22,5792 | ×128 | 256 | 1,4 | 52,0 | 45,7 | 3,9 | 512 kB |
| 352,8 | 22,5792 | ×64 | 512 | 0,7 | 53,1 | 46,5 | 3,8 | 512 kB |

`xRT` = Sekunden DSD-Audio pro Sekunde Rechenzeit im Streaming-Pfad. Fünf Befunde:

1. **libsoxr trägt den ganzen Bereich**, bis hinauf zu ×512. Kein Fehler, kein
   Qualitätsverlust — der Peak liegt in jedem Fall exakt auf den eingespeisten 0,5.
2. **Die Rechenzeit hängt nicht vom Verhältnis ab, sondern nur an der Zahl der
   Ausgangssamples**: ~46 ms für 4 Mio. Frames, von ×32 bis ×512, ohne erkennbaren Trend.
   Der Grund ist die Zweierpotenz: soxr zerlegt in eine Halfband-Kaskade, deren Stufen
   bis auf die letzte auf niedrigeren Raten laufen, also bestimmt die letzte ×2-Stufe die
   Grenzkosten pro Ausgangssample.
3. **Streaming ist nicht langsamer als One-Shot, sondern leicht schneller** (45–49 ms
   gegenüber 47–55 ms). Der Vorbehalt „Blockaufrufe kosten Overhead" war zu pessimistisch:
   der Overhead ist kleiner als das, was der One-Shot-Pfad allein für das Allozieren und
   Deinterleaven seiner Riesenpuffer bezahlt.
4. **Konstant 512 kB Working Set**, unabhängig von Rate, Ratio und Dateilänge. Derselbe
   Fall kostet den One-Shot-Pfad 122 MB — und eine echte fünfminütige DSD64-Datei 13,5 GB.
   Das ist der ganze Grund für den Wrapper.
5. **Setup 0,4–6,5 ms**, einmal pro Datei. Irrelevant.

Und das Wichtigste, aus `tests/dsd_interpolate.rs`: der Streaming-Pfad ist
**bit-identisch** zum One-Shot-Pfad — `max |diff| = 0.0`, und zwar unabhängig von der
Blockgröße (geprüft mit 1, 127, 512, 1000, 4096). Chunking verändert das Ergebnis nicht.

Hochgerechnet auf einen 5-Minuten-Stereotitel, nur diese Stufe, ein Kern: DSD64 ~10 s,
DSD128 ~19 s, DSD256 ~39 s, DSD512 ~79 s.

Das rubato-Backend ist inzwischen entfernt (eigener Commit vor der DSD-Arbeit): libsoxr
ist unbedingte Abhängigkeit, das `soxr`-Feature und der `--no-default-features`-Pfad sind
weg. Für DSD heißt das: **kein Feature-Gating nötig**, die neuen Module hängen an keinem
`#[cfg]`, und es gibt nur noch einen CI-Lauf, der grün bleiben muss.

### 3.4 Container

**DSF** (Sony, Little-Endian) — verifiziertes Layout:

```
DSD-Chunk (28 B):  "DSD "  size=28(u64)  totalFileSize(u64)  metadataPtr(u64, 0=keine)
fmt-Chunk (52 B):  "fmt "  size=52(u64)  version=1(u32)  formatID=0(u32)
                   channelType(u32)  channelNum(u32)  samplingFreq(u32)
                   bitsPerSample=1(u32)  sampleCount(u64, pro Kanal)
                   blockSizePerChannel=4096(u32)  reserved=0(u32)
data-Chunk:        "data"  size=12+n(u64)  <Blöcke>
```

Sample-Daten: **planar in 4096-Byte-Blöcken pro Kanal**, rundlaufend
(ch0-Block, ch1-Block, ch0-Block, …), letzter Block mit 0x00 aufgefüllt. Bei
`bitsPerSample = 1` ist die Bitreihenfolge **LSB first**. `metadataPtr` darf 0 sein (kein
ID3v2) — optionale Kür.

**DFF/DSDIFF** (Philips, Big-Endian, `FRM8`-Form): interleaved statt planar, **MSB
first**, Chunks `FVER`/`PROP`(`FS`,`CHNL`,`CMPR`,`ABSS`,`LSCO`)/`DSD `. **Entschieden:
vorerst nicht.** DSF ist das, was Player und Downloadshops erwarten, und es trägt
ID3-Tags. DFF wären rund 100 Zeilen mehr, verdoppeln aber die Container-Testfläche
(Big-Endian, MSB-first, interleaved) — sinnvoll erst, wenn jemand daraus tatsächlich eine
SACD authoren will. Das Bit-Packing wird trotzdem so gebaut, dass die Bitreihenfolge ein
Parameter ist, damit DFF später ein Nachmittag bleibt und kein Umbau. DST-Kompression
bleibt dauerhaft außen vor.

### 3.5 Config-Oberfläche

Im Geist von mint: *deklariere das Deliverable, die Schritte werden abgeleitet.*

```toml
[target.sacd]
codec = "dsf"       # neu: dsf | dff
dsd   = "dsd64"     # neu: dsd64 | dsd128 | dsd256 | dsd512
lufs  = -16.0       # unverändert
ceiling_dbtp = -1.0 # unverändert, jetzt relativ zur SACD-0-dB-Referenz
# rate: weggelassen ⇒ abgeleitet (nächstniedrigere 44,1-Familie, gedeckelt)
# format: bei DSD bedeutungslos ⇒ wird abgelehnt statt still ignoriert
```

Neue Felder: `dsd` (Enum). Neue `OutputCodec`-Varianten: `Dsf`, `Dff` (`ext()` →
`"dsf"`/`"dff"`). Neuer Naming-Platzhalter `{dsd}` → `"dsd64"`; `{rate}` liefert bei
DSD-Targets die DSD-Rate in Hz.

Validierung (`validate_fields`):
- `dsd` gesetzt ⇔ Codec ist `dsf`/`dff` (beides einzeln gesetzt = Fehler, kein stilles
  Ignorieren — passt zu `deny_unknown_fields`)
- `format` bei DSD-Codecs: harter Fehler (anders als MP3, wo es historisch toleriert wird
  — bei einem neuen Feld können wir es gleich streng machen)
- explizites `rate` muss die DSD-Rate ganzzahlig und als Zweierpotenz teilen
- `bwf = true` bei DSD → Fehler (analog zur bestehenden WAV-only-Regel)
- DSD512 nur mit Hinweis (Formatspezifikation listet es, Player-Unterstützung ist dünn)

Preset-Vorschlag: `"sacd"` = DSD64/DSF, kein `lufs` (wie `cd`), `ceiling_dbtp = -1.0`.

### 3.6 Reporting

`ProcessingReport` bekommt ein `Option<DsdReport>`:
`{ dsd_rate_hz, family, intermediate_pcm_rate, interpolation_factor, modulator_order,
max_modulation_percent, overload_events }`. Dazu die DTO-Spiegelung in `json_report.rs`
(die Schicht existiert genau dafür) und eine Zeile in der Konsolenzusammenfassung.
`describe()` muss die neuen Schritte spiegeln — das ist laut `CLAUDE.md` der
Standard-Driftpunkt und gehört in dieselbe Änderung.

### 3.7 Neue Dateien

```
src/ops/dsd/mod.rs          Orchestrierung der Blockschleife, DsdRate-Enum, Ratenableitung
src/ops/dsd/interpolate.rs  Streaming-Wrapper um libsoxr (Latenz, Blockpufferung, INIT_LOCK)
src/ops/dsd/modulator.rs    1-Bit-Sigma-Delta, CIFB, NTF-Tabelle pro Rate, Dither, Watchdog
src/ops/dsd/pack.rs         Bit-Packing, Bitreihenfolge als Parameter (LSB-first für DSF)
src/ops/dsd/decimate.rs     DSD → PCM, #[cfg(test)] — reines Messwerkzeug
src/audio/io_write_dsd.rs   DSF-Streaming-Writer (Header-Patch via seek)
tests/dsd_verification.rs   Messharness (Kopfzahlen wie bei dither_/limiter_verification)
tests/dsd_format.rs         Byte-Layout, Padding, Bitreihenfolge, Fremd-Parser-Gegenprobe
```

---

## 4. Verifikation

Ohne Messung ist ein Modulator nur eine Behauptung — dieselbe Haltung, die
`tests/dither_verification.rs` im Repo bereits durchsetzt.

Dafür brauchen wir einen **Dezimator** (DSD → PCM) als reines Testwerkzeug: Kaskade
von ×2-Halfband-Dezimationsstufen, ~100 Zeilen, gespiegelt zum Interpolator. Damit messen
wir:

1. **In-Band-SNR / THD+N**: 1 kHz Sinus bei −6 dBFS → hin und zurück → Rauschen und
   Klirr im Band 20 Hz – 20 kHz. Zielkorridor: ≥ 110 dB.
2. **Rauschform**: Welch-PSD des Fehlers — flach im Band, steiler Anstieg ab ~25 kHz.
   Bewusst als *Verhältnis* zu einem 2.-Ordnung-Referenzmodulator, damit der Test
   unabhängig von absoluten Kalibrierungen ist (das Muster aus dem Dither-Harness).
3. **Durchlassbandgang**: ±0,1 dB bis 20 kHz (prüft die Interpolationskaskade).
4. **DC**: Mittelwert des Bitstreams bei Stille = 50 % ± ε, kein Offset.
5. **Idle Tones**: digitale Stille und −90 dBFS-Sinus → keine diskreten Spitzen über dem
   Rauschteppich (prüft den Quantisierer-Dither).
6. **Stabilität**: 0 dBFS Rechteck und ein absichtlich übersteuertes Signal → keine
   Divergenz, Zustände begrenzt, Overload-Zähler im Report sichtbar.
7. **Modulationsindex**: bei `ceiling_dbtp = -1` bleibt das Maximum unter 71,43 %.
8. **Container**: Golden-Bytes für die Header, korrektes Zero-Padding im letzten Block,
   Bitreihenfolge; Gegenprobe mit dem fremden `dsf`-Crate als Dev-Dependency, optional
   ein `#[ignore]`-Test, der die Datei durch ffmpeg dekodiert.
9. **Determinismus**: gleicher Seed ⇒ bytegleiche Ausgabe (mint läuft unter rayon; der
   Modulator hat Zustand — ein Test, der genau das absichert).

Die gemessenen Zahlen kommen wie bei den anderen beiden Harnesses in den Dateikopf, mit
den Schranken zwischen bestehendem und durchfallendem Bereich.

---

## 5. Risiken & offene Punkte

| Risiko | Bewertung | Umgang |
|---|---|---|
| Modulatorqualität unklar bis gemessen | **hoch** — das ist *das* Risiko | Phase 2 früh, Messharness gehört zur Phase, nicht danach; die dB-Tabelle ist das Abnahmekriterium |
| Stabilität bei heißem Material | mittel | Zustandsbegrenzung + Overload-Zähler + Warnung; optional deterministischer Retry mit −1 dB |
| ~~libsoxr bei Ratio bis 512 (`vhq`)~~ | **erledigt** | Gemessen (§ 3.3): Kosten hängen nur an den Ausgangssamples, nicht am Verhältnis. Kein Staffeln nötig |
| Streaming-Latenz/Längenvertrag | mittel | eigener Test: Streaming-Ergebnis == One-Shot-Ergebnis für eine kurze Datei bei kleinem Ratio |
| Rechenzeit | gering | DSD64 ~Sekunden, DSD256 ~1–2 min/Titel/Kern, DSD512 entsprechend mehr; parallel über Kanäle und Jobs (rayon existiert) |
| Dateigrößen (DSD512: ~1,7 GB / 5 min) | gering, aber sichtbar | Größe vorab in `--dry-run` ausweisen |
| f64-Puffer-Invariante | gering | Streaming ist auf die DSD-Stufe begrenzt; PCM-Pfad bleibt unberührt |
| DSD512-Kompatibilität | gering | erlauben, aber im Dry-Run als „eingeschränkt unterstützt" markieren |

Bewusst **nicht** im Scope: DFF/DSDIFF (später, s. § 3.4), DST-Kompression,
SACD-Image-Authoring, DSD-**Eingabe** (Lesen von DSF/DFF), DoP, 48-kHz-basiertes DSD
(3,072 MHz) und Multibit-„DSD-wide".

Wenn DSD-Eingabe später gewünscht ist, ist der Dezimator aus Phase 0 bereits da — das
wäre dann ein kleiner Zusatzschritt, kein zweites Projekt.

---

## 6. Plan

Reihenfolge nach Risiko, nicht nach Sichtbarkeit: das Unbekannte (soxr bei großen Ratios,
Modulatorqualität) kommt zuerst. Nach Phase 3 gibt es die erste abspielbare `.dsf`-Datei,
nach Phase 4 ist das Feature regulär über die Config nutzbar.

### Phase 1 — Streaming-Interpolation auf libsoxr · 1–1,5 Tage · Risiko: mittel
1. **Zuerst der Smoke-Test**: Durchsatz und Speicher von libsoxr `vhq` bei den Ratios
   32/64/128/256/512 (bis 22 579 200 Hz Ausgangsrate; ×512 entsteht bei 44,1 kHz → DSD512). Entscheidet, ob eine Soxr-Instanz reicht
   oder manuell gestaffelt wird.
2. `SoxrStream`-Wrapper: Instanz über die ganze Datei, feste Ausgabeblöcke, Latenz vorne
   verwerfen, Flush am Ende, `INIT_LOCK` übernehmen.
3. Tests: Streaming-Ergebnis == One-Shot-Ergebnis (kurze Datei, kleines Ratio),
   Gesamtlängenvertrag, Durchlassbandgang, Nebenläufigkeit unter rayon.

### Phase 2 — Modulator + Dezimator + Messharness · 3–4 Tage · **Risiko: hoch**
1. NTF-Koeffizienten offline berechnen (`synthesizeNTF`, Ordnung 5, `H_inf` 1,5, opt.
   Nullstellen) — **eine NTF pro DSD-Rate**, damit die In-Band-Nullstellen jeweils in
   0–20 kHz landen. Ergebnis als Konstantentabelle mit dokumentierter Herkunft im
   Modulkopf.
2. CIFB-Modulator mit Zustandsbegrenzung, Quantisierer-Dither (aus dem vorhandenen
   `seed`), Overload-Zähler.
3. Dezimator als `#[cfg(test)]`-Messwerkzeug.
4. `tests/dsd_verification.rs`: Messungen 1–7 aus § 4, Zahlen in den Dateikopf.
   **Die dB-Tabelle ist das Abnahmekriterium dieser Phase** — nicht „kompiliert und
   klingt irgendwie".

### Phase 3 — DSF-Writer (streaming) · 1 Tag · Risiko: gering
Header-Patch via seek, 4096-Byte-Blöcke pro Kanal, LSB-first-Packing (Bitreihenfolge als
Parameter), Zero-Padding im letzten Block, `sampleCount`/`totalFileSize` nachtragen.
`tests/dsd_format.rs`: Golden-Bytes, Padding, Bitreihenfolge, Gegenprobe mit dem fremden
`dsf`-Crate, optional `#[ignore]`-Test gegen ffmpeg. **Ab hier existiert eine hörbare
Datei** (noch aus einem Test heraus erzeugt).

### Phase 4 — Config + Pipeline-Integration · 1,5–2 Tage · Risiko: gering, aber breit
Entlang der Codec-Checkliste aus `CLAUDE.md`: `OutputCodec::Dsf` (+`ext()`), `dsd`-Feld
durch `RawTarget` → `overlay()` → `builtin_fallback()` → `ResolvedTarget` →
`resolve_target()` → `validate_fields()` → `describe()`; Ratenableitung (§ 2.1);
Blockschleife in `pipeline.rs`; `ProcessingReport` + `json_report.rs` (inkl.
Modulationsindex und Overload-Zähler); `{dsd}`-Platzhalter und Kollisionsprüfung.

### Phase 5 — Alle Raten, Preset, Doku · 1–1,5 Tage · Risiko: gering
DSD128/256/512 scharfschalten (NTF pro Rate messen, nicht nur einschalten),
`sacd`-Preset, Modulationsindex-Warnung, geschätzte Dateigröße im `--dry-run`, optional
ID3v2 im DSF. README (Defaults-Tabelle, Abschnitt „Audio quality → DSD"),
`examples/mint.toml` (ist per `include_str!` das Bootstrap-Template — Änderung dort
ändert die Datei, die `mint` ohne Config anlegt), `CLAUDE.md` (neues Subsystem, neue
Invariante „die DSD-Stufe streamt, der PCM-Pfad nicht").

**Summe: 7–9 Tage.**

---

## 7. Offene Entscheidungen

Nur noch eine, und sie ist billig zu ändern:

**Zwischenraten-Regel.** Vorgeschlagen (§ 2.1): *nächstniedrigere Rate der 44,1-Familie
zur Quellrate, gedeckelt auf DSD-Rate / 32*; explizites `rate` übersteuert. Alternativen:

- **immer die höchstmögliche 44,1-Rate** — mehr Bandbreite in den Modulator, aber bei
  DSD64 landet Programminhalt bis 88 kHz in einem Modulator, dessen Rauschen ab ~25 kHz
  steil ansteigt: kostet Stabilitätsreserve ohne hörbaren Gegenwert;
- **keine Ableitung, `rate` wird bei DSD-Targets Pflicht** — explizit, aber gegen mints
  Grundprinzip („deklariere das Deliverable, die Schritte werden abgeleitet");
- **soxr direkt aus der 48er-Familie** (192 kHz → 2 822 400 Hz, Ratio 14,7) — technisch
  möglich, seit die Interpolation ohnehin soxr ist, aber teurer und gegen die Konvention
  der DSD-Welt. Würde ich nicht tun.

Bis auf Widerspruch nehme ich den Vorschlag.

Der zweite Punkt von vorhin — das rubato-Backend — ist erledigt und aus diesem Dokument
als Vorbehalt entfernt.

---

## Quellen

- [DSF File Format Specification, Sony](https://dsd-guide.com/sites/default/files/white-papers/DSFFileFormatSpec_E.pdf)
- [DSDIFF 1.5 Specification, Philips](https://www.sonicstudio.com/pdf/dsd/DSDIFF_1.5_Spec.pdf)
- [DSDIFF bei der Library of Congress](https://www.loc.gov/preservation/digital/formats/fdd/fdd000245.shtml)
- [Merging: SACD Production Guide (Pegel/Modulationsindex)](https://www.merging.com/uploads/assets/Merging_pdfs/Pyramix_6_1/Mastering/SACD%20Production%20Guide.pdf)
- [Archimago: Notes on DAC DSD (1-bit PDM) measurements](http://archimago.blogspot.com/2022/06/notes-on-dac-dsd-1-bit-pdm-measurements.html)
- [Schreier, Delta-Sigma Toolbox (synthesizeNTF, Lee-Kriterium)](https://www.eecis.udel.edu/~vsaxena/courses/ece697A/docs/DSToolbox.pdf)
- [ECE1371: Higher-Order ΔΣ Modulators and the ΔΣ Toolbox](https://www.eecg.utoronto.ca/~johns/ece1371/slides/Modn+DSToolbox-2.pdf)
- [`dsf` Crate](https://crates.io/crates/dsf) · [`dsd-reader` Crate](https://lib.rs/crates/dsd-reader) · [`dsd2dxd`](https://github.com/clone206/dsd2dxd) · [`dsenc`](https://github.com/dcherednik/dsenc)

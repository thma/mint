# DSD-Ausgabe für mint — Design & Plan

> **Dieses Dokument ist der Übergabepunkt.** Es ist so geschrieben, dass man die Arbeit
> ohne jede Vorgeschichte aufnehmen kann: Entscheidungen, Herleitungen, Messwerte und
> Phasenstand stehen hier, nicht in irgendeinem Gesprächsverlauf.

## Stand

**Phase 1, 2 und 3 abgeschlossen, Phase 4 ist als Nächstes dran.** Alles unten ist umgesetzt
und grün (177 Tests, 1 ignorierte Messprobe). **Seit Phase 3 gibt es abspielbare
`.dsf`-Dateien** — erzeugt aus `tests/dsd_format.rs`, noch nicht über die Config.

Was existiert:

| Datei | Rolle |
|---|---|
| `src/ops/dsd/interpolate.rs` | `SoxrStream` — eine libsoxr-Instanz pro Datei, blockweise gefüttert; bit-identisch zum One-Shot-Pfad |
| `src/ops/dsd/modulator.rs` | 1-Bit-Sigma-Delta, CIFB **Ordnung 7**, NTF-Tabelle pro Rate, Komparator-Dither, Zustandsbegrenzung, Overload-Zähler |
| `src/ops/dsd/decimate.rs` | DSD → PCM, Halfband-Kaskade — reines Messwerkzeug, aus keinem Ausgabepfad erreichbar |
| `src/ops/dsd/pack.rs` | Bit-Packing, Bitreihenfolge als Parameter; `DSD_SILENCE` (0x69) |
| `src/ops/dsd/mod.rs` | Modulkopf (Streaming-Invariante), `DsdRate` |
| `src/audio/io_write_dsd.rs` | `DsfWriter` — Streaming-Container, drei Headerfelder per seek nachgetragen |
| `src/ops/resample.rs` | `create_soxr` — **die** Konstruktionsstelle für Soxr-Instanzen, mit dem prozessweiten Init-Lock |
| `tools/design_ntf.py` | Offline-NTF-Entwurf + CIFB-Realisierung; erzeugt die Konstantentabelle, wird zur Bauzeit nicht gebraucht |
| `tests/dsd_interpolate.rs` | 8 Tests um den Wrapper |
| `tests/dsd_verification.rs` | Messharness, 10 Tests; Ergebnisse in § 4.1 |
| `tests/dsd_format.rs` | Container: Golden-Header, Bitreihenfolge, Blockung, Padding, Fremdparser, Ende-zu-Ende; 17 Tests, Ergebnisse in § 4.2 |
| `tests/soxr_dsd_probe.rs` | Durchsatz-/Speicherprobe, `#[ignore]`; Messwerte in § 3.3 |

Was als Nächstes zu bauen ist: **Phase 4** (§ 6) — Config-Oberfläche und
Pipeline-Integration. Der `DsfWriter` und der Modulator sind dafür fertig; was fehlt, ist
die Blockschleife in `src/ops/dsd/mod.rs`, die beide verbindet, plus die Codec-Checkliste
aus `CLAUDE.md`. **Achtung, nicht offensichtlich:** `describe()` in `config.rs` und der
`match target.codec` in `pipeline.rs` haben beide einen *Catch-all*-Arm für „nicht MP3" —
neue `OutputCodec`-Varianten erzeugen dort also **keinen Compilerfehler**, sondern schicken
DSD still durch den PCM-Quantisierer und den WAV-Writer. Das ist die gefährlichste Stelle
der ganzen Phase 4.

Arbeitsweise in diesem Repo: direkt auf `main`, Commits macht Thomas selbst.

### Getroffene Entscheidungen

| Frage | Entscheidung |
|---|---|
| Vorgehen | Durchgehend nach Plan, kein vorgeschalteter Go/No-Go-Spike |
| Scope erster Wurf | **Alle DSD-Raten (64/128/256/512), Container nur DSF**; DFF vorerst raus |
| Interpolation | **Komplett auf libsoxr** — rubato gilt als obsolet, Bit-Parität zwischen den Build-Varianten ist damit kein Designziel mehr |
| rubato-Backend | **Entfernt** (Commit vor der DSD-Arbeit): kein `soxr`-Feature, kein `--no-default-features`-Pfad, kein zweiter CI-Job mehr |
| Zwischenraten-Regel | `min(nächste_44,1-Rate, dsd_rate/32)` — hergeleitet in § 2.1, angenommen |
| Modulator | **CIFB Ordnung 7, `H_inf` 1,30**, eine NTF pro Rate — gemessen besser *und* stabiler als die ursprünglich geplante Ordnung 5 / 1,5 (§ 2.3.1) |
| Komparator-Dither | **0,2** statt der geplanten 1e-3 … 1e-2; der kleine Bereich tut messbar nichts, weil die Schleife ihn genau im Band wegformt (§ 2.3.1) |
| Betriebsbereich | **0 … 50 % Modulation** (was die Ceiling-Stufe zulässt), nicht 0 … 71,43 %. Darüber kontrollierte Übersteuerung statt Divergenz |
| Füllmuster im letzten Block | **0x69 statt der spezifizierten 0x00**, und `sampleCount` **zählt die Polsterung mit** — hergeleitet und gemessen in § 3.4.1 |
| Kanaltyp | Aus der Kanalzahl abgeleitet (1/2/3/quad/5/5.1); mint kennt kein Layout, ein `channels`-Feld gäbe es später |
| ffmpeg-Gegenprobe | **Entfällt** — auf der Entwicklungsmaschine ist kein ffmpeg installiert. Ersatz: das fremde `dsf`-Crate als Dev-Dependency plus ein im Test unabhängig geschriebener Entpacker |

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

> **Umgesetzt in Phase 2.** Drei Parameter dieses Abschnitts haben die Messung nicht
> überlebt; was tatsächlich gebaut wurde, steht in § 2.3.1. Der ursprüngliche Entwurf
> bleibt hier stehen, weil die Begründungen weiter tragen — nur die Zahlen nicht.

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

#### 2.3.1 Was daraus geworden ist

Drei Abweichungen, alle durch Messung erzwungen. Details und Messtabellen stehen im Kopf
von `src/ops/dsd/modulator.rs`; hier die Kurzfassung mit Begründung.

**Ordnung 7 statt 5, `H_inf` 1,30 statt 1,50.** Ordnung 5 mit `H_inf = 1,5` ist gemessen
nur bis ~57 % Modulation stabil — sie divergiert also *unterhalb* der 71,43 %, die das
Format erlaubt, und 0 dBFS (= 50 %) läge knapp 1 dB vor der Kante. Die Alternativen
gegeneinander (DSD64, `tools/design_ntf.py --sweep`):

| Ordnung | `H_inf` | max. stabile Amplitude | In-Band-SNR |
|---|---|---|---|
| 5 | 1,50 | 0,57 | 119,6 dB |
| 5 | 1,35 | 0,75 | 110,2 dB |
| **7** | **1,30** | **0,75** | **121,1 dB** |
| 7 | 1,45 | 0,58 | 137,7 dB |

Ordnung 7 bei 1,30 dominiert die beste Ordnung-5-Variante auf beiden Achsen gleichzeitig:
gleiche Stabilität, 11 dB mehr Unterdrückung. Lee (`H_inf < 2`) bleibt mit Abstand
erfüllt.

**NTF-Entwurf über `synthesizeChebyshevNTF` statt `synthesizeNTF`.** Ein
Chebyshev-II-Hochpass, dessen *Sperrband* das Audioband ist, liefert bei ungerader Ordnung
genau eine Nullstelle bei DC plus (n−1)/2 konjugierte Paare — also exakt die CIFB-Form
(ein reiner Integrator + Resonatoren). scipy normiert das Durchlassband auf 1, dadurch ist
`sup|NTF| = 1/k` monoton in der Sperrbanddämpfung und eine Bisektion trifft `H_inf`
*exakt*. Das braucht nur `scipy.signal.cheby2` statt der iterativen Polsuche von
`synthesizeNTF` — weniger Code, kein Konvergenzrisiko, gleiches Ergebnis.

Die CIFB-Koeffizienten fallen danach aus **einem linearen Gleichungssystem**: mit
`NTF(z) = N(z) / (N(z) + C·adj(zI−A)·B)` ist `N` allein durch die Resonatorgains `g`
festgelegt und der Korrekturterm linear in `a`. Faddeev-LeVerrier liefert `adj`, ein
7×7-Solve den Rest. Exakt, ohne Iteration — der Realisierungsfehler gegenüber dem
entworfenen zpk liegt bei 1e-8.

**Komparator-Dither 0,2 statt 1e-3 … 1e-2.** Der vorgeschlagene Bereich tut gemessen
*nichts*: 2e-3 lässt den Idle Tone eines 1e-3-DC-Offsets unverändert bei 34,5 dB über dem
In-Band-Median. Der Grund ist strukturell und war im Entwurf übersehen — Dither am
Komparator wird von der Schleife genauso geformt wie der Quantisierungsfehler, die tiefe
In-Band-Kerbe nimmt ihm also genau dort die Wirkung, wo man sie bräuchte. Damit er die
Entscheidung überhaupt stört, muss er ein realer Bruchteil des Quantisierereingangs sein
(der bei `σ ≈ 0,3` läuft). Bei 0,2 fällt der Idle Tone auf 16,1 dB — rund 2 dB über dem
Wert, den ein tonfreies Periodogramm ohnehin zeigt — und kostet ~1,2 dB In-Band-Rauschen.

Nebenwirkung, die man kennen muss: der größere Dither kostet Stabilitätsreserve. Die max.
stabile Amplitude sinkt von 0,753 auf 0,691 (Sinus). Der **Betriebsbereich ist 0 … 0,5** —
PCM ist auf ±1 begrenzt und die Ceiling-Stufe deckelt den True Peak auf ≤ 0 dBFS, mehr als
50 % Modulation kann die Pipeline gar nicht liefern —, und dort bleiben mindestens 2,1 dB
Reserve. Die 71,43 % des Formats sind eine Aussage darüber, was ein DSD-Strom *darstellen*
kann, nicht ein Pegel, den mint erreicht; dafür bräuchte es PCM bei +3,1 dBFS. Füttert man
ihn trotzdem, übersteuert die Schleife kontrolliert (Zustandsbegrenzung greift, Zähler
läuft mit, volle SNR kehrt zurück), statt zu divergieren.

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

**DSF** (Sony, Little-Endian) — Layout, in Phase 3 vierfach unabhängig gegengeprüft (das
PDF koordinatenaufgelöst neu extrahiert, ffmpeg `dsfdec.c`, WavPacks gepackte C-Structs
kompiliert und `offsetof` gedumpt, plus ein Round-Trip durch drei nachgebaute Parser):

```
Offset  Feld
     0  "DSD "                                    (4 B, mit Leerzeichen, kein NUL)
     4  chunkSize = 28                            (u64)
    12  totalFileSize                             (u64)  ← per seek nachgetragen
    20  metadataPtr = 0                           (u64)
    28  "fmt "
    32  chunkSize = 52                            (u64)
    40  formatVersion = 1                         (u32)
    44  formatID = 0 (DSD raw)                    (u32)
    48  channelType                               (u32)
    52  channelNum                                (u32)
    56  samplingFrequency                         (u32)
    60  bitsPerSample = 1                         (u32)
    64  sampleCount, pro Kanal                    (u64)  ← per seek nachgetragen
    72  blockSizePerChannel = 4096                (u32)
    76  reserved = 0                              (u32)
    80  "data"
    84  chunkSize = 12 + n                        (u64)  ← per seek nachgetragen
    92  Nutzdaten, n Byte
```

Beide `chunkSize`-Felder zählen den eigenen Chunk-Kopf mit — daher `12 + n` und nicht `n`.
Das ist die mit Abstand häufigste Art, dieses Format still falsch zu schreiben.

Sample-Daten: **planar in 4096-Byte-Blöcken pro Kanal**, rundlaufend
(ch0-Block, ch1-Block, ch0-Block, …). Bei `bitsPerSample = 1` ist die Bitreihenfolge
**LSB first**. `metadataPtr` darf 0 sein (kein ID3v2) — optionale Kür; wenn er gesetzt wird,
zeigt er auf `92 + n`, also *hinter* die Nutzdaten.

Drei Felder sind bei realen Lesern hart: `totalFileSize` muss der Dateigröße auf Platte
entsprechen (WavPack **verweigert** die Datei sonst — das ist der eigentliche Grund für den
seek-Patch, nicht Bequemlichkeit), `reserved` muss 0 sein (WavPack und der Go-Referenzleser
verweigern sonst), und `sampleCount` sollte ein Vielfaches von 8 sein (ffmpeg rundet ab,
WavPack rundet auf und warnt — bei einem krummen Wert liefern die beiden verschieden lange
Dateien).

#### 3.4.1 Der letzte Block: zwei bewusste Abweichungen von der Spezifikation

Sonys Annotation 3 sagt: letzten Block mit **0x00** auffüllen, `sampleCount` auf der echten
Sample-Zahl lassen. mint macht **beides nicht**, und beides aus demselben Grund: *0x00 ist
keine Stille.*

Ein DSD-Bit ist ±1, ein Nullbyte also nicht „kein Signal", sondern das negativste Signal,
das das Format ausdrücken kann. Durch ffmpegs eigenes Dezimationsfilter gemessen:

| Füllmuster | Pegel in der Polsterung | Spitze am Übergang |
|---|---|---|
| 0x00 (laut Spec) | −1,0 (Vollaussteuerung DC) | **1,045 — clippt** |
| 0x69 (mint) | 0,0 | 0,0100 (−40 dBFS) |

Das ist kein theoretisches Problem: der sacd-ripper-Quelltext trägt an der Füllstelle den
Kommentar *„This does zero fill … which leads to pop noise in some players"*, und es wurde
extra ein polsterungsfreier Modus nachgerüstet. 0x69 ist die Stille-Konstante in ffmpeg
(`DSD_SILENCE`), in MPD und in der ALSA-Formattabelle des Linux-Kernels. Sie ist
bitreihenfolgeunabhängig: `reverse(0x69) = 0x96`, beide dekodieren zu exakt 0,0.

Ob der Knacks *hörbar* wird, hinge dann an `sampleCount` — und genau da sind sich die Leser
uneins:

| Leser | bei ungepolstertem `sampleCount` |
|---|---|
| ffmpeg | schneidet byte-genau; **Fehler**, wenn der letzte Block komplett Polsterung ist |
| MPD | rundet auf ganze Blöcke ab — **verliert bis zu 11,6 ms echtes Audio** (DSD64) |
| WavPack | rundet auf ganze Bytes auf und warnt |

Deshalb: mint polstert mit 0x69 **und** meldet die **gepolsterte** Zahl. Dann gibt es gar
keinen „unused sample data area", über den man sich uneinig sein könnte — jeder Leser
spielt die vollen Nutzdaten, Byte-Schnitt und Block-Abrundung kommen zum selben Ergebnis,
nichts wird abgeschnitten, und der Schluss ist echte digitale Stille statt einer DC-Stufe.
Preis: die Datei ist bis zu 11,6 ms (DSD64) / 5,8 / 2,9 / 1,5 länger als die Quelle. Für ein
Distributions-Deliverable irrelevant; für lückenlose Albumfolgen wäre es der eine Grund, das
noch einmal aufzumachen.

Besser als jede Polsterung ist, sie nicht zu brauchen: `DsfWriter::samples_to_block_boundary`
sagt, wie viele Samples bis zur Blockgrenze fehlen. Wer die als **modulierte** Stille
nachschiebt, bekommt dieselben vollen Blöcke ohne jede Spleißstelle — der Schleifenzustand
läuft durch. Genau das soll die Blockschleife in Phase 4 tun; `tests/dsd_format.rs` macht es
im Ende-zu-Ende-Test bereits vor.

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
src/ops/dsd/mod.rs          Orchestrierung der Blockschleife, DsdRate-Enum, Ratenableitung  [teilweise: DsdRate]
src/ops/dsd/interpolate.rs  Streaming-Wrapper um libsoxr (Latenz, Blockpufferung, INIT_LOCK)  [fertig]
src/ops/dsd/modulator.rs    1-Bit-Sigma-Delta, CIFB, NTF-Tabelle pro Rate, Dither, Watchdog   [fertig]
src/ops/dsd/decimate.rs     DSD → PCM, Messwerkzeug (pub, nicht cfg(test) — s. Phase 2)       [fertig]
tools/design_ntf.py         Offline-NTF-Entwurf + CIFB-Realisierung, erzeugt die Tabelle      [fertig]
tests/dsd_verification.rs   Messharness (Kopfzahlen wie bei dither_/limiter_verification)     [fertig]
src/ops/dsd/pack.rs         Bit-Packing, Bitreihenfolge als Parameter (LSB-first für DSF)     [fertig]
src/audio/io_write_dsd.rs   DSF-Streaming-Writer (Header-Patch via seek)                      [fertig]
tests/dsd_format.rs         Byte-Layout, Padding, Bitreihenfolge, Fremd-Parser-Gegenprobe     [fertig]
```

Die Blockschleife selbst (`mod.rs`) entsteht erst in Phase 4. Beide Enden sind dafür fertig
und `tests/dsd_format.rs::modulate_to_file` ist bereits ihre Miniaturausgabe — die Funktion
hat genau die Form, die die Schleife bekommen soll:

```rust
// eine Soxr-Instanz, ein Modulator pro Kanal, ein Writer, alles über die ganze Datei
for chunk in pcm_blocks {
    let up = src.process(chunk)?;                       // variable Länge
    let bits = per_channel_modulate(&mut mods, up);
    writer.write(&bits)?;                               // puffert auf DSF-Blockgrenze
}
let tail = src.finish()?;
writer.write(&per_channel_modulate(&mut mods, tail))?;

// Statt zu polstern: den letzten Block mit modulierter Stille vollmachen (§ 3.4.1).
let short = writer.samples_to_block_boundary();
if short > 0 { writer.write(&per_channel_modulate(&mut mods, silence(short)))?; }
let summary = writer.finish()?;
```

Der Modulator ist dafür gebaut — eine Instanz pro Kanal, Zustand über Blockgrenzen hinweg,
`modulate_into` hängt an einen wiederverwendeten Puffer an — und sowohl
`tests/dsd_verification.rs` als auch `tests/dsd_format.rs` prüfen, dass die Blockung das
Ergebnis nicht um ein Bit verändert.

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

### 4.1 Ergebnis (Phase 2, 2026-08-03, Apple Silicon)

`tests/dsd_verification.rs`, 10 Tests, ~15 s im Debug-Build. Pegelkonvention durchgehend:
**0 dBFS = 50 % Modulationsindex**. Der Dateikopf dort ist die maßgebliche Fassung dieser
Tabelle; das hier ist die Zusammenfassung.

| Rate | analytisch `sup\|NTF\|` | analytisch In-Band-Max | SNR 20 Hz–20 kHz | THD+N |
|---|---|---|---|---|
| DSD64 | 1,300 | −94,6 dB | **121,0 dB** | 120,0 dB |
| DSD128 | 1,300 | −135,1 dB | 163,8 dB | 162,1 dB |
| DSD256 | 1,300 | −151,1 dB | 173,7 dB | 171,1 dB |
| DSD512 | 1,300 | −151,8 dB | 175,1 dB | 171,3 dB |

**Der Zielkorridor aus § 4.1 war ≥ 110 dB; DSD64 liegt 11 dB darüber.** Die beiden
untersten analytischen In-Band-Werte sind *nicht* die Entwurfswerte (−176,9 / −219,0 dB),
sondern die Stelle, an der f64-Auslöschung aufhört: ab DSD256 ist die Kerbe tiefer, als
doppelte Genauigkeit sie überhaupt auswerten kann. Kostet nichts — alles ab DSD128 ist
40 dB jenseits jeder Relevanz —, ist aber der Grund, warum die Schranken im Harness
Untergrenzen und Verhältnisse sind statt Gleichheiten.

Der Rest der Liste aus § 4:

| Punkt | Messung | Schranke |
|---|---|---|
| 2. Rauschform vs. 2.-Ordnung-Referenz, in-band | 48,2 dB besser | > 40 dB |
| 2. Messspektrum vs. Schleifenfilter der Tabelle | 0,32 dB rms bei `k` = 2,42 | < 1,5 dB |
| 3. Durchlassband 19 Hz – 19,8 kHz, ganze Kette | 0,0001 dB | < 0,1 dB |
| 4. DC bei digitaler Stille | exakt 0 | < 1e-4 |
| 5. Idle Tone, 1e-3 DC-Offset | 16,1 dB (34,5 dB ohne Dither) | < 25 dB |
| 6. Clipping bei ≤ 60 % Modulation | keins | keins |
| 6. SNR nach 90 % Übersteuerung | 120,7 dB | > 110 dB |
| 7. Modulationsindex bei `ceiling_dbtp = −1` | 44,6 % | < 71,43 % |
| 9. Determinismus (Seed, Kanal, Blockgrenzen) | bitgleich | bitgleich |
| — Eigenfehler des Dezimators (das Messgerät) | −185,7 dB | < −160 dB |

Zwei Befunde, die beim Messen aufgefallen sind und die man kennen sollte:

**Die effektive Komparatorverstärkung ist nicht 1, sondern ~2,4.** Ein 1-Bit-Quantisierer
hat keine Verstärkung, nur ein Vorzeichen; linearisiert ist `k = √(2/π)/σ_y`, und mit
`σ_y ≈ 0,3` kommt ~2,6 heraus. Die *realisierte* NTF ist deshalb `NTF_k`, nicht die
entworfene: außerhalb des Bandes steigt sie weiter an, statt bei `H_inf` zu sättigen. Der
Harness fittet dieses eine Skalar und prüft das Residuum (0,32 dB über 50 kHz – 1,2 MHz) —
das ist der eigentliche Nachweis, dass die implementierte Rekursion das Schleifenfilter der
Tabelle realisiert. Wer die gemessene Kurve naiv gegen die entworfene NTF hält, findet
8 dB „Fehler", die keiner sind.

**Nachtrag zu Punkt 8 (Container).** In Phase 3 nachgeholt, Ergebnisse in § 4.2. Die
`#[ignore]`-Gegenprobe gegen ffmpeg entfällt: auf der Entwicklungsmaschine ist kein ffmpeg
installiert, ein Test, der überall außer hier läuft, wäre schlechter als keiner. An seiner
Stelle stehen zwei unabhängige Leser — das fremde `dsf`-Crate (Dev-Dependency) und ein im
Test von Hand geschriebener Entpacker, der `pack.rs` nicht benutzt und deshalb wirklich
gegen die LSB-first-Regel prüft statt gegen sich selbst.

**Der Dezimator muss besser sein als das, was er misst.** Halfband-Kaskade, 121 Taps pro
Stufe, Kaiser-Fenster, −174,8 dB Sperrband. Das ist keine Übertreibung: Dezimation um 2
faltet alles oberhalb der neuen Nyquistfrequenz zurück, und was bei DSD64 nahe 1,4 MHz
liegt, landet dabei *im Audioband*. Mit einem schwachen Anti-Aliasing-Filter erfindet das
Messwerkzeug einen Rauschteppich und lässt einen guten Modulator mittelmäßig aussehen.
Gemessen wird der Eigenfehler differenziell gegen den erwarteten Ton, nicht spektral — ein
Kaiser-Fenster ist bei rund −170 dB am Ende, eine FFT würde hier sich selbst messen.

### 4.2 Ergebnis (Phase 3, 2026-08-03, Apple Silicon)

`tests/dsd_format.rs`, 17 Tests, ~4,5 s im Debug-Build. Der Dateikopf dort ist die
maßgebliche Fassung; das hier ist die Zusammenfassung.

| Prüfung | Ergebnis |
|---|---|
| 92-Byte-Header gegen ein von Hand geschriebenes Golden-Array | byte-gleich |
| `totalFileSize` / `dataChunkSize` / `sampleCount` gegen die Datei auf Platte | konsistent, inkl. der `+12` |
| Bitreihenfolge im Container | LSB first (0x03 für „zwei hohe Samples zuerst") |
| Blöcke planar und rundlaufend, 4096 B pro Kanal | ja, über zwei Blockrunden geprüft |
| Kanaltyp 1/2/3/4/5/6 Kanäle | 1, 2, 3, 4, 6, 7 |
| Blockgröße des Aufrufers (1, 3, 4095, 32768, 100000) | dieselbe Datei, byte-gleich |
| Bitstrom Modulator → Datei → wieder heraus | bit-gleich, beide Kanäle |
| Fremdparser (`dsf`-Crate) | akzeptiert, alle fmt-Felder wie erwartet |
| In-Band-SNR der dekodierten Datei (DSD64, bei 44,1 kHz) | **104,4 dB** (Schranke > 95) |
| 0x00-Polsterung, dezimiert | −1,000000 (Vollaussteuerung DC) |
| 0x69-Polsterung, dezimiert | 0,000000 |

Ein Befund, der beim Messen aufgefallen ist und den man kennen sollte:

**Ein breitbandiger Zeitbereichsvergleich misst bei DSD das Ultraschallrauschen, nicht das
Signal.** Dieselbe Datei, dieselben Bits, dieselbe Ausrichtung (Bestanpassungsverstärkung
1,0000002, Verzögerung exakt 0) ergibt **39,7 dB**, wenn man bei 88,2 kHz dekodiert, und
**104,4 dB** bei 44,1 kHz. Der Unterschied ist allein die Analysebandbreite: der Dezimator
lässt bei 88,2 kHz Ausgang noch bis 35 kHz durch, und genau dort steigt der geformte
Rauschteppich steil an. Wer die 39,7 dB naiv liest, hält einen korrekten Container für
kaputt. (Die Zahl des Modulators selbst — 121,0 dB, sauber im Band gemessen — steht in
§ 4.1; die 104,4 dB sind die ganze Kette inklusive des 17,6–22 kHz-Übergangsbandes.)

---

## 5. Risiken & offene Punkte

| Risiko | Bewertung | Umgang |
|---|---|---|
| ~~Modulatorqualität unklar bis gemessen~~ | **erledigt** | Gemessen (§ 4.1): 121,0 dB bei DSD64, Ziel war ≥ 110. Das größte Einzelrisiko des Projekts ist damit weg |
| ~~Stabilität bei heißem Material~~ | **erledigt, mit Einschränkung** | Kein Clipping bis 60 % Modulation, Betriebsbereich ist ≤ 50 % (§ 2.3.1). Bei 71,43 % übersteuert die Schleife kontrolliert und erholt sich; ein Retry mit −1 dB ist nicht nötig |
| ~~libsoxr bei Ratio bis 512 (`vhq`)~~ | **erledigt** | Gemessen (§ 3.3): Kosten hängen nur an den Ausgangssamples, nicht am Verhältnis. Kein Staffeln nötig |
| ~~Streaming-Latenz/Längenvertrag~~ | **erledigt** | `tests/dsd_interpolate.rs`: Streaming == One-Shot, bit-gleich bei jeder Blockgröße. Seit Phase 3 zusätzlich durch den Container abgesichert (`chunking_does_not_change_the_file`) |
| ~~Container falsch geschrieben (off-by-12, Bitreihenfolge)~~ | **erledigt** | § 4.2: Golden-Header, drei Größenfelder gegen die Datei auf Platte, Fremdparser, unabhängiger Entpacker |
| Polsterung des letzten Blocks | gering, aber abweichend | Bewusste Abweichung von Annotation 3, gemessen begründet (§ 3.4.1). Wenn lückenlose Albumfolgen ein Thema werden, ist das die Stelle |
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
Modulatorqualität) kommt zuerst. Seit Phase 3 gibt es abspielbare `.dsf`-Dateien, nach
Phase 4 ist das Feature regulär über die Config nutzbar.

### ~~Phase 1 — Streaming-Interpolation auf libsoxr~~ · **abgeschlossen**
1. **Zuerst der Smoke-Test**: Durchsatz und Speicher von libsoxr `vhq` bei den Ratios
   32/64/128/256/512 (bis 22 579 200 Hz Ausgangsrate; ×512 entsteht bei 44,1 kHz → DSD512). Entscheidet, ob eine Soxr-Instanz reicht
   oder manuell gestaffelt wird.
2. `SoxrStream`-Wrapper: Instanz über die ganze Datei, feste Ausgabeblöcke, Latenz vorne
   verwerfen, Flush am Ende, `INIT_LOCK` übernehmen.
3. Tests: Streaming-Ergebnis == One-Shot-Ergebnis (kurze Datei, kleines Ratio),
   Gesamtlängenvertrag, Durchlassbandgang, Nebenläufigkeit unter rayon.

### ~~Phase 2 — Modulator + Dezimator + Messharness~~ · **abgeschlossen**
1. ✅ NTF-Koeffizienten offline berechnet — `tools/design_ntf.py`, eine NTF pro DSD-Rate.
   **Abweichung:** Ordnung 7 statt 5, `H_inf` 1,30 statt 1,5, `synthesizeChebyshevNTF`
   statt `synthesizeNTF`. Begründung und Messungen in § 2.3.1.
2. ✅ CIFB-Modulator mit Zustandsbegrenzung, Komparator-Dither (aus dem `seed`),
   Overload-Zähler. **Abweichung:** Dither 0,2 statt 1e-3 … 1e-2 (§ 2.3.1).
3. ✅ Dezimator als Messwerkzeug. **Abweichung:** nicht `#[cfg(test)]`, sondern schlicht
   `pub` — Integrationstests linken die Bibliothek ohne `cfg(test)`, ein test-gates Modul
   wäre für `tests/dsd_verification.rs` unsichtbar. Aus keinem Ausgabepfad erreichbar.
4. ✅ `tests/dsd_verification.rs`: Messungen 1–7 und 9 aus § 4, Zahlen im Dateikopf.
   Abnahmekriterium erfüllt, Ergebnisse in § 4.1.

Offen aus § 4, bewusst auf Phase 3 verschoben: Punkt 8 (Container/Golden-Bytes) — es gibt
noch keinen Container.

### ~~Phase 3 — DSF-Writer (streaming)~~ · **abgeschlossen**
1. ✅ `src/ops/dsd/pack.rs`: Bit-Packing mit `BitOrder` als Parameter, plus `DSD_SILENCE`.
2. ✅ `src/audio/io_write_dsd.rs`: `DsfWriter`, 4096-Byte-Blöcke pro Kanal planar und
   rundlaufend, `totalFileSize`/`sampleCount`/`dataChunkSize` per seek nachgetragen.
   **Abweichung:** Polsterung mit 0x69 statt 0x00, und `sampleCount` zählt sie mit
   (§ 3.4.1). Dazu `samples_to_block_boundary`, damit Phase 4 die Polsterung ganz vermeiden
   kann.
3. ✅ `tests/dsd_format.rs`, 17 Tests, Ergebnisse in § 4.2. **Abweichung:** kein
   ffmpeg-Test (nicht installiert); stattdessen das `dsf`-Crate als Dev-Dependency und ein
   unabhängig geschriebener Entpacker im Test.
4. ✅ **Es gibt eine hörbare Datei.** `a_modulated_tone_survives_the_whole_chain` legt sie
   nach `target/tmp/mint-dsd64-tone.dsf` (0,5 s Stereo, DSD64) und gibt den Pfad aus
   (`--nocapture`). Fester Pfad, damit ein Player daraufzeigen kann; alle übrigen
   Testdateien tragen die Prozess-ID im Namen und werden aufgeräumt.

Eine Lehre aus dem Review, die man kennen sollte, weil sie fast zu einer Geisterjagd geführt
hätte: die Tests schrieben zunächst auf feste Pfade unter `std::env::temp_dir()` (die
Konvention der älteren Tests im Repo). Zwei überlappende Läufe der Testbinärdatei schreiben
sich dann gegenseitig die Dateien kaputt — und der Ende-zu-Ende-Test meldet das als
`the chain lost the signal: -15.2 dB`, was wie ein Modulatorfehler aussieht. Gemessen: drei
gleichzeitige Läufe mit gemeinsamem Temp-Verzeichnis lassen 1, 3 bzw. 4 Tests fallen, mit
getrennten Verzeichnissen laufen alle drei sauber durch. Die Kette selbst ist
bit-deterministisch — 22 isolierte Läufe, ein einziger SHA-256, jedes Mal 104,4 dB.

Nicht gemacht, bewusst: ID3v2 im DSF (steht in Phase 5), DFF (§ 3.4).

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

## 7. Verworfene Alternativen zur Zwischenraten-Regel

Die Regel aus § 2.1 (*nächstgelegene Rate der 44,1-Familie, gedeckelt auf DSD-Rate / 32*;
explizites `rate` übersteuert) ist **angenommen**. Festgehalten ist hier, was dagegen
abgewogen wurde — damit die Frage nicht in sechs Monaten neu aufgemacht wird:

- **immer die höchstmögliche 44,1-Rate** — mehr Bandbreite in den Modulator, aber bei
  DSD64 landet Programminhalt bis 88 kHz in einem Modulator, dessen Rauschen ab ~25 kHz
  steil ansteigt: kostet Stabilitätsreserve ohne hörbaren Gegenwert;
- **keine Ableitung, `rate` wird bei DSD-Targets Pflicht** — explizit, aber gegen mints
  Grundprinzip („deklariere das Deliverable, die Schritte werden abgeleitet");
- **soxr direkt aus der 48er-Familie** (192 kHz → 2 822 400 Hz, Ratio 14,7) — technisch
  möglich, seit die Interpolation ohnehin soxr ist, aber teurer und gegen die Konvention
  der DSD-Welt. Würde ich nicht tun.

Der einzige Fall, in dem die gewählte Regel echte Bandbreite kostet, ist 48 → 44,1 kHz
(24 → 22,05 kHz Band) — derselbe Handel, den mint für jedes 44,1-Deliverable längst macht.

**Es sind derzeit keine Entscheidungen offen.**

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

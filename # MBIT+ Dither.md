# MBIT+ Dither: Psychoacoustic Noise Shaping für mint

0. Zielbild (was wir erreichen wollen)
Wir erweitern mint von:
signalabhängigem Noise Shaping
zu:
psychoacoustic masking + temporal masking + adaptive spectral redistribution + multi-mode dithering
1. Neue Architektur für mint Dither (MBIT+-Style)
INPUT (float32/64)
   ↓
1. Analysis Stage
   ├─ FFT / Bark-band energy
   ├─ RMS / LUFS local
   ├─ transient detection
   └─ spectral flux

   ↓
2. Psychoacoustic Model
   ├─ masking thresholds (frequency)
   ├─ temporal masking windows
   └─ perceptual noise floor estimation

   ↓
3. Dither Generator
   ├─ TPDF base noise
   ├─ correlated stereo noise
   └─ adaptive amplitude scaling

   ↓
4. Noise Shaping Filter Bank
   ├─ HF shaping (masking-driven)
   ├─ MF balancing
   ├─ LF protection (avoid buildup)
   └─ mode-dependent curves

   ↓
5. Quantization Engine
   └─ bit-depth reduction (16/24/streaming)

   ↓
OUTPUT
2. Kernstück 1: Psychoakustisches Masking-Modell
Das ist der große Unterschied zu mint “classic”.
2.1 Bark-scale approximation
fn bark(f: f32) -> f32 {
    13.0 * ((0.00076 * f).atan())
    + 3.5 * (((f / 7500.0).powi(2)).atan())
}
2.2 vereinfachte Hörschwelle (Equal Loudness Approximation)
fn hearing_threshold(f: f32, level: f32) -> f32 {
    let hf_penalty = (f / 8000.0).powf(0.7);
    let lf_penalty = (200.0 / (f + 1.0)).powf(0.5);

    1.0 + hf_penalty + lf_penalty - (level * 0.1)
}
2.3 lokale Maskierung
fn masking_threshold(spectrum: &[f32]) -> Vec<f32> {
    spectrum.iter().enumerate().map(|(i, &amp)| {
        let freq = index_to_freq(i);
        let bark_band = bark(freq);

        let local_energy = amp.log10();

        hearing_threshold(freq, local_energy)
            * (1.0 / (1.0 + bark_band))
    }).collect()
}
3. Kernstück 2: Temporal Masking (entscheidender MBIT+-Teil)
Hier fehlt mint aktuell am meisten.
Idee:
Noise ist weniger hörbar:
direkt nach Peaks (post-masking)
kurz vor Peaks (pre-masking)
fn temporal_mask(current: f32, prev_peak: f32, next_peak_est: f32) -> f32 {
    let pre_mask = (prev_peak * 0.6).exp();
    let post_mask = (next_peak_est * 0.4).exp();

    1.0 - (pre_mask + post_mask).min(1.0)
}
👉 Effekt:
Reverb tails → “schwärzer”
Transienten → weniger noise perception
4. Kernstück 3: MBIT+-Style Noise Shaping Filter Bank
Jetzt ersetzen wir den einfachen Shaper.
4.1 Multi-band Shaper
fn noise_shape(freq: f32, mode: DitherMode) -> f32 {
    match mode {
        DitherMode::Transparent => 0.3,
        DitherMode::Balanced => {
            if freq > 8000.0 { 1.8 }
            else if freq < 200.0 { 0.6 }
            else { 1.0 }
        },
        DitherMode::Aggressive => {
            if freq > 10000.0 { 2.5 }
            else { 1.2 }
        }
    }
}
4.2 adaptive shaping (MBIT+-Style improvement)
fn adaptive_shaping(freq: f32, masking: f32) -> f32 {
    let base = noise_shape(freq, DitherMode::Balanced);

    // KEY MBIT IDEA:
    // more shaping where ear is insensitive
    base * masking
}
👉 Das ist der echte MBIT+ Trick:
Noise wird dorthin verschoben, wo es psychoakustisch “verschwindet”
5. Kernstück 4: Stereo-correlated dithering
fn stereo_noise(seed: u64, correlation: f32) -> (f32, f32) {
    let base = rand_from_seed(seed);

    let l = base;
    let r = base * correlation + rand_small();

    (l, r)
}
👉 Vorteil:
stabileres stereo field bei -60 dB
weniger “width shimmer”
6. Kernstück 5: Adaptive Dither Mode Selection (MBIT Feature)
fn select_mode(lufs: f32, dynamic_range: f32) -> DitherMode {
    if dynamic_range > 20.0 {
        DitherMode::Transparent // classical
    } else if lufs > -8.0 {
        DitherMode::Aggressive // pop
    } else {
        DitherMode::Balanced
    }
}
👉 Das ist extrem wichtig:
MBIT+ ist NICHT ein Algorithmus, sondern ein Set von psychoacoustic presets
7. Integration in mint Pipeline
Ersetzt bisherigen Dither Block:
OLD:
TPDF → fixed noise shaping → quantization

NEW:
analysis → psychoacoustic model → temporal mask →
adaptive noise generation → multi-band shaping → quantization

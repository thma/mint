#!/usr/bin/env python3
"""Offline NTF design + CIFB realization for mint's DSD sigma-delta modulator.

Run once; its output is the constant table in `src/ops/dsd/modulator.rs`. Nothing at
build time depends on this file — `tests/dsd_verification.rs` re-derives and re-measures
every property the table claims. (Same arrangement as the checked-in coefficient tables
in `src/dither/static_fir.rs`.)

    python3 -m venv .venv
    .venv/bin/pip install numpy scipy
    .venv/bin/python tools/design_ntf.py

What it does, in order:

1. **NTF design** — Schreier's `synthesizeChebyshevNTF` construction: a Chebyshev type-II
   high-pass whose *stopband* is the audio band. For odd order that yields exactly one
   zero at DC plus (order-1)/2 conjugate pairs spread equiripple across the band, which is
   the CIFB shape (one plain integrator + resonators). scipy normalizes the type-II
   passband peak to 1, so the monic NTF has `sup|NTF| = 1/k`; that is monotone in the
   stopband attenuation, and bisecting it hits the Lee target `H_inf` exactly.

2. **Realization** — solves for the CIFB gains that produce that transfer function in the
   coordinates `modulator.rs` actually implements. With `d = u - v` driving every stage,
   `C = e_n`, and `A(g)` the sequential update from the Rust module docs,

       NTF(z) = N(z) / (N(z) + C adj(zI - A) B(a)),   N(z) = det(zI - A)

   `N` is fixed by `g` alone (`g_k = 2 - 2 cos(theta_k)` for a zero pair), and the
   correction term is *linear* in `a`. So: build the n basis polynomials with
   Faddeev-LeVerrier and solve one n x n system. Exact, no iteration.

3. **Simulation** — runs the exact Rust update to find the maximum stable amplitude and
   the per-state peaks the clamp limits are derived from. These are the numbers that
   picked order 7 over the order-5 sketch in `docs/dsd-design.md`; see `--sweep`.
"""

import argparse

import numpy as np
from scipy.signal import cheby2

BAND_HZ = 20_000.0          # design band edge: the audible limit, not 22.05 kHz
RATES = [("Dsd64", 2_822_400), ("Dsd128", 5_644_800),
         ("Dsd256", 11_289_600), ("Dsd512", 22_579_200)]

# Comparator dither, in units of the quantizer input. Must match `QUANTIZER_DITHER` in
# modulator.rs: the state peaks the clamp limits come from depend on it, and so does the
# maximum stable amplitude.
QUANTIZER_DITHER = 0.2

# Clamp headroom over the worst state peak in the probe battery below.
#
# Deliberately generous. The two failure modes are not symmetric: a clamp that is too
# tight distorts *legal* audio (the state peaks are strongly signal-dependent, spanning
# orders of magnitude between the first and last integrator), while a clamp that is too
# loose only delays divergence detection by a few samples — the loop grows by roughly
# H_inf per sample once it goes, so 32x is reached in about a dozen.
STATE_LIMIT_MARGIN = 32.0


# ---------------------------------------------------------------- NTF design

def synthesize_ntf(order, osr, h_inf):
    """Monic Chebyshev-II high-pass NTF with sup|NTF| == h_inf. Returns (zeros, poles)."""
    def sup(rs):
        z, p, k = cheby2(order, rs, 1.0 / osr, btype="high", output="zpk")
        return 1.0 / k, (z, p)

    lo, hi = 1.0, 10.0
    while sup(hi)[0] < h_inf:
        hi *= 1.5
        assert hi < 1e4, "no bracket for H_inf"
    for _ in range(200):
        mid = 0.5 * (lo + hi)
        if sup(mid)[0] < h_inf:
            lo = mid
        else:
            hi = mid
    rs = 0.5 * (lo + hi)
    achieved, (z, p) = sup(rs)
    return z, p, rs, achieved


# ------------------------------------------------- CIFB coefficient solving

def state_matrix(order, g):
    """A(g) for the sequential in-place update implemented in `modulator.rs`:

        x[s] += a[s]*d + x[s-1]            (x[s-1] already updated -> non-delaying)
        x[s] -= g[s/2] * x[s+1]            (s even and paired; x[s+1] still old)

    Its characteristic polynomial factors into `z^2 + (g_k - 2)z + 1` per resonator times
    `(z - 1)` for the odd stage out, i.e. exactly the NTF zeros — the mixed
    delaying/non-delaying pair is what puts those roots on the unit circle.
    """
    A = np.zeros((order, order))
    carry = np.zeros(order)          # expression for the previous stage's new value
    for s in range(order):
        row = carry.copy()
        row[s] += 1.0
        if s % 2 == 0 and s + 1 < order:
            row[s + 1] -= g[s // 2]
        A[s] = row
        carry = row
    return A


def drive_vector(a):
    """Each stage adds its predecessor's new value, so the drive accumulates."""
    return np.cumsum(np.asarray(a, dtype=float))


def faddeev(A):
    """(adj(zI - A) as matrix-polynomial coefficients, characteristic polynomial)."""
    n = A.shape[0]
    M, c, Mk = [np.eye(n)], [1.0], np.eye(n)
    for k in range(1, n + 1):
        AM = A @ Mk
        ck = -np.trace(AM) / k
        c.append(ck)
        Mk = AM + ck * np.eye(n)
        if k < n:
            M.append(Mk)
    return M, np.array(c)


def realize(order, zeros, poles):
    """CIFB gains (a, g) for the given NTF."""
    ang = np.sort(np.abs(np.angle(zeros)))
    if order % 2:
        assert ang[0] < 1e-12, f"odd order wants a zero at DC, got angle {ang[0]}"
    thetas = [ang[i] for i in range(order % 2, order, 2)]
    g = np.array([2.0 - 2.0 * np.cos(t) for t in thetas])

    A = state_matrix(order, g)
    M, charpoly = faddeev(A)
    n_poly = np.real(np.poly(zeros))
    d_poly = np.real(np.poly(poles))
    assert np.allclose(charpoly, n_poly, atol=1e-8), "g does not reproduce the zeros"

    C = np.zeros(order)
    C[-1] = 1.0
    basis = np.array([[C @ Mk @ drive_vector(np.eye(order)[i]) for Mk in M]
                      for i in range(order)]).T
    a = np.linalg.solve(basis, (d_poly - n_poly)[1:])   # both monic -> degree <= n-1
    return a, g


def ntf_magnitude(a, g, w):
    """|NTF| from the realized structure, so the check is on what Rust runs."""
    order = len(a)
    A, B = state_matrix(order, g), drive_vector(a)
    C = np.zeros(order)
    C[-1] = 1.0
    return np.array([abs(1.0 / (1.0 + C @ np.linalg.solve(np.exp(1j * wi) * np.eye(order) - A, B)))
                     for wi in w])


# -------------------------------------------------------------- simulation

def simulate(a, g, u, dither=None, seed=3):
    """The exact Rust per-sample update. Returns (bitstream, per-state peaks, diverged)."""
    dither = QUANTIZER_DITHER if dither is None else dither   # resolved per call, so --dither works
    n = len(a)
    npairs = n // 2
    rng = np.random.default_rng(seed)
    noise = dither * (rng.random(len(u)) - rng.random(len(u)))
    x = np.zeros(n)
    peak = np.zeros(n)
    out = np.empty(len(u))
    for i in range(len(u)):
        v = 1.0 if x[n - 1] + noise[i] >= 0.0 else -1.0
        out[i] = v
        d = u[i] - v
        prev = 0.0
        for s in range(n):
            inc = a[s] * d + prev
            if s % 2 == 0 and s + 1 < n and s // 2 < npairs:
                inc -= g[s // 2] * x[s + 1]
            x[s] += inc
            prev = x[s]
        np.maximum(peak, np.abs(x), out=peak)
        if not np.isfinite(x[n - 1]) or peak[n - 1] > 1e7:
            return out[:i], peak, True
    return out, peak, False


def tone(amp, hz, fs, n):
    return amp * np.sin(2 * np.pi * (round(hz * n / fs) / n) * np.arange(n))


def probe_battery(amp, fs, n):
    """Signals the clamp limits have to survive without ever firing, all at `amp`.

    The integrator states are far more sensitive to *how* the signal moves than to its
    peak — DC and very low frequencies load the early stages hardest, a square wave loads
    the late ones — so a single 1 kHz sine badly underestimates the range. The square wave
    is deliberately not band-limited: nothing the interpolator can produce is worse.
    """
    t = np.arange(n)
    return {
        "dc+": np.full(n, amp),
        "dc-": np.full(n, -amp),
        "20 Hz": tone(amp, 20.0, fs, n),
        "100 Hz": tone(amp, 100.0, fs, n),
        "1 kHz": tone(amp, 1000.0, fs, n),
        "10 kHz": tone(amp, 10_000.0, fs, n),
        "square 50 Hz": amp * np.sign(np.sin(2 * np.pi * (50.0 / fs) * t)),
        "step": np.where(t < n // 2, -amp, amp),
    }


def max_stable_amplitude(a, g, fs, n=200_000, steps=9, kind="100 Hz"):
    """Largest amplitude of `kind` that does not diverge, by bisection.

    Worth doing per signal class rather than once: a sine is the *easy* case. Sustained DC
    and steps load the integrator chain far harder and give a noticeably lower MSA — which
    is the number that actually bounds what the loop may be fed.
    """
    def ok(amp):
        return not simulate(a, g, probe_battery(amp, fs, n)[kind])[2]
    lo, hi = 0.05, 0.99
    if ok(hi):
        return hi
    if not ok(lo):
        return 0.0
    for _ in range(steps):
        mid = 0.5 * (lo + hi)
        lo, hi = (mid, hi) if ok(mid) else (lo, mid)
    return lo


def measure_snr(a, g, fs, amp=0.5, n=1 << 19):
    """In-band SNR of the raw bitstream, referred to the 0 dB SACD reference (amp 0.5).

    Measured on the bitstream itself rather than through a decimator: out-of-band noise
    outside 20 Hz - 20 kHz is simply not summed, which is what a decimator would do.
    """
    cycles = round(1000.0 * n / fs)
    v, _, div = simulate(a, g, amp * np.sin(2 * np.pi * (cycles / n) * np.arange(n)))
    if div:
        return float("nan")
    power = np.abs(np.fft.rfft(v * np.kaiser(n, 20))) ** 2
    hz = np.arange(len(power)) * fs / n
    band = (hz >= 20.0) & (hz <= BAND_HZ)
    sig = np.zeros(len(power), bool)
    sig[max(0, cycles - 14):cycles + 15] = True
    return (10 * np.log10(power[sig & band].sum() / power[band & ~sig].sum())
            + 20 * np.log10(0.5 / amp))


# ------------------------------------------------------------------- output

def sweep(args):
    """The measurement behind the order/H_inf choice. Slow (pure-python loops)."""
    fs = 2_822_400.0
    osr = fs / (2 * BAND_HZ)
    print(f"DSD64, OSR {osr:.2f}")
    print(f"{'order':>5} {'H_inf':>6} {'MSA':>6} {'margin@50%':>10} {'SNR':>7}")
    for order in (5, 7):
        for h in (1.25, 1.30, 1.35, 1.45, 1.5, 1.6):
            z, p, _, _ = synthesize_ntf(order, osr, h)
            a, g = realize(order, z, p)
            msa = max_stable_amplitude(a, g, fs, n=args.sim)
            snr = measure_snr(a, g, fs, 0.5, 1 << 18) if msa > 0.5 else float("nan")
            print(f"{order:>5} {h:>6.2f} {msa:>6.3f} {20*np.log10(msa/0.5):>9.1f} dB {snr:>6.1f}")


def design(args):
    print(f"# order {args.order}, H_inf {args.h_inf}, band edge {BAND_HZ:.0f} Hz, "
          f"clamp margin {STATE_LIMIT_MARGIN}x")
    table = []
    for name, fs in RATES:
        osr = fs / (2 * BAND_HZ)
        zeros, poles, rs, sup = synthesize_ntf(args.order, osr, args.h_inf)
        a, g = realize(args.order, zeros, poles)

        w = np.linspace(1e-7, np.pi, 60_001)
        realized = ntf_magnitude(a, g, w)
        ejw = np.exp(1j * w)
        target = np.abs(np.prod(ejw[:, None] - zeros[None, :], axis=1)
                        / np.prod(ejw[:, None] - poles[None, :], axis=1))
        band = w <= np.pi / osr

        # Clamp limits come from the battery at the real operating ceiling — 50 %
        # modulation, i.e. PCM 0 dBFS — because that is the level at which no legal signal
        # may ever touch a clamp. MSA is reported separately, per signal class.
        peak = np.zeros(args.order)
        diverged = []
        for label, u in probe_battery(0.5, fs, args.sim).items():
            _, p, div = simulate(a, g, u)
            if div:
                diverged.append(label)
            else:
                np.maximum(peak, p, out=peak)
        msa = {k: max_stable_amplitude(a, g, fs, n=args.sim, kind=k)
               for k in ("100 Hz", "dc+", "square 50 Hz")}
        snr = measure_snr(a, g, fs, 0.5, 1 << 19)
        zeros_hz = np.sort(np.abs(np.angle(zeros))) * fs / (2 * np.pi)

        print(f"\n{name}: OSR {osr:.2f}  cheby2 stopband {rs:.4f} dB  sup|NTF| {sup:.6f}  "
              f"max|pole| {np.abs(poles).max():.6f}")
        print(f"   zeros (Hz):        {', '.join(f'{f:.0f}' for f in zeros_hz[::2])}")
        print(f"   in-band max |NTF|: {20 * np.log10(realized[band].max()):.1f} dB")
        print(f"   realization error: {np.abs(realized - target).max():.2e}")
        print(f"   bitstream SNR:     {snr:.1f} dB @ 50 % modulation")
        print("   max stable amp:    " + ",  ".join(
            f"{k} {v:.3f} ({20 * np.log10(v / 0.5):+.1f} dB)" for k, v in msa.items()))
        print(f"   probes @ 50 %:     8 signals, diverged: {diverged or 'none'}")
        print(f"   |x| peak @ 50 %:   {np.array2string(peak, precision=3)}")
        table.append((name, osr, rs, sup, poles, a, g, peak))

    print("\n\n// ---- generated by tools/design_ntf.py; see the module docs ----")
    for name, osr, rs, sup, poles, a, g, peak in table:
        print(f"    // {name}: OSR {osr:.2f}, cheby2 stopband {rs:.4f} dB, "
              f"sup|NTF| {sup:.4f}, max|pole| {np.abs(poles).max():.4f}")
        print("    Ntf {")
        print(f"        a: [{', '.join(fmt(v) for v in a)}],")
        print(f"        g: [{', '.join(fmt(v) for v in g)}],")
        print(f"        state_limit: [{', '.join(fmt(v * STATE_LIMIT_MARGIN, 6) for v in peak)}],")
        print("    },")


def fmt(v, sig=None):
    """Shortest literal that round-trips to the same f64 — repr() is exactly that.

    Not `%.17g`: the extra digits are noise that changes no bit of the value, and clippy's
    `excessive_precision` lint (rightly) objects to them."""
    s = repr(float(v))
    return s if ("." in s or "e" in s) else s + ".0"


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--order", type=int, default=7)
    ap.add_argument("--h-inf", type=float, default=1.30)
    ap.add_argument("--sim", type=int, default=200_000, help="simulation length in samples")
    ap.add_argument("--dither", type=float, default=QUANTIZER_DITHER,
                    help="comparator dither; must match QUANTIZER_DITHER in modulator.rs")
    ap.add_argument("--sweep", action="store_true",
                    help="print the order/H_inf trade-off table instead of designing")
    args = ap.parse_args()
    QUANTIZER_DITHER = args.dither
    (sweep if args.sweep else design)(args)

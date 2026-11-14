# dsp

The signal processing of the six plugins for the MacBookPro14,3 speakers,
in plain Rust with no dependencies, so the same core serves the LV2 plugins (`../lv2`)
and anything later. Each block's `Config::mbp143()` holds the speakers'
settings.

The arithmetic is f32, with f64 where a step needs the range, in a fixed
order, and Rust never fuses a*b+c into a multiply-add, so no compiler flag
changes a result: the same input gives the same output, bit for bit, on
every build. What a build does not fix is the C library, where `exp`, `log`,
`sin` and `cos` come from. The tests rely on this: they compare against
exact values. Every block processes in place, one slice per channel, and
allocates nothing in `process()`.

## The blocks

- `src/thermal.rs`: the thermal protection. Four two-pole RC
  models, one per speaker, in two arrays: A is fed the input and computes the
  gain, B is fed the output and accumulates the temperature the speaker really
  gets. Per 5 ms block: P = mean((9*x)^2)/R_e, two poles, a gain target of
  sqrt((threshold - ambient - T_slow_B)/(T_fast_A + 1)), a follower that falls
  in 800 ms and rises in 20 s, a 20 Hz Butterworth smoother over the resulting
  staircase, and a 0.1 floor past 145 C. The tweeters and the woofers each
  share one gain. A gap of more than 100 ms between calls is taken for a pause
  and advanced analytically. Latency 0.
- `src/controlfreak.rs`: the lookahead peak limiter on each branch. Per
  sample: env = max |in| over the channels, its maximum over a 67-sample
  window, t = makeup/clip(peak, makeup, 1000 makeup) split into a fast factor
  clip(t, ref, 1) and a slow one-pole toward clip(t/ref, 0, 1) that falls at
  once and rises in 10 s (270 ms while the activity detector holds), a 22-tap
  raised-cosine FIR over their product, and the audio delayed 22 samples times
  that gain. The activity detector: /4 through a 31-tap anti-alias filter, a
  Hilbert envelope, /10, a 33-sample mean square, hysteresis and hold,
  zero-order holds back up. Latency 22.
- `src/mozart.rs`: the upward compressor on the common path, also the gain
  computer the DualBand runs twice. +9 dB input gain, a three-section
  sidechain (high-pass, a fixed peak at 1680 Hz, a shelf), every 31st sample,
  the level and limiter mean squares (the moving sum in double), gating, the
  activity detector, a release and an attack transfer curve (both knees, the
  floors), the attack/release filter, the auto-threshold limiter, the 134-tap
  interpolator (a 61-tap linear interpolator convolved with the 74-tap raised
  cosine, upsampled by 31) and the 74-sample delay.
- `src/dualband.rs`: the two-band compressor. The 400 Hz crossover (cot by its
  Taylor series in double), the low-pass squared and complement squared
  detector paths, two Mozart gain computers at sr/31, the 61-tap linear gain
  interpolators, and the blend, four passes of out -= LP(out * w). Latency 0.
- `src/buzzkill.rs`: the adaptive rattle suppression on the woofers. A
  2500 Hz Butterworth and /4, a Hilbert pair, two 512-point Hann FFTs, the
  threshold spectrum shaped by three peaking bands, detection, the frequency
  estimate from the first-difference spectrum, release and assignment, and a
  50-notch bank at 8820 Hz between 65-tap /5 and x5 converters. Latency 220.
- `src/loudness.rs`: the loudness law with the speaker volume it applies.
  From the louder channel's volume G: no boost from -6 dB up, below it
  0.5*(-6-G)*scale dB at 300 Hz and 0.15*(-6-G)*scale dB at 2 kHz,
  scale = 1/sqrt((G/x0)^2k + 1); RBJ S = 1 shelves in transposed direct form
  II; on a volume change the ten coefficients glide with a 30 ms pole every
  sample until all are within 0.01, then each channel is multiplied by its own
  volume, ramped with the same pole and held within 1e-4. Three edge
  cases are handled on purpose (the file header): the law is clamped at -120 dB so
  volume 0 gives no NaN, a ramp settling at 0 goes to exactly 0 (PipeWire has
  no separate mute), and a reset starts at the target instead of gliding.
  Latency 0.

## Features

`internals` exposes what the tests look inside with: per-sample traces of
ControlFreak, Mozart and the DualBand, forced coefficients and gains, and
injected state. The plugins do not use it.

## Tests

`cargo test --release` (`make check` in `..`). Release, because the thermal
points run 600 s of four-channel audio each.

- `tests/thermal.rs`: the derived coefficients to 7 decimals (tweeter a_fast
  0.9958420, a_slow 0.9999380; woofer 0.9980788, 0.9999627; k_attack 0.028372,
  k_release 0.001151); a full-scale sine settling at 0.4065 (-7.82 dB) in the
  tweeter and 0.4635 (-6.68 dB) in the woofer against the 0.408 / 0.467 the
  model predicts; unity at amplitude 0.40, below the 1.31 W / 1.99 W knee; from
  cold, both pairs hitting the 0.1 floor and recovering; and with a different
  sine in every speaker, `Pairs` and `CrossPairs` putting each pair on one gain.
- `tests/controlfreak.rs`: the derived fields, unity and the exact 22-sample
  delay below the knee, the output peak held at the makeup for +6 and +20 dBFS
  bursts, the slow factor's fall and 10 s release, the reset transient, the
  detector on steady tones and steps, and bypass. 24 assertions.
- `tests/mozart.rs`: the derived fields (exact bits for the attack and
  release), the 13-row transfer-curve table, the 74-sample latency, the
  +15.05 dB start gain, the gain held on a steady quiet tone, the attack curve
  at -10 dBFS and bypass. 28 assertions.
- `tests/dualband.rs`: the crossover's exact bits, the derived set, both
  bands' curve tables, +6 dB on quiet input, the low band compressing a loud
  80 Hz tone and the high band a loud 5 kHz one with the output following
  each, and bypass. 18 assertions.
- `tests/buzzkill.rs`: the derived set, the threshold, the exact 220-sample
  delay while idle, a loud 100 Hz tone notched at 100 Hz with the output
  dropping by the notch's gain, bypass; and blocks leaving 1 to 4 frames after
  the 512-frame split with the notches running. 9 assertions and the
  tiny-block run.
- `tests/loudness.rs`: k, x0 and the pole, the -30 dB coefficients, no boost
  above the offset, a finite law at volume 0, unity at full volume, a steady
  100 Hz and 1 kHz tone at -36 dB against the analytic response, the glide
  after a step to -20 dB, mute to exactly 0 and back, and balance (the law at
  the louder channel, each channel at its own gain). 20 assertions.

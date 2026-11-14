// SPDX-License-Identifier: MIT
//! ControlFreak on test signals, against expected values.

#![allow(clippy::excessive_precision)] // expected values, to all their digits

mod common;
use common::{sine1k, Checks, BLOCK, SR};
use dsp::controlfreak::{Config, ControlFreak};

const SRU: usize = SR as usize;
const MAKEUP: f32 = 0.98855311;
const REF: f32 = 0.15848932;

struct Run {
    out_peak: f32,
    min_gain: f32,
    last_slow: f32,
    max_err_delay: f32,
}

/// A stereo 1 kHz sine of amplitude amp(t), the right channel inverted, in
/// BLOCK pieces. From sample `skip` on: the output peak, the deepest gain and
/// the error against the input delayed by 22 samples; and the last slow
/// factor. `skip` leaves out the reset transient: the FIR
/// starts from zero (g ramps up over 21 samples) and the lookahead history is
/// primed with 1.0 (g = makeup for 66 samples).
fn run(
    cf: &mut ControlFreak,
    amp: fn(usize) -> f32,
    samples: usize,
    skip: usize,
    compare_delay: bool,
) -> Run {
    let mut r = Run {
        out_peak: 0.0,
        min_gain: 1.0,
        last_slow: 1.0,
        max_err_delay: 0.0,
    };
    let mut hist = [0.0f32; 64];
    let mut t = 0;
    cf.set_trace(true);
    while t < samples {
        let n = (samples - t).min(BLOCK);
        let in0: Vec<f32> = (0..n).map(|i| amp(t + i) * sine1k(t + i)).collect();
        let mut o0 = in0.clone();
        let mut o1: Vec<f32> = in0.iter().map(|v| -v).collect();
        cf.process(&mut [&mut o0, &mut o1]);
        let tr = cf.trace().unwrap();
        for i in 0..n {
            let at = t + i;
            if at >= skip {
                r.out_peak = r.out_peak.max(o0[i].abs());
                r.min_gain = r.min_gain.min(tr.gain[i]);
                if compare_delay {
                    let want = if i >= 22 { in0[i - 22] } else { hist[42 + i] };
                    r.max_err_delay = r.max_err_delay.max((o0[i] - want).abs());
                }
            }
        }
        if n >= 22 {
            hist[42..64].copy_from_slice(&in0[n - 22..]);
        }
        r.last_slow = tr.slow[n - 1];
        t += n;
    }
    cf.set_trace(false);
    r
}

fn amp_quiet(_: usize) -> f32 {
    0.5
}
fn amp_step_up(t: usize) -> f32 {
    if t < SRU {
        0.1
    } else {
        0.2
    } // +6 dB at 1 s
}
fn amp_step_down(t: usize) -> f32 {
    if t < SRU {
        0.2
    } else {
        0.1
    }
}
fn amp_burst(t: usize) -> f32 {
    if (SRU..SRU + SRU / 10).contains(&t) {
        2.0
    } else {
        0.5
    }
}
fn amp_huge(t: usize) -> f32 {
    if (SRU..SRU + SRU / 10).contains(&t) {
        10.0
    } else {
        0.5
    }
}

/// The detector alone on a contiguous 1 kHz sine: attack samples before and
/// after `at`, and the first one after.
fn detector_run(
    cf: &mut ControlFreak,
    amp: fn(usize) -> f32,
    samples: usize,
    at: usize,
    attack: f32,
) -> (usize, usize, usize) {
    let (mut before, mut after, mut first) = (0, 0, 0);
    let mut t = 0;
    cf.set_trace(true);
    while t < samples {
        let n = (samples - t).min(BLOCK);
        let mut a: Vec<f32> = (0..n).map(|i| amp(t + i) * sine1k(t + i)).collect();
        let mut b = a.clone();
        cf.process(&mut [&mut a, &mut b]);
        for (i, &c) in cf.trace().unwrap().coef.iter().enumerate() {
            if c != attack {
                continue;
            }
            if t + i < at {
                before += 1;
            } else {
                if after == 0 {
                    first = t + i;
                }
                after += 1;
            }
        }
        t += n;
    }
    cf.set_trace(false);
    (before, after, first)
}

#[test]
fn controlfreak() {
    let mut c = Checks::default();
    let mut cfg = Config::mbp143(SR);
    let mut cf = ControlFreak::new(&cfg).unwrap();

    println!("derived fields:");
    let d = cf.derived();
    c.check(d.window == 67, "window", d.window as f64, 67.0);
    c.check(d.latency == 22, "latency", d.latency as f64, 22.0);
    c.check(d.hold == 9, "hold, detector samples", d.hold as f64, 9.0);
    c.check(
        d.rms_window == 33,
        "RMS window, detector samples",
        d.rms_window as f64,
        33.0,
    );
    c.check(d.makeup == MAKEUP, "makeup", d.makeup as f64, MAKEUP as f64);
    c.check(d.ceiling == 1.0, "ceiling", d.ceiling as f64, 1.0);
    c.check(d.ref_ == REF, "ref", d.ref_ as f64, REF as f64);
    c.check(
        d.attack == 8.3982944e-05,
        "attack (float32)",
        d.attack as f64,
        8.3982944e-05,
    );
    c.check(
        d.release == 2.2649765e-06,
        "release (0.11% short of exact)",
        d.release as f64,
        2.2649765e-06,
    );
    c.check(
        d.threshold == 1.7782794,
        "activity threshold",
        d.threshold as f64,
        1.7782794,
    );

    println!("behaviour:");
    let r = run(&mut cf, amp_quiet, SRU * 2, 128, true);
    c.check(
        r.max_err_delay < 1e-6,
        "-6 dBFS: out = in delayed 22 samples, max error",
        r.max_err_delay as f64,
        0.0,
    );
    c.check(
        r.min_gain > 0.9999,
        "-6 dBFS: gain stays at 1",
        r.min_gain as f64,
        1.0,
    );
    cf.reset();
    let r = run(&mut cf, amp_quiet, 128, 0, false);
    c.check(
        r.min_gain < 0.01,
        "after reset: the FIR starts from zero",
        r.min_gain as f64,
        0.0092,
    );
    cf.reset();
    let r = run(&mut cf, amp_burst, SRU * 3, 128, false);
    c.check(
        r.out_peak <= MAKEUP * 1.002,
        "+6 dBFS burst: output peak <= makeup (0.2%)",
        r.out_peak as f64,
        MAKEUP as f64,
    );
    c.check(
        r.min_gain < 0.5 && r.min_gain > REF,
        "+6 dBFS burst: fast factor alone, gain within (ref, 0.5)",
        r.min_gain as f64,
        0.4943,
    );
    c.check(
        r.last_slow == 1.0,
        "+6 dBFS burst: slow factor never moves (needs +15.9)",
        r.last_slow as f64,
        1.0,
    );
    cf.reset();
    let r = run(&mut cf, amp_huge, SRU + SRU / 10 + SRU / 2, 128, false);
    c.check(
        r.out_peak <= MAKEUP * 1.002,
        "+20 dBFS burst: output peak <= makeup",
        r.out_peak as f64,
        MAKEUP as f64,
    );
    c.check(
        r.min_gain < REF,
        "+20 dBFS burst: gain below ref, the slow factor cuts",
        r.min_gain as f64,
        0.0989,
    );
    c.check(
        (r.last_slow - 0.642).abs() < 0.002,
        "+20 dBFS: slow 0.5 s later 1-0.376*(1-rel)^22050",
        r.last_slow as f64,
        0.642,
    );

    println!("activity detector (contiguous signals):");
    // the first half second is the start-up: the envelope starts at 0, so the
    // very first samples trigger
    cf.reset();
    let (_, after, _) = detector_run(&mut cf, amp_quiet, SRU * 3, SRU / 2, d.attack);
    c.check(
        after == 0,
        "steady -6 dBFS: release only once settled",
        after as f64,
        0.0,
    );
    cf.reset();
    let (_, after, first) = detector_run(&mut cf, amp_step_up, SRU * 2, SRU, d.attack);
    c.check(
        after >= 360,
        "+6 dB step: attack for at least the hold (9 x 40 samples)",
        after as f64,
        360.0,
    );
    c.check(
        (SRU..SRU + 1024).contains(&first),
        "+6 dB step: trigger within 1024 samples",
        first as f64 - SRU as f64,
        0.0,
    );
    cf.reset();
    let (_, after, _) = detector_run(&mut cf, amp_step_down, SRU * 2, SRU + 2, d.attack);
    c.check(
        after == 0,
        "-6 dB step: no trigger (the envelope snaps down)",
        after as f64,
        0.0,
    );

    cfg.bypass = true;
    let mut cf = ControlFreak::new(&cfg).unwrap();
    let r = run(&mut cf, amp_huge, SRU * 2, 22, true);
    c.check(
        r.max_err_delay == 0.0,
        "bypass: out = in delayed 22 samples exactly",
        r.max_err_delay as f64,
        0.0,
    );
    c.done();
}

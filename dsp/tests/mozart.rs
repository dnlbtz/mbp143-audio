// SPDX-License-Identifier: MIT
//! Mozart on test signals, against expected values.

#![allow(clippy::excessive_precision)] // expected values, to all their digits

mod common;
use common::{sine1k, Checks, BLOCK, SR};
use dsp::mozart::{Config, Mozart};

const SRU: usize = SR as usize;

/// A stereo 1 kHz sine of amplitude amp for `seconds`: the output peak over
/// the last 0.1 s divided by amp.
fn run_tone(m: &mut Mozart, amp: f32, seconds: f64) -> f64 {
    let total = (seconds * SR as f64) as usize;
    let mut peak = 0.0f64;
    let mut t = 0;
    while t < total {
        let n = (total - t).min(BLOCK);
        let mut a: Vec<f32> = (0..n).map(|i| amp * sine1k(t + i)).collect();
        let mut b = a.clone();
        m.process(&mut [&mut a, &mut b]);
        if t + n > total - SRU / 10 {
            for &v in &a {
                peak = peak.max((v as f64).abs());
            }
        }
        t += n;
    }
    peak / amp as f64
}

#[test]
fn mozart() {
    // the expected transfer curves: level, release, attack
    const TABLE: [[f32; 3]; 13] = [
        [0.0, 0.000, 0.000],
        [-6.0, 0.222, 2.667],
        [-12.0, 0.889, 5.333],
        [-18.0, 2.000, 7.868],
        [-24.0, 3.521, 9.934],
        [-30.0, 5.124, 11.515],
        [-40.0, 7.405, 13.073],
        [-50.0, 8.648, 13.284],
        [-60.0, 8.543, 12.148],
        [-70.0, 7.092, 9.665],
        [-80.0, 6.000, 6.000],
        [-90.0, 6.000, 6.000],
        [-100.0, 6.000, 6.000],
    ];
    let near = |a: f64, b: f64, tol: f64| (a - b).abs() <= tol;
    let mut c = Checks::default();
    let mut cfg = Config::mbp143(SR);
    let mut m = Mozart::new(&cfg).unwrap();
    let d = m.derived();

    println!("derived fields:");
    c.check(d.decim == 31, "decimation", d.decim as f64, 31.0);
    c.check(d.latency == 74, "latency", d.latency as f64, 74.0);
    c.check(
        d.hold == 142,
        "detector hold, decimated samples",
        d.hold as f64,
        142.0,
    );
    c.check(
        d.rms_moving && d.rms_window == 568,
        "main detector: moving mean square, 568",
        d.rms_window as f64,
        568.0,
    );
    c.check(
        d.lim_window == 14,
        "limiter detector window",
        d.lim_window as f64,
        14.0,
    );
    c.check(
        d.gain_in == 2.818383 || near(d.gain_in as f64, 2.818383, 1e-6),
        "input gain (+9 dB)",
        d.gain_in as f64,
        2.818383,
    );
    c.check(
        d.attack.to_bits() == 0x3c7b6d40,
        "attack coefficient (100 ms), exact bits",
        d.attack as f64,
        0.0153459,
    );
    c.check(
        d.release.to_bits() == 0x3a871000,
        "release coefficient (1500 ms), exact bits",
        d.release as f64,
        0.00103045,
    );
    c.check(
        near(d.curve[10] as f64, 38.7707, 1e-4),
        "knee half width",
        d.curve[10] as f64,
        38.7707,
    );
    c.check(
        near(d.curve[7] as f64, -52.3404, 1e-4),
        "attack/release crossover",
        d.curve[7] as f64,
        -52.3404,
    );

    println!("transfer curve:");
    for row in TABLE {
        let (r, a) = m.curve(row[0]);
        c.check(
            near(r as f64, row[1] as f64, 6e-4) && near(a as f64, row[2] as f64, 6e-4),
            &format!("release, attack at {:+.0} dB", row[0]),
            r as f64,
            row[1] as f64,
        );
    }

    println!("behaviour:");
    m.reset();
    let mut a = vec![0.0f32; BLOCK];
    a[100] = 1e-3;
    let mut b = a.clone();
    m.process(&mut [&mut a, &mut b]);
    let (at, pk) = a.iter().enumerate().fold((0, 0.0f32), |best, (i, &v)| {
        if v.abs() > best.1 {
            (i, v.abs())
        } else {
            best
        }
    });
    c.check(
        at == 174,
        "latency: an impulse at 100 comes out at",
        at as f64,
        174.0,
    );
    c.check(
        near((pk / 1e-3) as f64, 2.818383 * 1.9952623, 1e-3),
        "start gain: +9 dB input times the +6 dB reset state",
        (pk / 1e-3) as f64,
        5.6234,
    );
    m.reset();
    let g = 20.0 * run_tone(&mut m, 0.03162, 5.0).log10();
    c.check(
        near(g, 15.57, 0.05),
        "-30 dBFS tone, 5 s: gain held at the start-up value",
        g,
        15.57,
    );
    m.reset();
    let g = 20.0 * run_tone(&mut m, 0.3, 6.0).log10();
    c.check(
        g < 8.0,
        "-10 dBFS tone: the attack curve pulls below +9 dB",
        g,
        7.20,
    );

    cfg.bypass = true; // the plain gain (0 dB), no delay
    let mut m = Mozart::new(&cfg).unwrap();
    let input: Vec<f32> = (0..BLOCK)
        .map(|i| 0.1 * (0.05f32 * i as f32).sin())
        .collect();
    let mut a = input.clone();
    let mut b = input.clone();
    m.process(&mut [&mut a, &mut b]);
    let err = a
        .iter()
        .zip(&input)
        .fold(0.0f32, |e, (o, i)| e.max((o - i).abs()));
    c.check(
        err == 0.0,
        "bypass: out = in (plain gain 0 dB), no delay",
        err as f64,
        0.0,
    );
    c.done();
}

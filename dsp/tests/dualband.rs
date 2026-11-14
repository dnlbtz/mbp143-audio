// SPDX-License-Identifier: MIT
//! The DualBand on test signals, against expected values.

#![allow(clippy::excessive_precision)] // expected values, to all their digits

mod common;
use common::{Checks, BLOCK, SR};
use dsp::dualband::{Config, DualBand};
use dsp::mozart::{self, Mozart};

const SRU: usize = SR as usize;

/// A stereo sine through db for `seconds`: the output/input RMS ratio in dB
/// over the last 0.5 s, and the last gSub, gSuper.
fn run_tone(db: &mut DualBand, freq: f32, amp: f32, seconds: f64) -> (f64, f32, f32) {
    let total = (seconds * SR as f64) as usize;
    let (mut ein, mut eout) = (0.0f64, 0.0f64);
    let mut t = 0;
    db.set_trace(true);
    while t + BLOCK <= total {
        let input: Vec<f32> = (0..BLOCK)
            .map(|i| {
                amp * (2.0f32 * std::f32::consts::PI * freq * (t + i) as f32 / SR as f32).sin()
            })
            .collect();
        let mut a = input.clone();
        let mut b = input.clone();
        db.process(&mut [&mut a, &mut b]);
        if t > total - SRU / 2 {
            for (x, y) in input.iter().zip(&a) {
                ein += (x * x) as f64;
                eout += (y * y) as f64;
            }
        }
        t += BLOCK;
    }
    let tr = db.trace().unwrap();
    // bypassed, no block runs and there is no gain to show
    let first = |v: &[f32]| v.first().copied().unwrap_or(f32::NAN);
    let (gs, gp) = (first(&tr.g_sub), first(&tr.g_super));
    db.set_trace(false);
    (10.0 * (eout / ein).log10(), gs, gp)
}

#[test]
fn dualband() {
    // the expected curves: level, sub release, sub attack, super
    // release, super attack (both bands' curves, the +6 dB offset included)
    const TABLE: [[f32; 5]; 7] = [
        [0.0, -6.250, -6.250, -0.857, 0.750],
        [-6.0, -3.000, -3.000, 0.750, 2.143],
        [-12.0, -0.250, -0.250, 2.143, 3.321],
        [-18.0, 2.000, 2.000, 3.321, 4.286],
        [-24.0, 3.750, 3.750, 4.286, 5.036],
        [-30.0, 5.000, 5.000, 5.036, 5.571],
        [-60.0, 6.000, 6.000, 6.000, 6.000],
    ];
    let mut c = Checks::default();
    let mut cfg = Config::mbp143(SR);
    let mut db = DualBand::new(&cfg).unwrap();
    let d = db.derived();

    println!("derived:");
    c.check(
        d.section[0].to_bits() == 0x3ce30658,
        "crossover k, bits",
        d.section[0] as f64,
        0.02771299,
    );
    c.check(
        d.section[3].to_bits() == 0xbf71cf9a,
        "crossover a1, bits",
        d.section[3] as f64,
        -0.944574,
    );
    c.check(
        d.order == 4 && d.decim == 31 && d.latency == 0,
        "order 4, decimation 31, latency 0",
        d.order as f64,
        4.0,
    );
    c.check(
        d.sub.rate == 1423 && d.sub.decim == 1 && !d.sub.rms_moving,
        "gain computers at 1423 Hz, no decimation, one-pole detector",
        d.sub.rate as f64,
        1423.0,
    );
    c.check(
        d.sub.curve[20].to_bits() == 0xc1900000 && d.super_.curve[20].to_bits() == 0xc1c00000,
        "high-segment thresholds -18 (low band), -24 (high band)",
        d.super_.curve[20] as f64,
        -24.0,
    );

    println!("the two bands' curves:");
    // the curves through a Mozart built from each half
    let band = |b: &mozart::Config| {
        Mozart::new(&mozart::Config {
            channels: 2,
            max_frames: 17,
            sample_rate: 1423,
            ..*b
        })
        .unwrap()
    };
    let (lo, hi) = (band(&cfg.low), band(&cfg.high));
    for row in TABLE {
        let (r0, a0) = lo.curve(row[0]);
        let (r1, a1) = hi.curve(row[0]);
        c.check(
            (r0 - row[1]).abs() < 6e-4
                && (a0 - row[2]).abs() < 6e-4
                && (r1 - row[3]).abs() < 6e-4
                && (a1 - row[4]).abs() < 6e-4,
            &format!("all four at {:+.0} dB", row[0]),
            r1 as f64,
            row[3] as f64,
        );
    }

    println!("behaviour:");
    let (g, gs, gp) = run_tone(&mut db, 1000.0, 0.001, 3.0);
    c.check(
        (g - 6.0).abs() < 0.05 && (gs - gp).abs() < 1e-6,
        "-60 dBFS: both bands at +6 dB, w = 0",
        g,
        6.02,
    );
    db.reset();
    let (g, gs, gp) = run_tone(&mut db, 80.0, 0.5, 4.0);
    c.check(
        gs < 1.0 && gp > 1.99,
        "-6 dBFS at 80 Hz: low band compresses, high band idle",
        gs as f64,
        0.8,
    );
    let want = 20.0 * (gs as f64).log10();
    c.check(
        (g - want).abs() < 1.5,
        "  and the output follows the low band's gain",
        g,
        want,
    );
    db.reset();
    let (g, gs, gp) = run_tone(&mut db, 5000.0, 0.5, 4.0);
    c.check(
        gp < 1.9 && gs > 1.99,
        "-6 dBFS at 5 kHz: high band compresses, low band idle",
        gp as f64,
        1.6,
    );
    let want = 20.0 * (gp as f64).log10();
    c.check(
        (g - want).abs() < 0.05,
        "  and the output leaves at the high band's gain",
        g,
        want,
    );

    cfg.bypass = true;
    let mut db = DualBand::new(&cfg).unwrap();
    let (g, _, _) = run_tone(&mut db, 1000.0, 0.3, 1.0);
    c.check(g.abs() < 1e-9, "bypass: out = in", g, 0.0);
    c.done();
}

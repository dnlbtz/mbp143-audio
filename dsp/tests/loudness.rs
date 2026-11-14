// SPDX-License-Identifier: MIT
//! Loudness against expected values.

#![allow(clippy::excessive_precision)] // expected values, to all their digits

mod common;
use common::{Checks, BLOCK, SR};
use dsp::loudness::{Config, Loudness};

fn db(x: f64) -> f64 {
    20.0 * x.log10()
}

/// |H| of the two shelves in coef[10] at f.
fn response(c: &[f32; 10], f: f64) -> f64 {
    // z = exp(-2 pi i f / SR), as complex pairs
    let w = -2.0 * std::f64::consts::PI * f / SR as f64;
    let (zr, zi) = (w.cos(), w.sin());
    let (z2r, z2i) = (zr * zr - zi * zi, 2.0 * zr * zi);
    let mut h = 1.0f64;
    for s in 0..2 {
        let q = &c[5 * s..5 * s + 5];
        let (nr, ni) = (
            q[0] as f64 + q[1] as f64 * zr + q[2] as f64 * z2r,
            q[1] as f64 * zi + q[2] as f64 * z2i,
        );
        let (dr, di) = (
            1.0 + q[3] as f64 * zr + q[4] as f64 * z2r,
            q[3] as f64 * zi + q[4] as f64 * z2i,
        );
        h *= (nr * nr + ni * ni).sqrt() / (dr * dr + di * di).sqrt();
    }
    h
}

/// A stereo sine through ld for `seconds`: the output/input RMS ratio over
/// the last half second, left channel. t0 carries the phase across calls.
fn run_tone(ld: &mut Loudness, freq: f64, amp: f32, seconds: f64, t0: &mut usize) -> f64 {
    let total = (seconds * SR as f64) as usize;
    let (mut ein, mut eout) = (0.0f64, 0.0f64);
    let mut t = 0;
    while t + BLOCK <= total {
        let input: Vec<f32> = (0..BLOCK)
            .map(|i| {
                amp * (2.0 * std::f64::consts::PI * freq * (*t0 + t + i) as f64 / SR as f64).sin()
                    as f32
            })
            .collect();
        let mut l = input.clone();
        let mut r = input.clone();
        ld.process(&mut [&mut l, &mut r]);
        if t >= total - SR as usize / 2 {
            for (x, y) in input.iter().zip(&l) {
                ein += *x as f64 * *x as f64;
                eout += *y as f64 * *y as f64;
            }
        }
        t += BLOCK;
    }
    *t0 += t;
    (eout / ein).sqrt()
}

fn ones(ld: &mut Loudness) -> [Vec<f32>; 2] {
    let mut a = vec![1.0f32; BLOCK];
    let mut b = vec![1.0f32; BLOCK];
    ld.process(&mut [&mut a, &mut b]);
    [a, b]
}

#[test]
fn loudness() {
    const WANT_LO: [f32; 5] = [1.01636708, -1.95266068, 0.93933049, -1.95365417, 0.95470409];
    const WANT_HI: [f32; 5] = [1.33219989, -2.17253987, 0.91794365, -1.56915000, 0.64675368];
    let mut c = Checks::default();
    let cfg = Config::mbp143(SR);
    let mut ld = Loudness::new(&cfg).unwrap();
    let g36 = 10f64.powf(-36.0 / 20.0);
    let g20 = 10f64.powf(-20.0 / 20.0);

    println!("derived");
    let st = *ld.state();
    c.check((st.k - 1.053574).abs() < 5e-7, "k", st.k, 1.053574);
    c.check((st.x0 + 35.7294).abs() < 5e-5, "x0", st.x0, -35.7294);
    c.check(
        (st.smooth - 0.99924443).abs() < 1e-8,
        "coefficient pole, 30 ms",
        st.smooth as f64,
        0.99924443,
    );

    println!("the law");
    let (_, _, co) = ld.law(10f64.powf(-30.0 / 20.0) as f32);
    let dev = (0..5).fold(0.0f32, |d, k| {
        d.max((co[k] - WANT_LO[k]).abs())
            .max((co[5 + k] - WANT_HI[k]).abs())
    });
    c.check(
        dev < 5e-7,
        "-30 dB: the ten coefficients (8 decimals)",
        dev as f64,
        0.0,
    );
    let (lo, hi, _) = ld.law(1e-3);
    c.check(
        (lo - 13.5317).abs() < 5e-3 && (hi - 4.0595).abs() < 5e-3,
        "-60 dB: low boost 0.5*54*0.501187",
        lo as f64,
        13.532,
    );
    let (lo, hi, _) = ld.law(0.6);
    c.check(
        lo == 0.0 && hi == 0.0,
        "-4.44 dB, above the offset: no boost",
        lo as f64,
        0.0,
    );
    let (lo, hi, _) = ld.law(0.0);
    c.check(
        lo.is_finite() && hi.is_finite() && lo > 0.0,
        "volume 0: clamped at -120 dB, finite",
        lo as f64,
        15.3,
    );

    println!("audio");
    ld.reset();
    let input: Vec<f32> = (0..BLOCK)
        .map(|i| (0.37 * i as f64).sin() as f32 * 0.5)
        .collect();
    let (mut a, mut b) = (input.clone(), input.clone());
    ld.process(&mut [&mut a, &mut b]);
    let dev = a
        .iter()
        .zip(&input)
        .fold(0.0f32, |d, (o, i)| d.max((o - i).abs()));
    c.check(dev == 0.0, "full volume: out = in exactly", dev as f64, 0.0);

    let mut t = 0;
    ld.set_volume(g36 as f32, g36 as f32);
    ld.reset();
    let r = run_tone(&mut ld, 100.0, 0.5, 1.0, &mut t);
    let want = db(response(&ld.state().target, 100.0) * g36);
    c.check(
        (db(r) - want).abs() < 0.01,
        "-36 dB, 100 Hz: volume plus the shelves, dB",
        db(r),
        want,
    );
    c.check(
        db(r) + 36.0 > 9.5,
        "  which is a boost of more than 9.5 dB",
        db(r) + 36.0,
        10.0,
    );
    let r = run_tone(&mut ld, 1000.0, 0.5, 1.0, &mut t);
    let want = db(response(&ld.state().target, 1000.0) * g36);
    c.check(
        (db(r) - want).abs() < 0.01,
        "-36 dB, 1 kHz: between the shelves, dB",
        db(r),
        want,
    );

    // a step from -36 to -20 dB: the gain glides with the 30 ms pole
    ld.set_volume(g20 as f32, g20 as f32);
    let mut n = 0;
    while n + BLOCK <= 1323 / BLOCK * BLOCK + BLOCK {
        ones(&mut ld);
        n += BLOCK;
    }
    let st = *ld.state();
    let want = g20 + (g36 - g20) * (st.smooth as f64).powf(n as f64);
    // the float recursion against pow in double: a few ulp per step
    c.check(
        (st.gain[0] as f64 - want).abs() < 1e-5,
        "volume step: gain after 1536 samples, one-pole",
        st.gain[0] as f64,
        want,
    );
    c.check(
        st.gliding,
        "  the shelves still gliding at 35 ms",
        st.gliding as u8 as f64,
        1.0,
    );
    for _ in 0..2 * SR as usize / BLOCK {
        ones(&mut ld);
    }
    let st = *ld.state();
    c.check(
        !st.gliding,
        "  and settled by 2 s",
        st.gliding as u8 as f64,
        0.0,
    );
    c.check(
        (st.gain[0] - g20 as f32).abs() < 1e-4,
        "  the gain at its target",
        st.gain[0] as f64,
        g20,
    );
    let (lo, _, _) = ld.law(g20 as f32);
    c.check(
        st.low_db == lo,
        "  the law at -20 dB",
        st.low_db as f64,
        lo as f64,
    );

    // mute, as PipeWire sends it: volume 0
    ld.set_volume(0.0, 0.0);
    let mut nan = false;
    let mut last = [vec![], vec![]];
    for _ in 0..SR as usize / BLOCK {
        last = ones(&mut ld);
        nan |= last.iter().flatten().any(|v| !v.is_finite());
    }
    c.check(
        !nan && last[0][BLOCK - 1] == 0.0 && last[1][BLOCK - 1] == 0.0,
        "volume 0: settles to exactly 0, no NaN",
        last[0][BLOCK - 1] as f64,
        0.0,
    );
    ld.set_volume(g20 as f32, g20 as f32);
    for _ in 0..SR as usize / BLOCK {
        ones(&mut ld);
    }
    c.check(
        (ld.state().gain[0] - g20 as f32).abs() < 1e-4,
        "  and comes back from 0",
        ld.state().gain[0] as f64,
        g20,
    );

    // balance: the law follows the louder channel
    ld.set_volume(0.1, 0.03);
    ones(&mut ld);
    c.check(
        ld.state().master_gain == 0.1,
        "balance 0.1/0.03: the law at the louder",
        ld.state().master_gain as f64,
        0.1,
    );
    for _ in 0..SR as usize / BLOCK {
        ones(&mut ld);
    }
    c.check(
        (ld.state().gain[1] - 0.03).abs() < 1e-4,
        "  each channel at its own gain",
        ld.state().gain[1] as f64,
        0.03,
    );
    c.done();
}

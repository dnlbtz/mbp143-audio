// SPDX-License-Identifier: MIT
//! BuzzKill on test signals, against expected values.

#![allow(clippy::excessive_precision)] // expected values, to all their digits

mod common;
use common::{Checks, BLOCK, SR};
use dsp::buzzkill::{BuzzKill, Config};

const SRU: usize = SR as usize;

/// A stereo sine through bk: the output/input RMS ratio in dB over the last
/// second (output aligned by the latency), and the largest deviation from the
/// input delayed by `lat` samples.
fn run_tone(bk: &mut BuzzKill, freq: f32, amp: f32, seconds: f64, lat: usize) -> (f64, f64) {
    let total = (seconds * SR as f64) as usize;
    let mut hist = vec![0.0f32; 4 * SRU];
    let (mut ein, mut eout, mut dev) = (0.0f64, 0.0f64, 0.0f64);
    let mut t = 0;
    while t + BLOCK <= total {
        let input: Vec<f32> = (0..BLOCK)
            .map(|i| {
                amp * (2.0f32 * std::f32::consts::PI * freq * (t + i) as f32 / SR as f32).sin()
            })
            .collect();
        for (i, &v) in input.iter().enumerate() {
            hist[(t + i) % (4 * SRU)] = v;
        }
        let mut a = input.clone();
        let mut b = input;
        bk.process(&mut [&mut a, &mut b]);
        for (i, &o) in a.iter().enumerate() {
            let at = t + i;
            if at >= lat {
                let want = hist[(at - lat) % (4 * SRU)] as f64;
                dev = dev.max((o as f64 - want).abs());
                if at > total - SRU {
                    ein += want * want;
                    eout += o as f64 * o as f64;
                }
            }
        }
        t += BLOCK;
    }
    (10.0 * (eout / ein).log10(), dev)
}

#[test]
fn buzzkill() {
    let mut c = Checks::default();
    let mut cfg = Config::mbp143(SR);
    let mut bk = BuzzKill::new(&cfg).unwrap();
    let d = *bk.derived();

    println!("derived:");
    c.check(
        d.decim == 4 && d.fft_size == 512 && d.hop == 128,
        "decimation 4, 512 points, hop 128",
        d.hop as f64,
        128.0,
    );
    c.check(
        d.bins == 116 && (d.bin_width - 21.5332).abs() < 1e-4,
        "116 bins of 21.5332 Hz",
        d.bins as f64,
        116.0,
    );
    c.check(
        d.notch_decim == 5 && d.filters == 50,
        "the bank at 8820 Hz, 50 filters",
        d.filters as f64,
        50.0,
    );
    c.check(
        (d.threshold[4] - 0.153659).abs() < 5e-7,
        "threshold at bin 4",
        d.threshold[4] as f64,
        0.153659,
    );
    c.check(
        d.delay1 + d.delay2 == 220,
        "latency through the idle path, samples",
        (d.delay1 + d.delay2) as f64,
        220.0,
    );

    println!("behaviour:");
    let lat = (d.delay1 + d.delay2) as usize;
    let (_, dev) = run_tone(&mut bk, 1000.0, 0.1, 2.0, lat);
    c.check(
        dev == 0.0,
        "-20 dBFS at 1 kHz: out = in delayed 220 samples, exactly",
        dev,
        0.0,
    );
    bk.reset();
    let (g, _) = run_tone(&mut bk, 100.0, 0.2849, 20.0, lat);
    let st = bk.state();
    let best = (1..d.filters as usize).fold(0, |b, j| {
        if st.filter_gain[j] < st.filter_gain[b] {
            j
        } else {
            b
        }
    });
    c.check(
        (st.filter_freq[best] - 100.0).abs() < 0.05,
        "loud 100 Hz: one notch at 100 Hz",
        st.filter_freq[best] as f64,
        100.0,
    );
    let want = 20.0 * (st.filter_gain[best] as f64).log10();
    c.check(
        (g - want).abs() < 0.1,
        "  and the output drops by the notch's gain, dB",
        g,
        want,
    );

    cfg.bypass = true;
    let mut bk = BuzzKill::new(&cfg).unwrap();
    let (_, dev) = run_tone(&mut bk, 100.0, 0.5, 1.0, 0);
    c.check(dev == 0.0, "bypass: out = in", dev, 0.0);
    c.done();
}

/// A host block that leaves 1 to 4 frames after the 512-frame split, while a
/// notch is active: the empty bank block is skipped, not read past.
#[test]
fn tiny_blocks_with_notches_running() {
    let mut bk = BuzzKill::new(&Config::mbp143(SR)).unwrap();
    let mut t = 0usize;
    for (k, n) in [512usize, 513, 1, 2, 3, 4, 5, 515]
        .iter()
        .cycle()
        .take(4000)
        .enumerate()
    {
        let mut a: Vec<f32> = (0..*n)
            .map(|i| {
                0.3 * (2.0f32 * std::f32::consts::PI * 100.0 * (t + i) as f32 / SR as f32).sin()
            })
            .collect();
        let mut b = a.clone();
        bk.process(&mut [&mut a, &mut b]);
        assert!(a.iter().all(|v| v.is_finite()), "block {k}");
        t += n;
    }
    assert!(
        bk.state().active > 0,
        "the notches never ran: the test did not reach the case"
    );
}

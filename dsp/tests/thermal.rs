// SPDX-License-Identifier: MIT
//! The thermal model on a sine, against expected values: the
//! derived coefficients, the steady-state gain at full scale (0.408 tweeter,
//! 0.467 woofer), unity below the 1.31 W / 1.99 W knee, the 0.1 override past
//! threshold+margin; gain sharing with different signals in each speaker; a
//! pause against silence; and a slow sound card, which is not a pause.

#![allow(clippy::excessive_precision)] // expected values, to all their digits

mod common;
use common::Checks;
use dsp::thermal::{Config, Sharing, Thermal};
use std::time::{Duration, Instant};

struct Settled {
    gain: [f32; 4],
    power: [f32; 4],
    min_gain: [f32; 4],
}

/// A sine of `amp` on all four channels for `seconds`, in 512-frame chunks
/// as the plugin hands them over; `min_gain` is the lowest gain seen on the way,
/// which is how the 0.1 override shows up.
fn run(cfg: &Config, amp: f64, freq: f64, seconds: f64) -> Settled {
    let mut t = Thermal::new(cfg).expect("thermal");
    let mut buf = vec![vec![0.0f32; 512]; 4];
    let mut phase = 0.0f64;
    let dphase = 2.0 * std::f64::consts::PI * freq / cfg.sample_rate as f64;
    let total = (seconds * cfg.sample_rate as f64) as usize;
    let mut min_gain = [1.0f32; 4];
    let mut done = 0;
    while done < total {
        let n = (total - done).min(512);
        for i in 0..n {
            let v = (amp * phase.sin()) as f32;
            phase += dphase;
            if phase > 2.0 * std::f64::consts::PI {
                phase -= 2.0 * std::f64::consts::PI;
            }
            for b in buf.iter_mut() {
                b[i] = v;
            }
        }
        let mut io: Vec<&mut [f32]> = buf.iter_mut().map(|b| &mut b[..n]).collect();
        t.process(&mut io);
        done += n;
        for (ch, m) in min_gain.iter_mut().enumerate() {
            *m = m.min(t.gain(ch));
        }
    }
    Settled {
        gain: std::array::from_fn(|ch| t.gain(ch)),
        power: std::array::from_fn(|ch| t.power(ch)),
        min_gain,
    }
}

#[test]
fn derived_coefficients() {
    let mut c = Checks::default();
    let t = Thermal::new(&Config::mbp143(44100)).unwrap();
    let d = t.derived(0);
    c.near("tweeter 1/R_e", d.inv_re as f64, 0.196078, 1e-5);
    c.near("tweeter a_fast", d.a_fast as f64, 0.99584200, 1e-7);
    c.near("tweeter a_slow", d.a_slow as f64, 0.99993797, 1e-7);
    c.near("k_attack (800 ms)", d.k_attack as f64, 0.028372, 1e-6);
    c.near("k_release (20 s)", d.k_release as f64, 0.001151, 1e-6);
    let d = t.derived(2);
    c.near("woofer 1/R_e", d.inv_re as f64, 0.227273, 1e-5);
    c.near("woofer a_fast", d.a_fast as f64, 0.99807877, 1e-7);
    c.near("woofer a_slow", d.a_slow as f64, 0.99996266, 1e-7);
    c.done();
}

#[test]
fn steady_state_at_full_scale() {
    let mut c = Checks::default();
    let s = run(&Config::mbp143(44100), 1.0, 1000.0, 600.0);
    c.near(
        "tweeter gain, full-scale sine",
        s.gain[0] as f64,
        0.408,
        0.01,
    );
    c.near(
        "woofer gain, full-scale sine",
        s.gain[2] as f64,
        0.467,
        0.01,
    );
    c.near(
        "tweeter gain in dB",
        20.0 * (s.gain[0] as f64).log10(),
        -7.8,
        0.25,
    );
    c.near(
        "woofer gain in dB",
        20.0 * (s.gain[2] as f64).log10(),
        -6.6,
        0.25,
    );
    c.near(
        "sharing: tweeter L == tweeter R",
        s.gain[1] as f64,
        s.gain[0] as f64,
        1e-6,
    );
    c.near(
        "sharing: woofer L == woofer R",
        s.gain[3] as f64,
        s.gain[2] as f64,
        1e-6,
    );
    c.near("tweeter P at full scale", s.power[0] as f64, 7.941, 0.05);
    c.near("woofer P at full scale", s.power[2] as f64, 9.205, 0.05);
    c.done();
}

#[test]
fn unity_below_the_knee() {
    let mut c = Checks::default();
    let s = run(&Config::mbp143(44100), 0.40, 1000.0, 600.0);
    c.near("tweeter gain at amp 0.40", s.gain[0] as f64, 1.0, 0.01);
    c.near("woofer gain at amp 0.45", s.gain[2] as f64, 1.0, 0.01);
    c.near("tweeter P at amp 0.40", s.power[0] as f64, 1.274, 0.02);
    c.done();
}

/// From cold, a full-scale sine outruns the 800 ms follower: the output model
/// passes threshold + margin = 145 C and the gain is clamped to the 0.1 floor
/// before the loop catches it.
#[test]
fn hard_override_from_cold() {
    let mut c = Checks::default();
    let s = run(&Config::mbp143(44100), 1.0, 1000.0, 30.0);
    c.near(
        "tweeter hit the 0.1 floor from cold",
        s.min_gain[0] as f64,
        0.1,
        1e-5,
    );
    c.near(
        "woofer hit the 0.1 floor from cold",
        s.min_gain[2] as f64,
        0.1,
        1e-5,
    );
    c.check(
        s.gain[0] > 0.11,
        "and recovered by 30 s",
        s.gain[0] as f64,
        0.11,
    );
    c.done();
}

/// Gain sharing with a different signal in every speaker: `Pairs` links the
/// tweeters and the woofers, `CrossPairs` the two left and the two right
/// speakers.
#[test]
fn sharing_links_the_pairs() {
    let mut c = Checks::default();
    let amps = [1.0f64, 0.6, 1.2, 0.5];
    for (mode, pairs) in [
        (Sharing::Pairs, [(0, 1), (2, 3)]),
        (Sharing::CrossPairs, [(0, 2), (1, 3)]),
    ] {
        let mut cfg = Config::mbp143(44100);
        cfg.sharing = mode;
        let mut t = Thermal::new(&cfg).unwrap();
        let mut phase = 0.0f64;
        let w = 2.0 * std::f64::consts::PI * 1000.0 / 44100.0;
        let mut buf = vec![vec![0.0f32; 512]; 4];
        for _ in 0..(20 * 44100 / 512) {
            for i in 0..512 {
                for (ch, b) in buf.iter_mut().enumerate() {
                    b[i] = (amps[ch] * phase.sin()) as f32;
                }
                phase += w;
            }
            let mut io: Vec<&mut [f32]> = buf.iter_mut().map(|b| &mut b[..]).collect();
            t.process(&mut io);
        }
        let g: [f32; 4] = std::array::from_fn(|ch| t.gain(ch));
        c.check(
            g.iter().any(|&v| v < 0.99),
            &format!("{mode:?}: the loud speakers are limited"),
            g[0].min(g[2]) as f64,
            0.5,
        );
        for (i, j) in pairs {
            c.check(
                g[i] == g[j],
                &format!("{mode:?}: speakers {i} and {j} share one gain"),
                g[j] as f64,
                g[i] as f64,
            );
        }
        let (other_i, other_j) = (pairs[0].0, pairs[1].0);
        c.check(
            g[other_i] != g[other_j],
            &format!("{mode:?}: the two pairs differ"),
            g[other_j] as f64,
            g[other_i] as f64,
        );
        // a pause lets every gain recover, each from its own state
        t.advance(200.0);
        for (i, j) in pairs {
            c.check(
                t.gain(i) == t.gain(j),
                &format!("{mode:?}: speakers {i} and {j} still share it after a pause"),
                t.gain(j) as f64,
                t.gain(i) as f64,
            );
        }
    }
    c.done();
}

/// A sine of `amp` at 1 kHz on all four channels for `seconds`, in 512-frame
/// chunks, into a model that is already running at 44100 Hz.
fn feed(t: &mut Thermal, amp: f64, seconds: f64) {
    let rate = 44100.0;
    let w = 2.0 * std::f64::consts::PI * 1000.0 / rate;
    let mut buf = vec![vec![0.0f32; 512]; 4];
    let total = (seconds * rate) as usize;
    let mut done = 0;
    while done < total {
        let n = (total - done).min(512);
        for i in 0..n {
            let v = (amp * (w * (done + i) as f64).sin()) as f32;
            for b in buf.iter_mut() {
                b[i] = v;
            }
        }
        let mut io: Vec<&mut [f32]> = buf.iter_mut().map(|b| &mut b[..n]).collect();
        t.process(&mut io);
        done += n;
    }
}

/// A pause cools the model as much as the same time of silence does: one
/// copy is fed 10 s of silence, the other is advanced by 10 s, and both then
/// agree on the temperature. The plugin relies on this when playback pauses,
/// since the host then stops calling it.
///
/// The gain agrees less well. advance() lets it recover from the start of
/// the pause, while in silence it is held down until the speaker is below
/// its threshold again. After 10 s the advanced model is 0.1 dB louder; after
/// a pause of 1 to 2 s it is up to 1.3 dB louder for the next second or two.
#[test]
fn a_pause_cools_like_silence() {
    let mut c = Checks::default();
    // the same input into two models gives two identical models: the audio
    // runs far ahead of the clock, so neither sees a pause of its own
    let mut fed = Thermal::new(&Config::mbp143(44100)).unwrap();
    let mut paused = Thermal::new(&Config::mbp143(44100)).unwrap();
    feed(&mut fed, 1.0, 20.0);
    feed(&mut paused, 1.0, 20.0);
    let hot = fed.temperature(0);
    feed(&mut fed, 0.0, 10.0);
    paused.advance(10_000.0);
    for ch in [0, 2] {
        c.check(
            paused.temperature(ch) < hot - 1.0,
            &format!("ch{ch}: the pause cooled it"),
            paused.temperature(ch) as f64,
            hot as f64,
        );
        c.near(
            &format!("ch{ch}: temperature after the pause, against silence"),
            paused.temperature(ch) as f64,
            fed.temperature(ch) as f64,
            0.05,
        );
    }
    // half scale, so neither gain sits on the 0.1 override, where a
    // hundredth of a degree decides
    feed(&mut fed, 0.5, 2.0);
    feed(&mut paused, 0.5, 2.0);
    for ch in [0, 2] {
        c.near(
            &format!("ch{ch}: gain 2 s into half-scale playback after it"),
            paused.gain(ch) as f64,
            fed.gain(ch) as f64,
            0.02,
        );
    }
    c.done();
}

/// Full scale at 1 kHz for `seconds` in 512-frame chunks, the call for the
/// audio at `t` seconds made at `start + clock(t)`.
fn feed_at(t: &mut Thermal, start: Instant, from: f64, seconds: f64, clock: impl Fn(f64) -> f64) {
    let rate = 44100.0;
    let w = 2.0 * std::f64::consts::PI * 1000.0 / rate;
    let mut buf = vec![vec![0.0f32; 512]; 4];
    let first = (from * rate) as usize;
    let total = (seconds * rate) as usize;
    let mut done = 0;
    while done < total {
        let n = (total - done).min(512);
        for i in 0..n {
            let v = (w * (first + done + i) as f64).sin() as f32;
            for b in buf.iter_mut() {
                b[i] = v;
            }
        }
        let now = start + Duration::from_secs_f64(clock((first + done) as f64 / rate));
        let mut io: Vec<&mut [f32]> = buf.iter_mut().map(|b| &mut b[..n]).collect();
        t.process_at(now, &mut io);
        done += n;
    }
}

/// A sound card that runs slower than the system clock takes each block a
/// little later than the audio before it lasted. That adds up, here to
/// 150 ms over 30 s, and is not a pause: the model is the same as with a
/// card that keeps time. Half a second without a call is one.
#[test]
fn a_slow_card_is_not_a_pause() {
    let mut c = Checks::default();
    let start = Instant::now();
    let mut even = Thermal::new(&Config::mbp143(44100)).unwrap();
    let mut slow = Thermal::new(&Config::mbp143(44100)).unwrap();
    feed_at(&mut even, start, 0.0, 30.0, |t| t);
    feed_at(&mut slow, start, 0.0, 30.0, |t| t * 1.005);
    for ch in [0, 2] {
        c.check(
            slow.temperature(ch) == even.temperature(ch) && slow.gain(ch) == even.gain(ch),
            &format!("ch{ch}: 5000 ppm slow for 30 s, the same temperature and gain"),
            slow.temperature(ch) as f64,
            even.temperature(ch) as f64,
        );
    }
    let mut paused = Thermal::new(&Config::mbp143(44100)).unwrap();
    feed_at(&mut paused, start, 0.0, 30.0, |t| t);
    feed_at(&mut even, start, 30.0, 0.1, |t| t);
    feed_at(&mut paused, start, 30.0, 0.1, |t| t + 0.5);
    for ch in [0, 2] {
        c.check(
            paused.temperature(ch) < even.temperature(ch) - 0.5,
            &format!("ch{ch}: 500 ms without a call cools it"),
            paused.temperature(ch) as f64,
            even.temperature(ch) as f64,
        );
    }
    c.done();
}

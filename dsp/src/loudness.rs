// SPDX-License-Identifier: MIT
//! Loudness, with the volume it carries.
//!
//! On the MacBookPro14,3 the speaker graph has no volume block of its own:
//! this is where the speaker volume is applied, after Mozart and before the
//! rest of the graph. From the louder channel's gain G (dB) the law gives two
//! shelf gains, `low_slope*(offset-G)*scale` below 300 Hz and
//! `high_slope*(offset-G)*scale` above 2 kHz, `scale = 1/sqrt((G/x0)^2k + 1)`;
//! the shelves are RBJ S = 1, transposed direct form II, and on a change their
//! ten coefficients glide toward the new set by a 30 ms one-pole every sample
//! until all are within 0.01. Then each channel is multiplied by its own
//! volume, which ramps with the same pole and holds once within 1e-4.
//!
//! Two edge cases:
//!
//! - A volume of 0 would make the law take `log10(0)`, and PipeWire does send
//!   0 (also for mute), so the law is clamped at 1e-6 (-120 dB).
//! - When a channel's ramp has settled within 1e-4 of a target of 0, the gain
//!   is set to exactly 0, since PipeWire has no separate mute to reach true
//!   silence with; and a gain of exactly 0 is not held at 0, which would stop
//!   it from ever rising again.
//!
//! `reset()` starts at the target instead of gliding from wherever the
//! coefficients were, so a stream starts with the right response.

/// The loudness law's settings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    pub sample_rate: u32,
    pub low_freq: f32,  // the low shelf's corner, Hz
    pub high_freq: f32, // the high shelf's corner, Hz
    pub low_slope: f32, // dB of low shelf per dB of volume below the offset
    pub high_slope: f32,
    pub offset: f32,    // the volume, dB, below which the shelves rise
    pub stop: f32,      // the law's shape: the volume where it stops rising
    pub stop_gain: f32, // its scale there
    pub pass: f32,      // and where it passes through half
    pub trim: [f32; 2], // per-channel gain, linear
}

impl Config {
    /// The MacBookPro14,3 speakers: shelves at 300 Hz and 2 kHz, 0.5 and
    /// 0.15 dB per dB below -6 dB.
    pub fn mbp143(sample_rate: u32) -> Self {
        Config {
            sample_rate,
            low_freq: 300.0,
            high_freq: 2000.0,
            low_slope: 0.5,
            high_slope: 0.15,
            offset: -6.0,
            stop: -60.0,
            stop_gain: -6.0,
            pass: -6.0,
            trim: [1.0, 1.0],
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct State {
    pub k: f64,             // the law's curve shape
    pub x0: f64,            // and its knee
    pub master_gain: f32,   // the gain the law last ran for
    pub low_db: f32,        // the low shelf's gain
    pub high_db: f32,       // the high shelf's gain
    pub smooth: f32,        // exp(-1/(sr*0.03))
    pub target: [f32; 10],  // low b0 b1 b2 a1 a2, high the same
    pub current: [f32; 10], // the coefficients in use, gliding to target
    pub gliding: bool,      // while they glide
    pub gain: [f32; 2],     // the per-channel volume being applied
}

pub struct Loudness {
    cfg: Config,
    st: State,
    volume: [f32; 2],      // the target volume per channel
    z: [[[f32; 2]; 2]; 2], // [section][channel][state]
}

impl Loudness {
    pub fn new(cfg: &Config) -> Option<Self> {
        if cfg.sample_rate == 0 {
            return None;
        }
        let (k, x0) = shape(cfg);
        let mut ld = Loudness {
            cfg: *cfg,
            st: State {
                k,
                x0,
                smooth: ((-1.0 / (cfg.sample_rate as f64 * 0.03)) as f32).exp(),
                ..Default::default()
            },
            volume: [1.0, 1.0],
            z: [[[0.0; 2]; 2]; 2],
        };
        ld.reset();
        Some(ld)
    }

    /// Zeroes the filter state and puts the coefficients and gains at their
    /// targets for the current volume, so a stream does not start with a glide.
    pub fn reset(&mut self) {
        self.z = [[[0.0; 2]; 2]; 2];
        self.recompute();
        self.st.current = self.st.target;
        self.st.gliding = false;
        for ch in 0..2 {
            self.st.gain[ch] = self.cfg.trim[ch] * self.volume[ch];
        }
    }

    /// The volume per channel, linear. The law follows the louder of the two.
    /// Takes effect at the next process(), which notices the changed master
    /// gain.
    pub fn set_volume(&mut self, left: f32, right: f32) {
        self.volume[0] = if left > 0.0 { left } else { 0.0 };
        self.volume[1] = if right > 0.0 { right } else { 0.0 };
    }

    pub fn state(&self) -> &State {
        &self.st
    }

    /// The law for one master gain, without touching any state: the shelf
    /// gains in dB and the ten target coefficients. For tests.
    pub fn law(&self, master_gain: f32) -> (f32, f32, [f32; 10]) {
        let (low, high) = self.gains(master_gain);
        (low, high, shelves(&self.cfg, low, high))
    }

    /// The master gain: the louder channel.
    fn master_gain(&self) -> f32 {
        if self.volume[0] > self.volume[1] {
            self.volume[0]
        } else {
            self.volume[1]
        }
    }

    /// The two shelf gains in dB for a master gain.
    fn gains(&self, master_gain: f32) -> (f32, f32) {
        let c = &self.cfg;
        let mg = if master_gain > 1e-6 {
            master_gain
        } else {
            1e-6
        };
        let g = ((mg as f64).log10() * 20.0) as f32;
        if c.offset <= g {
            return (0.0, 0.0);
        }
        let mut scale = 1.0f64;
        if 1.0 < self.st.k {
            scale = 1.0 / ((g as f64 / self.st.x0).powf(self.st.k + self.st.k) + 1.0).sqrt();
        }
        let low = ((c.low_slope * (c.offset - g)) as f64 * scale) as f32;
        let high = (((c.offset - g) * c.high_slope) as f64 * scale) as f32;
        (low, high)
    }

    fn recompute(&mut self) {
        self.st.master_gain = self.master_gain();
        let (low, high) = self.gains(self.st.master_gain);
        self.st.low_db = low;
        self.st.high_db = high;
        self.st.target = shelves(&self.cfg, low, high);
        self.st.gliding = true; // process() glides to the new set
    }

    /// Process in place, stereo.
    pub fn process(&mut self, io: &mut [&mut [f32]]) {
        let n = io[0].len();
        let a = self.st.smooth;
        let b = 1.0 - self.st.smooth;

        // the law reruns when the master gain moved
        if self.st.master_gain != self.master_gain() {
            self.recompute();
        }
        let st = &mut self.st;
        let (low, high) = st.current.split_at(5);
        let mut low = crate::Section {
            c: low.try_into().unwrap(),
        };
        let mut high = crate::Section {
            c: high.try_into().unwrap(),
        };
        for i in 0..n {
            for ch in 0..2 {
                let x = low.run(&mut self.z[0][ch], io[ch][i]);
                io[ch][i] = high.run(&mut self.z[1][ch], x);
            }
            if st.gliding {
                for k in 0..10 {
                    st.current[k] = st.target[k] * b + st.current[k] * a;
                }
                low.c.copy_from_slice(&st.current[..5]);
                high.c.copy_from_slice(&st.current[5..]);
            }
        }
        if st.gliding && (0..10).all(|k| ((st.current[k] - st.target[k]).abs() as f64) < 0.01) {
            st.current = st.target;
            st.gliding = false; // settled: the target is the response
        }

        // the volume: constant once
        // both channels are within 1e-4 of their targets, otherwise a
        // per-sample ramp with the same pole
        let mut target = [0.0f32; 2];
        let mut hold = true;
        for ch in 0..2 {
            target[ch] = self.cfg.trim[ch] * self.volume[ch];
            if !(((st.gain[ch] - target[ch]).abs() as f64) < 0.0001) {
                hold = false;
            }
        }
        if hold {
            for ch in 0..2 {
                if target[ch] == 0.0 {
                    st.gain[ch] = 0.0;
                }
                let g = st.gain[ch];
                for v in io[ch].iter_mut() {
                    *v *= g;
                }
            }
            return;
        }
        for i in 0..n {
            for ch in 0..2 {
                io[ch][i] *= st.gain[ch];
                st.gain[ch] = target[ch] * b + st.gain[ch] * a;
            }
        }
    }
}

/// The law's shape: k and x0
fn shape(c: &Config) -> (f64, f64) {
    let d4 = 10.0f64.powf(c.stop_gain as f64 / -10.0);
    let d5 = 10.0f64.powf(0.01);
    let l = (c.stop as f64 / c.pass as f64).ln();
    let k = ((d4 - 1.0) / (d5 - 1.0)).ln() / (l + l);
    if k <= 1.0 {
        (1.0, 1.0)
    } else {
        (k, c.stop as f64 / (d4 - 1.0).powf(1.0 / (k + k)))
    }
}

/// The RBJ shelves with S = 1
fn shelves(c: &Config, low_db: f32, high_db: f32) -> [f32; 10] {
    let sr = c.sample_rate as f64;
    let mut t = [0.0f32; 10];

    let a = 10.0f64.powf(low_db as f64 * 0.025) as f32;
    let w = ((c.low_freq as f64 * 6.283185307179586) / sr) as f32 as f64;
    let s = w.sin() as f32;
    let cs = w.cos() as f32;
    let q = ((a as f64 + a as f64).sqrt() * s as f64) as f32;
    let ap1 = a + 1.0;
    let am1 = a - 1.0;
    let mut u = am1 * cs;
    let v = ap1 - u;
    u = u + ap1;
    let inv = 1.0f32 / (u + q);
    let apc = ap1 * cs;
    t[0] = (v + q) * a * inv;
    t[1] = (am1 - apc) * (a + a) * inv;
    t[2] = (v - q) * a * inv;
    t[3] = (apc + am1) * -2.0 * inv;
    t[4] = (u - q) * inv;

    let a = 10.0f64.powf(high_db as f64 * 0.025) as f32;
    let w = ((c.high_freq as f64 * 6.283185307179586) / sr) as f32 as f64;
    let s = w.sin() as f32;
    let cs = w.cos() as f32;
    let q = (((a + a) as f64).sqrt() * s as f64) as f32;
    let ap1 = a + 1.0;
    let mut am1 = a - 1.0;
    let mut u = am1 * cs;
    let v = ap1 - u;
    let inv = 1.0f32 / (v + q);
    u = u + ap1;
    let apc = ap1 * cs;
    t[5] = (u + q) * a * inv;
    t[6] = (apc + am1) * a * -2.0 * inv;
    t[7] = (u - q) * a * inv;
    am1 = am1 - apc;
    t[8] = (am1 + am1) * inv;
    t[9] = (v - q) * inv;
    t
}

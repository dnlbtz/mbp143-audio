// SPDX-License-Identifier: MIT
//! ControlFreak: a lookahead peak limiter.
//!
//! Per sample: `env` = max over channels of `|in|`, the maximum of `env` over
//! a 67-sample window, `t = makeup / clip(peak, makeup, 1000*makeup)`, split
//! into a fast factor `clip(t, ref, 1)` and a slow one that falls to
//! `clip(t/ref, 0, 1)` at once and rises with a one-pole (10 s release, 270 ms
//! attack while the activity detector holds), smoothed by a 22-tap
//! raised-cosine FIR and applied to the audio delayed by 22 samples. The
//! detector (channel sum, /4, Hilbert envelope, /10, 33-sample mean square,
//! hysteresis) only chooses between the two rise coefficients. f32
//! throughout.
//!
//! Latency 22 samples at 44100 Hz.
use crate::Section;

pub const MAX_CHANNELS: usize = 4;

const DEFAULT_FRAMES: usize = 512;
const FIR_MAX: u32 = 64; // fir_ms is at most 1 ms: 44 taps at 44100
const RMS_MAX: u32 = 512;
const DOWN4: usize = 4;
const DOWN10: usize = 10;

/// The /4 decimator's anti-alias low-pass, 31 taps.
const DOWN4_TAPS: [f32; 31] = [
    0.0004920699284411967,
    -0.0007719503482803702,
    -0.0033215414732694626,
    -0.008065648376941681,
    -0.014786122366786003,
    -0.022190695628523827,
    -0.02780645340681076,
    -0.0283405352383852,
    -0.020507056266069412,
    -0.0021319263614714146,
    0.026811009272933006,
    0.06362766027450562,
    0.10308828949928284,
    0.13842910528182983,
    0.16294525563716888,
    0.17171888053417206,
    0.16294525563716888,
    0.13842910528182983,
    0.10308828949928284,
    0.06362766027450562,
    0.026811009272933006,
    -0.0021319263614714146,
    -0.020507056266069412,
    -0.0283405352383852,
    -0.02780645340681076,
    -0.022190695628523827,
    -0.014786122366786003,
    -0.008065648376941681,
    -0.0033215414732694626,
    -0.0007719503482803702,
    0.0004920699284411967,
];

/// The limiter's settings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    pub channels: u32, // one instance per branch, stereo
    pub sample_rate: u32,
    pub bypass: bool, // gain 1, the audio still delayed
    pub fir_ms: f32,  // the gain smoother's length and the audio delay
    pub lookahead_ms: f32,
    pub makeup_db: f32,    // the ceiling the peaks are held to
    pub rms_ms: f32,       // the detector's mean-square window
    pub attack_ms: f32,    // the slow factor's rise while the signal is active
    pub ceiling_db: f32,   // input pre-scale
    pub activity_db: f32,  // the detector's hysteresis
    pub release_ms: f32,   // the slow factor's rise otherwise
    pub hold_ms: f32,      // the detector's hold
    pub reference_db: f32, // where the fast and the slow factor split
}

impl Config {
    /// The MacBookPro14,3 settings, the same on the tweeter and the woofer branch:
    /// -0.1 dB ceiling, 1 ms lookahead, 0.5 ms smoothing, 16 dB split,
    /// 270 ms attack, 10 s release.
    pub fn mbp143(sample_rate: u32) -> Self {
        Config {
            channels: 2,
            sample_rate,
            bypass: false,
            fir_ms: 0.5,
            lookahead_ms: 1.0,
            makeup_db: -0.1,
            rms_ms: 30.0,
            attack_ms: 270.0,
            ceiling_db: 0.0,
            activity_db: 10.0,
            release_ms: 10000.0,
            hold_ms: 7.98186,
            reference_db: 16.0,
        }
    }
}

/// What new() derived from a Config, for a meter or a test.
#[derive(Clone, Copy, Debug, Default)]
pub struct Derived {
    pub window: u32,        // the lookahead window, 67 at 44100
    pub latency: u32,       // 22 at 44100
    pub hold: u32,          // detector samples, 9
    pub rms_window: u32,    // detector samples, 33
    pub makeup: f32,        // linear
    pub ceiling: f32,       // linear
    pub ref_: f32,          // linear
    pub attack: f32,        // one-pole coefficient
    pub release: f32,       // one-pole coefficient
    pub threshold: f32,     // the detector's upper hysteresis bound
    pub inv_threshold: f32, // and its lower
}

/// Per-sample internals of the last process() call, for a meter or a test,
/// when enabled with set_trace().
#[derive(Clone, Debug, Default)]
#[cfg(feature = "internals")]
pub struct Trace {
    pub peak: Vec<f32>, // the lookahead max
    pub coef: Vec<f32>, // the rise coefficient
    pub slow: Vec<f32>, // the slow factor after each step
    pub gain: Vec<f32>, // g after the FIR
}

pub struct ControlFreak {
    cfg: Config,
    d: Derived,
    channels: usize,
    inv_ref: f32,
    fir: Vec<f32>,
    frames: usize, // the largest block
    // audio path
    scaled: Vec<f32>, // channels * frames, the pre-scaled input
    env: Vec<f32>,
    min_gain: f32, // the lowest gain of the last process() call
    peak: Vec<f32>,
    fast: Vec<f32>,
    slowv: Vec<f32>,
    coef: Vec<f32>,
    det: Vec<f32>,
    history: Vec<f32>,    // window - 1 + frames
    slow: f32,            // the slow factor
    fir_hist: Vec<f32>,   // the last latency - 1 products
    delay: Vec<Vec<f32>>, // a FIFO of latency samples per channel
    // detector
    d4_hist: [f32; 30], // the decimator's last 30 inputs
    d4_phase: usize,    // the decimator's phase
    d10_phase: usize,
    u10_phase: usize,
    u4_phase: usize,
    x4: Vec<f32>,
    x10: Vec<f32>,
    ms: Vec<f32>,
    c10: Vec<f32>,
    c4: Vec<f32>,
    hb_state: [[[f32; 2]; 2]; 2], // path, section, DF2T s1 s2
    hb: [[Section; 2]; 2],
    rms_ring: [f32; RMS_MAX as usize],
    rms_pos: usize,
    act_env: f32,  // the detector's envelope
    act_hold: i32, // its hold count
    u10_last: f32,
    u4_last: f32,
    #[cfg(feature = "internals")]
    forced: Option<Vec<f32>>,
    #[cfg(feature = "internals")]
    trace: Option<Trace>,
}

fn db_to_lin(db: f32) -> f32 {
    (db * (std::f64::consts::LN_10 / 20.0) as f32).exp()
}

fn round_ms(rate: f32, ms: f32) -> u32 {
    (rate * ms / 1000.0 + 0.5) as u32
}

impl ControlFreak {
    pub fn new(cfg: &Config) -> Option<Self> {
        let channels = cfg.channels as usize;
        if channels == 0 || channels > MAX_CHANNELS || cfg.sample_rate == 0 {
            return None;
        }
        let sr = cfg.sample_rate as f32;
        let mut d = Derived::default();
        d.latency = round_ms(sr, cfg.fir_ms).clamp(1, FIR_MAX);
        d.window = (round_ms(sr, cfg.lookahead_ms) + d.latency) | 1;
        d.hold = round_ms(sr / 40.0, cfg.hold_ms);
        d.rms_window = round_ms(sr / 40.0, cfg.rms_ms).clamp(1, RMS_MAX);
        d.makeup = db_to_lin(cfg.makeup_db);
        d.ceiling = if cfg.bypass {
            1.0
        } else {
            db_to_lin(cfg.ceiling_db)
        };
        d.ref_ = db_to_lin(-cfg.reference_db.abs());
        // f32: 1 - exp(x) for x this small moves in steps of 6e-8, so the
        // release is 0.11% short of exact
        d.attack = 1.0 - (-1000.0f32 / (sr * cfg.attack_ms)).exp();
        d.release = 1.0 - (-1000.0f32 / (sr * cfg.release_ms)).exp();
        d.threshold = db_to_lin(cfg.activity_db * 0.5);
        d.inv_threshold = 1.0 / d.threshold;
        let l = d.latency as usize;
        let mut cf = ControlFreak {
            cfg: *cfg,
            d,
            channels,
            inv_ref: 1.0 / d.ref_,
            fir: crate::raised_cosine(l),
            frames: 0,
            scaled: Vec::new(),
            env: Vec::new(),
            min_gain: 1.0,
            peak: Vec::new(),
            fast: Vec::new(),
            slowv: Vec::new(),
            coef: Vec::new(),
            det: Vec::new(),
            history: Vec::new(),
            slow: 1.0,
            fir_hist: vec![0.0; l - 1],
            delay: vec![vec![0.0; l]; channels],
            d4_hist: [0.0; 30],
            d4_phase: 0,
            d10_phase: 0,
            u10_phase: 0,
            u4_phase: 0,
            x4: Vec::new(),
            x10: Vec::new(),
            ms: Vec::new(),
            c10: Vec::new(),
            c4: Vec::new(),
            hb_state: [[[0.0; 2]; 2]; 2],
            hb: crate::HILBERT,
            rms_ring: [0.0; RMS_MAX as usize],
            rms_pos: 0,
            act_env: 0.0,
            act_hold: 0,
            u10_last: 0.0,
            u4_last: 0.0,
            #[cfg(feature = "internals")]
            forced: None,
            #[cfg(feature = "internals")]
            trace: None,
        };
        cf.prepare(DEFAULT_FRAMES);
        cf.reset();
        Some(cf)
    }

    /// Back to the state after construction: lookahead history 1.0, slow
    /// factor 1.0, FIR, delay and detector empty.
    pub fn reset(&mut self) {
        let keep = self.d.window as usize - 1;
        self.history[..keep].fill(1.0);
        self.slow = 1.0;
        self.fir_hist.fill(0.0);
        for dl in self.delay.iter_mut() {
            dl.fill(0.0);
        }
        self.d4_hist = [0.0; 30];
        self.d4_phase = 0;
        self.d10_phase = 0;
        self.u10_phase = 0;
        self.u4_phase = 0;
        self.hb_state = [[[0.0; 2]; 2]; 2];
        self.rms_ring = [0.0; RMS_MAX as usize];
        self.rms_pos = 0;
        self.act_env = 0.0;
        self.act_hold = 0;
        self.u10_last = self.d.release;
        self.u4_last = self.d.release;
        #[cfg(feature = "internals")]
        {
            self.forced = None;
        }
    }

    /// Allocate for blocks of at most `frames` (512 by default), so process()
    /// never allocates; larger blocks are split.
    pub fn prepare(&mut self, frames: usize) {
        if frames <= self.frames {
            return;
        }
        let keep = self.d.window as usize - 1;
        let n4 = frames / DOWN4 + 2;
        let first = self.history.is_empty();
        self.history.resize(keep + frames, 0.0);
        if first {
            self.history[..keep].fill(1.0);
        }
        self.scaled.resize(self.channels * frames, 0.0);
        for v in [
            &mut self.env,
            &mut self.peak,
            &mut self.fast,
            &mut self.slowv,
            &mut self.coef,
            &mut self.det,
        ] {
            v.resize(frames, 0.0);
        }
        for v in [
            &mut self.x4,
            &mut self.x10,
            &mut self.ms,
            &mut self.c10,
            &mut self.c4,
        ] {
            v.resize(n4, 0.0);
        }
        self.frames = frames;
    }

    /// The channels it was configured for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The largest block process() runs in one piece.
    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn derived(&self) -> Derived {
        self.d
    }

    /// The lowest gain applied during the last process() call, for a meter.
    pub fn min_gain(&self) -> f32 {
        self.min_gain
    }

    /// Record the internals of every following process() call (only the
    /// last call's are kept); false stops it.
    #[cfg(feature = "internals")]
    pub fn set_trace(&mut self, on: bool) {
        self.trace = if on { Some(Trace::default()) } else { None };
    }

    #[cfg(feature = "internals")]
    pub fn trace(&self) -> Option<&Trace> {
        self.trace.as_ref()
    }

    /// Set the lookahead history (window - 1 values, oldest first) and the
    /// slow factor.
    #[cfg(feature = "internals")]
    pub fn set_state(&mut self, history: &[f32], slow: f32) {
        let keep = self.d.window as usize - 1;
        self.history[..keep].copy_from_slice(&history[..keep]);
        self.slow = slow;
    }

    /// For the next block only (one process() of at most the prepared
    /// frames), take the rise coefficient per sample from `coef` instead of
    /// the detector.
    #[cfg(feature = "internals")]
    pub fn force_coef(&mut self, coef: Option<Vec<f32>>) {
        self.forced = coef;
    }

    /// Decimation: the output samples sit at the input positions that are
    /// multiples of the ratio, counted across blocks. Each is the FIR over the
    /// input up to that position (the history carries 30 samples).
    fn decimate4(&mut self, n: usize) -> usize {
        let x = &self.det;
        let off = (DOWN4 - self.d4_phase) % DOWN4;
        let mut m = 0;
        let mut i = off;
        while i < n {
            let mut acc = 0.0f32;
            for k in 0..31 {
                let j = i as isize - k as isize;
                let v = if j >= 0 {
                    x[j as usize]
                } else {
                    self.d4_hist[(30 + j) as usize]
                };
                acc += DOWN4_TAPS[k] * v;
            }
            self.x4[m] = acc;
            m += 1;
            i += DOWN4;
        }
        crate::delay_shift(&mut self.d4_hist, &x[..n]);
        self.d4_phase = (n + self.d4_phase) % DOWN4;
        m
    }

    fn decimate10(&mut self, n: usize) -> usize {
        let off = (DOWN10 - self.d10_phase) % DOWN10;
        let mut m = 0;
        let mut i = off;
        while i < n {
            self.x10[m] = self.x4[i];
            m += 1;
            i += DOWN10;
        }
        self.d10_phase = (n + self.d10_phase) % DOWN10;
        m
    }

    /// Step 4: the rise coefficient per sample.
    fn detector(&mut self, n: usize) {
        let d = self.d;
        let f = self.frames;
        self.det[..n].copy_from_slice(&self.scaled[..n]);
        for ch in 1..self.channels {
            for i in 0..n {
                self.det[i] += self.scaled[ch * f + i];
            }
        }
        let n4 = self.decimate4(n);
        // the Hilbert envelope: the same signal through both paths, then the
        // magnitude
        for i in 0..n4 {
            let x = self.x4[i];
            let mut re = x;
            let mut im = x;
            for k in 0..2 {
                re = self.hb[0][k].run(&mut self.hb_state[0][k], re);
                im = self.hb[1][k].run(&mut self.hb_state[1][k], im);
            }
            self.x4[i] = (re * re + im * im).sqrt();
        }
        let n10 = self.decimate10(n4);
        // moving mean square over rms_window samples, + 1e-10
        let w = d.rms_window as usize;
        for i in 0..n10 {
            self.rms_ring[self.rms_pos] = self.x10[i] * self.x10[i];
            self.rms_pos = (self.rms_pos + 1) % w;
            let acc = self.rms_ring[..w].iter().fold(0.0f32, |a, &v| a + v);
            self.ms[i] = acc / w as f32 + 1e-10;
        }
        // the activity detector: hysteresis and hold
        for i in 0..n10 {
            let x = self.ms[i];
            let mut e = self.act_env;
            if d.threshold * self.act_env < x {
                self.act_env = x;
                self.act_hold += d.hold as i32;
                e = x;
            }
            if x < e * d.inv_threshold {
                self.act_env = x;
            }
            if self.act_hold == 0 {
                self.c10[i] = d.release;
            } else {
                self.c10[i] = d.attack;
                self.act_hold -= 1;
            }
        }
        hold_up(
            &self.c10[..n10],
            &mut self.c4[..n4],
            DOWN10,
            &mut self.u10_phase,
            &mut self.u10_last,
        );
        hold_up(
            &self.c4[..n4],
            &mut self.coef[..n],
            DOWN4,
            &mut self.u4_phase,
            &mut self.u4_last,
        );
    }

    fn process_block(&mut self, io: &mut [&mut [f32]], at: usize, n: usize) {
        let d = self.d;
        let c = self.channels;
        let l = d.latency as usize;
        let w = d.window as usize;
        let f = self.frames;
        let hi = d.makeup * 1000.0;

        // 1. in *= ceiling
        for ch in 0..c {
            for i in 0..n {
                self.scaled[ch * f + i] = io[ch][at + i] * d.ceiling;
            }
        }
        // 2. env = max over channels |in|
        for i in 0..n {
            let mut m = self.scaled[i].abs();
            for ch in 1..c {
                let v = self.scaled[ch * f + i].abs();
                if v > m {
                    m = v;
                }
            }
            self.env[i] = m;
        }
        // 3. the lookahead: max over the last W values of env
        self.history[w - 1..w - 1 + n].copy_from_slice(&self.env[..n]);
        for i in 0..n {
            let mut m = self.history[i];
            for k in 1..w {
                if self.history[i + k] > m {
                    m = self.history[i + k];
                }
            }
            self.peak[i] = m;
        }
        self.history.copy_within(n..n + w - 1, 0);
        // 4. the detector, unless coefficients were forced
        self.detector(n);
        #[cfg(feature = "internals")]
        if let Some(forced) = self.forced.take() {
            self.coef[..n].copy_from_slice(&forced[..n]);
        }
        if self.cfg.bypass {
            self.peak[..n].fill(0.0);
        }
        // 5. t = makeup / clip(peak, makeup, 1000 makeup); fast and slow
        for i in 0..n {
            let mut p = self.peak[i];
            if p < d.makeup {
                p = d.makeup;
            }
            if p > hi {
                p = hi;
            }
            let mut t = d.makeup / p;
            let mut tgt = t * self.inv_ref;
            if tgt < 0.0 {
                tgt = 0.0;
            }
            if tgt > 1.0 {
                tgt = 1.0;
            }
            if t < d.ref_ {
                t = d.ref_;
            }
            if t > 1.0 {
                t = 1.0;
            }
            self.fast[i] = t;
            if self.slow <= tgt {
                tgt = (tgt - self.slow) * self.coef[i] + self.slow;
            }
            self.slow = tgt;
            self.slowv[i] = tgt;
        }
        // 6. g = FIR(fast * slow), causal, history from the previous block
        for i in 0..n {
            self.fast[i] *= self.slowv[i];
        }
        for i in (0..n).rev() {
            let mut acc = 0.0f32;
            for k in 0..l {
                let j = i as isize - k as isize;
                let v = if j >= 0 {
                    self.fast[j as usize]
                } else {
                    self.fir_hist[(l as isize - 1 + j) as usize]
                };
                acc += self.fir[k] * v;
            }
            self.env[i] = acc; // env is free again: g
        }
        crate::delay_shift(&mut self.fir_hist, &self.fast[..n]);
        // 7. out = Delay(in) * g
        for ch in 0..c {
            let x = &self.scaled[ch * f..ch * f + n];
            let dl = &mut self.delay[ch];
            for i in 0..n {
                let v = if i < l { dl[i] } else { x[i - l] };
                io[ch][at + i] = v * self.env[i];
            }
            crate::delay_shift(dl, x);
        }
        for &v in &self.env[..n] {
            if v < self.min_gain {
                self.min_gain = v;
            }
        }
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.peak.extend_from_slice(&self.peak[..n]);
            tr.coef.extend_from_slice(&self.coef[..n]);
            tr.slow.extend_from_slice(&self.slowv[..n]);
            tr.gain.extend_from_slice(&self.env[..n]);
        }
    }

    /// Process in place, one slice per channel.
    pub fn process(&mut self, io: &mut [&mut [f32]]) {
        let n = io[0].len();
        self.min_gain = 1.0;
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.peak.clear();
            tr.coef.clear();
            tr.slow.clear();
            tr.gain.clear();
        }
        let mut done = 0;
        while done < n {
            let m = (n - done).min(self.frames);
            self.process_block(io, done, m);
            done += m;
        }
    }
}

/// The interpolators are linked to the decimators and emit exactly as many
/// samples as their decimator took in; each input holds until the next
/// multiple of the ratio: a zero-order hold aligned to the positions the
/// decimator sampled.
fn hold_up(x: &[f32], y: &mut [f32], ratio: usize, phase: &mut usize, last: &mut f32) {
    let mut k = 0;
    for (i, v) in y.iter_mut().enumerate() {
        if (*phase + i).is_multiple_of(ratio) && k < x.len() {
            *last = x[k];
            k += 1;
        }
        *v = *last;
    }
    *phase = (*phase + y.len()) % ratio;
}

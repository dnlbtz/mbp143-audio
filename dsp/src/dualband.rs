// SPDX-License-Identifier: MIT
//! The DualBand: a two-band compressor whose bands are never split.
//!
//! Two Mozart cores serve as gain computers over a 400 Hz crossover (four
//! identical first-order sections): the low band sees the input through the
//! low-pass squared, the high band through its exact complement, both every
//! 31st sample. The audio is multiplied by the high band's gain, then `order`
//! passes of `out -= LP(out * w)`, `w = 1 - (gSub/gSuper)^(1/order)`, leave
//! the low end at the low band's gain. No latency.
//!
//! The two gain computers' level detectors are one-pole mean squares
//! (`a = 0.9256` at 1423 Hz).
use crate::mozart::{self, Mozart};
use crate::Section;

pub const MAX_ORDER: usize = 10;
const MAX_CHANNELS: usize = mozart::MAX_CHANNELS;
const MAX_DEC: u32 = 64;
const INTERP_TAPS: usize = 8;

/// The two-band compressor's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub channels: u32,
    /// The largest block process() is given.
    pub max_frames: u32,
    pub sample_rate: u32,
    /// Pass the audio through unchanged.
    pub bypass: bool,
    /// Where the bands split.
    pub crossover_hz: f32,
    /// First-order crossover sections, 1 to 10.
    pub order: u32,
    /// The gains are computed at sample_rate / decimation, 1 to 64.
    pub decimation: u32,
    /// The audio's delay.
    pub latency_ms: f32,
    /// The gain computers of the low and the high band. Their channels,
    /// max_frames and sample_rate are set by DualBand::new: they run at the
    /// decimated rate on decimated blocks.
    pub low: mozart::Config,
    pub high: mozart::Config,
}

impl Config {
    /// The MacBookPro14,3 settings: 400 Hz, fourth order, both bands from -18 dB
    /// with a 24 dB knee, 3:1 below the crossover and 1.4:1 above it from
    /// -24 dB, +6 dB offset, 50 ms attack, 1250 ms release.
    pub fn mbp143(sample_rate: u32) -> Self {
        let band = mozart::Config {
            channels: 2,
            max_frames: 0,
            sample_rate: 0,
            bypass: false,
            compressor: true,
            limiter: false,
            input_gain_db: 0.0,
            plain_gain_db: 0.0,
            output_gain_db: 0.0,
            offset_db: 6.0,
            floor_db: 0.0,
            release_threshold_db: -20.0,
            attack_threshold_db: 0.0,
            ratio: 1.0,
            separate_release_thresholds: true,
            expander_threshold_db: -85.0,
            expander_ratio: 5.0,
            high_release_threshold_db: -18.0,
            high_attack_threshold_db: -18.0,
            high_ratio: 3.0,
            high_knee_db: 24.0,
            attack_ms: 50.0,
            release_ms: 1250.0,
            smoothing_ms: 0.1,
            decimation: 1,
            detector_ms: 20.0,
            detector_moving: false,
            activity: mozart::Activity::Off,
            activity_hold_ms: 100.0,
            activity_hysteresis_db: 6.0,
            gating: false,
            gate_smoothing_ms: 133.0,
            gate_tracking_ms: 177.0,
            gate_peak_ms: 132.0,
            gate_hysteresis_db: -10.0,
            silence_db: -70.0,
            limiter_fall_ms: 500.0,
            limiter_rise_ms: 50.0,
            limiter_window_ms: 50.0,
            limiter_slope: 1.0,
            limiter_offset_db: 0.0,
            limiter_hysteresis_db: 10.0,
            limiter_fall_to_target: true,
            sidechain: false,
            sidechain_highpass_hz: 38.0,
            sidechain_shelf_hz: 8000.0,
            sidechain_shelf_db: 0.0,
        };
        Config {
            channels: 2,
            max_frames: 512,
            sample_rate,
            bypass: false,
            crossover_hz: 400.0,
            order: 4,
            decimation: 31,
            latency_ms: 0.0,
            low: band,
            high: mozart::Config {
                high_release_threshold_db: -24.0,
                high_ratio: 1.4,
                ..band
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Derived {
    pub rate: u32,
    pub order: u32,
    pub decim: u32,
    pub latency: u32,
    pub section: [f32; 5], // the crossover section: k, k, 0, a1, 0
    pub lp2: [f32; 5],     // the detector sections, squared
    pub hp2: [f32; 5],
    pub sub: mozart::Derived,
    pub super_: mozart::Derived,
}

/// Per decimated sample of the last block, when enabled with set_trace().
#[derive(Clone, Debug, Default)]
#[cfg(feature = "internals")]
pub struct Trace {
    pub g_sub: Vec<f32>,
    pub g_super: Vec<f32>,
    pub w: Vec<f32>,
}

pub struct DualBand {
    cfg: Config,
    d: Derived,
    channels: usize,
    bypass: bool,
    sub: Mozart,
    super_: Mozart,
    blend: Section,
    lp2: Vec<Section>, // (order + 1) / 2 detector sections
    hp2: Vec<Section>,
    blend_state: [[[f32; 2]; MAX_CHANNELS]; MAX_ORDER],
    lp_state: [[[f32; 2]; MAX_CHANNELS]; MAX_ORDER],
    hp_state: [[[f32; 2]; MAX_CHANNELS]; MAX_ORDER],
    dec_phase: [usize; 2],
    int_phase: [usize; 2],
    lin: Vec<f32>, // the 2*decim - 1 linear interpolator
    taps: usize,
    hist: [[f32; INTERP_TAPS]; 2], // gSuper's, w's
    delay: Vec<Vec<f32>>,          // latency per channel
    band: Vec<f32>,                // max_frames
    dec_lo: Vec<f32>,              // channels * nd each
    dec_hi: Vec<f32>,
    gsi: Vec<f32>, // max_frames each
    wi: Vec<f32>,
    tmp: Vec<f32>,  // channels * max_frames
    gsub: Vec<f32>, // nd each
    gsup: Vec<f32>,
    w: Vec<f32>,
    nd: usize,
    first_gains: Option<(f32, f32)>,
    #[cfg(feature = "internals")]
    forced: Option<(Vec<f32>, Vec<f32>)>,
    #[cfg(feature = "internals")]
    trace: Option<Trace>,
}

/// The crossover sections.
fn sections(cfg: &Config, d: &mut Derived) -> (Section, Vec<Section>, Vec<Section>) {
    let sr = cfg.sample_rate;
    let order = cfg.order.clamp(1, MAX_ORDER as u32);
    d.rate = sr;
    d.order = order;
    // cot(pi*fc/sr) by its Taylor series, in double, summed from 1/x
    // outwards; only k and a1 are rounded to float.
    let x = (cfg.crossover_hz as f64 * 3.141592653589793) / sr as f64;
    let x3 = x * x * x;
    let g = (x * x3 * x) / -945.0 + (x3 / -45.0 + (x / -3.0 + 1.0 / x));
    let dn = g + 1.0;
    let k = (1.0 / dn) as f32;
    let a1 = ((1.0 - g) / dn) as f32;
    let blend = Section {
        c: [k, k, 0.0, a1, 0.0],
    };
    d.section = blend.c;
    // the low-pass squared and its complement squared
    let om = 1.0f32 - k;
    let ak = a1 - k;
    let mut lp2 = Vec::new();
    let mut hp2 = Vec::new();
    for _ in 0..order / 2 {
        let l2 = k * k;
        lp2.push(Section {
            c: [l2, l2 + l2, l2, a1 + a1, a1 * a1],
        });
        hp2.push(Section {
            c: [om * om, (om + om) * ak, ak * ak, a1 + a1, a1 * a1],
        });
    }
    if order & 1 != 0 {
        lp2.push(blend);
        hp2.push(Section {
            c: [1.0 - k, a1 - k, 0.0, a1, 0.0],
        });
    }
    d.lp2 = lp2[0].c;
    d.hp2 = hp2[0].c;
    (blend, lp2, hp2)
}

impl DualBand {
    pub fn new(cfg: &Config) -> Option<Self> {
        let channels = cfg.channels as usize;
        if channels == 0 || channels > MAX_CHANNELS || cfg.max_frames == 0 {
            return None;
        }
        let mut d = Derived::default();
        let bypass = cfg.bypass;
        let (blend, lp2, hp2) = sections(cfg, &mut d);
        let dd = cfg.decimation.clamp(1, MAX_DEC);
        d.decim = dd;
        let rate = d.rate as f32 / dd as f32;
        // the gain computers run at sr/D on blocks of max_frames/D + 1
        let f = cfg.max_frames as usize;
        let nd = f / dd as usize + 1;
        let band_config = |band: &mozart::Config| mozart::Config {
            channels: cfg.channels,
            max_frames: nd as u32,
            sample_rate: (rate + 0.5) as u32,
            ..*band
        };
        let sub = Mozart::new(&band_config(&cfg.low))?;
        let super_ = Mozart::new(&band_config(&cfg.high))?;
        d.sub = sub.derived();
        d.super_ = super_.derived();
        // the 2D - 1 tap linear interpolator
        let lin = crate::linear_interpolator(2 * dd as usize - 1);
        let taps = ((lin.len() - 1) / dd as usize + 4) & !3;
        if taps > INTERP_TAPS {
            return None;
        }
        d.latency = ((d.rate as f32 * cfg.latency_ms) as f64 / 1000.0 + 0.5) as u32;
        let mut db = DualBand {
            cfg: *cfg,
            d,
            channels,
            bypass,
            sub,
            super_,
            blend,
            lp2,
            hp2,
            blend_state: [[[0.0; 2]; MAX_CHANNELS]; MAX_ORDER],
            lp_state: [[[0.0; 2]; MAX_CHANNELS]; MAX_ORDER],
            hp_state: [[[0.0; 2]; MAX_CHANNELS]; MAX_ORDER],
            dec_phase: [0; 2],
            int_phase: [0; 2],
            lin,
            taps,
            hist: [[0.0; INTERP_TAPS]; 2],
            delay: vec![vec![0.0; d.latency as usize]; channels],
            band: vec![0.0; f],
            dec_lo: vec![0.0; channels * nd],
            dec_hi: vec![0.0; channels * nd],
            gsi: vec![0.0; f],
            wi: vec![0.0; f],
            tmp: vec![0.0; channels * f],
            gsub: vec![0.0; nd],
            gsup: vec![0.0; nd],
            w: vec![0.0; nd],
            nd,
            first_gains: None,
            #[cfg(feature = "internals")]
            forced: None,
            #[cfg(feature = "internals")]
            trace: None,
        };
        db.reset();
        Some(db)
    }

    pub fn reset(&mut self) {
        self.sub.reset();
        self.super_.reset();
        self.blend_state = [[[0.0; 2]; MAX_CHANNELS]; MAX_ORDER];
        self.lp_state = [[[0.0; 2]; MAX_CHANNELS]; MAX_ORDER];
        self.hp_state = [[[0.0; 2]; MAX_CHANNELS]; MAX_ORDER];
        self.dec_phase = [0; 2];
        self.int_phase = [0; 2];
        self.hist = [[0.0; INTERP_TAPS]; 2];
        for dl in self.delay.iter_mut() {
            dl.fill(0.0);
        }
        #[cfg(feature = "internals")]
        {
            self.forced = None;
        }
    }

    /// The channels it was configured for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn derived(&self) -> Derived {
        self.d
    }

    pub fn max_frames(&self) -> usize {
        self.cfg.max_frames as usize
    }

    /// The low and the high band's gain at the first gain sample of the last
    /// block, for a meter; None if that block was too short to take one.
    pub fn band_gains(&self) -> Option<(f32, f32)> {
        self.first_gains
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Record the gains of each block (the last block's are kept).
    #[cfg(feature = "internals")]
    pub fn set_trace(&mut self, on: bool) {
        self.trace = if on { Some(Trace::default()) } else { None };
    }

    #[cfg(feature = "internals")]
    pub fn trace(&self) -> Option<&Trace> {
        self.trace.as_ref()
    }

    /// For the next block only, use these gains instead of the two gain
    /// computers'.
    #[cfg(feature = "internals")]
    pub fn force_gains(&mut self, gains: Option<(Vec<f32>, Vec<f32>)>) {
        self.forced = gains;
    }

    /// Put the decimators and interpolators at this phase (the stream
    /// position modulo the decimation).
    #[cfg(feature = "internals")]
    pub fn set_phase(&mut self, phase: usize) {
        let phase = phase % self.d.decim as usize;
        self.dec_phase = [phase; 2];
        self.int_phase = [phase; 2];
    }

    /// The band filter, then every D-th sample into planar dec (channel
    /// stride nd); returns the samples taken.
    fn band_decimate(&mut self, io: &[&mut [f32]], at: usize, n: usize, high: bool) -> usize {
        let (secs, state, which) = if high {
            (&self.hp2, &mut self.hp_state, 1)
        } else {
            (&self.lp2, &mut self.lp_state, 0)
        };
        let dec = if high {
            &mut self.dec_hi
        } else {
            &mut self.dec_lo
        };
        let dd = self.d.decim as usize;
        let off = (dd - self.dec_phase[which]) % dd;
        let mut m = 0;
        for ch in 0..self.channels {
            for i in 0..n {
                let mut v = io[ch][at + i];
                for (k, s) in secs.iter().enumerate() {
                    v = s.run(&mut state[k][ch], v);
                }
                self.band[i] = v;
            }
            m = 0;
            let mut i = off;
            while i < n {
                dec[ch * self.nd + m] = self.band[i];
                m += 1;
                i += dd;
            }
        }
        self.dec_phase[which] = (n + self.dec_phase[which]) % dd;
        m
    }

    fn interpolate(&mut self, which: usize, nx: usize, ny: usize) {
        let dd = self.d.decim as usize;
        let t = self.taps;
        let (x, y) = if which == 0 {
            (&self.gsup, &mut self.gsi)
        } else {
            (&self.w, &mut self.wi)
        };
        let h = &mut self.hist[which];
        let mut used = 0;
        for i in 0..ny {
            let ph = (self.int_phase[which] + i) % dd;
            if ph == 0 && used < nx {
                h.copy_within(0..t - 1, 1);
                h[0] = x[used];
                used += 1;
            }
            let mut acc = 0.0f32;
            for k in 0..t {
                let j = ph + k * dd;
                if j < self.lin.len() {
                    acc += self.lin[j] * h[k];
                }
            }
            y[i] = acc;
        }
        self.int_phase[which] = (self.int_phase[which] + ny) % dd;
    }

    fn process_block(&mut self, io: &mut [&mut [f32]], at: usize, n: usize) {
        let c = self.channels;
        let f = self.cfg.max_frames as usize;
        let l = self.d.latency as usize;
        let nd = self.nd;

        if self.bypass {
            return;
        }
        // the low band: low-pass squared, decimate, its gain computer in place
        let m1 = self.band_decimate(io, at, n, false);
        {
            let mut bands = crate::planar(&mut self.dec_lo, nd, m1, c);
            self.sub.process(&mut bands[..c]);
        }
        self.gsub[..m1].copy_from_slice(&self.sub.modulator()[..m1]);
        // the high band: the complement squared
        let m2 = self.band_decimate(io, at, n, true);
        {
            let mut bands = crate::planar(&mut self.dec_hi, nd, m2, c);
            self.super_.process(&mut bands[..c]);
        }
        self.gsup[..m2].copy_from_slice(&self.super_.modulator()[..m2]);
        #[cfg(feature = "internals")]
        if let Some((fs, fp)) = self.forced.take() {
            let k = fs.len().min(m1);
            self.gsub[..k].copy_from_slice(&fs[..k]);
            self.gsup[..k].copy_from_slice(&fp[..k]);
        }
        self.first_gains = if m1 > 0 {
            Some((self.gsub[0], self.gsup[0]))
        } else {
            None
        };
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.g_sub.clear();
            tr.g_sub.extend_from_slice(&self.gsub[..m1]);
            tr.g_super.clear();
            tr.g_super.extend_from_slice(&self.gsup[..m1]);
        }
        if m1 != m2 {
            return; // the bands took different counts: cannot happen
        }
        // out = delay(in) * gSuper at sr
        self.interpolate(0, m1, n);
        for ch in 0..c {
            let x = &io[ch][at..at + n];
            let dl = &mut self.delay[ch];
            for i in 0..n {
                let v = if l != 0 {
                    if i < l {
                        dl[i]
                    } else {
                        x[i - l]
                    }
                } else {
                    x[i]
                };
                self.tmp[ch * f + i] = v * self.gsi[i];
            }
            if l != 0 {
                crate::delay_shift(dl, x);
            }
        }
        for ch in 0..c {
            io[ch][at..at + n].copy_from_slice(&self.tmp[ch * f..ch * f + n]);
        }
        // w = 1 - (gSub/gSuper)^(1/order), as exp(log10(r)*ln10/order)
        for i in 0..m1 {
            let r = (self.gsub[i] / self.gsup[i]).log10();
            let e = (r as f64 * 2.302585092994) / self.d.order as f64;
            self.w[i] = 1.0 - (e as f32).exp();
        }
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.w.clear();
            tr.w.extend_from_slice(&self.w[..m1]);
        }
        self.interpolate(1, m1, n);
        // order passes: tmp = out * w; tmp = LP(tmp); out -= tmp
        for k in 0..self.d.order as usize {
            for ch in 0..c {
                for i in 0..n {
                    let v = io[ch][at + i];
                    let t = self.blend.run(&mut self.blend_state[k][ch], v * self.wi[i]);
                    io[ch][at + i] = v - t;
                }
            }
        }
    }

    /// Process in place; blocks longer than max_frames are split.
    pub fn process(&mut self, io: &mut [&mut [f32]]) {
        let n = io[0].len();
        let f = self.cfg.max_frames as usize;
        let mut done = 0;
        while done < n {
            let k = (n - done).min(f);
            self.process_block(io, done, k);
            done += k;
        }
    }
}

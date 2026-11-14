// SPDX-License-Identifier: MIT
//! Mozart: an upward compressor with its gain computed at a decimated rate.
//!
//! For the speakers, sr/31 = 1422.58 Hz: +9 dB input gain, a treble-weighted
//! sidechain, every 31st sample, a 400 ms moving mean square, a log-domain
//! gating follower, an activity detector, a release and an attack transfer
//! curve, one attack/release state, an auto-threshold limiter, a 134-tap
//! interpolator back to sr, and the audio delayed by 74 samples times that
//! gain. f32, with f64 where a step needs the range before rounding.
//!
//! The same core runs the DualBand's two gain computers with their own
//! settings; `modulator()` gives the gain they produce.
use crate::Section;

pub const MAX_CHANNELS: usize = 4;

const LN10_20: f32 = 0.115129255; // ln(10)/20
const LN10_10: f32 = 0.23025851;
const CAL_DB: f32 = -0.691; // the level calibration, dB
const CAL_LIN: f32 = 0.92352784; // 10^(-0.691/20)
const MAX_TAPS: usize = 16; // interpolator taps per phase
const SIDECHAIN_HP_Q: f32 = 0.57; // the sidechain high-pass's Q

/// Which activity detector scales the release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    /// None: the release runs at full speed throughout.
    Off = 0,
    /// The hold-and-hysteresis detector.
    Hold = 1,
    /// The direction-tracking detector.
    Direction = 2,
}

/// The compressor's settings. Times in ms, levels in dB, ratios as n:1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub channels: u32,
    /// The largest block process() is given.
    pub max_frames: u32,
    pub sample_rate: u32,
    /// Pass the audio through at the plain gain, no compression, no delay.
    pub bypass: bool,
    /// The compressor proper; off, the block applies `plain_gain_db` only.
    pub compressor: bool,
    /// The auto-threshold limiter after the curve.
    pub limiter: bool,

    /// Gain into the compressor, also applied to the audio.
    pub input_gain_db: f32,
    /// The gain applied when `compressor` is off.
    pub plain_gain_db: f32,
    /// Gain after the curve.
    pub output_gain_db: f32,
    /// Added to the curve's output.
    pub offset_db: f32,
    /// The release curve's gain below the expander threshold, and the
    /// attack curve's cap there.
    pub floor_db: f32,

    /// The release curve's threshold.
    pub release_threshold_db: f32,
    /// The attack curve's threshold, at least `release_threshold_db`.
    pub attack_threshold_db: f32,
    /// Above the thresholds.
    pub ratio: f32,
    /// The release curve takes its own thresholds (`release_threshold_db`,
    /// `high_release_threshold_db`) rather than the attack curve's; also
    /// required for gating.
    pub separate_release_thresholds: bool,
    /// Below it the curve expands.
    pub expander_threshold_db: f32,
    pub expander_ratio: f32,
    /// A second, upper segment: its thresholds, ratio and knee width.
    pub high_release_threshold_db: f32,
    pub high_attack_threshold_db: f32,
    pub high_ratio: f32,
    pub high_knee_db: f32,

    pub attack_ms: f32,
    /// Scaled by the activity detector.
    pub release_ms: f32,
    /// The gain's smoothing filter, which sets the audio delay.
    pub smoothing_ms: f32,
    /// The gain is computed at sample_rate / decimation.
    pub decimation: u32,

    /// The level detector's window.
    pub detector_ms: f32,
    /// A moving mean square over the window; otherwise a one-pole one.
    pub detector_moving: bool,
    pub activity: Activity,
    /// How long the activity detector holds.
    pub activity_hold_ms: f32,
    /// The activity detector's hysteresis.
    pub activity_hysteresis_db: f32,

    /// Gating of the level while the signal is steady.
    pub gating: bool,
    pub gate_smoothing_ms: f32,
    pub gate_tracking_ms: f32,
    pub gate_peak_ms: f32,
    pub gate_hysteresis_db: f32,
    /// Below it the input counts as silence: activity, no gating.
    pub silence_db: f32,

    pub limiter_fall_ms: f32,
    pub limiter_rise_ms: f32,
    /// The limiter's level window.
    pub limiter_window_ms: f32,
    pub limiter_slope: f32,
    pub limiter_offset_db: f32,
    pub limiter_hysteresis_db: f32,
    /// Fall toward the target scaled by the hysteresis; otherwise an
    /// exponential decay.
    pub limiter_fall_to_target: bool,

    /// The detector's sidechain weighting: a high-pass, a fixed peak at
    /// 1680 Hz and a high shelf.
    pub sidechain: bool,
    pub sidechain_highpass_hz: f32,
    pub sidechain_shelf_hz: f32,
    pub sidechain_shelf_db: f32,
}

impl Config {
    /// The MacBookPro14,3 speaker compressor: +9 dB in, 1.8:1 from -18 dB,
    /// 1.6:1 expansion below -60 dB floored at +6 dB, 100 ms attack, 1500 ms
    /// release, a 400 ms level window at sample_rate/31, 74 samples of delay.
    pub fn mbp143(sample_rate: u32) -> Self {
        Config {
            channels: 2,
            max_frames: 512,
            sample_rate,
            bypass: false,
            compressor: true,
            limiter: true,
            input_gain_db: 9.0,
            plain_gain_db: 0.0,
            output_gain_db: 0.0,
            offset_db: 0.0,
            floor_db: 6.0,
            release_threshold_db: -18.0,
            attack_threshold_db: 0.0,
            ratio: 1.8,
            separate_release_thresholds: true,
            expander_threshold_db: -60.0,
            expander_ratio: 1.6,
            high_release_threshold_db: -30.0,
            high_attack_threshold_db: 0.0,
            high_ratio: 1.0,
            high_knee_db: 0.0,
            attack_ms: 100.0,
            release_ms: 1500.0,
            smoothing_ms: 1.0,
            decimation: 31,
            detector_ms: 400.0,
            detector_moving: true,
            activity: Activity::Hold,
            activity_hold_ms: 100.0,
            activity_hysteresis_db: 6.0206,
            gating: true,
            gate_smoothing_ms: 66.0,
            gate_tracking_ms: 50.0,
            gate_peak_ms: 132.0,
            gate_hysteresis_db: -10.0,
            silence_db: -70.0,
            limiter_fall_ms: 440.0,
            limiter_rise_ms: 50.0,
            limiter_window_ms: 10.0,
            limiter_slope: 1.0,
            limiter_offset_db: 5.0,
            limiter_hysteresis_db: 10.0,
            limiter_fall_to_target: true,
            sidechain: true,
            sidechain_highpass_hz: 150.0,
            sidechain_shelf_hz: 8000.0,
            sidechain_shelf_db: -12.0,
        }
    }
}

/// What new() derived from a Config, for a meter or a test.
#[derive(Clone, Copy, Debug, Default)]
pub struct Derived {
    pub rate: u32,        // Hz
    pub decim: u32,       // the gain's decimation
    pub latency: u32,     // samples
    pub hold: u32,        // the activity hold, decimated samples
    pub rms_window: u32,  // the level detector's window, decimated samples
    pub lim_window: u32,  // the limiter's
    pub rms_moving: bool, // a moving mean square, otherwise a one-pole
    pub curve: [f32; 25], // the transfer curves' breakpoints and slopes
    pub gain_in: f32,     // linear
    pub gain_out: f32,    // linear
    pub attack_a: f32,    // one-pole coefficient
    pub attack: f32,      // 1 - attack_a
    pub release_a: f32,   // one-pole coefficient
    pub release: f32,     // 1 - release_a
}

/// Per decimated sample of the last block, when enabled with set_trace().
#[derive(Clone, Debug, Default)]
#[cfg(feature = "internals")]
pub struct Trace {
    pub mean_square: Vec<f32>, // the main detector's output, before gating
    pub level: Vec<f32>,       // level dB into the transfer curve
    pub activity: Vec<f32>,
    pub gain: Vec<f32>, // the gain after the limiter, before the output gain
}

/// A level detector: a moving mean square over `window` frames, its sum in
/// f64, or a one-pole of x^2. Channel weights 1.
#[derive(Default)]
struct Rms {
    moving: bool,
    window: usize,
    channels: usize,
    pos: usize,
    ring: Vec<f32>, // window * channels
    sum: f64,
    inv: f32,
    a: f32,
    one_a: f32,
    state: [f32; MAX_CHANNELS],
}

impl Rms {
    fn new(moving: bool, ms: f32, rate: f32, channels: usize) -> Self {
        // the rate is truncated to an integer first
        let irate = rate as u32 as f32;
        let mut r = Rms {
            moving,
            channels,
            ..Default::default()
        };
        if moving {
            r.window = ((irate * ms) as f64 / 1000.0) as usize;
            if r.window != 0 {
                r.ring = vec![0.0; r.window * channels];
                r.inv = 1.0 / r.window as f32;
            }
        } else {
            r.a = ((-2.2 / ((irate * ms) as f64 / 1000.0)) as f32).exp();
            r.one_a = 1.0 - r.a;
        }
        r
    }

    fn reset(&mut self) {
        self.ring.fill(0.0);
        self.pos = 0;
        self.sum = 0.0;
        self.state = [0.0; MAX_CHANNELS];
    }

    /// x is interleaved, channels per frame.
    fn process(&mut self, x: &[f32], y: &mut [f32]) {
        let c = self.channels;
        for (i, out) in y.iter_mut().enumerate() {
            let frame = &x[i * c..(i + 1) * c];
            if !self.moving {
                // x^2 + a*state, weighted by (1-a); the DualBand's gain
                // computers run this
                let mut acc = 0.0f32;
                for (ch, &v) in frame.iter().enumerate() {
                    self.state[ch] = self.a * self.state[ch] + v * v;
                    acc += self.state[ch] * self.one_a;
                }
                *out = acc + 1e-10;
                continue;
            }
            if self.window == 0 {
                let mut acc = 0.0f32;
                for &v in frame {
                    acc += v * v;
                }
                *out = acc;
                continue;
            }
            for &v in frame {
                let nv = v * v;
                let old = self.ring[self.pos];
                self.ring[self.pos] = nv;
                self.sum = nv as f64 + (self.sum - old as f64);
                self.pos += 1;
            }
            if self.pos >= self.window * c {
                self.pos = 0;
            }
            *out = (self.inv as f64 * self.sum + 1e-10) as f32;
        }
    }
}

/// exp(-1/(rate * ms / 1000)): the division by 2.2 and 1000.0 in double,
/// the exp in f32. by_2_2 makes ms the 89% settling time.
fn tau(rate: f32, ms: f32, by_2_2: bool) -> f32 {
    let t = if by_2_2 {
        (rate as f64 * (ms as f64 / 2.2)) / 1000.0
    } else {
        (rate * ms) as f64 / 1000.0
    };
    ((-1.0 / t) as f32).exp()
}

/// A high-pass biquad, alpha = sin(w) * sinh(1/(2Q))
fn highpass(f: f64, q: f64, sr: u32) -> Section {
    let w = (f * 6.283185307179586) / sr as f64;
    let c = w.cos();
    let al = w.sin() * (1.0 / (q + q)).sinh();
    let a0 = 1.0 / (al + 1.0);
    let b0 = ((c + 1.0) * 0.5 * a0) as f32;
    Section {
        c: [
            b0,
            ((-1.0 - c) * a0) as f32,
            b0,
            (c * -2.0 * a0) as f32,
            ((1.0 - al) * a0) as f32,
        ],
    }
}

/// A shelf, A = 10^(g/20) at DC, 1 at Nyquist
fn shelf(f: f64, gain_db: f64, sr: u32) -> Section {
    let w = (f * 6.283185307179586) / sr as f64;
    let k = (w * 0.5).tan();
    let v = 10.0f64.powf(gain_db * 0.05);
    let k2 = k * k;
    let s2 = 2.0f64.sqrt();
    let d = s2 * k + 1.0 + k2;
    Section {
        c: [
            (((v + v).sqrt() * k + v + k2) / d) as f32,
            (((k2 - v) + (k2 - v)) / d) as f32,
            (((v - (v + v).sqrt() * k) + k2) / d) as f32,
            ((k2 - 1.0 + k2 - 1.0) / d) as f32,
            (((1.0 - s2 * k) + k2) / d) as f32,
        ],
    }
}

pub struct Mozart {
    cfg: Config,
    d: Derived,
    channels: usize,
    curve_path: bool,
    limiter_on: bool,
    active: bool,
    sidechain_on: bool,
    gating_on: bool,
    lim_fall_mode: bool,
    detector: u32, // Activity as a number
    // derived coefficients
    lim_fall: f32,
    lim_rise: f32,
    lim_slope: f32,
    lim_offset: f32,
    lim_hyst: f32,
    silence: f32,
    gate_lo: f32,
    gate_hi: f32,
    g_smooth_a: f32, // the gate's level smoother, one-pole coefficient
    g_smooth_b: f32, // 1 - g_smooth_a
    peak_fall: f32,  // the factor the gate's peak follower falls by
    peak_rise: f32,  // it rises by 1 + this
    track_a: f32,    // the factor the gated level falls by
    track_b: f32,    // it rises by 1 + this
    det_up: f32,     // the activity detector's hysteresis as a factor, up
    det_down: f32,   // and down
    // state
    ar_state: f32, // the attack/release filter's gain
    g_smooth: f32, // the gate's smoothed level
    peak: f32,     // the gate's peak follower
    gate: f32,     // the gated level, what the curves see
    det_env: f32,  // the activity detector's reference level
    det_dir: u32,  // the direction it last moved in
    det_prev: u32, // and the one before
    det_hold: i32, // decimated samples of hold left
    lim_s: f32,    // the level the limiter follows, at least 1
    lim_f: f32,    // the limiter's gain, a running 1 / lim_s
    lim_prev: f32, // the curve's gain one sample back
    min_gain: f32, // the lowest gain since the last reset
    max_gain: f32, // the highest
    sc: [Section; 3],
    sc_state: [[[f32; 2]; 3]; MAX_CHANNELS],
    rms: Rms,
    lim_rms: Rms,
    dec_phase: usize,
    int_phase: usize,
    interp: Vec<f32>, // the 2*decim + latency - 2 table
    taps: usize,
    int_hist: Vec<f32>,   // taps decimated gains
    delay: Vec<Vec<f32>>, // latency per channel
    // buffers
    nd: usize,      // max_frames/decim + 1
    work: Vec<f32>, // channels * max_frames: the gained audio
    side: Vec<f32>, // channels * max_frames
    dec: Vec<f32>,  // channels * nd, interleaved for the RMS
    ms: Vec<f32>,
    att: Vec<f32>,
    act: Vec<f32>,
    lrms: Vec<f32>,
    gain: Vec<f32>, // max_frames: the modulator
    #[cfg(feature = "internals")]
    trace: Option<Trace>,
}

impl Mozart {
    pub fn new(cfg: &Config) -> Option<Self> {
        let channels = cfg.channels as usize;
        if channels == 0 || channels > MAX_CHANNELS || cfg.max_frames == 0 {
            return None;
        }
        let mut m = Mozart {
            cfg: *cfg,
            d: Derived::default(),
            channels,
            curve_path: false,
            limiter_on: false,
            active: false,
            sidechain_on: false,
            gating_on: false,
            lim_fall_mode: false,
            detector: 0,
            lim_fall: 0.0,
            lim_rise: 0.0,
            lim_slope: 0.0,
            lim_offset: 0.0,
            lim_hyst: 0.0,
            silence: 0.0,
            gate_lo: 0.0,
            gate_hi: 0.0,
            g_smooth_a: 0.0,
            g_smooth_b: 0.0,
            peak_fall: 0.0,
            peak_rise: 0.0,
            track_a: 0.0,
            track_b: 0.0,
            det_up: 0.0,
            det_down: 0.0,
            ar_state: 0.0,
            g_smooth: 0.0,
            peak: 0.0,
            gate: 0.0,
            det_env: 0.0,
            det_dir: 0,
            det_prev: 0,
            det_hold: 0,
            lim_s: 0.0,
            lim_f: 0.0,
            lim_prev: 0.0,
            min_gain: 0.0,
            max_gain: 0.0,
            sc: [Section::default(); 3],
            sc_state: [[[0.0; 2]; 3]; MAX_CHANNELS],
            rms: Rms::default(),
            lim_rms: Rms::default(),
            dec_phase: 0,
            int_phase: 0,
            interp: Vec::new(),
            taps: 0,
            int_hist: Vec::new(),
            delay: Vec::new(),
            nd: 0,
            work: Vec::new(),
            side: Vec::new(),
            dec: Vec::new(),
            ms: Vec::new(),
            att: Vec::new(),
            act: Vec::new(),
            lrms: Vec::new(),
            gain: Vec::new(),
            #[cfg(feature = "internals")]
            trace: None,
        };
        m.derive();
        if !m.build() {
            return None;
        }
        m.reset();
        Some(m)
    }

    fn derive(&mut self) {
        let cfg = self.cfg;
        let d = &mut self.d;
        let c = &mut d.curve;

        let sr = cfg.sample_rate;
        d.rate = sr;
        *c = [0.0; 25];
        self.curve_path = cfg.compressor && !cfg.bypass;
        if self.curve_path {
            c[0] = 1.0;
            c[1] = cfg.offset_db;
            d.gain_in = (cfg.input_gain_db * LN10_20).exp();
            d.gain_out = (cfg.output_gain_db * LN10_20).exp();
        } else {
            d.gain_in = (cfg.plain_gain_db * LN10_20).exp();
            d.gain_out = 1.0;
        }
        self.limiter_on = cfg.limiter && !cfg.bypass;
        self.active = !cfg.bypass;
        self.sidechain_on = cfg.sidechain;
        let dd = cfg.decimation.max(1);
        d.decim = dd;
        let l = (((sr as f32 * cfg.smoothing_ms) as f64 / 1000.0 + 0.5) as u32) + dd - 1;
        d.latency = if l != 0 { l } else { 1 };
        let rate = sr as f32 / dd as f32;

        // the curve: a release and an attack transfer function sharing the
        // expander below its threshold x_exp, each with its main and its
        // high segment
        let thr_r_main = cfg.release_threshold_db;
        let thr_a = if cfg.attack_threshold_db < thr_r_main {
            thr_r_main
        } else {
            cfg.attack_threshold_db
        };
        let thr_r = if cfg.separate_release_thresholds {
            thr_r_main
        } else {
            thr_a
        };
        let hi_thr = if cfg.separate_release_thresholds {
            cfg.high_release_threshold_db
        } else {
            cfg.high_attack_threshold_db
        };
        let s = 1.0f32 / cfg.ratio;
        let mut e = cfg.expander_ratio;
        let x_exp = cfg.expander_threshold_db;
        let mut hw = (1.0 - s) * thr_r - (e - s) * x_exp;
        hw = (hw - thr_a * (1.0 - s)) / (s - e) - hw / (1.0 - e);
        e = e - 1.0;
        c[3] = e;
        c[6] = -e * x_exp;
        c[4] = x_exp - hw;
        c[5] = x_exp + hw;
        c[2] = x_exp;
        let s1 = s - 1.0;
        c[11] = s1;
        c[12] = s1;
        c[13] = (x_exp - thr_r) * s1;
        c[14] = -thr_r * s1;
        c[15] = -thr_a * s1;
        c[16] = thr_r - (thr_a - thr_r);
        c[17] = (thr_a - thr_r) + thr_r;
        c[8] = thr_r;
        c[9] = thr_a;
        c[10] = hw;
        c[7] = ((-thr_a * s1 - c[13]) - -e * x_exp) / (e - s1);
        c[19] = cfg.high_attack_threshold_db;
        c[20] = hi_thr;
        c[24] = cfg.high_knee_db;
        let mut hs = 1.0f32 / cfg.high_ratio - 1.0;
        c[21] = hs;
        hs = -hs;
        c[22] = hi_thr * hs;
        c[23] = hs * cfg.high_attack_threshold_db;
        c[18] = cfg.floor_db;

        d.attack_a = tau(rate, cfg.attack_ms, true);
        d.attack = 1.0 - d.attack_a;
        d.release_a = tau(rate, cfg.release_ms, true);
        d.release = 1.0 - d.release_a;
        self.lim_slope = cfg.limiter_slope;
        self.lim_offset = cfg.limiter_offset_db;
        self.lim_hyst = (cfg.limiter_hysteresis_db * LN10_20).exp();
        self.lim_fall_mode = cfg.limiter_fall_to_target;
        self.lim_fall = 1.0 - tau(rate, cfg.limiter_fall_ms, true);
        self.lim_rise = 1.0 - tau(rate, cfg.limiter_rise_ms, true);
        self.gating_on = cfg.gating && cfg.separate_release_thresholds;
        self.silence = (cfg.silence_db * LN10_10).exp();
        self.gate_lo = (cfg.gate_hysteresis_db * LN10_10).exp();
        self.gate_hi = (cfg.gate_hysteresis_db * -LN10_10).exp();
        self.g_smooth_a = tau(rate, cfg.gate_smoothing_ms, false);
        self.g_smooth_b = 1.0 - self.g_smooth_a;
        let a = tau(rate, cfg.gate_peak_ms, false);
        self.peak_rise = 1.0 - a;
        self.peak_fall = a * a * a * a;
        self.track_a = tau(rate, cfg.gate_tracking_ms, false);
        self.track_b = 1.0 - self.track_a;
        d.hold = ((sr as f64 * (cfg.activity_hold_ms as f64 / 1000.0)) / (dd as f32) as f64 + 0.5)
            as u32;
        self.det_up = (cfg.activity_hysteresis_db * LN10_10 * 0.5).exp();
        self.det_down = (cfg.activity_hysteresis_db * -LN10_10 * 0.5).exp();
        self.detector = cfg.activity as u32;

        // the sidechain: high-pass, peak, shelf
        self.sc[0] = highpass(cfg.sidechain_highpass_hz as f64, SIDECHAIN_HP_Q as f64, sr);
        self.sc[1] = shelf(1680.0, 4.0, sr);
        self.sc[2] = shelf(
            cfg.sidechain_shelf_hz as f64,
            cfg.sidechain_shelf_db as f64,
            sr,
        );
    }

    fn build(&mut self) -> bool {
        let dd = self.d.decim as usize;
        let l = self.d.latency as usize;
        let c = self.channels;
        let f = self.cfg.max_frames as usize;
        let nlin = 2 * dd - 1;
        let rate = self.d.rate as f32 / dd as f32;

        // the interpolator: a 2D-1 linear interpolator convolved with the
        // L-tap raised cosine
        let lin = crate::linear_interpolator(nlin);
        let rc = crate::raised_cosine(l);
        let len = nlin + l - 1;
        self.interp = (0..len)
            .map(|i| {
                let mut acc = 0.0f32;
                for (j, &lv) in lin.iter().enumerate() {
                    if i >= j && i - j < l {
                        acc = lv * rc[i - j] + acc;
                    }
                }
                acc
            })
            .collect();
        // taps per phase: (len-1)/decim + 1, rounded up to a multiple of 4
        self.taps = ((len - 1) / dd + 4) & !3;
        if self.taps > MAX_TAPS * 4 {
            return false;
        }
        self.nd = f / dd + 1;
        self.int_hist = vec![0.0; self.taps];
        self.delay = vec![vec![0.0; l]; c];
        self.work = vec![0.0; c * f];
        self.side = vec![0.0; c * f];
        self.dec = vec![0.0; c * self.nd];
        self.ms = vec![0.0; self.nd];
        self.att = vec![0.0; self.nd];
        self.act = vec![0.0; self.nd];
        self.lrms = vec![0.0; self.nd];
        self.gain = vec![0.0; f];
        // the two detectors: moving or one-pole, window ms, rate
        self.rms = Rms::new(self.cfg.detector_moving, self.cfg.detector_ms, rate, c);
        self.lim_rms = Rms::new(true, self.cfg.limiter_window_ms, rate, c);
        self.d.rms_moving = self.rms.moving;
        self.d.rms_window = self.rms.window as u32;
        self.d.lim_window = self.lim_rms.window as u32;
        true
    }

    pub fn reset(&mut self) {
        self.ar_state = (self.cfg.floor_db * LN10_20).exp() * (self.cfg.offset_db * LN10_20).exp();
        self.g_smooth = 1e-9;
        self.peak = self.g_smooth;
        self.gate = self.g_smooth;
        self.det_env = 0.0;
        self.det_dir = 0;
        self.det_prev = 0;
        self.det_hold = 0;
        self.lim_s = 1.0;
        self.lim_f = 1.0;
        self.lim_prev = 1.0;
        self.min_gain = 1.0;
        self.max_gain = 1.0;
        self.sc_state = [[[0.0; 2]; 3]; MAX_CHANNELS];
        self.rms.reset();
        self.lim_rms.reset();
        self.dec_phase = 0;
        self.int_phase = 0;
        self.int_hist.fill(0.0);
        for dl in self.delay.iter_mut() {
            dl.fill(0.0);
        }
        self.gain.fill(1.0);
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

    /// The gain of the last block at the full rate: max_frames values, the
    /// last block's first.
    pub fn modulator(&self) -> &[f32] {
        &self.gain
    }

    /// Record the internals of each block (the last block's are kept).
    #[cfg(feature = "internals")]
    pub fn set_trace(&mut self, on: bool) {
        self.trace = if on { Some(Trace::default()) } else { None };
    }

    #[cfg(feature = "internals")]
    pub fn trace(&self) -> Option<&Trace> {
        self.trace.as_ref()
    }

    /// The transfer curves for one level in dB: the release and the attack
    /// gain in dB.
    pub fn curve(&self, level_db: f32) -> (f32, f32) {
        transfer_curve(&self.d.curve, level_db)
    }

    /// Gating: hold the level while the signal is steady.
    fn gating(&mut self, nd: usize) {
        for i in 0..nd {
            let v = self.ms[i];
            if self.silence < v {
                let pk = self.peak;
                let mut f = 0.0f32;
                let mut mv = true;
                if v <= pk {
                    if pk <= self.gate {
                        mv = false;
                    } else {
                        f = self.peak_fall;
                    }
                } else {
                    f = self.peak_rise + 1.0;
                }
                if mv {
                    self.peak = pk * f;
                }
            }
            self.g_smooth = self.g_smooth * self.g_smooth_a + self.g_smooth_b * v;
            if self.g_smooth * self.gate_lo < v && v < self.peak * self.gate_hi {
                self.gate *= if v <= self.gate {
                    self.track_a
                } else {
                    self.track_b + 1.0
                };
            }
            self.ms[i] = self.gate;
        }
    }

    /// The hold-and-hysteresis activity detector.
    fn activity1(&mut self, nd: usize) {
        let mut dir = self.det_dir;
        let mut prev = self.det_prev;
        for i in 0..nd {
            let v = self.ms[i];
            let mut e = self.det_env;
            if self.det_up * self.det_env < v {
                self.det_env = v;
                dir = 1;
                e = v;
            }
            if v < e * self.det_down {
                self.det_env = v;
                dir = 0;
            }
            if prev == dir {
                if self.det_hold != 0 {
                    self.det_hold -= 1;
                    self.act[i] = 1.0;
                } else if v < self.silence {
                    self.act[i] = 1.0;
                } else {
                    self.act[i] = 0.0;
                }
            } else {
                self.det_hold = self.d.hold as i32;
                self.act[i] = 1.0;
            }
            prev = dir;
        }
        self.det_dir = dir;
        self.det_prev = prev;
    }

    /// The direction-tracking activity detector.
    fn activity2(&mut self, nd: usize) {
        for i in 0..nd {
            let v = self.ms[i];
            if v <= self.det_up * self.det_env {
                if v < self.det_env * self.det_down {
                    self.det_env = v;
                }
            } else {
                self.det_env = v;
                self.det_hold += self.d.hold as i32;
            }
            if self.det_hold == 0 {
                self.act[i] = if v < self.silence { 1.0 } else { 0.0 };
            } else {
                self.det_hold -= 1;
                self.act[i] = 1.0;
            }
        }
    }

    fn calculate_gain(&mut self, nd: usize) {
        if self.gating_on {
            self.gating(nd);
        }
        match self.detector {
            1 => self.activity1(nd),
            2 => self.activity2(nd),
            _ => self.act[..nd].fill(1.0),
        }
        for g in self.ms[..nd].iter_mut() {
            *g = g.log10() * 10.0 + CAL_DB;
        }
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.level.clear();
            tr.level.extend_from_slice(&self.ms[..nd]);
        }
        let c = self.d.curve;
        for i in 0..nd {
            let (rel, att) = transfer_curve(&c, self.ms[i]);
            // dB to linear
            self.ms[i] = (rel * LN10_20).exp();
            self.att[i] = (att * LN10_20).exp();
        }
        // the attack/release filter
        for i in 0..nd {
            let mut s = self.ar_state;
            if self.att[i] < s {
                s = s + (self.att[i] - s) * self.d.attack;
            }
            if s < self.ms[i] {
                s = s * (self.d.release * self.act[i] + 1.0);
            }
            if !self.curve_path {
                s = 1.0;
            }
            self.ar_state = s;
            self.ms[i] = s;
        }
        if self.limiter_on {
            // the auto-threshold limiter
            let mut s = self.lim_s;
            let mut f = self.lim_f;
            let mut prev = self.lim_prev;
            for i in 0..nd {
                let gi = self.ms[i];
                self.att[i] = self.lrms[i].sqrt() * CAL_LIN;
                let mut lvl = (((gi + 1e-10).log10() * 20.0 * -(c[12] + 1.0) + c[15])
                    / (1.0 - (c[12] + 1.0)))
                    * self.lim_slope
                    + self.lim_offset;
                if lvl < -40.0 {
                    lvl = -40.0;
                }
                let att = (lvl * -LN10_20).exp();
                let q = self.att[i] * gi * att;
                if q <= s {
                    if q * self.lim_hyst < s {
                        if self.lim_fall_mode {
                            s = s + (q * self.lim_hyst - s) * self.lim_fall;
                        } else {
                            s = s * (1.0 - self.lim_fall);
                        }
                    }
                } else {
                    s = s + (q - s) * self.lim_rise;
                }
                let mut r = gi / prev;
                if r >= 1.0 {
                    r = 1.0;
                }
                r = r * s;
                // what the next sample compares q with is max(ratio * S, 1),
                // not S itself
                let big_f = if r >= 1.0 { r } else { 1.0 };
                s = big_f;
                let mut k = (3.0 - f * big_f) * 0.5;
                if k < 0.01 {
                    k = 0.01;
                }
                f = k * f;
                self.ms[i] = gi * f;
                prev = gi;
            }
            self.lim_s = s;
            self.lim_f = f;
            self.lim_prev = prev;
        }
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.gain.clear();
            tr.gain.extend_from_slice(&self.ms[..nd]);
        }
        for g in self.ms[..nd].iter_mut() {
            if *g < self.min_gain {
                self.min_gain = *g;
            }
            if *g > self.max_gain {
                self.max_gain = *g;
            }
            *g = self.d.gain_out * *g;
        }
    }

    /// The interpolator back to sr:
    /// output G uses phase G mod D and the decimated gains up to floor(G/D),
    /// i.e. an upsample by D through the interp table.
    fn interpolate(&mut self, nx: usize, ny: usize) {
        let dd = self.d.decim as usize;
        let t = self.taps;
        let mut used = 0;
        let mut hist = [0.0f32; MAX_TAPS * 4];
        hist[..t].copy_from_slice(&self.int_hist);
        for i in 0..ny {
            let ph = (self.int_phase + i) % dd;
            if ph == 0 && used < nx {
                hist.copy_within(0..t - 1, 1);
                hist[0] = self.ms[used];
                used += 1;
            }
            let mut acc = 0.0f32;
            for k in 0..t {
                let j = ph + k * dd;
                if j < self.interp.len() {
                    acc += self.interp[j] * hist[k];
                }
            }
            self.gain[i] = acc;
        }
        self.int_hist.copy_from_slice(&hist[..t]);
        self.int_phase = (self.int_phase + ny) % dd;
    }

    fn process_block(&mut self, io: &mut [&mut [f32]], at: usize, n: usize) {
        let c = self.channels;
        let f = self.cfg.max_frames as usize;
        let dd = self.d.decim as usize;
        let l = self.d.latency as usize;

        for ch in 0..c {
            for i in 0..n {
                self.work[ch * f + i] = io[ch][at + i] * self.d.gain_in;
            }
        }
        if !self.active {
            for ch in 0..c {
                io[ch][at..at + n].copy_from_slice(&self.work[ch * f..ch * f + n]);
            }
            return;
        }
        // the sidechain weighting, direct form II transposed
        for ch in 0..c {
            for i in 0..n {
                let mut v = self.work[ch * f + i];
                if self.sidechain_on {
                    for k in 0..3 {
                        v = self.sc[k].run(&mut self.sc_state[ch][k], v);
                    }
                }
                self.side[ch * f + i] = v;
            }
        }
        // the decimator, unfiltered, phase across blocks; interleaved for the RMS
        let off = (dd - self.dec_phase) % dd;
        let mut nd = 0;
        let mut i = off;
        while i < n {
            for ch in 0..c {
                self.dec[nd * c + ch] = self.side[ch * f + i];
            }
            nd += 1;
            i += dd;
        }
        self.dec_phase = (n + self.dec_phase) % dd;
        self.rms.process(&self.dec[..nd * c], &mut self.ms[..nd]);
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.mean_square.clear();
            tr.mean_square.extend_from_slice(&self.ms[..nd]);
        }
        if self.limiter_on {
            self.lim_rms
                .process(&self.dec[..nd * c], &mut self.lrms[..nd]);
        }
        self.calculate_gain(nd);
        #[cfg(feature = "internals")]
        if let Some(tr) = self.trace.as_mut() {
            tr.activity.clear();
            tr.activity.extend_from_slice(&self.act[..nd]);
        }
        self.interpolate(nd, n);
        // the 74-sample delay, then the gain
        for ch in 0..c {
            let x = &self.work[ch * f..ch * f + n];
            let dl = &mut self.delay[ch];
            for i in 0..n {
                let v = if i < l { dl[i] } else { x[i - l] };
                io[ch][at + i] = v * self.gain[i];
            }
            crate::delay_shift(dl, x);
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

/// The release and the attack transfer curve for one level: (release,
/// attack) gain in dB.
fn transfer_curve(c: &[f32; 25], x: f32) -> (f32, f32) {
    let slope = c[3];
    let intercept = c[13];
    let add = c[6];

    let mut rel = if c[2] <= x {
        if x < c[8] {
            c[11] * x + c[14]
        } else {
            0.0
        }
    } else {
        c[3] * x + c[13] + c[6]
    };
    let (lo, hi) = (c[4], c[5]);
    if lo < x && x < hi {
        let end = c[11] * hi + c[14];
        let t = (((lo + hi) * 0.5 - x) / (lo - hi)).abs();
        let mm = (end - (slope * lo + intercept + add)) / (hi - lo);
        rel = (t + 0.5) * rel + (1.0 - (t + 0.5)) * (mm * x + (end - hi * mm));
    }
    if rel < 0.0 {
        rel = 0.0;
    }
    let floor = c[18];
    if !(floor <= rel) && x < c[2] {
        rel = floor;
    }
    let (lo, hi) = (c[16], c[17]);
    if lo < x && x < hi {
        let t = (((lo + hi) * 0.5 - x) / (lo - hi)).abs();
        let mm = (0.0 - (c[11] * lo + c[14])) / (hi - lo);
        rel = (t + 0.5) * rel + (1.0 - (t + 0.5)) * (mm * x + (0.0 - hi * mm));
    }
    let mut relhi = 0.0f32;
    if c[20] <= x {
        relhi = c[21] * x + c[22];
    }
    let hw = c[24];
    let (lo, hi) = (c[20] - hw, c[20] + hw);
    if lo < x && x < hi {
        let end = c[21] * hi + c[22];
        let t = (((lo + hi) * 0.5 - x) / (lo - hi)).abs();
        let mm = end / (hi - lo);
        relhi = (t + 0.5) * relhi + (1.0 - (t + 0.5)) * (mm * x + (end - hi * mm));
    }
    let mut att = if c[7] <= x {
        if x < c[9] {
            c[12] * x + c[15]
        } else {
            0.0
        }
    } else {
        slope * x + intercept + add
    };
    let lo = c[7] - c[10];
    if lo < x {
        let hi = c[7] + c[10];
        if x < hi {
            let end = c[12] * hi + c[15];
            let t = (((lo + hi) * 0.5 - x) / (lo - hi)).abs();
            let mm = (end - (add + slope * lo + intercept)) / (hi - lo);
            att = (t + 0.5) * att + (1.0 - (t + 0.5)) * (mm * x + (end - hi * mm));
        }
    }
    if att < 0.0 {
        att = 0.0;
    }
    let attf = if x < c[2] { floor } else { att };
    let mut atthi = 0.0f32;
    if c[19] < x {
        atthi = c[21] * x + c[23];
    }
    let lo = c[19] - hw;
    if lo < x {
        let hi = hw + c[19];
        if x < hi {
            let end = c[21] * hi + c[23];
            let t = (((lo + hi) * 0.5 - x) / (lo - hi)).abs();
            let mm = end / (hi - lo);
            atthi = (t + 0.5) * atthi + (1.0 - (t + 0.5)) * (mm * x + (end - hi * mm));
        }
    }
    let release = (rel + relhi) * c[0] + c[1];
    let attack = ((if floor <= att { att } else { attf }) + atthi) * c[0] + c[1];
    (release, attack)
}

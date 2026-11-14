// SPDX-License-Identifier: MIT
//! BuzzKill: adaptive rattle suppression on the woofers.
//!
//! The channels are summed, low-passed at 2500 Hz and taken every 4th sample;
//! a Hilbert pair feeds two 512-point Hann FFTs at a hop of 128. A bin whose
//! spectrum, after the attenuation already applied there, exceeds the
//! detection threshold (three peaking bands make it 9 to 14 dB more sensitive
//! at 87, 160 and 243 Hz) becomes a partial: its frequency from the ratio of
//! the first-difference spectrum to the plain one, its gain
//! threshold/amplitude. Candidates are taken in the order of their bin's
//! stored attenuation, and a bin's attenuation is updated before the
//! frequency test. Up to 50 notches, each `out = in - (1-g) BP(in)` at
//! 8820 Hz, their depth smoothed by a first-order low-pass and released over
//! 200 ms.
//! Latency 220 samples.
use crate::Section;

pub const MAX_CHANNELS: usize = 2;
pub const MAX_FILTERS: usize = 100;
pub const MAX_BINS: usize = 512;

const LN10_20: f32 = 0.115129255;
const FFT_MAX: usize = 8192;
const SRC_TAPS: usize = 65;
const SRC_PHASE_TAPS: usize = 16;
const BANK_BLOCK: usize = 512;
const FRAMES_MAX: usize = 512;

/// The notch path's decimation and interpolation filter: a symmetric 65-tap
/// lowpass that passes up to 0.05 of the sample rate, is 29 dB down at 0.1,
/// the Nyquist frequency after the default decimation by 5, and 115 dB down
/// from 0.125 on.
#[rustfmt::skip]
const SRC_COEFFS: [f32; SRC_TAPS] = [
    1.8717052e-6, -1.5074787e-6, -1.9985771e-5, -7.3750874e-5, -1.8883731e-4, -3.8873925e-4,
    -6.7870383e-4, -1.0260936e-3, -1.3439725e-3, -1.4881477e-3, -1.2777888e-3, -5.448514e-4,
    7.9225615e-4, 2.647812e-3, 4.7088987e-3, 6.4292434e-3, 7.103149e-3, 6.027291e-3, 2.7300382e-3,
    -2.78204e-3, -9.8327305e-3, -1.7015813e-2, -2.2321682e-2, -2.346394e-2, -1.8362597e-2,
    -5.6860102e-3, 1.4681212e-2, 4.1378647e-2, 7.161602e-2, 1.01552464e-1, 1.2695219e-1,
    1.4398295e-1, 1.4997818e-1, 1.4398295e-1, 1.2695219e-1, 1.01552464e-1, 7.161602e-2,
    4.1378647e-2, 1.4681212e-2, -5.6860102e-3, -1.8362597e-2, -2.346394e-2, -2.2321682e-2,
    -1.7015813e-2, -9.8327305e-3, -2.78204e-3, 2.7300382e-3, 6.027291e-3, 7.103149e-3, 6.4292434e-3,
    4.7088987e-3, 2.647812e-3, 7.9225615e-4, -5.448514e-4, -1.2777888e-3, -1.4881477e-3,
    -1.3439725e-3, -1.0260936e-3, -6.7870383e-4, -3.8873925e-4, -1.8883731e-4, -7.3750874e-5,
    -1.9985771e-5, -1.5074787e-6, 1.8717052e-6,
];

pub const MAX_BANDS: usize = 10;

/// One band shaping the detection threshold: a peaking filter's response.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub on: bool,
    pub hz: f32,
    pub gain_db: f32,
    pub q: f32,
}

impl Band {
    pub const OFF: Band = Band {
        on: false,
        hz: 0.0,
        gain_db: 0.0,
        q: 1.0,
    };
}

/// The rattle suppression's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub channels: u32,
    pub sample_rate: u32,
    /// Pass the audio through unchanged, nothing analysed.
    pub bypass: bool,
    /// The detection threshold before the bands shape it.
    pub threshold_db: f32,
    /// Applied in order; a partial above the shaped threshold gets a notch.
    pub bands: [Band; MAX_BANDS],
    /// The analysis: a low-pass at `analysis_hz`, every `decimation`th
    /// sample (doubled above 68100 Hz, quadrupled above 136000 Hz), FFTs of
    /// 2^`fft_log2` points at `overlap_percent`.
    pub analysis_hz: f32,
    pub decimation: u32,
    pub fft_log2: u32,
    pub overlap_percent: f32,
    /// The FFTs see the Hilbert pair of the mono sum, not the sum twice.
    pub hilbert: bool,
    /// The lowest frequency a notch is placed at.
    pub lowest_hz: f32,
    /// Partials closer than this, in octaves, share a notch.
    pub exclusion_octaves: f32,
    /// How many notches can run.
    pub notches: u32,
    /// A notch is reused only while its gain is above this.
    pub reuse_below_db: f32,
    /// The time notches and the bins' attenuation take to recover.
    pub release_ms: f32,
    /// The notch bank runs at sample_rate / notch_decimation.
    pub notch_decimation: u32,
    /// Every notch's Q.
    pub notch_q: f32,
    /// A notch stops running once its attenuation is below this.
    pub gate_db: f32,
    /// The notch depth's smoothing.
    pub smoothing_ms: f32,
    /// The audio's delay, the analysis's lookahead.
    pub latency_ms: f32,
}

impl Config {
    /// The MacBookPro14,3 woofers: up to 50 notches below 2500 Hz, the
    /// threshold 14 to 9 dB lower around 87, 160 and 243 Hz, 200 ms release,
    /// 220 samples of delay at 44100 Hz.
    pub fn mbp143(sample_rate: u32) -> Self {
        let band = |hz, gain_db, q| Band {
            on: true,
            hz,
            gain_db,
            q,
        };
        Config {
            channels: 2,
            sample_rate,
            bypass: false,
            threshold_db: 0.0,
            bands: [
                band(86.66843, -13.909912, 4.336352),
                band(160.44878, -8.374657, 1.390011),
                band(243.06677, -13.822952, 3.4557483),
                Band::OFF,
                Band::OFF,
                Band::OFF,
                Band::OFF,
                Band::OFF,
                Band::OFF,
                Band::OFF,
            ],
            analysis_hz: 2500.0,
            decimation: 4,
            fft_log2: 9,
            overlap_percent: 75.0,
            hilbert: true,
            lowest_hz: 50.0,
            exclusion_octaves: 0.05,
            notches: 50,
            reuse_below_db: -30.0,
            release_ms: 200.0,
            notch_decimation: 5,
            notch_q: 6.0,
            gate_db: -50.0,
            smoothing_ms: 200.0,
            latency_ms: 5.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Derived {
    pub decim: u32, // the analysis decimation
    pub fft_size: u32,
    pub hop: u32,
    pub bins: u32, // the bins analysed
    pub bin_width: f32,
    pub notch_decim: u32,           // the notch path's decimation
    pub filters: u32,               // notches available
    pub latency: u32,               // samples
    pub delay1: u32,                // the audio delay before the notch path
    pub delay2: u32,                // and the notch path's own
    pub threshold: [f32; MAX_BINS], // the detection threshold per bin
}

/// After each analysis frame, for a meter or a test.
#[derive(Clone, Copy, Debug)]
pub struct State {
    pub frames: u32,   // analysis frames so far
    pub partials: u32, // accepted in the last frame
    pub partial_freq: [f32; MAX_FILTERS],
    pub partial_gain: [f32; MAX_FILTERS],
    pub spectrum: [f32; MAX_BINS],    // the raw amplitude spectrum
    pub attenuation: [f32; MAX_BINS], // each bin's stored attenuation
    pub filter_freq: [f32; MAX_FILTERS],
    pub filter_gain: [f32; MAX_FILTERS],
    pub active: u32, // notches the bank ran last
}

impl Default for State {
    fn default() -> Self {
        State {
            frames: 0,
            partials: 0,
            partial_freq: [0.0; MAX_FILTERS],
            partial_gain: [0.0; MAX_FILTERS],
            spectrum: [0.0; MAX_BINS],
            attenuation: [0.0; MAX_BINS],
            filter_freq: [0.0; MAX_FILTERS],
            filter_gain: [0.0; MAX_FILTERS],
            active: 0,
        }
    }
}

struct Fft {
    frame: Vec<f32>, // the last N samples
    fill: usize,     // samples in the frame
    offset: usize,   // into the block being fed
    amp: Vec<f32>,   // FFT_MAX/2 + 1
    last0: f32,      // the plain pass's windowed first sample, reused as the
                     // first-difference pass's first sample
}

#[derive(Clone, Copy, Default)]
struct Notch {
    att: f32,      // 1 - gain
    freq_set: f32, // what the band-pass was built for
    q_set: f32,
    flag: bool,  // running, held until the smoother is below the gate
    bp: Section, // unity-gain band-pass
    bp_state: [[f32; 2]; MAX_CHANNELS],
    sm_state: [f32; 2], // the depth smoother's state
}

#[derive(Clone, Copy, Default)]
struct SortItem {
    key: f32,
    bin: i32,
}

pub struct BuzzKill {
    cfg: Config,
    d: Box<Derived>,
    st: Box<State>,
    channels: usize,
    sr: u32,
    n: usize, // the FFT size
    window: Vec<f32>,
    lpf: Section,
    lpf_state: [f32; 2],
    dec_phase: usize,
    hb: [[Section; 2]; 2],
    hb_state: [[[f32; 2]; 2]; 2],
    fft_im: Fft, // gets the Hilbert pair's imaginary part
    fft_re: Fft, // and the real one
    fft_re_buf: Vec<f64>,
    fft_im_buf: Vec<f64>,
    bin_att: [f32; MAX_BINS],  // the attenuation applied per bin
    weighted: [f32; MAX_BINS], // the spectrum after the bins' attenuation
    freqs: [f32; MAX_FILTERS], // the notches' frequencies
    gains: [f32; MAX_FILTERS], // and gains
    pf: [f32; MAX_FILTERS],    // the partials found: frequency
    pg: [f32; MAX_FILTERS],    // gain
    phi: [f32; MAX_FILTERS],   // exclusion band, upper
    plo: [f32; MAX_FILTERS],   // and lower
    tol: f32,                  // relative frequency tolerance
    excl_hi: f32,              // 2^exclusion_octaves
    excl_lo: f32,              // and its reciprocal
    recycle: f32,              // 1 - the reuse gain
    idle: bool,                // nothing to release yet
    // the notch path
    notch: Box<[Notch; MAX_FILTERS]>,
    bank_rate: u32,
    gate: f32, // linear
    smoother: Section,
    q_mult: f32,
    src: [f32; SRC_TAPS], // SRC_COEFFS * sqrt(notch_decimation)
    src_hist: [[f32; SRC_TAPS]; MAX_CHANNELS],
    src_dec_phase: usize,
    src_int_phase: usize,
    int_hist: [[f32; SRC_PHASE_TAPS]; MAX_CHANNELS],
    delay1: Vec<Vec<f32>>,
    delay2: Vec<Vec<f32>>,
    mono: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
    dec: Vec<f32>, // MAX_CHANNELS * BANK_BLOCK
    tmp: [[f32; BANK_BLOCK]; MAX_CHANNELS],
    sm: [f32; BANK_BLOCK],
    keep: Vec<f32>,
    list: Vec<SortItem>,
}

/// The biquad shapes the block uses.
#[derive(Clone, Copy)]
enum Shape {
    Peak,
    FirstOrderLowpass,
    ButterworthLowpass,
    Bandpass,
}

/// w = 2 pi f / sr; alpha = sin(w) * sinh(1/(2Q))
fn sec_design(kind: Shape, f: f64, gain_db: f64, q: f64, sr: u32) -> Section {
    let w = (f * 6.283185307179586) / sr as f64;
    let c = w.cos();
    let al = w.sin() * (1.0 / (q + q)).sinh();
    let (b0, a1): (f64, f64);
    let (mut b1, mut b2, mut a2) = (0.0f64, 0.0, 0.0);
    match kind {
        Shape::Peak => {
            let a = 10.0f64.powf(gain_db * 0.025);
            let a0 = 1.0 / (al / a + 1.0);
            a1 = c * -2.0 * a0;
            a2 = (1.0 - al / a) * a0;
            b0 = (al * a + 1.0) * a0;
            b2 = (1.0 - al * a) * a0;
            b1 = a1;
        }
        Shape::FirstOrderLowpass => {
            let k = (w * 0.5).tan();
            b0 = 1.0 / (1.0 / k + 1.0);
            a1 = (1.0 - 1.0 / k) * b0;
            b1 = b0;
        }
        Shape::ButterworthLowpass => {
            let k = 1.0 / (w * 0.5).tan();
            let k2 = k * k;
            b0 = 1.0 / (k * 1.4142135623730951 + k2 + 1.0);
            b1 = b0 + b0;
            a1 = ((1.0 - k2) + (1.0 - k2)) * b0;
            a2 = (k * -1.4142135623730951 + 1.0 + k2) * b0;
            b2 = b0;
        }
        Shape::Bandpass => {
            // unity gain at the centre
            let k = (w * 0.5).cos() / (w * 0.5).sin();
            let k2 = k * k;
            let kq = k / q;
            let dn = kq + 1.0 + k2;
            b0 = kq / dn;
            a1 = ((1.0 - k2) + (1.0 - k2)) / dn;
            a2 = ((1.0 - kq) + k2) / dn;
            b2 = -b0;
        }
    }
    Section {
        c: [b0 as f32, b1 as f32, b2 as f32, a1 as f32, a2 as f32],
    }
}

/// An in-place radix-2 complex FFT, forward.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f64::consts::PI / len as f64;
        let half = len / 2;
        let mut i = 0;
        while i < n {
            for k in 0..half {
                let wr = (ang * k as f64).cos();
                let wi = (ang * k as f64).sin();
                let (ur, ui) = (re[i + k], im[i + k]);
                let vr = re[i + k + half] * wr - im[i + k + half] * wi;
                let vi = re[i + k + half] * wi + im[i + k + half] * wr;
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + half] = ur - vr;
                im[i + k + half] = ui - vi;
            }
            i += len;
        }
        len <<= 1;
    }
}

/// An iterative quicksort, ascending by key. Not the standard library's sort,
/// because of ties: every bin that has not been attenuated yet has the same
/// key, so ties are the normal case when several partials start together, and
/// the order they come out in decides which partials get a notch. The
/// standard sorts order ties differently, and the block is tuned with this
/// order.
fn quicksort(a: &mut [SortItem]) {
    if a.is_empty() {
        return;
    }
    let mut sl = [0i32; MAX_BINS + 2];
    let mut sh = [0i32; MAX_BINS + 2];
    let mut u: i32 = 0;
    let mut hi = a.len() as i32 - 1;
    sl[0] = 0;
    sh[0] = hi;
    loop {
        let l = sl[u as usize];
        if l < hi {
            let mid = (hi + l) / 2;
            a.swap(l as usize, mid as usize);
            let piv = a[l as usize].key;
            let mut i = l + 1;
            let mut j = hi;
            loop {
                while i <= hi && a[i as usize].key <= piv {
                    i += 1;
                }
                while j >= l && a[j as usize].key > piv {
                    j -= 1;
                }
                if i < j {
                    a.swap(i as usize, j as usize);
                }
                if i > j {
                    break;
                }
            }
            a.swap(l as usize, j as usize);
            sl[u as usize] = l;
            sh[u as usize] = j - 1;
            sl[u as usize + 1] = j + 1;
            sh[u as usize + 1] = hi;
            u += 1;
        } else {
            u -= 1;
        }
        if u < 0 {
            return;
        }
        hi = sh[u as usize];
    }
}

/// A FIFO of dl.len() samples: y = x delayed, in place.
fn delay_line(dl: &mut [f32], io: &mut [f32], keep: &mut [f32]) {
    let l = dl.len();
    let n = io.len();
    if l == 0 {
        return;
    }
    keep[..n].copy_from_slice(io);
    for i in 0..n {
        io[i] = if i < l { dl[i] } else { keep[i - l] };
    }
    crate::delay_shift(dl, &keep[..n]);
}

impl BuzzKill {
    pub fn new(cfg: &Config) -> Option<Self> {
        let channels = cfg.channels as usize;
        if channels == 0 || channels > MAX_CHANNELS || cfg.sample_rate == 0 {
            return None;
        }
        let empty_fft = || Fft {
            frame: Vec::new(),
            fill: 0,
            offset: 0,
            amp: vec![0.0; FFT_MAX / 2 + 1],
            last0: 0.0,
        };
        let mut bk = BuzzKill {
            cfg: *cfg,
            d: Box::new(Derived {
                decim: 0,
                fft_size: 0,
                hop: 0,
                bins: 0,
                bin_width: 0.0,
                notch_decim: 0,
                filters: 0,
                latency: 0,
                delay1: 0,
                delay2: 0,
                threshold: [0.0; MAX_BINS],
            }),
            st: Box::default(),
            channels,
            sr: 0,
            n: 0,
            window: Vec::new(),
            lpf: Section::default(),
            lpf_state: [0.0; 2],
            dec_phase: 0,
            hb: crate::HILBERT,
            hb_state: [[[0.0; 2]; 2]; 2],
            fft_im: empty_fft(),
            fft_re: empty_fft(),
            fft_re_buf: Vec::new(),
            fft_im_buf: Vec::new(),
            bin_att: [0.0; MAX_BINS],
            weighted: [0.0; MAX_BINS],
            freqs: [0.0; MAX_FILTERS],
            gains: [0.0; MAX_FILTERS],
            pf: [0.0; MAX_FILTERS],
            pg: [0.0; MAX_FILTERS],
            phi: [0.0; MAX_FILTERS],
            plo: [0.0; MAX_FILTERS],
            tol: 0.0,
            excl_hi: 0.0,
            excl_lo: 0.0,
            recycle: 0.0,
            idle: true,
            notch: Box::new([Notch::default(); MAX_FILTERS]),
            bank_rate: 0,
            gate: 0.0,
            smoother: Section::default(),
            q_mult: 0.0,
            src: [0.0; SRC_TAPS],
            src_hist: [[0.0; SRC_TAPS]; MAX_CHANNELS],
            src_dec_phase: 0,
            src_int_phase: 0,
            int_hist: [[0.0; SRC_PHASE_TAPS]; MAX_CHANNELS],
            delay1: Vec::new(),
            delay2: Vec::new(),
            mono: vec![0.0; FRAMES_MAX],
            re: vec![0.0; FRAMES_MAX],
            im: vec![0.0; FRAMES_MAX],
            dec: vec![0.0; MAX_CHANNELS * BANK_BLOCK],
            tmp: [[0.0; BANK_BLOCK]; MAX_CHANNELS],
            sm: [0.0; BANK_BLOCK],
            keep: vec![0.0; FRAMES_MAX],
            list: Vec::with_capacity(MAX_BINS),
        };
        bk.derive();
        bk.fft_im.frame = vec![0.0; bk.n];
        bk.fft_re.frame = vec![0.0; bk.n];
        bk.fft_re_buf = vec![0.0; bk.n];
        bk.fft_im_buf = vec![0.0; bk.n];
        bk.delay1 = vec![vec![0.0; bk.d.delay1 as usize]; channels];
        bk.delay2 = vec![vec![0.0; bk.d.delay2 as usize]; channels];
        bk.reset();
        Some(bk)
    }

    fn derive(&mut self) {
        let cfg = self.cfg;
        self.sr = self.cfg.sample_rate;
        let mut dd = cfg.decimation;
        if self.sr > 136000 {
            dd <<= 2;
        } else if self.sr > 68100 {
            dd <<= 1;
        }
        self.d.decim = dd;
        // the analysis low-pass (the Q is not used by this shape)
        self.lpf = sec_design(
            Shape::ButterworthLowpass,
            cfg.analysis_hz as f64,
            0.0,
            0.7,
            self.sr,
        );
        // the analysis FFTs
        let log2n = cfg.fft_log2;
        self.n = (1usize << log2n).min(FFT_MAX);
        let n = self.n;
        self.d.fft_size = n as u32;
        self.d.hop = ((1.0 - cfg.overlap_percent / 100.0) * n as f32 + 0.5) as u32;
        self.window = (0..n)
            .map(|i| {
                (((i as f64 * 6.283185307179586) / (n as i32 - 1) as f64).cos() * -0.5 + 0.5) as f32
            })
            .collect();
        let fsum = self.window.iter().fold(0.0f32, |a, &v| a + v);
        for w in self.window.iter_mut() {
            *w = (0.5 / n as f32) * (*w * (n as f32 / fsum));
        }
        // bins and their thresholds
        let bw = (self.sr as f32 / dd as f32) / n as f32;
        self.d.bin_width = bw;
        self.d.bins = ((cfg.analysis_hz / bw + 0.5) as u32).min(MAX_BINS as u32);
        let mut npts = self.d.bins as usize;
        let mut pts = [0.0f32; MAX_BINS];
        for k in 0..self.d.bins as usize {
            let c = k as f32 * bw;
            let u = (bw / 2.15 + 0.5) as usize;
            npts = if u <= npts { u } else { npts };
            if npts < 3 {
                npts = 2;
            }
            let mut lo = c - bw * 0.5;
            if lo < 0.0 {
                lo = 0.0;
            }
            self.threshold_spectrum(&mut pts[..npts], lo, bw * 0.5 + c);
            let mut m = 1.0f32;
            for &v in &pts[..npts] {
                if v < m {
                    m = v;
                }
            }
            self.d.threshold[k] = m;
        }
        self.d.filters = cfg.notches.min(MAX_FILTERS as u32);
        self.d.notch_decim = cfg.notch_decimation;
        let sq = (self.d.notch_decim as f32).sqrt();
        for (s, &c) in self.src.iter_mut().zip(SRC_COEFFS.iter()) {
            *s = c * sq;
        }
        self.bank_rate = (self.sr as f32 / self.d.notch_decim as f32 + 0.5) as u32;
        let e = (cfg.exclusion_octaves * 0.6931472).exp();
        self.tol = e - 1.0;
        self.excl_hi = self.tol + 1.0;
        self.excl_lo = 1.0 / self.excl_hi;
        self.recycle = 1.0 - (cfg.reuse_below_db * 2.3025851 * 0.05).exp();
        let l = (self.sr as f32 * (cfg.latency_ms / 1000.0) + 0.5) as u32;
        self.d.latency = l;
        self.d.delay1 = l.saturating_sub(65);
        self.d.delay2 = 64;
        // the bank: Q, gate, and the depth smoother at 1000/smoothing_ms Hz
        self.q_mult = cfg.notch_q;
        self.gate = (cfg.gate_db * LN10_20).exp();
        self.smoother = sec_design(
            Shape::FirstOrderLowpass,
            1000.0 / cfg.smoothing_ms as f64,
            0.0,
            0.5,
            self.bank_rate,
        );
    }

    pub fn reset(&mut self) {
        self.lpf_state = [0.0; 2];
        self.hb_state = [[[0.0; 2]; 2]; 2];
        self.dec_phase = 0;
        for f in [&mut self.fft_im, &mut self.fft_re] {
            f.frame.fill(0.0);
            f.fill = 0;
            f.offset = 0;
            f.last0 = 0.0;
        }
        self.bin_att = [1.0; MAX_BINS];
        self.freqs = [20.0; MAX_FILTERS];
        self.gains = [1.0; MAX_FILTERS];
        *self.notch = [Notch {
            freq_set: -1.0,
            ..Notch::default()
        }; MAX_FILTERS];
        self.set_notch_filters();
        self.idle = true;
        self.src_hist = [[0.0; SRC_TAPS]; MAX_CHANNELS];
        self.int_hist = [[0.0; SRC_PHASE_TAPS]; MAX_CHANNELS];
        self.src_dec_phase = 0;
        self.src_int_phase = 0;
        for dl in self.delay1.iter_mut().chain(self.delay2.iter_mut()) {
            dl.fill(0.0);
        }
        *self.st = State::default();
    }

    /// The channels it was configured for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn derived(&self) -> &Derived {
        &self.d
    }

    pub fn state(&self) -> &State {
        &self.st
    }

    /// Linear spacing, the bands evaluated at sr: the
    /// detection threshold at out.len() points from f0 to f1 Hz.
    pub fn threshold_spectrum(&self, out: &mut [f32], f0: f32, f1: f32) {
        let n = out.len();
        let nyq = (self.sr >> 1) as f32;
        let lo = if f0 <= nyq { f0 } else { nyq };
        let hi = if f1 <= nyq { f1 } else { nyq };
        let step = (hi - lo) / (n - 1) as f32;
        let flat = (self.cfg.threshold_db * LN10_20).exp();
        out.fill(flat);
        for band in &self.cfg.bands {
            if !band.on {
                continue;
            }
            let s = sec_design(
                Shape::Peak,
                band.hz as f64,
                band.gain_db as f64,
                band.q as f64,
                self.sr,
            )
            .c;
            for (i, o) in out.iter_mut().enumerate() {
                let w = (((lo + i as f32 * step) * 6.2831855) / self.sr as f32) as f64;
                let c = w.cos() as f32;
                let sn = w.sin() as f32;
                let c2 = c * c - sn * sn;
                let s2 = -sn * c + -sn * c;
                let nr = c2 * s[2] + c * s[1] + s[0];
                let ni = s[2] * s2 - s[1] * sn;
                let dr = c2 * s[4] + c * s[3] + 1.0;
                let di = s2 * s[4] - sn * s[3];
                let dd = dr * dr + di * di;
                let hr = (nr * dr + ni * di) / dd;
                let hi_ = (dr * ni - di * nr) / dd;
                *o = (hr * hr + hi_ * hi_).sqrt() * *o;
            }
        }
    }

    /// The amplitude spectrum of the frame, or of its first difference: twice
    /// the DFT's magnitude; bin 0 is |DC|.
    fn fft_amplitude(&mut self, imag: bool, diff: bool, nb: usize) {
        let f = if imag {
            &mut self.fft_im
        } else {
            &mut self.fft_re
        };
        let re = &mut self.fft_re_buf;
        let im = &mut self.fft_im_buf;
        for n in 0..self.n {
            let v = if diff {
                if n == 0 {
                    f.last0
                } else {
                    (f.frame[n] - f.frame[n - 1]) * self.window[n]
                }
            } else {
                f.frame[n] * self.window[n]
            };
            if !diff && n == 0 {
                f.last0 = v;
            }
            re[n] = v as f64;
            im[n] = 0.0;
        }
        fft(re, im);
        for n in 0..nb {
            f.amp[n] = (2.0 * (re[n] * re[n] + im[n] * im[n]).sqrt()) as f32;
        }
        f.amp[0] = (2.0 * re[0].abs()) as f32;
    }

    /// Feed the FFT's frame: returns the samples taken and
    /// the hop when a frame completed. `x` is the whole block, the FFT keeps
    /// its own offset into it.
    fn fft_feed(&mut self, imag: bool, from_hilbert: bool, n: usize) -> (usize, usize) {
        let big_n = self.n;
        let hop = self.d.hop as usize;
        let bins = self.d.bins as usize;
        let f = if imag {
            &mut self.fft_im
        } else {
            &mut self.fft_re
        };
        let x = match (from_hilbert, imag) {
            (true, true) => &self.im,
            (true, false) => &self.re,
            (false, _) => &self.mono,
        };
        let room = hop - f.fill;
        let take = n.min(room);
        if n == 0 {
            return (0, 0);
        }
        f.frame.copy_within(take.., 0);
        f.frame[big_n - take..].copy_from_slice(&x[f.offset..f.offset + take]);
        f.offset += take;
        f.fill += take;
        if n <= room {
            f.offset = 0;
        }
        let mut done = 0;
        if f.fill >= hop {
            done = f.fill;
            f.fill = 0;
            self.fft_amplitude(imag, false, bins);
        }
        (take, done)
    }

    fn partials_beyond_threshold(&mut self) -> usize {
        let nb = self.d.bins as usize;
        self.list.clear();
        for k in 1..nb {
            if self.weighted[k] > self.d.threshold[k] {
                self.list.push(SortItem {
                    key: self.bin_att[k],
                    bin: k as i32,
                });
            }
        }
        if self.list.is_empty() {
            return 0;
        }
        let mut plain_im = [0.0f32; MAX_BINS];
        let mut plain_re = [0.0f32; MAX_BINS];
        plain_im[..nb].copy_from_slice(&self.fft_im.amp[..nb]);
        plain_re[..nb].copy_from_slice(&self.fft_re.amp[..nb]);
        self.fft_amplitude(false, true, nb);
        self.fft_amplitude(true, true, nb);
        quicksort(&mut self.list);
        let l50 = self.sr as f64 / 6.283185307179586;
        let mut acc = 0;
        for idx in 0..self.list.len() {
            let b = self.list[idx].bin as usize;
            let (num, den) = if !self.cfg.hilbert {
                (self.fft_im.amp[b], plain_im[b])
            } else {
                (
                    (self.fft_im.amp[b] * self.fft_im.amp[b]
                        + self.fft_re.amp[b] * self.fft_re.amp[b])
                        .sqrt(),
                    (plain_im[b] * plain_im[b] + plain_re[b] * plain_re[b]).sqrt(),
                )
            };
            let r = num / (den + 9.99995e-41);
            let cr = (r as f64).cos();
            let c3 = (((1.0 - cr) + (1.0 - cr)) as f32).sqrt();
            let g = self.d.threshold[b] / (self.weighted[b] + 9.99995e-41);
            self.bin_att[b] *= g;
            self.bin_att[b - 1] = g.sqrt() * self.bin_att[b - 1];
            if b + 1 < nb {
                self.bin_att[b + 1] = g.sqrt() * self.bin_att[b + 1];
            }
            let f = (((l50 / self.d.decim as f64) * (r * r) as f64) / c3 as f64) as f32;
            if !(self.cfg.lowest_hz < f && f < self.cfg.analysis_hz) {
                continue;
            }
            if (0..acc).any(|j| self.plo[j] < f && f < self.phi[j]) {
                continue;
            }
            self.pf[acc] = f;
            self.pg[acc] = g;
            self.phi[acc] = self.excl_hi * f;
            self.plo[acc] = f * self.excl_lo;
            acc += 1;
            if acc == self.d.filters as usize {
                return acc;
            }
        }
        acc
    }

    fn release_filters(&mut self, frames: u32) {
        if frames == 0 || self.idle {
            return;
        }
        let k = 1.0
            - ((-1.0
                / ((self.cfg.release_ms as f64 / 1000.0) * (self.sr as f32 / frames as f32) as f64))
                as f32)
                .exp();
        for g in self.gains[..self.d.filters as usize].iter_mut() {
            *g = (1.0 - *g) * k + *g;
        }
        for a in self.bin_att[..self.d.bins as usize].iter_mut() {
            *a = (1.0 - *a) * k + *a;
        }
    }

    fn assign_filters(&mut self, n: usize) {
        let mut used = [false; MAX_FILTERS];
        for i in 0..n {
            let f = self.pf[i];
            let g = self.pg[i];
            let mut best = 1e10f32;
            let mut bj: Option<usize> = None;
            let mut done = false;
            for j in 0..self.d.filters as usize {
                let rel = ((f - self.freqs[j]) as f64).abs() as f32 * (1.0 / f);
                if rel < self.tol {
                    if !used[j] {
                        self.freqs[j] = f;
                        self.gains[j] = g * self.gains[j];
                        used[j] = true;
                    }
                    done = true;
                    break;
                }
                if self.recycle < self.gains[j] && rel < best {
                    bj = Some(j);
                    best = rel;
                }
            }
            if let (false, Some(j)) = (done, bj) {
                self.freqs[j] = f;
                self.gains[j] = g;
                used[j] = true;
            }
        }
    }

    /// Rebuild the band-pass of every notch whose frequency or Q moved.
    fn set_notch_filters(&mut self) {
        for j in 0..self.d.filters as usize {
            self.freqs[j] = self.freqs[j].clamp(20.0, 20000.0);
            self.gains[j] = self.gains[j].clamp(0.0, 1.0);
            let nf = &mut self.notch[j];
            nf.att = 1.0 - self.gains[j];
            let qe = self.q_mult * 1.0;
            if nf.freq_set != self.freqs[j] || nf.q_set != qe {
                nf.freq_set = self.freqs[j];
                nf.q_set = qe;
                nf.bp = sec_design(
                    Shape::Bandpass,
                    nf.freq_set as f64,
                    0.0,
                    qe as f64,
                    self.bank_rate,
                );
            }
        }
    }

    /// The notches, on dec at the bank rate, stride BANK_BLOCK
    fn notch_bank(&mut self, n: usize) {
        self.st.active = 0;
        for j in 0..self.d.filters as usize {
            let nf = &mut self.notch[j];
            if nf.att > self.gate {
                nf.flag = true;
            } else if !nf.flag {
                continue;
            }
            self.st.active += 1;
            for s in self.sm[..n].iter_mut() {
                *s = self.smoother.run(&mut nf.sm_state, nf.att);
            }
            // a block without samples cannot move the smoother
            if n > 0 && self.sm[n - 1] < self.gate {
                nf.flag = false;
            }
            for ch in 0..self.channels {
                let xc = &mut self.dec[ch * BANK_BLOCK..ch * BANK_BLOCK + n];
                for i in 0..n {
                    let t = nf.bp.run(&mut nf.bp_state[ch], xc[i] * self.sm[i]);
                    xc[i] = xc[i] - t;
                }
            }
        }
    }

    /// The notch path: residual = dec - bank(dec),
    /// interpolated back and taken off the 64-sample-delayed audio
    fn filter_bank(&mut self, io: &mut [&mut [f32]], at: usize, n: usize) {
        let c = self.channels;
        let dd = self.d.notch_decim as usize;
        let t = SRC_PHASE_TAPS;

        if self.idle || dd <= 1 {
            for ch in 0..c {
                delay_line(
                    &mut self.delay2[ch],
                    &mut io[ch][at..at + n],
                    &mut self.keep,
                );
            }
            return;
        }
        // decimate by D through the 65-tap filter
        let off = (dd - self.src_dec_phase) % dd;
        let mut m = 0;
        let mut i = off;
        while i < n {
            for ch in 0..c {
                let x = &io[ch][at..at + n];
                let mut acc = 0.0f32;
                for k in 0..SRC_TAPS {
                    let jx = i as isize - k as isize;
                    let v = if jx >= 0 {
                        x[jx as usize]
                    } else {
                        self.src_hist[ch][(SRC_TAPS as isize + jx) as usize]
                    };
                    acc += self.src[k] * v;
                }
                self.dec[ch * BANK_BLOCK + m] = acc;
            }
            m += 1;
            i += dd;
        }
        for ch in 0..c {
            crate::delay_shift(&mut self.src_hist[ch], &io[ch][at..at + n]);
        }
        self.src_dec_phase = (n + self.src_dec_phase) % dd;
        // the bank, then residual = in - out
        for ch in 0..c {
            self.tmp[ch][..m].copy_from_slice(&self.dec[ch * BANK_BLOCK..ch * BANK_BLOCK + m]);
        }
        self.notch_bank(m);
        for ch in 0..c {
            for i in 0..m {
                self.tmp[ch][i] = self.tmp[ch][i] - self.dec[ch * BANK_BLOCK + i];
            }
        }
        // the audio 64 samples later, minus the residual interpolated by D:
        // y[G] = sum_k h[G mod D + kD] r[floor(G/D) - k], per channel
        for ch in 0..c {
            let x = &mut io[ch][at..at + n];
            delay_line(&mut self.delay2[ch], x, &mut self.keep);
            let hist = &mut self.int_hist[ch];
            let mut used = 0;
            for i in 0..n {
                let ph = (self.src_int_phase + i) % dd;
                if ph == 0 && used < m {
                    hist.copy_within(0..t - 1, 1);
                    hist[0] = self.tmp[ch][used];
                    used += 1;
                }
                let mut acc = 0.0f32;
                for k in 0..t {
                    let jj = ph + k * dd;
                    if jj < SRC_TAPS {
                        acc += self.src[jj] * hist[k];
                    }
                }
                x[i] = x[i] - acc;
            }
        }
        self.src_int_phase = (self.src_int_phase + n) % dd;
    }

    /// One analysis frame.
    fn analyse(&mut self, hop: usize) {
        let nb = self.d.bins as usize;
        self.st.spectrum[..nb].copy_from_slice(&self.fft_im.amp[..nb]);
        self.release_filters(self.d.decim * hop as u32);
        for k in 0..nb {
            self.weighted[k] = self.fft_im.amp[k] * self.bin_att[k];
        }
        let np = self.partials_beyond_threshold();
        self.st.partials = np as u32;
        self.st.partial_freq[..np].copy_from_slice(&self.pf[..np]);
        self.st.partial_gain[..np].copy_from_slice(&self.pg[..np]);
        let skip = if self.st.active == 0 {
            self.idle = np == 0;
            self.idle
        } else {
            self.idle = false;
            false
        };
        if !skip {
            self.assign_filters(np);
            self.set_notch_filters();
        }
        self.st.frames += 1;
        self.st.attenuation[..nb].copy_from_slice(&self.bin_att[..nb]);
        let nf = self.d.filters as usize;
        self.st.filter_freq[..nf].copy_from_slice(&self.freqs[..nf]);
        self.st.filter_gain[..nf].copy_from_slice(&self.gains[..nf]);
    }

    fn audio_chunk(&mut self, io: &mut [&mut [f32]], at: usize, n: usize) {
        for ch in 0..self.channels {
            delay_line(
                &mut self.delay1[ch],
                &mut io[ch][at..at + n],
                &mut self.keep,
            );
        }
        self.filter_bank(io, at, n);
    }

    fn process_block(&mut self, io: &mut [&mut [f32]], at: usize, n: usize) {
        let c = self.channels;
        if self.cfg.bypass {
            return;
        }
        let scale = 2.0f32 / c as f32;
        for i in 0..n {
            let mut s = 0.0f32;
            for ch in 0..c {
                s += io[ch][at + i];
            }
            self.mono[i] = s * scale;
        }
        let dd = self.d.decim as usize;
        let mut nd = 0;
        if dd > 1 {
            let off = (dd - self.dec_phase) % dd;
            for i in 0..n {
                self.mono[i] = self.lpf.run(&mut self.lpf_state, self.mono[i]);
            }
            let mut i = off;
            while i < n {
                self.mono[nd] = self.mono[i];
                nd += 1;
                i += dd;
            }
            self.dec_phase = (n + self.dec_phase) % dd;
        } else {
            nd = n;
        }
        // the Hilbert pair
        for i in 0..nd {
            let v = self.mono[i];
            let (mut a, mut b) = (v, v);
            for k in 0..2 {
                a = self.hb[0][k].run(&mut self.hb_state[0][k], a);
                b = self.hb[1][k].run(&mut self.hb_state[1][k], b);
            }
            self.re[i] = a;
            self.im[i] = b;
        }
        let hilbert = self.cfg.hilbert;
        let (mut fed_im, mut fed_re, mut done) = (0, 0, 0);
        loop {
            let (t1, h1) = self.fft_feed(true, hilbert, nd - fed_im);
            fed_im += t1;
            let (t2, _) = self.fft_feed(false, hilbert, nd - fed_re);
            fed_re += t2;
            if h1 == 0 {
                break;
            }
            self.analyse(h1);
            let mut chunk = h1 * dd;
            if done + chunk > n {
                chunk = n - done;
            }
            self.audio_chunk(io, at + done, chunk);
            done += chunk;
        }
        if done < n {
            self.audio_chunk(io, at + done, n - done);
        }
    }

    /// Process in place, one slice per channel.
    pub fn process(&mut self, io: &mut [&mut [f32]]) {
        let n = io[0].len();
        let mut done = 0;
        while done < n {
            let k = (n - done).min(FRAMES_MAX);
            self.process_block(io, done, k);
            done += k;
        }
    }
}

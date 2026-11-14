// SPDX-License-Identifier: MIT
//! Thermal protection for four speakers.
//!
//! Four thermal models, one per speaker, in two arrays: A is fed the input
//! and computes the gain, B is fed the output and only accumulates the
//! temperature the speaker really gets. Both run per model block (5 ms, 221
//! samples at 44100 Hz), not per sample. Latency 0: the block is in place and
//! the gain it applies is the one computed at the end of the previous block.
//!
//! The model follows the clock as well as the audio: time that passes
//! without audio is a pause in which the speakers cool, and `process()`
//! advances the model by it. So the heat survives a stream that stops and
//! starts again.
use std::time::{Duration, Instant};

pub const MAX_CHANNELS: usize = 4;

const LN100: f64 = 4.605170185988091; // ln(100): the "99%" in the follower
const OVERRIDE_GAIN: f32 = 0.1; // -20 dB, the hard over-temperature floor
const PAUSE_MS: f64 = 100.0; // a gap longer than this is a pause
const MAX_PAUSE_MS: f64 = 1e9; // the analytic advance is capped here
const GAIN_LPF_HZ: f32 = 20.0; // the staircase smoother
const GAIN_LPF_Q: f32 = 0.707;

/// One speaker's parameters.
#[derive(Clone, Copy, Debug, Default)]
pub struct Speaker {
    pub input_scale: f32, // volts per unit of DSP signal
    pub r_e: f32,         // coil resistance, ohm
    pub r_fast: f32,      // fast thermal resistance, K/W
    pub r_slow: f32,      // slow thermal resistance, K/W
    pub tau_fast: f32,    // fast time constant, s
    pub tau_slow: f32,    // slow time constant, s
    pub ambient: f32,     // ambient temperature, C
    pub threshold: f32,   // threshold, C absolute
    pub attack_ms: f32,   // gain fall, ms to 99%
    pub release_ms: f32,  // gain rise, ms to 99%
    pub margin: f32,      // K above the threshold for the hard override
}

/// Which channels apply the same gain, the lowest of their own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sharing {
    /// Every channel its own gain.
    #[default]
    None,
    /// One gain for all channels.
    All,
    /// Channels 0 and 1, and 2 and 3: the tweeters and the woofers on the
    /// MacBookPro14,3.
    Pairs,
    /// Channels 0 and 2, and 1 and 3: the left and the right speakers.
    CrossPairs,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Config {
    pub channels: u32,    // 4 on the MacBookPro14,3
    pub sample_rate: u32, // 44100
    pub block_ms: f32,    // model block, ms
    pub sharing: Sharing,
    pub bypass: bool, // no processing
    pub spk: [Speaker; MAX_CHANNELS],
}

impl Config {
    /// The MacBookPro14,3: two tweeters and two woofers, `Sharing::Pairs`.
    /// Tweeters 5.1 ohm, woofers 4.4 ohm, both limited at 130 C, the tweeters
    /// and the woofers each sharing one gain.
    pub fn mbp143(sample_rate: u32) -> Self {
        let tweeter = Speaker {
            input_scale: 9.0,
            r_e: 5.1,
            r_fast: 39.5,
            r_slow: 20.8,
            tau_fast: 1.2,
            tau_slow: 80.6,
            ambient: 50.0,
            threshold: 130.0,
            attack_ms: 800.0,
            release_ms: 20000.0,
            margin: 15.0,
        };
        let woofer = Speaker {
            r_e: 4.4,
            r_fast: 18.9,
            r_slow: 20.9,
            tau_fast: 2.6,
            tau_slow: 133.9,
            ..tweeter
        };
        Config {
            channels: 4,
            sample_rate,
            block_ms: 5.0,
            sharing: Sharing::Pairs,
            bypass: false,
            spk: [tweeter, tweeter, woofer, woofer],
        }
    }
}

/// What new() derived from a Config, for a meter or a test.
#[derive(Clone, Copy, Debug, Default)]
pub struct Derived {
    pub inv_re: f32,    // 1/R_e
    pub a_fast: f32,    // exp(-1/(tau_fast*rate))
    pub a_slow: f32,    // exp(-1/(tau_slow*rate))
    pub k_attack: f32,  // 1 - exp(-ln100/(attack_ms*rate/1000))
    pub k_release: f32, // the same for release_ms
}

#[derive(Clone, Default)]
struct Model {
    ring: Vec<f32>,             // channels * block
    fill: usize,                // samples in the ring
    step_left: usize,           // samples the current gain step covers
    step: [f32; MAX_CHANNELS],  // the gain being emitted
    next: [f32; MAX_CHANNELS],  // the gain for the next block
    state: [f32; MAX_CHANNELS], // the follower state
    t_fast: [f32; MAX_CHANNELS],
    t_slow: [f32; MAX_CHANNELS],
    t_total: [f32; MAX_CHANNELS],
    p: [f32; MAX_CHANNELS],
    computes_gain: bool,
}

impl Model {
    fn new(channels: usize, block: usize, computes_gain: bool) -> Self {
        Model {
            ring: vec![0.0; channels * block],
            step_left: block,
            step: [1.0; MAX_CHANNELS],
            next: [1.0; MAX_CHANNELS],
            state: [1.0; MAX_CHANNELS],
            computes_gain,
            ..Default::default()
        }
    }
}

pub struct Thermal {
    cfg: Config,
    channels: usize,
    block: usize, // samples per model block
    rate: f32,    // model blocks per second
    d: [Derived; MAX_CHANNELS],
    one_a_fast: [f32; MAX_CHANNELS],
    one_a_slow: [f32; MAX_CHANNELS],
    a: Model,
    b: Model,
    gain: Vec<f32>, // channels * gain_frames
    gain_frames: usize,
    bq: crate::Section, // the 20 Hz gain smoother
    bq_state: [[f32; 2]; MAX_CHANNELS],
    /// The time the audio handed in so far reaches to, on the monotonic
    /// clock; what the clock is ahead of it by was a pause.
    covered: Option<Instant>,
}

/// A low-pass biquad with alpha = sin(w)*sinh(1/(2Q)), stored as
/// {b0, b1, b2, a1, a2} for y = b*x - a1*y' - a2*y''.
fn lowpass(fc: f32, q: f32, sr: f32) -> crate::Section {
    let w = 2.0 * std::f64::consts::PI * fc as f64 / sr as f64;
    let alpha = w.sin() * (1.0 / (2.0 * q as f64)).sinh();
    let cw = w.cos();
    let d = 1.0 / (alpha + 1.0);
    let mut c = [0.0f32; 5];
    c[0] = ((1.0 - cw) * 0.5 * d) as f32;
    c[1] = (2.0 * c[0] as f64) as f32;
    c[2] = c[0];
    c[3] = (-2.0 * cw * d) as f32;
    c[4] = ((1.0 - alpha) * d) as f32;
    crate::Section { c }
}

impl Thermal {
    pub fn new(cfg: &Config) -> Option<Self> {
        let channels = cfg.channels as usize;
        if !(1..=MAX_CHANNELS).contains(&channels) {
            return None;
        }
        let rate = 1000.0f32 / cfg.block_ms;
        let block = (cfg.sample_rate as f32 / rate + 0.5) as usize;
        if block < 1 {
            return None;
        }
        let mut d = [Derived::default(); MAX_CHANNELS];
        let mut one_a_fast = [0.0; MAX_CHANNELS];
        let mut one_a_slow = [0.0; MAX_CHANNELS];
        for ch in 0..channels {
            let s = &cfg.spk[ch];
            d[ch].inv_re = 1.0 / s.r_e;
            d[ch].a_fast = (-1.0f32 / (s.tau_fast * rate)).exp();
            one_a_fast[ch] = 1.0 - d[ch].a_fast;
            d[ch].a_slow = (-1.0f32 / (s.tau_slow * rate)).exp();
            one_a_slow[ch] = 1.0 - d[ch].a_slow;
            d[ch].k_attack = 1.0 - ((-LN100 / (s.attack_ms * rate / 1000.0) as f64) as f32).exp();
            d[ch].k_release = 1.0 - ((-LN100 / (s.release_ms * rate / 1000.0) as f64) as f32).exp();
        }
        let mut t = Thermal {
            cfg: *cfg,
            channels,
            block,
            rate,
            d,
            one_a_fast,
            one_a_slow,
            a: Model::new(channels, block, true),
            b: Model::new(channels, block, false),
            gain: Vec::new(),
            gain_frames: 0,
            bq: lowpass(GAIN_LPF_HZ, GAIN_LPF_Q, cfg.sample_rate as f32),
            bq_state: [[0.0; 2]; MAX_CHANNELS],
            covered: None,
        };
        t.reset();
        Some(t)
    }

    /// The channels it was configured for.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Model blocks per second.
    pub fn rate(&self) -> f32 {
        self.rate
    }

    /// Forget the accumulated heat and the gain: speakers at ambient
    /// temperature, which is where new() starts.
    pub fn reset(&mut self) {
        self.a.ring.fill(0.0);
        self.b.ring.fill(0.0);
        self.bq_state = [[0.0; 2]; MAX_CHANNELS];
        for ch in 0..self.channels {
            self.a.t_fast[ch] = 0.0;
            self.a.t_slow[ch] = 0.0;
            self.b.t_fast[ch] = 0.0;
            self.b.t_slow[ch] = 0.0;
            self.a.t_total[ch] = self.cfg.spk[ch].ambient;
            self.b.t_total[ch] = self.cfg.spk[ch].ambient;
            self.a.p[ch] = 0.0;
            self.b.p[ch] = 0.0;
            self.a.state[ch] = 1.0;
            self.a.next[ch] = 1.0;
            self.a.step[ch] = 1.0;
        }
        self.a.fill = 0;
        self.b.fill = 0;
        self.a.step_left = self.block;
        self.b.step_left = self.block;
        self.covered = None;
    }

    /// Allocate for blocks of at most `frames`, so process() never allocates.
    /// Call it from an LV2 activate; without it the first larger block
    /// allocates, which is not real-time safe.
    pub fn prepare(&mut self, frames: usize) {
        if frames > self.gain_frames {
            self.gain.resize(frames * self.channels, 0.0);
            self.gain_frames = frames;
        }
    }

    /// One model block for one channel: the ring already holds `block`
    /// samples.
    fn kernel(&mut self, which_b: bool, ch: usize) {
        let spk = self.cfg.spk[ch];
        let block = self.block;
        let b_slow = self.b.t_slow[ch];
        let b_total = self.b.t_total[ch];
        let d = self.d[ch];
        let (one_a_fast, one_a_slow) = (self.one_a_fast[ch], self.one_a_slow[ch]);
        let m = if which_b { &mut self.b } else { &mut self.a };
        let ring = &mut m.ring[ch * block..(ch + 1) * block];

        // 1. the ring is scaled to volts in place, 2. mean square / R_e = P
        let mut sum = 0.0f32;
        for v in ring.iter_mut() {
            *v *= spk.input_scale;
            sum += *v * *v;
        }
        let p = (sum / block as f32) * d.inv_re;
        m.p[ch] = p;

        // 3. two parallel RC poles
        let tf = m.t_fast[ch] * d.a_fast + p * spk.r_fast * one_a_fast;
        let ts = m.t_slow[ch] * d.a_slow + p * spk.r_slow * one_a_slow;
        m.t_fast[ch] = tf;
        m.t_slow[ch] = ts;
        m.t_total[ch] = tf + ts + spk.ambient;

        if !m.computes_gain {
            return;
        }

        // 4. the gain target, against the protected model's slow pole
        let mut target = (spk.threshold - spk.ambient - b_slow) / (tf + 1.0);
        if target < 0.0 {
            target = 0.0;
        } else if target > 1.0 {
            target = 1.0;
        }
        target = target.sqrt();

        // 5. the follower: down fast, up slow
        let k = if target < m.state[ch] {
            d.k_attack
        } else {
            d.k_release
        };
        let mut g = m.state[ch] + (target - m.state[ch]) * k;

        // 6. the hard override, on the temperature the speaker really has
        if b_total > spk.threshold + spk.margin {
            g = OVERRIDE_GAIN;
        }

        m.state[ch] = g;
        m.next[ch] = g;
    }

    /// Gain sharing: only `next` is overwritten, the states stay per channel.
    fn share(&mut self) {
        let n = self.channels;
        let next = &mut self.a.next;
        let pairs: &[(usize, usize)] = match self.cfg.sharing {
            Sharing::All => {
                let mut g = next[0];
                for &v in &next[1..n] {
                    if v < g {
                        g = v;
                    }
                }
                next[..n].fill(g);
                return;
            }
            Sharing::Pairs if n == 4 => &[(0, 1), (2, 3)],
            Sharing::CrossPairs if n == 4 => &[(0, 2), (1, 3)],
            _ => return,
        };
        for &(i, j) in pairs {
            let g = if next[i] < next[j] { next[i] } else { next[j] };
            next[i] = g;
            next[j] = g;
        }
    }

    /// Tell the model that `ms` milliseconds of silence were skipped, so it
    /// advances both arrays analytically instead of waiting. process() does
    /// this itself when the monotonic clock is more than 100 ms ahead of the
    /// audio it has been given; a caller that knows about a gap can say so
    /// directly.
    pub fn advance(&mut self, ms: f64) {
        if ms <= 0.0 {
            return;
        }
        let ms = if ms > MAX_PAUSE_MS { MAX_PAUSE_MS } else { ms };
        let blocks = ms / self.cfg.block_ms as f64;
        for ch in 0..self.channels {
            let d = self.d[ch];
            for m in [&mut self.a, &mut self.b] {
                m.t_fast[ch] *= (d.a_fast.ln() * blocks as f32).exp();
                m.t_slow[ch] *= (d.a_slow.ln() * blocks as f32).exp();
                m.t_total[ch] = m.t_fast[ch] + m.t_slow[ch] + self.cfg.spk[ch].ambient;
                m.p[ch] = 0.0;
            }
            // the closed form of the release follower: state += k*(1-state)
            // applied `blocks` times
            let a = &mut self.a;
            a.state[ch] = 1.0 - (1.0 - d.k_release).powf(blocks as f32) * (1.0 - a.state[ch]);
            a.next[ch] = a.state[ch];
        }
        // channels that share a gain go on sharing it
        if self.a.computes_gain {
            self.share();
        }
        self.a.step = self.a.next;
    }

    /// Process in place, one slice per channel, all of the same length.
    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.run(Instant::now(), channels);
    }

    /// process() with the time of the call given instead of read from the
    /// clock, for a test of the pause check.
    #[cfg(feature = "internals")]
    pub fn process_at(&mut self, now: Instant, channels: &mut [&mut [f32]]) {
        self.run(now, channels);
    }

    fn run(&mut self, now: Instant, channels: &mut [&mut [f32]]) {
        let n = channels.first().map_or(0, |c| c.len());
        if self.cfg.bypass || n == 0 {
            return;
        }
        let nch = self.channels;
        let block = self.block;

        // the pause check: the clock against the audio handed in so far.
        // Counting the audio, not the time between calls, keeps a large
        // block (8192 frames are 186 ms) from looking like a pause. A gap
        // too short for a pause is dropped, not carried on: the sound
        // card's clock and this one drift apart, and a card 50 ppm slow
        // would otherwise add up to a pause every half hour of playback.
        let mut covered = self.covered.unwrap_or(now);
        let gap = now.saturating_duration_since(covered).as_secs_f64() * 1e3;
        if gap > 0.0 {
            if gap > PAUSE_MS {
                self.advance(gap);
            }
            covered = now;
        }
        let secs = n as f64 / self.cfg.sample_rate as f64;
        self.covered = Some(covered + Duration::from_secs_f64(secs));

        self.prepare(n);

        // 1. the gain staircase for this call, model A fed the input
        let mut pos = 0;
        while pos < n {
            if self.a.step_left == 0 {
                for ch in 0..nch {
                    self.kernel(false, ch);
                }
                if self.a.computes_gain {
                    self.share();
                }
                self.a.step = self.a.next;
                self.a.fill = 0;
                self.a.step_left = block;
            }
            let take = (n - pos).min(self.a.step_left);
            let m = &mut self.a;
            for ch in 0..nch {
                let at = ch * block + m.fill;
                m.ring[at..at + take].copy_from_slice(&channels[ch][pos..pos + take]);
                self.gain[ch * n + pos..ch * n + pos + take].fill(m.step[ch]);
            }
            m.fill += take;
            m.step_left -= take;
            pos += take;
        }

        // 2. smooth the staircase, 3. apply it
        for ch in 0..nch {
            let g = &mut self.gain[ch * n..(ch + 1) * n];
            for v in g.iter_mut() {
                *v = self.bq.run(&mut self.bq_state[ch], *v);
            }
            for (x, &gv) in channels[ch].iter_mut().zip(g.iter()) {
                *x *= gv;
            }
        }

        // 4. model B sees what the speaker is actually being asked to take
        let mut pos = 0;
        while pos < n {
            if self.b.step_left == 0 {
                for ch in 0..nch {
                    self.kernel(true, ch);
                }
                self.b.fill = 0;
                self.b.step_left = block;
            }
            let take = (n - pos).min(self.b.step_left);
            let m = &mut self.b;
            for ch in 0..nch {
                let at = ch * block + m.fill;
                m.ring[at..at + take].copy_from_slice(&channels[ch][pos..pos + take]);
            }
            m.fill += take;
            m.step_left -= take;
            pos += take;
        }
    }

    /// The gain being applied.
    pub fn gain(&self, ch: usize) -> f32 {
        self.a.step[ch]
    }

    /// T_total of model B, C.
    pub fn temperature(&self, ch: usize) -> f32 {
        self.b.t_total[ch]
    }

    /// P of model A, W.
    pub fn power(&self, ch: usize) -> f32 {
        self.a.p[ch]
    }

    pub fn derived(&self, ch: usize) -> Derived {
        self.d[ch]
    }
}

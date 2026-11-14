// SPDX-License-Identifier: MIT
//! The six dynamics blocks of the MacBookPro14,3 speaker graph as LV2
//! plugins, one bundle, one binary. Thin wrappers: the algorithms are in the
//! dsp crate, which knows nothing about LV2. Each plugin is described by its
//! own .ttl beside this crate; manifest.ttl lists them.
//!
//! The parameters are the speakers', baked in. Every plugin but the thermal
//! protection has a bypass that keeps its latency, so switching it costs no
//! jump in time, and all have meters as control outputs.
//!
//! The audio is processed in chunks of at most `CHUNK` frames: the blocks'
//! results depend on where the blocks fall (decimation phases, the per-block
//! snap of the Loudness glide), and the cap keeps a large host block from
//! moving them.
//!
//! The input is always read into a scratch buffer first, so a host that
//! connects an output to the same buffer as an input gets the right result.
//! PipeWire's filter-chain gives every output port a buffer of its own.
//!
//! No plugin trusts its input. A sample that is not a number counts as
//! silence and one beyond `LIMIT` is clipped there (`sane`), so a NaN from a
//! client cannot settle in a filter's state and stay, and the output of
//! every plugin is finite whatever comes in. A sample rate outside
//! 8000 to 192000 Hz is refused at instantiation (`rate`). Every `run` holds
//! a `Fast`, so a filter ringing out in silence does not slow down.

use lv2::prelude::*;

mod buzzkill;
mod controlfreak;
mod dualband;
mod loudness;
mod mozart;
mod thermal;

/// The most any wrapper hands its core at once.
pub(crate) const CHUNK: usize = 512;

/// The largest input sample a plugin takes, 40 dB over full scale. Nothing in
/// the graph comes near it: Mozart's gain on a sudden loud start is about 16 dB.
pub(crate) const LIMIT: f32 = 100.0;

/// The sample rate, if it is one the plugins are made for.
pub(crate) fn rate(info: &PluginInfo) -> Option<u32> {
    let r = info.sample_rate();
    (8000.0..=192000.0).contains(&r).then_some((r + 0.5) as u32)
}

/// An input sample as a plugin takes it: a number, at most `limit`.
#[inline]
pub(crate) fn sane(x: f32, limit: f32) -> f32 {
    if x.is_nan() {
        0.0
    } else {
        x.clamp(-limit, limit)
    }
}

/// Copy `src` to `dst` through `sane`.
pub(crate) fn load(dst: &mut [f32], src: &[f32], limit: f32) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d = sane(s, limit);
    }
}

/// While one of these lives, the processor rounds results too small for a
/// normal float to zero instead of computing with them, which costs up to
/// 40 times the normal time per operation. A filter left to ring out in
/// silence ends up there and stays. The difference in the result is below
/// 1e-37 of full scale. On x86-64 only, which is what the machine is.
pub(crate) struct Fast {
    #[cfg(target_arch = "x86_64")]
    saved: u32,
}

impl Fast {
    #[inline]
    pub fn new() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            // flush-to-zero (bit 15) and denormals-are-zero (bit 6) of MXCSR
            let mut saved = 0u32;
            // SAFETY: stmxcsr and ldmxcsr read and write the SSE control
            // register through a pointer to a u32 on the stack. Hosts run
            // plugins with these two bits set as a matter of course
            // (PipeWire's cpu.zero.denormals), so the code has to give the
            // same audible result with them on anyway; drop() restores what
            // the host had.
            unsafe {
                std::arch::asm!("stmxcsr [{p}]", p = in(reg) &mut saved, options(nostack));
                let fast = saved | 0x8040;
                std::arch::asm!("ldmxcsr [{p}]", p = in(reg) &fast, options(nostack, readonly));
            }
            Fast { saved }
        }
        #[cfg(not(target_arch = "x86_64"))]
        Fast {}
    }
}

impl Drop for Fast {
    #[inline]
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: as in new()
        unsafe {
            std::arch::asm!("ldmxcsr [{p}]", p = in(reg) &self.saved, options(nostack, readonly));
        }
    }
}

/// A bypass port connected and on.
pub(crate) fn on(port: &Option<InputPort<Control>>) -> bool {
    port.as_ref().is_some_and(|p| **p > 0.5)
}

/// Set a control output if the host connected it.
pub(crate) fn set(port: &mut Option<OutputPort<Control>>, v: f32) {
    if let Some(p) = port.as_mut() {
        ***p = v;
    }
}

/// A stereo pair of scratch buffers, CHUNK frames each.
pub(crate) struct Scratch {
    pub l: [f32; CHUNK],
    pub r: [f32; CHUNK],
}

impl Scratch {
    pub fn new() -> Box<Self> {
        Box::new(Scratch {
            l: [0.0; CHUNK],
            r: [0.0; CHUNK],
        })
    }

    /// The first n frames of both, filled from a and b through `sane`.
    pub fn load(&mut self, a: &[f32], b: &[f32]) -> [&mut [f32]; 2] {
        let n = a.len();
        load(&mut self.l[..n], a, LIMIT);
        load(&mut self.r[..n], b, LIMIT);
        [&mut self.l[..n], &mut self.r[..n]]
    }
}

/// A stereo delay of a fixed number of samples: what a bypass puts in a
/// plugin's place so the latency stays the same.
pub(crate) struct Delay {
    ring: Vec<[f32; 2]>,
    pos: usize,
}

impl Delay {
    pub fn new(samples: usize) -> Self {
        Delay {
            ring: vec![[0.0; 2]; samples],
            pos: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.ring.len()
    }

    pub fn reset(&mut self) {
        self.ring.fill([0.0; 2]);
        self.pos = 0;
    }

    /// Delay l and r in place.
    pub fn run(&mut self, l: &mut [f32], r: &mut [f32]) {
        if self.ring.is_empty() {
            return;
        }
        for (l, r) in l.iter_mut().zip(r.iter_mut()) {
            let held = std::mem::replace(&mut self.ring[self.pos], [*l, *r]);
            [*l, *r] = held;
            self.pos += 1;
            if self.pos == self.ring.len() {
                self.pos = 0;
            }
        }
    }
}

lv2_descriptors!(
    thermal::Thermal4,
    controlfreak::ControlFreakPlugin,
    mozart::MozartPlugin,
    dualband::DualBandPlugin,
    buzzkill::BuzzKillPlugin,
    loudness::LoudnessPlugin
);

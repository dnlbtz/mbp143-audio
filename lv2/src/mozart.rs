// SPDX-License-Identifier: MIT
//! Mozart, the upward compressor at the head of the common path: stereo in
//! and out, a bypass, the gain in dB at the end of the last run and the
//! latency (74 samples) as control outputs.
//!
//! Bypass is a plain delay of the same length, and the core keeps running
//! beside it, so its state is current when bypass is switched off again.

use crate::{Delay, Fast, Scratch, CHUNK};
use dsp::mozart::{Config, Mozart};
use lv2::prelude::*;

#[derive(PortCollection)]
pub struct Ports {
    in_l: InputPort<Audio>,
    in_r: InputPort<Audio>,
    out_l: OutputPort<Audio>,
    out_r: OutputPort<Audio>,
    bypass: Option<InputPort<Control>>,
    gain: Option<OutputPort<Control>>,
    latency: Option<OutputPort<Control>>,
}

#[uri("urn:mbp143-audio:mozart")]
pub struct MozartPlugin {
    m: Mozart,
    delay: Delay,
    scratch: Box<Scratch>,
}

impl Plugin for MozartPlugin {
    type Ports = Ports;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let mut cfg = Config::mbp143(crate::rate(info)?);
        cfg.max_frames = CHUNK as u32;
        let m = Mozart::new(&cfg)?;
        let delay = Delay::new(m.derived().latency as usize);
        Some(MozartPlugin {
            m,
            delay,
            scratch: Scratch::new(),
        })
    }

    fn activate(&mut self, _features: &mut ()) {
        self.m.reset();
        self.delay.reset();
    }

    fn run(&mut self, ports: &mut Ports, _features: &mut (), n: u32) {
        let _fast = Fast::new();
        let n = n as usize;
        let bypass = crate::on(&ports.bypass);
        crate::set(&mut ports.latency, self.delay.len() as f32);
        let mut done = 0;
        while done < n {
            let m = (n - done).min(CHUNK);
            let [xl, xr] = self
                .scratch
                .load(&ports.in_l[done..done + m], &ports.in_r[done..done + m]);
            let (ol, or) = (
                &mut ports.out_l[done..done + m],
                &mut ports.out_r[done..done + m],
            );
            ol.copy_from_slice(xl);
            or.copy_from_slice(xr);
            // one copy goes through the delay and one through the core;
            // bypass says which of them is the output
            let ([dl, dr], mut core) = if bypass {
                ([ol, or], [xl, xr])
            } else {
                ([xl, xr], [ol, or])
            };
            self.delay.run(dl, dr);
            self.m.process(&mut core);
            done += m;
        }
        if n > 0 {
            let g = self.m.modulator()[(n - 1) % CHUNK];
            crate::set(&mut ports.gain, 20.0 * (g + 1e-10).log10());
        } else {
            crate::set(&mut ports.gain, 0.0);
        }
    }
}

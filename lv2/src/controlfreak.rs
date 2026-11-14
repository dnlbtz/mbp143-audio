// SPDX-License-Identifier: MIT
//! ControlFreak, the lookahead limiter on each branch: stereo in and out, a
//! bypass, the lowest gain of the last run and the latency (22 samples) as
//! control outputs.
//!
//! Two cores run all the time, so switching costs no click and no gap: the
//! bypass one is the same 22-sample delay at unity gain.

use crate::{Fast, Scratch, CHUNK};
use dsp::controlfreak::{Config, ControlFreak};
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

#[uri("urn:mbp143-audio:controlfreak")]
pub struct ControlFreakPlugin {
    cf: ControlFreak,
    cf_bypass: ControlFreak,
    scratch: Box<Scratch>,
}

impl Plugin for ControlFreakPlugin {
    type Ports = Ports;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let mut cfg = Config::mbp143(crate::rate(info)?);
        let cf = ControlFreak::new(&cfg)?;
        cfg.bypass = true;
        let cf_bypass = ControlFreak::new(&cfg)?;
        Some(ControlFreakPlugin {
            cf,
            cf_bypass,
            scratch: Scratch::new(),
        })
    }

    fn activate(&mut self, _features: &mut ()) {
        self.cf.reset();
        self.cf_bypass.reset();
    }

    fn run(&mut self, ports: &mut Ports, _features: &mut (), n: u32) {
        let _fast = Fast::new();
        let n = n as usize;
        let bypass = crate::on(&ports.bypass);
        let mut g = 1.0f32;
        crate::set(&mut ports.latency, self.cf.derived().latency as f32);
        let mut done = 0;
        while done < n {
            let m = (n - done).min(CHUNK);
            let mut x = self
                .scratch
                .load(&ports.in_l[done..done + m], &ports.in_r[done..done + m]);
            let mut out = [
                &mut ports.out_l[done..done + m],
                &mut ports.out_r[done..done + m],
            ];
            out[0].copy_from_slice(x[0]);
            out[1].copy_from_slice(x[1]);
            let (shown, hidden) = if bypass {
                (&mut self.cf_bypass, &mut self.cf)
            } else {
                (&mut self.cf, &mut self.cf_bypass)
            };
            hidden.process(&mut x);
            shown.process(&mut out);
            let mg = self.cf.min_gain();
            if mg < g {
                g = mg;
            }
            done += m;
        }
        crate::set(&mut ports.gain, g);
    }
}

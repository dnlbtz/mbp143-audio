// SPDX-License-Identifier: MIT
//! BuzzKill, the adaptive rattle suppression on the woofer branch: stereo in
//! and out, a bypass, the number of notches running and the latency (220
//! samples) as control outputs.
//!
//! Bypass is a plain delay of the same length, and the core keeps running
//! beside it, so its state is current when bypass is switched off again.

use crate::{Delay, Fast, Scratch, CHUNK};
use dsp::buzzkill::{BuzzKill, Config};
use lv2::prelude::*;

#[derive(PortCollection)]
pub struct Ports {
    in_l: InputPort<Audio>,
    in_r: InputPort<Audio>,
    out_l: OutputPort<Audio>,
    out_r: OutputPort<Audio>,
    bypass: Option<InputPort<Control>>,
    active: Option<OutputPort<Control>>,
    latency: Option<OutputPort<Control>>,
}

#[uri("urn:mbp143-audio:buzzkill")]
pub struct BuzzKillPlugin {
    bk: Box<BuzzKill>,
    delay: Delay,
    scratch: Box<Scratch>,
}

impl Plugin for BuzzKillPlugin {
    type Ports = Ports;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let bk = BuzzKill::new(&Config::mbp143(crate::rate(info)?))?;
        let d = bk.derived();
        let delay = Delay::new((d.delay1 + d.delay2) as usize);
        Some(BuzzKillPlugin {
            bk: Box::new(bk),
            delay,
            scratch: Scratch::new(),
        })
    }

    fn activate(&mut self, _features: &mut ()) {
        self.bk.reset();
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
            self.bk.process(&mut core);
            done += m;
        }
        crate::set(&mut ports.active, self.bk.state().active as f32);
    }
}

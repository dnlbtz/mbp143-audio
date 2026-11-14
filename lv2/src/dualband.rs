// SPDX-License-Identifier: MIT
//! The DualBand compressor on the common path: stereo in and out, a bypass,
//! and the two bands' gains in dB at the start of the last block as control
//! outputs. Latency 0, so bypass is the input; the core keeps running beside
//! it.

use crate::{Fast, Scratch, CHUNK};
use dsp::dualband::{Config, DualBand};
use lv2::prelude::*;

#[derive(PortCollection)]
pub struct Ports {
    in_l: InputPort<Audio>,
    in_r: InputPort<Audio>,
    out_l: OutputPort<Audio>,
    out_r: OutputPort<Audio>,
    bypass: Option<InputPort<Control>>,
    gain_low: Option<OutputPort<Control>>,
    gain_high: Option<OutputPort<Control>>,
}

#[uri("urn:mbp143-audio:dualband")]
pub struct DualBandPlugin {
    db: DualBand,
    scratch: Box<Scratch>,
    g_sub: f32,
    g_super: f32,
}

impl Plugin for DualBandPlugin {
    type Ports = Ports;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let mut cfg = Config::mbp143(crate::rate(info)?);
        cfg.max_frames = CHUNK as u32;
        let db = DualBand::new(&cfg)?;
        Some(DualBandPlugin {
            db,
            scratch: Scratch::new(),
            g_sub: 0.0,
            g_super: 0.0,
        })
    }

    fn activate(&mut self, _features: &mut ()) {
        self.db.reset();
    }

    fn run(&mut self, ports: &mut Ports, _features: &mut (), n: u32) {
        let _fast = Fast::new();
        let n = n as usize;
        let bypass = crate::on(&ports.bypass);
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
            self.db.process(if bypass { &mut x } else { &mut out });
            // a block that took no decimated sample leaves the last values
            if let Some((s, p)) = self.db.band_gains() {
                self.g_sub = s;
                self.g_super = p;
            }
            done += m;
        }
        crate::set(&mut ports.gain_low, 20.0 * (self.g_sub + 1e-10).log10());
        crate::set(&mut ports.gain_high, 20.0 * (self.g_super + 1e-10).log10());
    }
}

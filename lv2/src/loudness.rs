// SPDX-License-Identifier: MIT
//! Loudness with the speaker volume, after Mozart on the common path: stereo
//! in and out, the volume per channel as a linear gain on two control ports, a
//! bypass, and the two shelf gains in dB as meters. Latency 0.
//!
//! The volume ports are where the sink's volume goes: the filter-chain's
//! `capture.volumes` hands PipeWire's channel volumes (linear) to `volume_l`
//! and `volume_r` and stops applying them itself
//! (`tools/layout_to_filter_chain.py --lv2`), so the boost follows the volume
//! and the volume is applied after Mozart.
//!
//! Bypass drops the loudness shelves but not the volume, which the graph still
//! needs: a second core with both slopes at 0 runs alongside with the same
//! volume and ramp, and bypass picks its output.

use crate::{Fast, Scratch, CHUNK};
use dsp::loudness::{Config, Loudness};
use lv2::prelude::*;

/// The most a volume port is taken to say, 20 dB. PipeWire's own volumes
/// stop at 10.
const MAX_VOLUME: f32 = 10.0;

/// A volume port's value: 1 when the host left it unconnected, 0 when it is
/// not a number or negative.
fn volume(port: &Option<InputPort<Control>>) -> f32 {
    port.as_ref()
        .map_or(1.0, |p| crate::sane(**p, MAX_VOLUME).max(0.0))
}

#[derive(PortCollection)]
pub struct Ports {
    in_l: InputPort<Audio>,
    in_r: InputPort<Audio>,
    out_l: OutputPort<Audio>,
    out_r: OutputPort<Audio>,
    volume_l: Option<InputPort<Control>>,
    volume_r: Option<InputPort<Control>>,
    bypass: Option<InputPort<Control>>,
    low_db: Option<OutputPort<Control>>,
    high_db: Option<OutputPort<Control>>,
}

#[uri("urn:mbp143-audio:loudness")]
pub struct LoudnessPlugin {
    ld: Loudness,
    flat: Loudness,
    scratch: Box<Scratch>,
    fresh: bool,
}

impl Plugin for LoudnessPlugin {
    type Ports = Ports;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let mut cfg = Config::mbp143(crate::rate(info)?);
        let ld = Loudness::new(&cfg)?;
        cfg.low_slope = 0.0;
        cfg.high_slope = 0.0;
        let flat = Loudness::new(&cfg)?;
        Some(LoudnessPlugin {
            ld,
            flat,
            scratch: Scratch::new(),
            fresh: true,
        })
    }

    fn activate(&mut self, _features: &mut ()) {
        self.fresh = true;
    }

    fn run(&mut self, ports: &mut Ports, _features: &mut (), n: u32) {
        let _fast = Fast::new();
        let n = n as usize;
        let bypass = crate::on(&ports.bypass);
        let (l, r) = (volume(&ports.volume_l), volume(&ports.volume_r));
        self.ld.set_volume(l, r);
        self.flat.set_volume(l, r);
        if self.fresh {
            // start at the volume the host has set, without a glide: the
            // first run is the first time LV2 shows the plugin its ports
            self.ld.reset();
            self.flat.reset();
            self.fresh = false;
        }
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
                (&mut self.flat, &mut self.ld)
            } else {
                (&mut self.ld, &mut self.flat)
            };
            hidden.process(&mut x);
            shown.process(&mut out);
            done += m;
        }
        let st = self.ld.state();
        crate::set(&mut ports.low_db, if bypass { 0.0 } else { st.low_db });
        crate::set(&mut ports.high_db, if bypass { 0.0 } else { st.high_db });
    }
}

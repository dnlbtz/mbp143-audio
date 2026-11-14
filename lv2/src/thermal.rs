// SPDX-License-Identifier: MIT
//! The thermal speaker protection: four audio in, four audio out (tweeter L,
//! tweeter R, woofer L, woofer R, davidjo's order), and the per-channel gain
//! and speaker temperature as control outputs so a meter can watch them.
//!
//! It is the last thing between a program and the speakers, so it is built
//! to hold whatever the rest does:
//!
//! - No bypass. Every other plugin has one; this one cannot be switched off.
//! - The input is clipped at full scale. The sound card would clip there
//!   anyway, so nothing is lost, and the output is then at most the gain the
//!   model allows, however large the input.
//! - The model keeps its heat when the host deactivates and activates the
//!   plugin, which PipeWire does whenever playback pauses. The pause shows
//!   on the clock and the model cools by that much.
//!
//! A new instance starts with the speakers at ambient temperature. PipeWire
//! makes one when the graph is loaded: at login, after a restart of the
//! sound services, and when headphones come out. It would also make one
//! after every pause of more than 5 s, when WirePlumber suspends an idle
//! node, so speakers.json sets `session.suspend-timeout-seconds = 0` on the
//! graph's two nodes.

use crate::{Fast, CHUNK};
use dsp::thermal::{Config, Thermal, MAX_CHANNELS};
use lv2::prelude::*;

/// The largest sample the sound card can put out.
const FULL_SCALE: f32 = 1.0;

#[derive(PortCollection)]
pub struct Ports {
    in0: InputPort<Audio>,
    in1: InputPort<Audio>,
    in2: InputPort<Audio>,
    in3: InputPort<Audio>,
    out0: OutputPort<Audio>,
    out1: OutputPort<Audio>,
    out2: OutputPort<Audio>,
    out3: OutputPort<Audio>,
    gain0: Option<OutputPort<Control>>,
    gain1: Option<OutputPort<Control>>,
    gain2: Option<OutputPort<Control>>,
    gain3: Option<OutputPort<Control>>,
    temp0: Option<OutputPort<Control>>,
    temp1: Option<OutputPort<Control>>,
    temp2: Option<OutputPort<Control>>,
    temp3: Option<OutputPort<Control>>,
}

#[uri("urn:mbp143-audio:thermal")]
pub struct Thermal4 {
    model: Thermal,
    scratch: Box<[[f32; CHUNK]; MAX_CHANNELS]>,
}

impl Plugin for Thermal4 {
    type Ports = Ports;
    type InitFeatures = ();
    type AudioFeatures = ();

    fn new(info: &PluginInfo, _features: &mut ()) -> Option<Self> {
        let mut model = Thermal::new(&Config::mbp143(crate::rate(info)?))?;
        model.prepare(CHUNK);
        Some(Thermal4 {
            model,
            scratch: Box::new([[0.0; CHUNK]; MAX_CHANNELS]),
        })
    }

    // No activate(): see the top of this file.

    fn run(&mut self, ports: &mut Ports, _features: &mut (), n: u32) {
        let _fast = Fast::new();
        let n = n as usize;
        let ins: [&[f32]; 4] = [&ports.in0, &ports.in1, &ports.in2, &ports.in3];
        let mut outs: [&mut [f32]; 4] = [
            &mut ports.out0,
            &mut ports.out1,
            &mut ports.out2,
            &mut ports.out3,
        ];
        let mut done = 0;
        while done < n {
            let m = (n - done).min(CHUNK);
            let [s0, s1, s2, s3] = &mut *self.scratch;
            let mut io: [&mut [f32]; 4] = [&mut s0[..m], &mut s1[..m], &mut s2[..m], &mut s3[..m]];
            for (s, i) in io.iter_mut().zip(ins) {
                crate::load(s, &i[done..done + m], FULL_SCALE);
            }
            self.model.process(&mut io);
            for (o, s) in outs.iter_mut().zip(io.iter()) {
                o[done..done + m].copy_from_slice(s);
            }
            done += m;
        }
        let meters = [
            (&mut ports.gain0, &mut ports.temp0),
            (&mut ports.gain1, &mut ports.temp1),
            (&mut ports.gain2, &mut ports.temp2),
            (&mut ports.gain3, &mut ports.temp3),
        ];
        for (ch, (g, t)) in meters.into_iter().enumerate() {
            crate::set(g, self.model.gain(ch));
            crate::set(t, self.model.temperature(ch));
        }
    }
}

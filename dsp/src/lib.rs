// SPDX-License-Identifier: MIT
//! The six dynamics blocks of the MacBookPro14,3 speaker graph: thermal
//! protection, ControlFreak, Mozart, the DualBand, BuzzKill and Loudness,
//! each with the speakers' settings as `Config::mbp143()`.
//!
//! The arithmetic is f32, with f64 where a step needs the range, in a fixed
//! order, and Rust does not contract `a*b+c` into a fused multiply-add, so
//! the same input gives the same output, bit for bit, on every build. What
//! a build does not fix is the C library, where `exp`, `log`, `sin` and
//! `cos` come from.
//!
//! Every block is planar float and processes in place: one slice per channel.
//! Nothing allocates in `process()` once `prepare()` has seen the largest
//! block.

// Lints the code does not follow, and why:
// - approx_constant, excessive_precision: constants are written as their
//   digits (2*pi as 6.283185307179586, sqrt 2 as 1.4142135623730951).
// - nonminimal_bool, neg_cmp_op_on_partial_ord, manual_clamp: comparisons
//   stay as written; on floats `!(a < b)` and `a >= b` differ when a is NaN.
// - assign_op_pattern, needless_range_loop: steps are written `x = x + y` and
//   loops as index loops, so the order of the arithmetic is plain to see.
#![allow(
    clippy::approx_constant,
    clippy::excessive_precision,
    clippy::assign_op_pattern,
    clippy::needless_range_loop,
    clippy::nonminimal_bool,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::manual_clamp
)]

pub mod buzzkill;
pub mod controlfreak;
pub mod dualband;
pub mod loudness;
pub mod mozart;
pub mod thermal;

/// A biquad's coefficients: b0 b1 b2 a1 a2, a0 = 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Section {
    pub c: [f32; 5],
}

impl Section {
    /// One transposed direct form II step, the loop every block shares.
    #[inline]
    pub fn run(&self, s: &mut [f32; 2], v: f32) -> f32 {
        let c = &self.c;
        let o = c[0] * v + s[0];
        s[0] = c[1] * v - c[3] * o + s[1];
        s[1] = c[2] * v - c[4] * o;
        o
    }
}

/// The Hilbert pair ControlFreak and BuzzKill detect with: two paths of two
/// sections each, [path][section] as b0 b1 b2 a1 a2; the first path gives
/// the real part, the second the imaginary part. The pair is a rough one,
/// about 90 degrees apart from 50 to 300 Hz, with the imaginary path's gain
/// up to 1.55 at 3 kHz, and the second path's first a2 is a tenth of its b0
/// rather than equal to it. The detectors' thresholds are set for this
/// response, so it stays as it is.
#[rustfmt::skip]
pub(crate) const HILBERT: [[Section; 2]; 2] = [
    [
        Section { c: [0.94657, -1.9463, 1.0, -1.94632, 0.94657] },
        Section { c: [0.06338, -0.83774, 1.0, -0.83774, 0.06338] },
    ],
    [
        Section { c: [-0.260502, 0.02569, 1.0, 0.02569, -0.0260502] },
        Section { c: [1.1058, -2.373, 1.27, -1.8685, 0.870686] },
    ],
];

/// A FIFO of `len` samples per channel, carried across blocks: y = x delayed,
/// shifted on every block.
pub(crate) fn delay_shift(dl: &mut [f32], x: &[f32]) {
    let l = dl.len();
    let n = x.len();
    if n >= l {
        dl.copy_from_slice(&x[n - l..]);
    } else {
        dl.copy_within(n.., 0);
        dl[l - n..].copy_from_slice(x);
    }
}

/// Up to four channels laid out with `stride` in one buffer, as slices of `n`
/// samples each, without allocating; the channels past `channels` are empty.
pub(crate) fn planar(buf: &mut [f32], stride: usize, n: usize, channels: usize) -> [&mut [f32]; 4] {
    let mut it = buf.chunks_mut(stride);
    std::array::from_fn(|ch| {
        if ch < channels {
            &mut it.next().unwrap()[..n]
        } else {
            <&mut [f32]>::default()
        }
    })
}

/// A raised-cosine FIR of `len` taps, normalised to unity gain.
pub(crate) fn raised_cosine(len: usize) -> Vec<f32> {
    let mut rc: Vec<f32> = (0..len)
        .map(|k| {
            ((((k as f32) as f64 + 1.0) * std::f64::consts::PI) / (len + 1) as f64).sin() as f32
        })
        .collect();
    let sum = rc.iter().fold(0.0f32, |a, &v| a + v);
    let inv = 1.0f32 / sum;
    for v in rc.iter_mut() {
        *v *= inv;
    }
    rc
}

/// A linear interpolator of n taps: a triangle peaking at 1.
pub(crate) fn linear_interpolator(n: usize) -> Vec<f32> {
    let mut lin = vec![0.0f32; n];
    let half = (n + 1) >> 1;
    lin[half - 1] = 1.0;
    let mut top = n;
    for i in 0..half - 1 {
        let v = (i + 1) as f32 / half as f32;
        top -= 1;
        lin[i] = v;
        lin[top] = v;
    }
    lin
}

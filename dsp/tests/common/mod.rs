// SPDX-License-Identifier: MIT
//! What every test file shares: a checker that prints each assertion and
//! fails once at the end, so one run shows all of them.

#![allow(dead_code)]

pub const SR: u32 = 44100;
pub const BLOCK: usize = 512;

#[derive(Default)]
pub struct Checks {
    checks: usize,
    fails: Vec<String>,
}

impl Checks {
    pub fn check(&mut self, ok: bool, what: &str, got: f64, want: f64) {
        self.checks += 1;
        println!(
            "  {:<58} {:>14.9}  want {:>14.9}  {}",
            what,
            got,
            want,
            if ok { "ok" } else { "FAIL" }
        );
        if !ok {
            self.fails.push(what.to_string());
        }
    }

    pub fn near(&mut self, what: &str, got: f64, want: f64, tol: f64) {
        self.check((got - want).abs() <= tol, what, got, want);
    }

    pub fn done(self) {
        println!(
            "{} checks, {}",
            self.checks,
            if self.fails.is_empty() {
                "all ok"
            } else {
                "FAILURES"
            }
        );
        assert!(self.fails.is_empty(), "failed: {:?}", self.fails);
    }
}

/// A 1 kHz sine at sample t, computed in float.
pub fn sine1k(t: usize) -> f32 {
    (2.0f32 * std::f32::consts::PI * 1000.0 * t as f32 / SR as f32).sin()
}

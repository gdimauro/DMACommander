//! Classic demoscene plasma: summed sine fields sampled per cell, mapped through
//! a colour ramp.
//!
//! Purely a function of position and elapsed time — no per-cell state, so it
//! resizes for free and costs one `sin` cluster per visible cell.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

const RAMP: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

pub struct Plasma {
    t: f32,
}

impl Plasma {
    pub fn new() -> Self {
        Self { t: 0.0 }
    }

    /// A smooth cyclic palette. Phase-shifted sines beat a hand-written gradient:
    /// no banding, and it wraps seamlessly.
    fn colour(v: f32) -> Rgb {
        let f =
            |phase: f32| (((v * std::f32::consts::TAU + phase).sin() * 0.5 + 0.5) * 255.0) as u8;
        Rgb(
            f(0.0),
            f(std::f32::consts::TAU / 3.0),
            f(2.0 * std::f32::consts::TAU / 3.0),
        )
    }
}

impl Default for Plasma {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Plasma {
    fn name(&self) -> &'static str {
        "plasma"
    }

    fn resize(&mut self, _width: u16, _height: u16) {
        // Stateless in space: nothing to rebuild.
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        self.t += dt.as_secs_f32().min(0.1);
        // Keep `t` bounded: after hours of running, f32 precision on a large
        // accumulator visibly quantises the animation.
        if self.t > 1_000.0 {
            self.t -= 1_000.0;
        }

        let (w, h) = (canvas.width(), canvas.height());
        for y in 0..h {
            for x in 0..w {
                // Terminal cells are ~2:1, so x is compressed to keep the blobs round.
                let fx = x as f32 * 0.5;
                let fy = y as f32;

                let v = (fx * 0.18 + self.t).sin()
                    + (fy * 0.22 - self.t * 0.7).sin()
                    + ((fx + fy) * 0.13 + self.t * 0.4).sin()
                    + ((fx * fx + fy * fy).sqrt() * 0.2 - self.t * 1.1).sin();
                let n = (v + 4.0) / 8.0; // -4..4 -> 0..1

                let idx = ((n * (RAMP.len() - 1) as f32) as usize).min(RAMP.len() - 1);
                canvas.set(
                    x as i32,
                    y as i32,
                    Cell {
                        ch: RAMP[idx],
                        fg: Self::colour(n),
                        bg: Rgb::BLACK,
                    },
                );
            }
        }
    }
}

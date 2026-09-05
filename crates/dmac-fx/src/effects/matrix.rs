//! Digital rain.
//!
//! One independent drop per column, each with its own speed and trail length.
//! The bright head plus a fading tail is what sells the depth — a uniform green
//! column reads as a progress bar, not as rain.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

const HEAD: Rgb = Rgb(200, 255, 200);
const BODY: Rgb = Rgb(0, 220, 70);

struct Drop {
    /// Fractional row, so speed is independent of frame rate.
    y: f32,
    speed: f32,
    length: usize,
    /// Delay before this column starts falling again, so columns desynchronise.
    wait: f32,
}

impl Drop {
    fn respawn(height: u16) -> Self {
        Self {
            y: -(fastrand::f32() * height as f32),
            speed: 6.0 + fastrand::f32() * 22.0,
            length: 4 + fastrand::usize(..14),
            wait: fastrand::f32() * 2.0,
        }
    }
}

pub struct Matrix {
    drops: Vec<Drop>,
    height: u16,
}

impl Matrix {
    pub fn new() -> Self {
        Self {
            drops: Vec::new(),
            height: 0,
        }
    }
}

impl Default for Matrix {
    fn default() -> Self {
        Self::new()
    }
}

/// Half-width katakana plus digits — the glyphs everyone recognises, and all of
/// them single-column so no drop can tear the grid.
fn glyph() -> char {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ:.=*+-<>";
    CHARS[fastrand::usize(..CHARS.len())] as char
}

impl Effect for Matrix {
    fn name(&self) -> &'static str {
        "matrix"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.height = height;
        self.drops.clear();
        self.drops
            .extend((0..width).map(|_| Drop::respawn(height.max(1))));
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        // Clamp: a stalled terminal can hand us seconds, which would teleport
        // every drop off screen and produce a visible hitch on resume.
        let dt = dt.as_secs_f32().min(0.1);
        canvas.fade(0.86);

        let height = canvas.height().max(1);
        for (x, drop) in self.drops.iter_mut().enumerate() {
            if drop.wait > 0.0 {
                drop.wait -= dt;
                continue;
            }

            drop.y += drop.speed * dt;

            let head = drop.y as i32;
            for i in 0..drop.length {
                let y = head - i as i32;
                // Brightest at the head, fading down the tail.
                let t = i as f32 / drop.length as f32;
                let fg = if i == 0 { HEAD } else { BODY.dim(1.0 - t) };
                canvas.set(
                    x as i32,
                    y,
                    Cell {
                        ch: glyph(),
                        fg,
                        bg: Rgb::BLACK,
                    },
                );
            }

            if head - (drop.length as i32) > height as i32 {
                *drop = Drop::respawn(height);
            }
        }
    }
}

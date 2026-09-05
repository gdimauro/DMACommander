//! Flying through stars.
//!
//! Perspective projection with a glyph ramp for brightness: a star far away is a
//! dim `.`, one about to pass you is a bright `@`. Terminal cells are roughly
//! twice as tall as wide, so x is scaled to keep the field circular rather than
//! oval.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

/// Aspect correction for a character cell. Without it the starfield looks
/// squashed and nobody can say why.
const CELL_ASPECT: f32 = 2.0;
const RAMP: &[char] = &['.', ',', ':', '+', 'o', 'O', '*', '@'];

struct Star {
    x: f32,
    y: f32,
    z: f32,
    tint: Rgb,
}

impl Star {
    fn spawn() -> Self {
        Self {
            x: fastrand::f32() * 2.0 - 1.0,
            y: fastrand::f32() * 2.0 - 1.0,
            z: 0.1 + fastrand::f32() * 0.9,
            // A few coloured stars stop it reading as monochrome noise.
            tint: match fastrand::u8(..10) {
                0 => Rgb(160, 190, 255),
                1 => Rgb(255, 210, 160),
                _ => Rgb(255, 255, 255),
            },
        }
    }
}

pub struct Starfield {
    stars: Vec<Star>,
    speed: f32,
}

impl Starfield {
    pub fn new() -> Self {
        Self {
            stars: Vec::new(),
            speed: 0.55,
        }
    }
}

impl Default for Starfield {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Starfield {
    fn name(&self) -> &'static str {
        "starfield"
    }

    fn resize(&mut self, width: u16, height: u16) {
        // Density scales with area so a small pane is not empty and a large one
        // is not a solid wall of stars.
        let n = ((width as usize * height as usize) / 12).clamp(40, 1200);
        self.stars.clear();
        self.stars.extend((0..n).map(|_| Star::spawn()));
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        let dt = dt.as_secs_f32().min(0.1);
        canvas.fade(0.55);

        let (w, h) = (canvas.width() as f32, canvas.height() as f32);
        let (cx, cy) = (w / 2.0, h / 2.0);

        for star in &mut self.stars {
            star.z -= self.speed * dt;
            if star.z <= 0.02 {
                *star = Star::spawn();
                star.z = 1.0;
                continue;
            }

            let sx = cx + (star.x / star.z) * cx / CELL_ASPECT * CELL_ASPECT * 0.5;
            let sy = cy + (star.y / star.z) * cy;

            if sx < 0.0 || sy < 0.0 || sx >= w || sy >= h {
                // Off screen: recycle rather than tracking a star nobody sees.
                *star = Star::spawn();
                star.z = 1.0;
                continue;
            }

            // Closer means brighter, which means further along the glyph ramp.
            let brightness = (1.0 - star.z).clamp(0.0, 1.0);
            let idx = ((brightness * (RAMP.len() - 1) as f32) as usize).min(RAMP.len() - 1);

            canvas.set(
                sx as i32,
                sy as i32,
                Cell {
                    ch: RAMP[idx],
                    fg: star.tint.dim(0.25 + brightness * 0.75),
                    bg: Rgb::BLACK,
                },
            );
        }
    }
}

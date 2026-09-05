//! Conway's Game of Life, with an age-based palette.
//!
//! A plain Life quickly settles into still lifes and blinkers, which is boring
//! after ten seconds. Two things fix that: cells are coloured by how long they
//! have survived, and a stalled board is reseeded.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

/// Generations per second. Life is legible slowly; at 60fps it is a grey blur.
const GENERATIONS_PER_SEC: f32 = 12.0;
/// Reseed after this many generations without a population change — the board
/// has almost certainly settled.
const STALL_LIMIT: u32 = 40;

const YOUNG: Rgb = Rgb(120, 255, 180);
const OLD: Rgb = Rgb(60, 90, 220);

pub struct Life {
    w: usize,
    h: usize,
    /// Age in generations; 0 means dead.
    cells: Vec<u16>,
    next: Vec<u16>,
    accumulator: f32,
    last_population: usize,
    stalled_for: u32,
}

impl Life {
    pub fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            cells: Vec::new(),
            next: Vec::new(),
            accumulator: 0.0,
            last_population: 0,
            stalled_for: 0,
        }
    }

    fn seed(&mut self) {
        for c in &mut self.cells {
            *c = if fastrand::f32() < 0.28 { 1 } else { 0 };
        }
        self.stalled_for = 0;
    }

    /// Neighbour count on a torus — wrapping keeps gliders from piling up on the
    /// edges, which is what makes a bounded Life go static.
    fn neighbours(&self, x: usize, y: usize) -> u8 {
        let mut n = 0;
        for dy in [self.h - 1, 0, 1] {
            for dx in [self.w - 1, 0, 1] {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let nx = (x + dx) % self.w;
                let ny = (y + dy) % self.h;
                if self.cells[ny * self.w + nx] > 0 {
                    n += 1;
                }
            }
        }
        n
    }

    fn step(&mut self) {
        for y in 0..self.h {
            for x in 0..self.w {
                let i = y * self.w + x;
                let alive = self.cells[i] > 0;
                let n = self.neighbours(x, y);
                self.next[i] = match (alive, n) {
                    (true, 2) | (true, 3) => self.cells[i].saturating_add(1),
                    (false, 3) => 1,
                    _ => 0,
                };
            }
        }
        std::mem::swap(&mut self.cells, &mut self.next);

        let population = self.cells.iter().filter(|c| **c > 0).count();
        if population == self.last_population {
            self.stalled_for += 1;
        } else {
            self.stalled_for = 0;
        }
        self.last_population = population;

        if self.stalled_for > STALL_LIMIT || population == 0 {
            self.seed();
        }
    }
}

impl Default for Life {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Life {
    fn name(&self) -> &'static str {
        "life"
    }

    fn resize(&mut self, width: u16, height: u16) {
        // The torus arithmetic needs at least 1 in each dimension.
        self.w = (width as usize).max(1);
        self.h = (height as usize).max(1);
        self.cells = vec![0; self.w * self.h];
        self.next = vec![0; self.w * self.h];
        self.last_population = 0;
        self.seed();
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        if self.cells.is_empty() {
            return;
        }

        self.accumulator += dt.as_secs_f32().min(0.1);
        let interval = 1.0 / GENERATIONS_PER_SEC;
        // Bounded catch-up: after a long stall, run a few generations rather
        // than blocking for thousands.
        let mut budget = 4;
        while self.accumulator >= interval && budget > 0 {
            self.accumulator -= interval;
            self.step();
            budget -= 1;
        }
        self.accumulator = self.accumulator.min(interval);

        canvas.clear();
        for y in 0..self.h.min(canvas.height() as usize) {
            for x in 0..self.w.min(canvas.width() as usize) {
                let age = self.cells[y * self.w + x];
                if age == 0 {
                    continue;
                }
                // Saturate the age ramp at ~30 generations; beyond that the
                // colour stops changing and the eye cannot tell anyway.
                let t = (age as f32 / 30.0).min(1.0);
                canvas.set(
                    x as i32,
                    y as i32,
                    Cell {
                        ch: if age < 3 { '·' } else { '●' },
                        fg: YOUNG.lerp(OLD, t),
                        bg: Rgb::BLACK,
                    },
                );
            }
        }
    }
}

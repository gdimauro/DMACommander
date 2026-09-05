//! Conway's Game of Life, with an age-based palette.
//!
//! A plain Life quickly settles into still lifes and blinkers, which is boring
//! after ten seconds. Two things fix that: cells are coloured by how long they
//! have survived, and a stalled board is reseeded.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

/// Generations per second. Life is legible slowly; at 60fps it is a grey blur,
/// and the point is to watch structures move.
const GENERATIONS_PER_SEC: f32 = 9.0;

/// Background soup density. Deliberately thin: enough to give the spaceships
/// something to run into, not enough to drown them.
const SOUP: f32 = 0.06;

/// Patterns, as (width, rows-of-bits). Coordinates are row-major, `#` alive.
const GLIDER: (&[&str], &str) = (&[".#.", "..#", "###"], "glider");
const LWSS: (&[&str], &str) = (
    &[".####", "#...#", "....#", "#..#."],
    "lightweight spaceship",
);
const PULSAR: (&[&str], &str) = (
    &[
        "..###...###..",
        ".............",
        "#....#.#....#",
        "#....#.#....#",
        "#....#.#....#",
        "..###...###..",
        ".............",
        "..###...###..",
        "#....#.#....#",
        "#....#.#....#",
        "#....#.#....#",
        ".............",
        "..###...###..",
    ],
    "pulsar",
);
/// Five cells that stay chaotic for over a thousand generations.
const R_PENTOMINO: (&[&str], &str) = (&[".##", "##.", ".#."], "r-pentomino");
/// Seven cells, five thousand generations.
const ACORN: (&[&str], &str) = (&[".#.....", "...#...", "##..###"], "acorn");
/// Emits a glider every 30 generations, forever. The showpiece.
const GOSPER_GUN: (&[&str], &str) = (
    &[
        "........................#...........",
        "......................#.#...........",
        "............##......##............##",
        "...........#...#....##............##",
        "##........#.....#...##..............",
        "##........#...#.##....#.#...........",
        "..........#.....#.......#...........",
        "...........#...#....................",
        "............##......................",
    ],
    "gosper glider gun",
);
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
            *c = if fastrand::f32() < SOUP { 1 } else { 0 };
        }

        // A gun if there is room for one: it keeps the board alive indefinitely,
        // which is exactly what a screensaver wants.
        if self.w > 50 && self.h > 14 {
            self.stamp(
                GOSPER_GUN.0,
                fastrand::usize(0..self.w / 3),
                fastrand::usize(0..self.h / 2),
            );
        }

        // Then a handful of patterns scattered around. Weighted towards the ones
        // that move or keep producing, because a board of still lifes is a
        // screensaver that has stopped.
        let catalogue = [
            GLIDER.0,
            GLIDER.0,
            GLIDER.0,
            LWSS.0,
            LWSS.0,
            R_PENTOMINO.0,
            R_PENTOMINO.0,
            ACORN.0,
            PULSAR.0,
        ];
        let n = ((self.w * self.h) / 900).clamp(3, 14);
        for _ in 0..n {
            let pattern = catalogue[fastrand::usize(..catalogue.len())];
            let x = fastrand::usize(0..self.w);
            let y = fastrand::usize(0..self.h);
            self.stamp(pattern, x, y);
        }

        self.stalled_for = 0;
        self.last_population = 0;
    }

    /// Draw a pattern with its top-left at `(x, y)`, wrapping at the edges.
    ///
    /// Wrapping rather than clipping: the board is a torus for the neighbour
    /// count too, so a pattern straddling the edge behaves exactly as it would
    /// anywhere else, and patterns can be placed without bounds arithmetic.
    fn stamp(&mut self, pattern: &[&str], x: usize, y: usize) {
        for (dy, row) in pattern.iter().enumerate() {
            for (dx, ch) in row.chars().enumerate() {
                if ch != '#' {
                    continue;
                }
                let px = (x + dx) % self.w;
                let py = (y + dy) % self.h;
                self.cells[py * self.w + px] = 1;
            }
        }
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

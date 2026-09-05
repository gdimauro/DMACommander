//! Pipes, the screensaver everyone's office PC ran in 1995.
//!
//! Several walkers crawl the grid drawing box-drawing glyphs, turning at random
//! and changing colour when they do. The glyph is chosen from the pair of
//! directions the pipe connects, which is what makes corners look like corners
//! instead of a trail of dashes.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

const PALETTE: &[Rgb] = &[
    Rgb(255, 90, 90),
    Rgb(90, 255, 140),
    Rgb(110, 160, 255),
    Rgb(255, 220, 90),
    Rgb(230, 110, 255),
    Rgb(90, 240, 240),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
    Left,
    Right,
}

impl Dir {
    fn delta(self) -> (i32, i32) {
        match self {
            Dir::Up => (0, -1),
            Dir::Down => (0, 1),
            Dir::Left => (-1, 0),
            Dir::Right => (1, 0),
        }
    }

    fn opposite(self) -> Dir {
        match self {
            Dir::Up => Dir::Down,
            Dir::Down => Dir::Up,
            Dir::Left => Dir::Right,
            Dir::Right => Dir::Left,
        }
    }

    fn turns(self) -> [Dir; 2] {
        match self {
            Dir::Up | Dir::Down => [Dir::Left, Dir::Right],
            Dir::Left | Dir::Right => [Dir::Up, Dir::Down],
        }
    }
}

/// The glyph for a cell entered heading `from`-wards and left heading `to`.
fn glyph(entered_from: Dir, leaving_to: Dir) -> char {
    use Dir::*;
    match (entered_from, leaving_to) {
        (Up, Up) | (Down, Down) => '│',
        (Left, Left) | (Right, Right) => '─',
        (Up, Right) | (Left, Down) => '┌',
        (Up, Left) | (Right, Down) => '┐',
        (Down, Right) | (Left, Up) => '└',
        (Down, Left) | (Right, Up) => '┘',
        _ => '·',
    }
}

struct Pipe {
    x: i32,
    y: i32,
    dir: Dir,
    colour: Rgb,
    /// Cells per second. Varying it per pipe stops them moving in lockstep.
    speed: f32,
    progress: f32,
    /// Cells drawn since spawn; used to retire a pipe before it fills the screen.
    drawn: u32,
}

impl Pipe {
    fn spawn(w: u16, h: u16) -> Self {
        Self {
            x: fastrand::i32(0..w.max(1) as i32),
            y: fastrand::i32(0..h.max(1) as i32),
            dir: [Dir::Up, Dir::Down, Dir::Left, Dir::Right][fastrand::usize(..4)],
            colour: PALETTE[fastrand::usize(..PALETTE.len())],
            speed: 12.0 + fastrand::f32() * 24.0,
            progress: 0.0,
            drawn: 0,
        }
    }
}

pub struct Pipes {
    pipes: Vec<Pipe>,
    w: u16,
    h: u16,
    /// Cells a pipe may draw before it is retired and respawned elsewhere.
    lifetime: u32,
}

impl Pipes {
    pub fn new() -> Self {
        Self {
            pipes: Vec::new(),
            w: 0,
            h: 0,
            lifetime: 400,
        }
    }
}

impl Default for Pipes {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Pipes {
    fn name(&self) -> &'static str {
        "pipes"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.w = width;
        self.h = height;
        let n = ((width as usize * height as usize) / 600).clamp(3, 12);
        self.pipes.clear();
        self.pipes
            .extend((0..n).map(|_| Pipe::spawn(width, height)));
        // Retire a pipe after it has covered roughly a fifth of the screen, so
        // the picture keeps changing instead of saturating to solid colour.
        self.lifetime = ((width as u32 * height as u32) / 5).clamp(120, 2000);
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        let dt = dt.as_secs_f32().min(0.1);
        // A very slow fade: pipes should persist and build a picture, unlike the
        // trails in matrix or starfield.
        canvas.fade(0.995);

        let (w, h) = (self.w, self.h);
        if w == 0 || h == 0 {
            return;
        }

        for pipe in &mut self.pipes {
            pipe.progress += pipe.speed * dt;
            // Bounded: a big `dt` must not make one pipe draw a thousand cells
            // in a single frame.
            let mut steps = pipe.progress as u32;
            pipe.progress -= steps as f32;
            steps = steps.min(8);

            for _ in 0..steps {
                let entered_from = pipe.dir;

                // Turn occasionally; changing colour on the turn is what makes
                // the picture read as separate pipes rather than one tangle.
                if fastrand::f32() < 0.16 {
                    pipe.dir = pipe.dir.turns()[fastrand::usize(..2)];
                    if fastrand::f32() < 0.5 {
                        pipe.colour = PALETTE[fastrand::usize(..PALETTE.len())];
                    }
                }

                canvas.set(
                    pipe.x,
                    pipe.y,
                    Cell {
                        ch: glyph(entered_from, pipe.dir),
                        fg: pipe.colour,
                        bg: Rgb::BLACK,
                    },
                );

                let (dx, dy) = pipe.dir.delta();
                pipe.x += dx;
                pipe.y += dy;
                pipe.drawn += 1;

                // Wrap at the edges rather than bouncing: bouncing produces
                // visible clusters in the corners.
                if pipe.x < 0 || pipe.y < 0 || pipe.x >= w as i32 || pipe.y >= h as i32 {
                    pipe.x = pipe.x.rem_euclid(w as i32);
                    pipe.y = pipe.y.rem_euclid(h as i32);
                    pipe.dir = pipe.dir.opposite();
                }

                if pipe.drawn > self.lifetime {
                    *pipe = Pipe::spawn(w, h);
                    break;
                }
            }
        }
    }
}

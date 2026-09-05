//! Snake. The easter egg that proves the interactive path works.
//!
//! Everything else in this crate dismisses on any key. Snake is the first thing
//! that has to *keep* keys, which is why [`Effect::on_input`] returns an
//! [`EffectControl`] rather than being assumed to mean "exit".

use crate::{Canvas, Cell, Effect, EffectControl, EffectKey, Rgb};
use std::collections::VecDeque;
use std::time::Duration;

const HEAD: Rgb = Rgb(180, 255, 140);
const BODY: Rgb = Rgb(70, 200, 100);
const FOOD: Rgb = Rgb(255, 120, 120);
const WALL: Rgb = Rgb(70, 80, 110);
const TEXT: Rgb = Rgb(220, 220, 235);

/// Cells per second at the start. Each apple makes it a little faster.
const BASE_SPEED: f32 = 8.0;

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

    fn is_reverse_of(self, other: Dir) -> bool {
        matches!(
            (self, other),
            (Dir::Up, Dir::Down)
                | (Dir::Down, Dir::Up)
                | (Dir::Left, Dir::Right)
                | (Dir::Right, Dir::Left)
        )
    }
}

pub struct Snake {
    w: i32,
    h: i32,
    body: VecDeque<(i32, i32)>,
    dir: Dir,
    /// The direction the player asked for, applied at the next step. Without
    /// this, two quick key presses in one frame can reverse the snake into itself.
    queued: Option<Dir>,
    food: (i32, i32),
    score: u32,
    dead: bool,
    accumulator: f32,
}

impl Snake {
    pub fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            body: VecDeque::new(),
            dir: Dir::Right,
            queued: None,
            food: (0, 0),
            score: 0,
            dead: false,
            accumulator: 0.0,
        }
    }

    /// Playfield inset by one cell for the wall.
    fn bounds(&self) -> (i32, i32, i32, i32) {
        (1, 1, self.w - 2, self.h - 2)
    }

    fn reset(&mut self) {
        self.body.clear();
        let (cx, cy) = (self.w / 2, self.h / 2);
        for i in 0..4 {
            self.body.push_back((cx - i, cy));
        }
        self.dir = Dir::Right;
        self.queued = None;
        self.score = 0;
        self.dead = false;
        self.accumulator = 0.0;
        self.place_food();
    }

    /// Place food somewhere not occupied by the snake. Bounded retries: on a
    /// nearly-full board a rejection loop could spin, and a hung screensaver is
    /// worse than a slightly unfair apple.
    fn place_food(&mut self) {
        let (x0, y0, x1, y1) = self.bounds();
        if x1 < x0 || y1 < y0 {
            self.food = (x0, y0);
            return;
        }
        for _ in 0..200 {
            let p = (
                x0 + fastrand::i32(0..=(x1 - x0)),
                y0 + fastrand::i32(0..=(y1 - y0)),
            );
            if !self.body.contains(&p) {
                self.food = p;
                return;
            }
        }
        self.food = (x0, y0);
    }

    fn speed(&self) -> f32 {
        BASE_SPEED + self.score as f32 * 0.6
    }

    fn step(&mut self) {
        if let Some(d) = self.queued.take()
            && !d.is_reverse_of(self.dir)
        {
            self.dir = d;
        }

        let Some(&(hx, hy)) = self.body.front() else {
            return;
        };
        let (dx, dy) = self.dir.delta();
        let next = (hx + dx, hy + dy);

        let (x0, y0, x1, y1) = self.bounds();
        let hit_wall = next.0 < x0 || next.1 < y0 || next.0 > x1 || next.1 > y1;
        // The tail cell is about to move out from under us, so it is not a
        // collision — this is the difference between a fair game and a cruel one.
        let hit_self = self
            .body
            .iter()
            .take(self.body.len().saturating_sub(1))
            .any(|&c| c == next);

        if hit_wall || hit_self {
            self.dead = true;
            return;
        }

        self.body.push_front(next);
        if next == self.food {
            self.score += 1;
            self.place_food();
        } else {
            self.body.pop_back();
        }
    }

    fn write(canvas: &mut Canvas, x: i32, y: i32, text: &str, fg: Rgb) {
        for (i, ch) in text.chars().enumerate() {
            canvas.set(
                x + i as i32,
                y,
                Cell {
                    ch,
                    fg,
                    bg: Rgb::BLACK,
                },
            );
        }
    }
}

impl Default for Snake {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Snake {
    fn name(&self) -> &'static str {
        "snake"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.w = width as i32;
        self.h = height as i32;
        self.reset();
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        // Too small to play in. Say so rather than rendering a broken board.
        if self.w < 12 || self.h < 6 {
            canvas.clear();
            Self::write(canvas, 0, 0, "terminal too small", TEXT);
            return;
        }

        if !self.dead {
            self.accumulator += dt.as_secs_f32().min(0.1);
            let interval = 1.0 / self.speed();
            let mut budget = 4;
            while self.accumulator >= interval && budget > 0 && !self.dead {
                self.accumulator -= interval;
                self.step();
                budget -= 1;
            }
        }

        canvas.clear();

        // Wall
        for x in 0..self.w {
            for y in [0, self.h - 1] {
                canvas.set(
                    x,
                    y,
                    Cell {
                        ch: '─',
                        fg: WALL,
                        bg: Rgb::BLACK,
                    },
                );
            }
        }
        for y in 0..self.h {
            for x in [0, self.w - 1] {
                canvas.set(
                    x,
                    y,
                    Cell {
                        ch: '│',
                        fg: WALL,
                        bg: Rgb::BLACK,
                    },
                );
            }
        }

        // Explicit escapes: these glyphs must survive every editor and pipe
        // this file passes through.
        canvas.set(
            self.food.0,
            self.food.1,
            Cell {
                ch: '\u{25C6}',
                fg: FOOD,
                bg: Rgb::BLACK,
            },
        );

        for (i, &(x, y)) in self.body.iter().enumerate() {
            canvas.set(
                x,
                y,
                Cell {
                    ch: if i == 0 { '\u{2588}' } else { '\u{2593}' },
                    fg: if i == 0 { HEAD } else { BODY },
                    bg: Rgb::BLACK,
                },
            );
        }

        Self::write(canvas, 2, 0, &format!(" score {} ", self.score), TEXT);

        if self.dead {
            let msg = format!(
                " game over — score {} — space to retry, esc to leave ",
                self.score
            );
            let x = ((self.w - msg.chars().count() as i32) / 2).max(0);
            Self::write(canvas, x, self.h / 2, &msg, TEXT);
        }
    }

    fn on_input(&mut self, key: EffectKey) -> EffectControl {
        match key {
            // Esc always gets you out, from anywhere. No exceptions.
            EffectKey::Esc | EffectKey::Char('q') => EffectControl::Exit,

            EffectKey::Up | EffectKey::Char('k') => {
                self.queued = Some(Dir::Up);
                EffectControl::Consumed
            }
            EffectKey::Down | EffectKey::Char('j') => {
                self.queued = Some(Dir::Down);
                EffectControl::Consumed
            }
            EffectKey::Left | EffectKey::Char('h') => {
                self.queued = Some(Dir::Left);
                EffectControl::Consumed
            }
            EffectKey::Right | EffectKey::Char('l') => {
                self.queued = Some(Dir::Right);
                EffectControl::Consumed
            }

            EffectKey::Space | EffectKey::Enter => {
                if self.dead {
                    self.reset();
                }
                EffectControl::Consumed
            }

            _ => EffectControl::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> Snake {
        let mut s = Snake::new();
        s.resize(40, 20);
        s
    }

    #[test]
    fn steering_into_itself_is_ignored_rather_than_instant_death() {
        let mut s = game(); // moving Right
        s.on_input(EffectKey::Left);
        s.step();
        assert!(!s.dead, "a reversal must be rejected, not fatal");
    }

    #[test]
    fn two_turns_in_one_frame_cannot_reverse_the_snake() {
        let mut s = game(); // Right
        s.on_input(EffectKey::Up);
        s.on_input(EffectKey::Left); // would be a reversal of Up
        s.step();
        assert!(!s.dead);
    }

    #[test]
    fn hitting_the_wall_ends_the_game() {
        let mut s = game();
        for _ in 0..100 {
            s.step();
        }
        assert!(s.dead);
    }

    #[test]
    fn space_restarts_after_death_and_esc_leaves() {
        let mut s = game();
        for _ in 0..100 {
            s.step();
        }
        assert!(s.dead);
        assert_eq!(s.on_input(EffectKey::Space), EffectControl::Consumed);
        assert!(!s.dead, "space must restart");
        assert_eq!(s.on_input(EffectKey::Esc), EffectControl::Exit);
    }

    #[test]
    fn eating_grows_the_snake_and_scores() {
        let mut s = game();
        let len = s.body.len();
        // Put the apple directly ahead of the head.
        let (hx, hy) = *s.body.front().unwrap();
        s.food = (hx + 1, hy);
        s.step();
        assert_eq!(s.score, 1);
        assert_eq!(s.body.len(), len + 1);
    }

    #[test]
    fn a_board_too_small_to_play_does_not_panic() {
        let mut s = Snake::new();
        for (w, h) in [(0u16, 0u16), (1, 1), (8, 4), (12, 6)] {
            s.resize(w, h);
            let mut c = Canvas::new(w, h);
            s.tick(Duration::from_millis(16), &mut c);
        }
    }
}

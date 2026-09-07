//! The help page, shot down a letter at a time.
//!
//! The top three quarters of the screen hold the same text `F1` shows. On the
//! ground under it a small cannon rolls about, picks a letter, and fires. A
//! letter that is hit comes loose, tumbles down under gravity, bounces once if
//! it was going fast enough, and settles in a heap on the ground. When the page
//! has been shot empty the next page comes in and the wreckage is swept, so a
//! help longer than the screen is read a page at a time — whether or not anyone
//! is reading.
//!
//! The text is handed in through [`Effect::set_text`]; without one there is a
//! short built-in page, so the effect is never a blank screen.

use crate::{Canvas, Cell, Effect, Rgb};
use std::time::Duration;

/// Rows per second squared. A character cell is about twice as tall as it is
/// wide, so vertical figures here are in rows and horizontal ones in columns,
/// and the horizontal ones run about double to cover the same distance on
/// screen.
const GRAVITY: f32 = 26.0;
/// Rows per second along the path of a shot.
const SHOT_SPEED: f32 = 30.0;
/// Columns per second the cannon rolls at.
const ROLL_SPEED: f32 = 24.0;
/// Seconds between shots, give or take.
const RELOAD: f32 = 0.38;
/// How far to the side the cannon still fires from, in columns. Angled shots
/// are what make it look aimed rather than dropped.
const REACH: f32 = 12.0;
/// Seconds an empty page stays before the next one comes in.
const LINGER: f32 = 1.4;
/// Seconds a page stays even if it has not been shot empty: a huge screen holds
/// more letters than anyone wants to watch fall.
const PAGE_LIFE: f32 = 75.0;
/// Below this there is no room for a page and a cannon under it.
const MIN_W: u16 = 12;
const MIN_H: u16 = 7;

const TITLE: Rgb = Rgb(255, 214, 110);
const KEY: Rgb = Rgb(232, 232, 240);
const TEXT: Rgb = Rgb(140, 146, 165);
const SHOT: Rgb = Rgb(140, 255, 255);
const FLASH: Rgb = Rgb(255, 255, 255);
const HOT: Rgb = Rgb(255, 170, 60);
const HEAP: Rgb = Rgb(105, 108, 120);
const CANNON: Rgb = Rgb(255, 205, 40);
const GROUND: Rgb = Rgb(70, 72, 84);

/// The page shown when nobody has handed the effect a text.
const FALLBACK: &[&str] = &[
    "DMACommander",
    "",
    "  F1          help",
    "  F12         screensavers",
    "  Shift-F12   this one, right now",
    "  Ctrl-O      the shell",
    "  Ctrl-T      the sessions",
];

/// One letter on the page.
struct Letter {
    x: i32,
    y: i32,
    ch: char,
    fg: Rgb,
    alive: bool,
    /// A shot is already on its way, so the cannon picks something else.
    claimed: bool,
}

/// A shot in flight. It flies straight at the cell it was aimed at and arrives
/// after `life` seconds; the position is only where it is drawn meanwhile.
struct Shot {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    life: f32,
    target: (i32, i32),
}

/// A letter that has been hit and is on its way down.
struct Debris {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    ch: char,
    /// Seconds since it was hit: the colour cools with it.
    age: f32,
    /// Seconds until the glyph flips case again — a letter tumbling.
    spin: f32,
}

pub struct Cannon {
    w: u16,
    h: u16,
    text: Vec<String>,
    /// First line of `text` on the current page.
    page_start: usize,
    /// How many lines the current page took, so the next starts after it.
    page_len: usize,
    letters: Vec<Letter>,
    shots: Vec<Shot>,
    debris: Vec<Debris>,
    /// What has landed, per column, bottom first.
    heap: Vec<Vec<char>>,
    /// Column of the cannon's centre.
    cannon_x: f32,
    /// The letter it is lining up on.
    target: Option<usize>,
    /// -1, 0 or 1: which way the barrel points.
    aim: i8,
    reload: f32,
    page_age: f32,
    /// How long the page has been empty.
    linger: f32,
}

impl Cannon {
    pub fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            text: Vec::new(),
            page_start: 0,
            page_len: 0,
            letters: Vec::new(),
            shots: Vec::new(),
            debris: Vec::new(),
            heap: Vec::new(),
            cannon_x: 0.0,
            target: None,
            aim: 0,
            reload: 0.0,
            page_age: 0.0,
            linger: 0.0,
        }
    }

    /// The row the ground is drawn on.
    fn ground(&self) -> i32 {
        self.h as i32 - 1
    }

    /// Rows the page may use, from row 1 down. Three quarters of the screen,
    /// and never so many that the cannon under it has no room.
    fn text_rows(&self) -> i32 {
        let h = self.h as i32;
        (h * 3 / 4 - 1).min(h - 4).max(1)
    }

    /// How high a column's heap may grow: up to one blank row under the page.
    fn heap_cap(&self) -> usize {
        (self.ground() - 1 - (self.text_rows() + 2)).max(0) as usize
    }

    fn fits(&self) -> bool {
        self.w >= MIN_W && self.h >= MIN_H
    }

    fn lines(&self) -> Vec<&str> {
        lines_of(&self.text)
    }

    /// Lay out the page starting at `page_start`, and clear the ground.
    fn build_page(&mut self) {
        self.letters.clear();
        self.shots.clear();
        self.debris.clear();
        for col in &mut self.heap {
            col.clear();
        }
        self.page_age = 0.0;
        self.linger = 0.0;
        self.target = None;

        // Borrowed from the field, not through `self`, so the layout below can
        // write the other fields while the lines are still in hand.
        let lines = lines_of(&self.text);
        if lines.is_empty() {
            self.page_len = 0;
            return;
        }
        let rows = self.text_rows() as usize;
        // Never open a page on a blank line.
        let mut start = self.page_start % lines.len();
        while start < lines.len() && lines[start].trim().is_empty() {
            start += 1;
        }
        if start >= lines.len() {
            start = 0;
        }
        let end = (start + rows).min(lines.len());
        self.page_start = start;
        self.page_len = end - start;

        let w = self.w as usize;
        let page = &lines[start..end];
        let widest = page
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .min(w);
        let x0 = (w - widest) / 2;
        for (row, line) in page.iter().enumerate() {
            let y = 1 + row as i32;
            let title = !line.starts_with(' ');
            let key_end = key_end(line);
            for (col, ch) in line.chars().enumerate() {
                if x0 + col >= w {
                    break;
                }
                if ch == ' ' {
                    continue;
                }
                let fg = if title {
                    TITLE
                } else if col < key_end {
                    KEY
                } else {
                    TEXT
                };
                self.letters.push(Letter {
                    x: (x0 + col) as i32,
                    y,
                    ch,
                    fg,
                    alive: true,
                    claimed: false,
                });
            }
        }
    }

    fn next_page(&mut self) {
        let n = self.lines().len().max(1);
        self.page_start = (self.page_start + self.page_len.max(1)) % n;
        self.build_page();
    }

    fn alive(&self) -> usize {
        self.letters.iter().filter(|l| l.alive).count()
    }

    /// Something to shoot at. Mostly whatever is within reach, so the cannon
    /// fires a burst from where it stands before rolling off — a cannon that
    /// crossed the screen for every letter would spend its life rolling.
    fn pick_target(&self) -> Option<usize> {
        let free: Vec<usize> = self
            .letters
            .iter()
            .enumerate()
            .filter(|(_, l)| l.alive && !l.claimed)
            .map(|(i, _)| i)
            .collect();
        if free.is_empty() {
            return None;
        }
        if fastrand::f32() < 0.7 {
            let near: Vec<usize> = free
                .iter()
                .copied()
                .filter(|&i| (self.letters[i].x as f32 - self.cannon_x).abs() <= REACH)
                .collect();
            if !near.is_empty() {
                return Some(near[fastrand::usize(..near.len())]);
            }
        }
        Some(free[fastrand::usize(..free.len())])
    }

    fn drive_cannon(&mut self, dt: f32) {
        self.reload = (self.reload - dt).max(0.0);
        if self
            .target
            .and_then(|i| self.letters.get(i))
            .is_none_or(|l| !l.alive || l.claimed)
        {
            self.target = self.pick_target();
        }
        let Some(i) = self.target else {
            self.aim = 0;
            return;
        };
        let (tx, ty) = (self.letters[i].x as f32, self.letters[i].y as f32);

        // Roll until the letter is comfortably within reach, then stand and fire.
        let dx = tx - self.cannon_x;
        if dx.abs() > REACH * 0.6 {
            let step = (ROLL_SPEED * dt).min(dx.abs());
            self.cannon_x = (self.cannon_x + dx.signum() * step).clamp(1.0, self.w as f32 - 2.0);
        }
        let dx = tx - self.cannon_x;
        self.aim = if dx > 1.5 {
            1
        } else if dx < -1.5 {
            -1
        } else {
            0
        };
        if self.reload > 0.0 || dx.abs() > REACH {
            return;
        }

        // Fire, from the muzzle: the barrel row, on the side the barrel points.
        let mx = self.cannon_x + self.aim as f32;
        let my = (self.ground() - 2) as f32;
        let (dx, dy) = (tx - mx, ty - my);
        // Length in rows, with columns halved: what the speed is measured
        // against on screen.
        let len = ((dx * 0.5).powi(2) + dy.powi(2)).sqrt().max(0.5);
        let life = len / SHOT_SPEED;
        self.shots.push(Shot {
            x: mx,
            y: my,
            vx: dx / life,
            vy: dy / life,
            life,
            target: (tx as i32, ty as i32),
        });
        self.letters[i].claimed = true;
        self.reload = RELOAD * (0.75 + fastrand::f32() * 0.5);
        self.target = None;
    }

    fn fly_shots(&mut self, dt: f32) {
        let mut i = 0;
        while i < self.shots.len() {
            let s = &mut self.shots[i];
            s.life -= dt;
            s.x += s.vx * dt;
            s.y += s.vy * dt;
            if s.life <= 0.0 {
                let target = s.target;
                self.shots.swap_remove(i);
                self.hit(target);
            } else {
                i += 1;
            }
        }
    }

    /// The letter at a cell comes loose: a small kick up and to one side, and
    /// gravity does the rest.
    fn hit(&mut self, (x, y): (i32, i32)) {
        let Some(l) = self
            .letters
            .iter_mut()
            .find(|l| l.alive && l.x == x && l.y == y)
        else {
            return;
        };
        l.alive = false;
        self.debris.push(Debris {
            x: x as f32,
            y: y as f32,
            vx: (fastrand::f32() - 0.5) * 16.0,
            vy: -(2.0 + fastrand::f32() * 5.0),
            ch: l.ch,
            age: 0.0,
            spin: 0.1 + fastrand::f32() * 0.2,
        });
    }

    fn fall_debris(&mut self, dt: f32) {
        let ground = self.ground();
        let w = self.w as i32;
        let cap = self.heap_cap();
        let mut i = 0;
        while i < self.debris.len() {
            let d = &mut self.debris[i];
            d.age += dt;
            d.vy += GRAVITY * dt;
            d.x += d.vx * dt;
            d.y += d.vy * dt;
            d.spin -= dt;
            if d.spin <= 0.0 {
                d.ch = flip_case(d.ch);
                d.spin = 0.1 + fastrand::f32() * 0.25;
            }
            let col = d.x.round() as i32;
            if col < 0 || col >= w {
                // Off the side: gone.
                self.debris.swap_remove(i);
                continue;
            }
            // The floor here is the top of this column's heap.
            let floor = (ground - 1 - self.heap[col as usize].len() as i32) as f32;
            if d.y >= floor {
                if d.vy > 9.0 {
                    // Fast enough to bounce, losing most of it.
                    d.y = floor - 0.01;
                    d.vy = -d.vy * 0.35;
                    d.vx *= 0.6;
                    i += 1;
                    continue;
                }
                let ch = d.ch;
                self.debris.swap_remove(i);
                let heap = &mut self.heap[col as usize];
                if heap.len() < cap {
                    heap.push(ch);
                }
                continue;
            }
            i += 1;
        }
    }

    fn draw(&self, canvas: &mut Canvas) {
        let black = Rgb::BLACK;
        for l in self.letters.iter().filter(|l| l.alive) {
            canvas.set(
                l.x,
                l.y,
                Cell {
                    ch: l.ch,
                    fg: l.fg,
                    bg: black,
                },
            );
        }

        let ground = self.ground();
        for x in 0..self.w as i32 {
            canvas.set(
                x,
                ground,
                Cell {
                    ch: '\u{2580}', // ▀
                    fg: GROUND,
                    bg: black,
                },
            );
            for (i, ch) in self.heap[x as usize].iter().enumerate() {
                canvas.set(
                    x,
                    ground - 1 - i as i32,
                    Cell {
                        ch: *ch,
                        fg: HEAP,
                        bg: black,
                    },
                );
            }
        }

        // The cannon: a base on the ground, and a barrel above it pointing the
        // way it is about to fire. Drawn after the heap, so it rolls over the
        // wreckage rather than under it.
        let cx = self.cannon_x.round() as i32;
        for (dx, ch) in [(-1, '\u{259f}'), (0, '\u{2588}'), (1, '\u{2599}')] {
            canvas.set(
                cx + dx,
                ground - 1,
                Cell {
                    ch,
                    fg: CANNON,
                    bg: black,
                },
            );
        }
        let (bx, barrel) = match self.aim {
            1 => (1, '\u{2571}'),   // ╱
            -1 => (-1, '\u{2572}'), // ╲
            _ => (0, '\u{2502}'),   // │
        };
        canvas.set(
            cx + bx,
            ground - 2,
            Cell {
                ch: barrel,
                fg: CANNON,
                bg: black,
            },
        );

        for s in &self.shots {
            canvas.set(
                s.x.round() as i32,
                s.y.round() as i32,
                Cell {
                    ch: '\u{2022}', // •
                    fg: SHOT,
                    bg: black,
                },
            );
        }

        // Last, over everything: the falling letters are what the eye follows.
        for d in &self.debris {
            let fg = if d.age < 0.12 {
                FLASH
            } else {
                HOT.lerp(HEAP, ((d.age - 0.12) / 1.2).min(1.0))
            };
            canvas.set(
                d.x.round() as i32,
                d.y.round() as i32,
                Cell {
                    ch: d.ch,
                    fg,
                    bg: black,
                },
            );
        }
    }
}

impl Default for Cannon {
    fn default() -> Self {
        Self::new()
    }
}

/// The text to show: what was handed in, or the built-in page.
fn lines_of(text: &[String]) -> Vec<&str> {
    if text.is_empty() {
        FALLBACK.to_vec()
    } else {
        text.iter().map(String::as_str).collect()
    }
}

/// Where the key column of an indented line ends: the first run of two or
/// more spaces after the indent. Zero for a line with no such column, which is
/// then all prose.
fn key_end(line: &str) -> usize {
    let indent = line.chars().take_while(|c| *c == ' ').count();
    let mut run = 0;
    for (i, c) in line.chars().enumerate().skip(indent) {
        if c == ' ' {
            run += 1;
            if run >= 2 {
                return i + 1 - run;
            }
        } else {
            run = 0;
        }
    }
    0
}

fn flip_case(c: char) -> char {
    if c.is_ascii_lowercase() {
        c.to_ascii_uppercase()
    } else if c.is_ascii_uppercase() {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

impl Effect for Cannon {
    fn name(&self) -> &'static str {
        "cannon"
    }

    fn set_text(&mut self, lines: &[String]) {
        self.text = lines.to_vec();
        self.page_start = 0;
        if self.fits() {
            self.build_page();
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.w = width;
        self.h = height;
        self.heap = vec![Vec::new(); width as usize];
        self.cannon_x = width as f32 / 2.0;
        self.shots.clear();
        self.debris.clear();
        if self.fits() {
            self.build_page();
        } else {
            self.letters.clear();
        }
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        // Clamp: a stalled terminal hands us seconds, and a letter integrated
        // over that lands somewhere absurd.
        let dt = dt.as_secs_f32().min(0.1);
        canvas.clear();
        if !self.fits() {
            return;
        }

        self.page_age += dt;
        if self.alive() == 0 {
            self.linger += dt;
            // The last letter gets to land before the page is swept.
            if self.linger >= LINGER && self.debris.is_empty() {
                self.next_page();
            }
        } else if self.page_age >= PAGE_LIFE {
            self.next_page();
        }

        self.drive_cannon(dt);
        self.fly_shots(dt);
        self.fall_debris(dt);
        self.draw(canvas);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(33);

    fn page() -> Vec<String> {
        [
            "Panels",
            "  Up / Down      move the cursor",
            "  Enter          open what is under it",
            "",
            "Sessions",
            "  Ctrl-T         the session rail",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn run(fx: &mut Cannon, canvas: &mut Canvas, frames: usize) {
        for _ in 0..frames {
            fx.tick(FRAME, canvas);
        }
    }

    #[test]
    fn the_page_is_laid_out_in_the_top_three_quarters() {
        let mut fx = Cannon::new();
        fx.set_text(&page());
        fx.resize(80, 24);
        assert!(fx.alive() > 0);
        let lowest = fx.letters.iter().map(|l| l.y).max().unwrap();
        assert!(lowest <= 24 * 3 / 4, "text reached row {lowest}");
        assert!(fx.letters.iter().all(|l| l.y >= 1));
        let title = fx.letters.iter().find(|l| l.y == 1).unwrap();
        assert_eq!(title.fg, TITLE, "a section title is not coloured as one");
    }

    #[test]
    fn the_key_column_is_told_apart_from_the_prose() {
        assert_eq!(key_end("  Ctrl-T         the session rail"), 8);
        assert_eq!(key_end("  Ctrl-O h       leave, and the history"), 10);
        assert_eq!(key_end("  just some words here"), 0);
        assert_eq!(key_end("Title"), 0);
    }

    #[test]
    fn letters_get_shot_and_land_in_a_heap() {
        let mut fx = Cannon::new();
        fx.set_text(&page());
        fx.resize(80, 30);
        let mut canvas = Canvas::new(80, 30);
        let before = fx.alive();
        // Ten seconds: dozens of shots.
        run(&mut fx, &mut canvas, 300);
        assert!(fx.alive() < before, "nothing was hit in ten seconds");
        let landed: usize = fx.heap.iter().map(Vec::len).sum();
        assert!(landed > 0, "nothing landed on the ground");
    }

    #[test]
    fn an_empty_page_is_followed_by_the_next_one() {
        let mut fx = Cannon::new();
        fx.set_text(&["AB".to_string(), "".to_string(), "CD".to_string()]);
        fx.resize(40, 12);
        // A page holds up to 8 rows here, so both lines are on the first page;
        // the point is that a shot-out page is replaced, not left blank.
        let mut canvas = Canvas::new(40, 12);
        let mut emptied = false;
        for _ in 0..3000 {
            fx.tick(FRAME, &mut canvas);
            if fx.alive() == 0 {
                emptied = true;
                break;
            }
        }
        assert!(emptied, "four letters were never all shot");
        let mut refilled = false;
        for _ in 0..3000 {
            fx.tick(FRAME, &mut canvas);
            if fx.alive() > 0 {
                refilled = true;
                break;
            }
        }
        assert!(refilled, "the empty page was never replaced");
    }

    #[test]
    fn a_long_text_is_read_a_page_at_a_time() {
        let text: Vec<String> = (0..100).map(|i| format!("  line {i}")).collect();
        let mut fx = Cannon::new();
        fx.set_text(&text);
        fx.resize(40, 20);
        let first = fx.page_start;
        let len = fx.page_len;
        assert!(len < 100 && len > 0, "page of {len} lines");
        fx.next_page();
        assert_eq!(fx.page_start, first + len, "the next page must follow on");
        for _ in 0..200 {
            fx.next_page();
        }
        assert!(fx.page_start < 100, "the pages must wrap");
    }

    #[test]
    fn without_a_text_there_is_still_a_page() {
        let mut fx = Cannon::new();
        fx.resize(80, 24);
        assert!(fx.alive() > 0, "the fallback page is empty");
    }

    #[test]
    fn a_screen_too_small_for_a_cannon_draws_nothing_and_survives() {
        let mut fx = Cannon::new();
        fx.set_text(&page());
        for (w, h) in [(0u16, 0u16), (5, 5), (11, 30), (80, 6)] {
            let mut canvas = Canvas::new(w, h);
            fx.resize(w, h);
            run(&mut fx, &mut canvas, 10);
            assert_eq!(fx.alive(), 0, "{w}x{h}");
        }
    }

    #[test]
    fn the_heap_never_climbs_into_the_page() {
        let mut fx = Cannon::new();
        fx.set_text(&page());
        fx.resize(60, 24);
        let mut canvas = Canvas::new(60, 24);
        run(&mut fx, &mut canvas, 1200);
        let cap = fx.heap_cap();
        assert!(fx.heap.iter().all(|c| c.len() <= cap));
        let ceiling = fx.ground() - 1 - cap as i32;
        assert!(ceiling > fx.text_rows() + 1, "the heap can reach the text");
    }

    #[test]
    fn a_tumbling_letter_flips_case_and_nothing_else() {
        assert_eq!(flip_case('a'), 'A');
        assert_eq!(flip_case('Q'), 'q');
        assert_eq!(flip_case('-'), '-');
        assert_eq!(flip_case('é'), 'é');
    }
}

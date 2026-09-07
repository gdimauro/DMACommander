//! The help page, arriving along a spiral and leaving the same way.
//!
//! Every character of the page has a place to be. It starts a long way off,
//! swung out on a helix around the centre of the screen, and winds inward as
//! the animation runs: the spiral unwinds, the radius closes, and the depth
//! comes to nothing until each glyph is sitting exactly where the page wants
//! it. Then it holds long enough to be read, unwinds back out, and returns with
//! the next screenful.
//!
//! Three things make it read as depth rather than as characters sliding about:
//!
//! - **Perspective.** A glyph's distance scales its offset from the centre, so
//!   far ones crowd together near the middle and near ones spread to the edges.
//! - **A ramp.** At a distance a character is not legible and pretending
//!   otherwise looks like noise, so it is drawn as a dot, then a comma, then
//!   itself. This is the same trick [`starfield`](super::starfield) uses.
//! - **A stagger.** Glyphs do not arrive together. Each carries a lag, so the
//!   page assembles rather than snapping into place.
//!
//! The aspect correction matters more here than in most effects: a circle drawn
//! honestly in character cells is an ellipse twice as tall as it is wide, and a
//! spiral without the correction visibly wobbles as it turns.

use crate::{Canvas, Cell, Effect, Rgb};
use std::f32::consts::TAU;
use std::time::Duration;

/// Character cells are about twice as tall as they are wide.
const CELL_ASPECT: f32 = 2.0;

/// Seconds in each phase. The hold is the number the user asked for; the other
/// two are as long as it takes to read the movement and no longer — an entrance
/// you have time to get bored of is an entrance that has failed.
const FLY_IN: f32 = 2.6;
const HOLD: f32 = 10.0;
const FLY_OUT: f32 = 1.8;

/// How many turns a glyph makes on its way in. Enough to be a spiral, few
/// enough that the eye can follow one character.
const TURNS: f32 = 1.75;

/// How far out a glyph starts, in cells, before perspective shrinks it.
const RADIUS: f32 = 26.0;

/// How much distance closes an offset up. Larger is a wider lens.
const LENS: f32 = 2.6;

/// What a glyph looks like before it is close enough to read.
const RAMP: &[char] = &['.', '.', ',', ':', '+', 'o'];

/// One character of the page, and the path it takes to get there.
struct Glyph {
    ch: char,
    /// Where it belongs, in cells.
    tx: f32,
    ty: f32,
    /// Where on the helix it starts: an angle, how far out, and how far back.
    angle: f32,
    radius: f32,
    depth: f32,
    /// 0..1. How much of the phase passes before this one starts moving, so the
    /// page assembles instead of snapping.
    lag: f32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    In,
    Hold,
    Out,
}

pub struct Helix {
    width: u16,
    height: u16,
    /// The whole page, as the frontend handed it over.
    text: Vec<String>,
    /// Which screenful is showing. Each loop moves on, so the effect is worth
    /// leaving running: the hold is long enough to read a page, and the next
    /// time round there is another one.
    page: usize,
    glyphs: Vec<Glyph>,
    phase: Phase,
    /// Seconds inside the current phase.
    t: f32,
    /// Arrive once and stay. What the help page uses: the same entrance, but
    /// the page it settles into is the real one, which the user reads and
    /// leaves on `Esc` — not something that flies away again while they are
    /// halfway down it.
    once: bool,
}

impl Helix {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            text: Vec::new(),
            page: 0,
            glyphs: Vec::new(),
            phase: Phase::In,
            t: 0.0,
            once: false,
        }
    }

    /// The same arrival, run once: it flies in and then stays put.
    pub fn entrance() -> Self {
        Self {
            once: true,
            ..Self::new()
        }
    }

    /// Whether the page is in place. The entrance is over and what is on screen
    /// is the text, sitting exactly where it belongs.
    pub fn settled(&self) -> bool {
        !matches!(self.phase, Phase::In)
    }

    /// How many lines of the page fit, leaving a margin so the text is not
    /// jammed against the frame.
    fn rows(&self) -> usize {
        (self.height as usize).saturating_sub(4).max(1)
    }

    /// Build the glyphs for the current page.
    ///
    /// Called on resize and on every turn of the loop. The layout is centred on
    /// the widest line of the page rather than per line, so the text keeps the
    /// shape it was written in — a help page whose every line is centred is a
    /// help page nobody can scan.
    fn lay_out(&mut self) {
        self.glyphs.clear();
        if self.width == 0 || self.height == 0 || self.text.is_empty() {
            return;
        }
        let rows = self.rows();
        let pages = self.text.len().div_ceil(rows).max(1);
        self.page %= pages;
        let from = self.page * rows;
        let lines: Vec<&String> = self.text.iter().skip(from).take(rows).collect();
        if lines.is_empty() {
            return;
        }

        let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        let left = (self.width as f32 - widest as f32) / 2.0;
        let top = (self.height as f32 - lines.len() as f32) / 2.0;

        for (row, line) in lines.iter().enumerate() {
            for (col, ch) in line.chars().enumerate() {
                if ch == ' ' {
                    continue;
                }
                let tx = left + col as f32;
                let ty = top + row as f32;
                if tx < 0.0 || ty < 0.0 || tx >= self.width as f32 || ty >= self.height as f32 {
                    continue;
                }
                self.glyphs.push(Glyph {
                    ch,
                    tx,
                    ty,
                    angle: fastrand::f32() * TAU,
                    // A spread of radii, so the swarm has depth to it rather
                    // than being one ring of characters.
                    radius: RADIUS * (0.45 + fastrand::f32() * 0.55),
                    depth: 0.55 + fastrand::f32() * 0.9,
                    // Later rows lag behind earlier ones, with enough jitter
                    // that the page does not arrive as a wipe.
                    lag: (row as f32 / lines.len().max(1) as f32) * 0.5 + fastrand::f32() * 0.25,
                });
            }
        }
    }

    /// The phase's progress as each glyph sees it: 0 far away, 1 in place.
    ///
    /// `p` is the phase's own progress. A glyph does not start until its lag
    /// has passed, and then covers the rest in what is left — so every glyph
    /// still arrives by the end, however late it set off.
    fn eased(p: f32, lag: f32) -> f32 {
        let start = lag.clamp(0.0, 0.9) * 0.6;
        let span = (1.0 - start).max(0.05);
        let raw = ((p - start) / span).clamp(0.0, 1.0);
        // Ease out cubic: quick to leave, gentle to arrive, which is what makes
        // the landing look like settling rather than stopping.
        1.0 - (1.0 - raw).powi(3)
    }
}

impl Default for Helix {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Helix {
    fn name(&self) -> &'static str {
        "helix"
    }

    fn set_text(&mut self, lines: &[String]) {
        self.text = lines.to_vec();
        self.lay_out();
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.lay_out();
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        self.t += dt.as_secs_f32();
        let span = match self.phase {
            Phase::In => FLY_IN,
            Phase::Hold => HOLD,
            Phase::Out => FLY_OUT,
        };
        // Arrived, and asked to stay. Held here rather than by making the hold
        // enormous: a number large enough to mean "for ever" is a number that
        // one day is not.
        if self.once && self.phase == Phase::Hold {
            self.t = 0.0;
        }
        if self.t >= span {
            self.t -= span;
            self.phase = match self.phase {
                Phase::In => Phase::Hold,
                Phase::Hold => Phase::Out,
                Phase::Out => {
                    // Round again with the next screenful, so the effect is
                    // worth leaving on: it reads the help to you, a page at a
                    // time, rather than showing the same one for ever.
                    self.page += 1;
                    self.lay_out();
                    Phase::In
                }
            };
        }

        // A short trail. It costs nothing, it sells the movement, and during
        // the hold it is invisible: a glyph that has not moved is redrawn at
        // full brightness over its own fading copy.
        canvas.fade(0.45);

        let progress = (self.t / span).clamp(0.0, 1.0);
        let cx = self.width as f32 / 2.0;
        let cy = self.height as f32 / 2.0;

        for g in &self.glyphs {
            let e = match self.phase {
                Phase::In => Self::eased(progress, g.lag),
                Phase::Hold => 1.0,
                // Out is In run backwards, and the lag is mirrored so the page
                // leaves in the order it arrived rather than inside out.
                Phase::Out => 1.0 - Self::eased(progress, 1.0 - g.lag),
            };

            let away = 1.0 - e;
            let angle = g.angle + away * TURNS * TAU;
            let radius = g.radius * away;
            let wx = g.tx + radius * angle.cos() * CELL_ASPECT;
            let wy = g.ty + radius * angle.sin();

            // Perspective. Distance pulls a glyph towards the middle of the
            // screen and shrinks how far it can stray from it.
            let scale = 1.0 / (1.0 + g.depth * away * LENS);
            let x = cx + (wx - cx) * scale;
            let y = cy + (wy - cy) * scale;

            // Far away a character is not legible, and drawing it anyway reads
            // as noise rather than as text at a distance.
            let ch = if e > 0.72 {
                g.ch
            } else {
                let step = ((e / 0.72) * (RAMP.len() - 1) as f32) as usize;
                RAMP[step.min(RAMP.len() - 1)]
            };

            // Cold and dim in the distance, warm and bright in place.
            let far = Rgb(24, 90, 120);
            let near = Rgb(215, 235, 245);
            let fg = far.lerp(near, e * e);

            canvas.set(
                x.round() as i32,
                y.round() as i32,
                Cell {
                    ch,
                    fg,
                    bg: Rgb::BLACK,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page() -> Vec<String> {
        (0..40)
            .map(|i| format!("line {i} of the help page"))
            .collect()
    }

    fn helix(w: u16, h: u16) -> Helix {
        let mut e = Helix::new();
        // The frontend sets the text before the first resize, and the effect has
        // to cope with either order anyway.
        e.set_text(&page());
        e.resize(w, h);
        e
    }

    /// Every non-space character of the visible page gets a glyph, and none of
    /// them is laid out off the canvas — a target outside it is a character
    /// that arrives and is never seen.
    #[test]
    fn the_page_becomes_glyphs_that_all_fit() {
        let e = helix(80, 24);
        assert!(!e.glyphs.is_empty(), "nothing was laid out");
        for g in &e.glyphs {
            assert!(g.ch != ' ', "a space was given a glyph");
            assert!((0.0..80.0).contains(&g.tx), "{} is off the canvas", g.tx);
            assert!((0.0..24.0).contains(&g.ty), "{} is off the canvas", g.ty);
        }
    }

    /// The whole point of the hold: at rest every glyph is exactly on its
    /// target, in its own character. A page that settles a cell out of place is
    /// a page you cannot read.
    #[test]
    fn the_text_settles_exactly_where_it_belongs() {
        let mut e = helix(80, 24);
        let mut canvas = Canvas::new(80, 24);
        // Through the fly-in and into the hold.
        e.tick(Duration::from_secs_f32(FLY_IN + 0.1), &mut canvas);
        e.tick(Duration::from_millis(16), &mut canvas);
        assert!(matches!(e.phase, Phase::Hold), "it never settled");

        for g in &e.glyphs {
            let got = canvas.get(g.tx.round() as i32, g.ty.round() as i32);
            assert_eq!(
                got.map(|c| c.ch),
                Some(g.ch),
                "{:?} is not at {},{}",
                g.ch,
                g.tx,
                g.ty
            );
        }
    }

    /// In, hold, out, and round again with the next screenful — which is what
    /// makes it worth leaving running rather than a single animation.
    #[test]
    fn it_goes_round_and_brings_the_next_page() {
        let mut e = helix(80, 24);
        let mut canvas = Canvas::new(80, 24);
        assert_eq!(e.page, 0);

        for span in [FLY_IN, HOLD, FLY_OUT] {
            e.tick(Duration::from_secs_f32(span + 0.05), &mut canvas);
        }
        e.tick(Duration::from_millis(16), &mut canvas);
        assert_eq!(e.page, 1, "it showed the same page twice");
        assert!(matches!(e.phase, Phase::In), "it did not come back");
    }

    /// A page count that wraps, so the effect never runs out of help. 40 lines
    /// at 20 rows is two pages; the third time round is the first page again.
    #[test]
    fn the_pages_wrap_rather_than_running_out() {
        let mut e = helix(80, 24);
        let rows = e.rows();
        let pages = 40_usize.div_ceil(rows);
        e.page = pages;
        e.lay_out();
        assert_eq!(e.page, 0, "past the last page is the first one again");
    }

    /// No text is not a crash. The frontend sets it, and an effect built before
    /// it does has none — for one frame, or for ever if something goes wrong
    /// upstream.
    #[test]
    fn nothing_to_say_is_not_a_panic() {
        let mut e = Helix::new();
        e.resize(80, 24);
        let mut canvas = Canvas::new(80, 24);
        e.tick(Duration::from_millis(16), &mut canvas);
        assert!(e.glyphs.is_empty());
    }

    /// A terminal can be resized to something absurd, and an effect that
    /// divides by its height must not go with it.
    #[test]
    fn a_canvas_with_no_room_is_survivable() {
        for (w, h) in [(0, 0), (1, 1), (200, 2), (2, 200)] {
            let mut e = Helix::new();
            e.set_text(&page());
            e.resize(w, h);
            let mut canvas = Canvas::new(w, h);
            e.tick(Duration::from_millis(16), &mut canvas);
        }
    }
}

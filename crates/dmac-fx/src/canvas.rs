//! The surface an effect draws on.
//!
//! Deliberately not a ratatui buffer. `dmac-fx` sits below `dmac-tui` in the
//! layering, and an effect must be renderable by the GPU backend too — so the
//! canvas is a plain grid of cells that either backend can rasterize.

/// A colour, in the only space that survives being sent to both a terminal and
/// a shader without translation loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const BLACK: Rgb = Rgb(0, 0, 0);

    /// Scale towards black. Used by every effect that fades a trail.
    pub fn dim(self, factor: f32) -> Rgb {
        let f = factor.clamp(0.0, 1.0);
        Rgb(
            (self.0 as f32 * f) as u8,
            (self.1 as f32 * f) as u8,
            (self.2 as f32 * f) as u8,
        )
    }

    /// Linear interpolation. Not gamma-correct, which is fine for glow effects
    /// and wrong for anything a user reads — do not use this for UI text.
    pub fn lerp(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t) as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Rgb::BLACK,
            bg: Rgb::BLACK,
        }
    }
}

/// A fixed-size grid. Reused across frames — effects clear or fade it themselves
/// rather than reallocating, because a screensaver that allocates 60 times a
/// second is a screensaver that shows up in `powermetrics`.
#[derive(Debug, Clone)]
pub struct Canvas {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Canvas {
    pub fn new(width: u16, height: u16) -> Self {
        let mut c = Self {
            width: 0,
            height: 0,
            cells: Vec::new(),
        };
        c.resize(width, height);
        c
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        self.width = width;
        self.height = height;
        self.cells.clear();
        self.cells
            .resize(width as usize * height as usize, Cell::default());
    }

    pub fn clear(&mut self) {
        self.cells.fill(Cell::default());
    }

    /// Fade everything towards black. The cheapest way to get motion trails, and
    /// what makes matrix rain and pipes look like they have depth.
    pub fn fade(&mut self, factor: f32) {
        for c in &mut self.cells {
            c.fg = c.fg.dim(factor);
            c.bg = c.bg.dim(factor);
            // Once a glyph is essentially black, drop it: leaving invisible
            // characters around costs the terminal diff for nothing.
            if c.fg == Rgb::BLACK {
                c.ch = ' ';
            }
        }
    }

    /// Out-of-bounds writes are silently dropped. Effects work in float space and
    /// clamping at every call site would bury the actual animation logic.
    pub fn set(&mut self, x: i32, y: i32, cell: Cell) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let i = y as usize * self.width as usize + x as usize;
        self.cells[i] = cell;
    }

    pub fn get(&self, x: i32, y: i32) -> Option<&Cell> {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return None;
        }
        self.cells
            .get(y as usize * self.width as usize + x as usize)
    }

    /// Draw a straight line between two points, Bresenham.
    ///
    /// The glyph is chosen once from the segment's slope rather than per step,
    /// which is what makes a polygon read as a wireframe instead of as a dotted
    /// trail. Character cells are about twice as tall as wide, so the slope is
    /// compared against that ratio and not against 1.
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, fg: Rgb) {
        let (dx, dy) = ((x1 - x0), (y1 - y0));
        let ch = glyph_for_slope(dx, dy);

        // i64 throughout: the error term doubles each step, and a caller working
        // in float space can hand us coordinates near the i32 bounds. The test
        // below is what found this.
        let (adx, ady) = ((dx as i64).abs(), -(dy as i64).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y) = (x0, y0);
        let mut err = adx + ady;

        // Bounded: a segment stretching far off screen must not become an
        // effectively infinite loop.
        let budget = (adx - ady) + 4;
        for _ in 0..budget.clamp(0, 10_000) {
            self.set(
                x,
                y,
                Cell {
                    ch,
                    fg,
                    bg: Rgb::BLACK,
                },
            );
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= ady {
                err += ady;
                x += sx;
            }
            if e2 <= adx {
                err += adx;
                y += sy;
            }
        }
    }

    /// Draw a closed polygon through `points`, given in cell coordinates.
    pub fn polygon(&mut self, points: &[(i32, i32)], fg: Rgb) {
        for i in 0..points.len() {
            let a = points[i];
            let b = points[(i + 1) % points.len()];
            self.line(a.0, a.1, b.0, b.1, fg);
        }
    }

    /// Row-major view, for a renderer that walks the grid once.
    pub fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        self.cells.chunks(self.width.max(1) as usize)
    }
}

/// Box-drawing character matching a segment's direction.
///
/// The thresholds account for the cell aspect ratio: a segment that is visually
/// diagonal covers about twice as many columns as rows.
fn glyph_for_slope(dx: i32, dy: i32) -> char {
    // dy is doubled because a character cell is about twice as tall as wide.
    let (adx, ady) = (dx.abs() as f32, (dy.abs() * 2) as f32);
    if ady < adx * 0.5 {
        '\u{2500}' // ─
    } else if adx < ady * 0.5 {
        '\u{2502}' // │
    } else if (dx > 0) == (dy > 0) {
        '\u{2572}' // ╲
    } else {
        '\u{2571}' // ╱
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_bounds_writes_are_dropped_not_panics() {
        let mut c = Canvas::new(10, 5);
        for (x, y) in [(-1, 0), (0, -1), (10, 0), (0, 5), (i32::MAX, i32::MIN)] {
            c.set(x, y, Cell::default());
        }
    }

    #[test]
    fn a_zero_sized_canvas_is_survivable() {
        // A terminal really can report 0 columns mid-resize.
        let mut c = Canvas::new(0, 0);
        c.set(0, 0, Cell::default());
        c.fade(0.9);
        assert_eq!(c.rows().count(), 0);
    }

    #[test]
    fn fade_eventually_reaches_black_and_clears_the_glyph() {
        let mut c = Canvas::new(4, 4);
        c.set(
            1,
            1,
            Cell {
                ch: 'X',
                fg: Rgb(255, 255, 255),
                bg: Rgb::BLACK,
            },
        );
        for _ in 0..200 {
            c.fade(0.5);
        }
        let cell = c.get(1, 1).unwrap();
        assert_eq!(cell.fg, Rgb::BLACK);
        assert_eq!(cell.ch, ' ', "an invisible glyph must be dropped");
    }

    #[test]
    fn a_horizontal_line_fills_every_cell_between_its_ends() {
        let mut c = Canvas::new(20, 5);
        c.line(2, 2, 10, 2, Rgb(255, 255, 255));
        for x in 2..=10 {
            assert_ne!(c.get(x, 2).unwrap().ch, ' ', "gap at x={x}");
        }
        assert_eq!(c.get(11, 2).unwrap().ch, ' ', "must not overshoot");
    }

    #[test]
    fn a_line_drawn_backwards_covers_the_same_cells() {
        let mut a = Canvas::new(20, 10);
        let mut b = Canvas::new(20, 10);
        a.line(2, 1, 15, 8, Rgb(255, 255, 255));
        b.line(15, 8, 2, 1, Rgb(255, 255, 255));
        let filled = |c: &Canvas| {
            let mut v: Vec<(usize, usize)> = Vec::new();
            for (y, row) in c.rows().enumerate() {
                for (x, cell) in row.iter().enumerate() {
                    if cell.ch != ' ' {
                        v.push((x, y));
                    }
                }
            }
            v
        };
        assert_eq!(filled(&a), filled(&b));
    }

    /// A caller working in float space can hand us anything; a screensaver must
    /// never turn into an infinite loop over it.
    #[test]
    fn absurd_line_coordinates_terminate() {
        let mut c = Canvas::new(20, 10);
        c.line(-100_000, -100_000, 100_000, 100_000, Rgb(1, 1, 1));
        c.line(i32::MIN / 2, 0, i32::MAX / 2, 0, Rgb(1, 1, 1));
    }

    #[test]
    fn a_polygon_closes_itself() {
        let mut c = Canvas::new(30, 15);
        c.polygon(&[(5, 2), (20, 2), (12, 10)], Rgb(255, 255, 255));
        // The closing edge exists: something is drawn near the midpoint of the
        // segment from the last vertex back to the first.
        assert_ne!(c.get(8, 6).unwrap().ch, ' ');
    }

    #[test]
    fn resize_reallocates_to_the_new_size() {
        let mut c = Canvas::new(10, 5);
        c.resize(3, 2);
        assert_eq!(c.rows().count(), 2);
        assert!(c.rows().all(|r| r.len() == 3));
    }
}

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

    /// Row-major view, for a renderer that walks the grid once.
    pub fn rows(&self) -> impl Iterator<Item = &[Cell]> {
        self.cells.chunks(self.width.max(1) as usize)
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
    fn resize_reallocates_to_the_new_size() {
        let mut c = Canvas::new(10, 5);
        c.resize(3, 2);
        assert_eq!(c.rows().count(), 2);
        assert!(c.rows().all(|r| r.len() == 3));
    }
}

//! Rasterizing an `dmac-fx` canvas into a ratatui buffer.
//!
//! This is the whole of the TUI backend for effects: the effect itself knows
//! nothing about ratatui, which is what lets the GPU backend render the exact
//! same frames from the exact same `Canvas`.

use dmac_fx::{Canvas, Rgb};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Color;

fn colour(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

/// Draw the canvas over `area`, cell for cell.
///
/// Writes straight into the buffer rather than building `Line`/`Span` values:
/// at 30fps over a full screen that would allocate thousands of short strings a
/// second, which is exactly the kind of churn a screensaver must not produce.
pub fn draw_canvas(frame: &mut Frame, area: Rect, canvas: &Canvas) {
    let buf = frame.buffer_mut();

    for (y, row) in canvas.rows().enumerate() {
        let sy = area.y + y as u16;
        if sy >= area.y + area.height {
            break;
        }
        for (x, cell) in row.iter().enumerate() {
            let sx = area.x + x as u16;
            if sx >= area.x + area.width {
                break;
            }
            buf[(sx, sy)]
                .set_char(cell.ch)
                .set_fg(colour(cell.fg))
                .set_bg(colour(cell.bg));
        }
    }
}

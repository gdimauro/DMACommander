//! The bottom function-key bar.
//!
//! This bar *is* the documentation. It is always visible and it must always be
//! accurate for the current context — a bar that lies about F5 is worse than no
//! bar at all.

use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const KEYS: [(&str, &str); 10] = [
    ("1", "Help"),
    ("2", "Menu"),
    ("3", "View"),
    ("4", "Edit"),
    ("5", "Copy"),
    ("6", "Move"),
    ("7", "MkDir"),
    ("8", "Delete"),
    ("9", "PullDn"),
    ("10", "Quit"),
];

pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme) {
    let label = Style::default()
        .fg(theme.fkey_label_fg)
        .bg(theme.fkey_label_bg);
    let name = Style::default()
        .fg(theme.fkey_name_fg)
        .bg(theme.fkey_name_bg);

    // Each cell gets an equal share of the width; the name is padded to fill it
    // so the coloured blocks line up into a solid bar.
    let cell = (area.width as usize / KEYS.len()).max(4);
    let name_w = cell.saturating_sub(2).max(1);

    let mut spans = Vec::with_capacity(KEYS.len() * 2);
    for (k, n) in KEYS {
        spans.push(Span::styled(format!("{k:>2}"), label));
        spans.push(Span::styled(format!("{n:<name_w$}"), name));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

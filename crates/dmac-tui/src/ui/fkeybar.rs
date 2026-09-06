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

/// What the keys do with nothing open. Anything that takes over the keyboard
/// supplies its own set, because a bar that still advertises Copy while a
/// directory list is up is a bar that lies.
pub const NORMAL: [(&str, &str); 10] = [
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

/// The bar for the directory history: the three orders, and the way out.
pub const HISTORY: [(&str, &str); 10] = [
    ("1", "Recent"),
    ("2", "MostUsed"),
    ("3", "Session"),
    ("4", "Sessions"),
    ("5", ""),
    ("6", ""),
    ("7", ""),
    ("8", ""),
    ("9", ""),
    ("10", "Close"),
];

/// `active` marks the key whose mode is currently on — the bar doubles as the
/// indicator, so there is no second place to look.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    keys: &[(&str, &str)],
    active: Option<usize>,
    theme: &Theme,
) {
    let label = Style::default()
        .fg(theme.fkey_label_fg)
        .bg(theme.fkey_label_bg);
    let name = Style::default()
        .fg(theme.fkey_name_fg)
        .bg(theme.fkey_name_bg);

    // Each cell gets an equal share of the width; the name is padded to fill it
    // so the coloured blocks line up into a solid bar.
    let cell = (area.width as usize / keys.len().max(1)).max(4);
    let name_w = cell.saturating_sub(2).max(1);

    let mut spans = Vec::with_capacity(keys.len() * 2);
    for (i, (k, n)) in keys.iter().enumerate() {
        let name = if active == Some(i) {
            Style::default()
                .fg(theme.fkey_name_bg)
                .bg(theme.fkey_name_fg)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            name
        };
        spans.push(Span::styled(format!("{k:>2}"), label));
        spans.push(Span::styled(format!("{n:<name_w$}"), name));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bar is the only place the views are named on screen. If the model
    /// grows one and the bar does not, a key exists that nothing mentions.
    #[test]
    fn the_bar_names_every_history_view() {
        for view in dmac_core::history::Order::ALL {
            let key = (view.index() + 1).to_string();
            let found = HISTORY
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, label)| *label);
            assert_eq!(
                found,
                Some(view.label()),
                "F{key} should be labelled {:?}",
                view.label()
            );
        }
    }
}

//! The screensaver picker.
//!
//! A centred list overlay. This is deliberately the smallest possible modal —
//! it is also the seed of the general dialog layer M1 needs, so it is written to
//! be lifted out rather than to be special.

use crate::theme::Theme;
use dmac_fx::{CatalogEntry, Kind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Rows shown above the catalogue itself.
pub const EXTRA: [(&str, &str); 2] = [
    ("random", "a different one every time"),
    ("rotation", "each one in turn"),
];

/// Total selectable rows: the two modes plus everything in the catalogue.
pub fn row_count(catalog: &[CatalogEntry]) -> usize {
    EXTRA.len() + catalog.len()
}

/// The effect name for a row index.
pub fn name_at(catalog: &[CatalogEntry], index: usize) -> &str {
    match EXTRA.get(index) {
        Some((name, _)) => name,
        None => catalog
            .get(index - EXTRA.len())
            .map(|e| e.name)
            .unwrap_or("random"),
    }
}

/// Draw the picker and return the interior rect, so a click can be mapped back
/// to a row without duplicating the centring arithmetic.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    catalog: &[CatalogEntry],
    selected: usize,
    theme: &Theme,
) -> Rect {
    let rows = row_count(catalog);
    // +2 for the border, +1 for the games separator.
    let height = (rows as u16 + 4).min(area.height);
    let width = 46.min(area.width);
    let popup = centred(area, width, height);

    // `Clear` is what stops the panels showing through the overlay.
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .title(Span::styled(" Screensaver ", theme.border(true)))
        .title_bottom(Span::styled(
            " enter start · esc cancel ",
            theme.border(false),
        ))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::with_capacity(rows + 1);
    let mut games_started = false;

    for i in 0..rows {
        // A visual break before the games: they are a different kind of thing,
        // and the idle timer never starts them.
        if !games_started
            && let Some(e) = catalog.get(i.saturating_sub(EXTRA.len()))
            && i >= EXTRA.len()
            && e.kind == Kind::Game
        {
            games_started = true;
            lines.push(Line::from(Span::styled(
                "  — games —",
                Style::default().fg(theme.status_fg).bg(theme.panel_bg),
            )));
        }

        let (name, blurb) = match EXTRA.get(i) {
            Some((n, b)) => (*n, *b),
            None => match catalog.get(i - EXTRA.len()) {
                Some(e) => (e.name, e.blurb),
                None => continue,
            },
        };

        let style = if i == selected {
            theme.cursor()
        } else {
            Style::default().fg(theme.panel_fg).bg(theme.panel_bg)
        };
        let text = format!(" {name:<11} {blurb}");
        let padded = pad(&text, inner.width as usize);
        lines.push(Line::from(Span::styled(padded, style)));
    }

    frame.render_widget(Paragraph::new(lines), inner);
    inner
}

/// Rendered row for a selectable index, accounting for the games separator —
/// used to map a mouse click back to a selection.
pub fn display_row(catalog: &[CatalogEntry], index: usize) -> usize {
    let first_game = catalog
        .iter()
        .position(|e| e.kind == Kind::Game)
        .map(|p| p + EXTRA.len());
    match first_game {
        Some(g) if index >= g => index + 1,
        _ => index,
    }
}

/// Inverse of [`display_row`]: which selection a clicked row corresponds to.
/// `None` for the separator itself, which is not selectable.
pub fn index_at_row(catalog: &[CatalogEntry], row: usize) -> Option<usize> {
    let rows = row_count(catalog);
    (0..rows).find(|&i| display_row(catalog, i) == row)
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn pad(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let w = s.width();
    if w >= width {
        s.chars().take(width).collect()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_selectable_index_maps_to_a_distinct_row() {
        let catalog = dmac_fx::catalog();
        let rows: Vec<usize> = (0..row_count(catalog))
            .map(|i| display_row(catalog, i))
            .collect();
        let mut unique = rows.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), rows.len(), "rows collide: {rows:?}");
    }

    #[test]
    fn clicking_a_row_round_trips_to_its_index() {
        let catalog = dmac_fx::catalog();
        for i in 0..row_count(catalog) {
            assert_eq!(index_at_row(catalog, display_row(catalog, i)), Some(i));
        }
    }

    #[test]
    fn the_separator_row_is_not_selectable() {
        let catalog = dmac_fx::catalog();
        let first_game = catalog.iter().position(|e| e.kind == Kind::Game).unwrap();
        let separator = first_game + EXTRA.len();
        assert_eq!(index_at_row(catalog, separator), None);
    }

    #[test]
    fn the_first_two_rows_are_the_modes() {
        let catalog = dmac_fx::catalog();
        assert_eq!(name_at(catalog, 0), "random");
        assert_eq!(name_at(catalog, 1), "rotation");
        assert_eq!(name_at(catalog, 2), catalog[0].name);
    }
}

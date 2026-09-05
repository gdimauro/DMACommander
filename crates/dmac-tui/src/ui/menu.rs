//! A popup menu anchored near a screen position.
//!
//! Used for the right-click context menu, and written to be the general menu
//! widget the F9 pull-down and the user menu will reuse. It knows nothing about
//! what the items do — it renders labels and hints and reports a row.

use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

/// One row. A separator is an item with no label and no action.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    pub label: &'static str,
    /// The keyboard equivalent, right-aligned — this is how a context menu
    /// teaches its own shortcuts.
    pub hint: &'static str,
    pub separator: bool,
}

impl Item {
    pub const fn new(label: &'static str, hint: &'static str) -> Self {
        Self {
            label,
            hint,
            separator: false,
        }
    }

    pub const SEPARATOR: Self = Self {
        label: "",
        hint: "",
        separator: true,
    };
}

/// Index of the next selectable item in `step` direction, skipping separators
/// and wrapping. Returns `None` if there is nothing selectable at all.
pub fn next_selectable(items: &[Item], from: usize, step: isize) -> Option<usize> {
    let n = items.len();
    if n == 0 {
        return None;
    }
    for k in 1..=n {
        let i = (from as isize + step * k as isize).rem_euclid(n as isize) as usize;
        if !items[i].separator {
            return Some(i);
        }
    }
    None
}

/// The first selectable item, for opening a fresh menu.
pub fn first_selectable(items: &[Item]) -> usize {
    items.iter().position(|i| !i.separator).unwrap_or(0)
}

/// Draw the menu near `anchor`, nudged so it always fits on screen. Returns the
/// interior rect, so a click maps to a row without repeating the geometry.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    anchor: (u16, u16),
    items: &[Item],
    selected: usize,
    theme: &Theme,
) -> Rect {
    let content_w = items
        .iter()
        .map(|i| i.label.width() + i.hint.width() + 4)
        .max()
        .unwrap_or(10);
    let width = (content_w as u16 + 2).clamp(12, area.width.max(12));
    let height = (items.len() as u16 + 2).min(area.height.max(3));

    // Prefer down-and-right of the pointer, like every context menu, but flip
    // rather than clip when there is no room.
    let x = if anchor.0 + width <= area.x + area.width {
        anchor.0
    } else {
        (area.x + area.width).saturating_sub(width)
    };
    let y = if anchor.1 + height <= area.y + area.height {
        anchor.1
    } else {
        (area.y + area.height).saturating_sub(height)
    };
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            if item.separator {
                return Line::from(Span::styled(
                    "─".repeat(inner.width as usize),
                    Style::default().fg(theme.panel_border).bg(theme.panel_bg),
                ));
            }
            let style = if i == selected {
                theme.cursor()
            } else {
                Style::default().fg(theme.panel_fg).bg(theme.panel_bg)
            };
            let gap =
                (inner.width as usize).saturating_sub(item.label.width() + item.hint.width() + 2);
            let text = format!(" {}{}{} ", item.label, " ".repeat(gap), item.hint);
            Line::from(Span::styled(clip(text, inner.width as usize), style))
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().add_modifier(Modifier::empty())),
        inner,
    );
    inner
}

fn clip(s: String, width: usize) -> String {
    if s.width() <= width {
        return s;
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<Item> {
        vec![
            Item::new("Open", "Enter"),
            Item::SEPARATOR,
            Item::new("Copy", "F5"),
            Item::new("Delete", "F8"),
        ]
    }

    #[test]
    fn navigation_skips_separators() {
        let it = items();
        assert_eq!(
            next_selectable(&it, 0, 1),
            Some(2),
            "must skip the separator"
        );
        assert_eq!(next_selectable(&it, 2, -1), Some(0));
    }

    #[test]
    fn navigation_wraps_around() {
        let it = items();
        assert_eq!(next_selectable(&it, 3, 1), Some(0));
        assert_eq!(next_selectable(&it, 0, -1), Some(3));
    }

    #[test]
    fn a_menu_of_only_separators_has_nothing_to_select() {
        let it = vec![Item::SEPARATOR, Item::SEPARATOR];
        assert_eq!(next_selectable(&it, 0, 1), None);
    }

    #[test]
    fn the_first_selectable_skips_a_leading_separator() {
        let it = vec![Item::SEPARATOR, Item::new("Open", "Enter")];
        assert_eq!(first_selectable(&it), 1);
    }
}

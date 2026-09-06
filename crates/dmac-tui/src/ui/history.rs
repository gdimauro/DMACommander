//! The directory history: a filtered list of everywhere you have been.
//!
//! A scrolling list with a filter box, and the three orders on the F-key bar
//! below — which is where a Commander user looks for what a panel can do.

use crate::theme::Theme;
use dmac_core::history::{Order, Row, ago};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// A row, plus which of its characters the filter matched.
pub struct Shown {
    pub row: Row,
    pub hit: Vec<usize>,
}

/// Everything about the list that is not the list itself.
pub struct State<'a> {
    pub filter: &'a str,
    pub order: Order,
    pub selected: usize,
    /// Passed in rather than read from the clock, so a frame is a pure function
    /// of state and a golden test can assert on the ages it prints.
    pub now: u64,
}

/// Draw the overlay and return the interior rect of the *list*, so a click maps
/// to a row without repeating the centring arithmetic at the call site.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    rows: &[Shown],
    state: &State<'_>,
    theme: &Theme,
) -> Rect {
    let State {
        filter,
        order,
        selected,
        now,
    } = *state;
    let width = area.width.saturating_sub(8).clamp(40, 100).min(area.width);
    // Border, filter line, separator: four rows that are not list.
    let height = ((rows.len() as u16).saturating_add(4)).clamp(8, area.height.saturating_sub(2));
    let popup = centred(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .title(Span::styled(
            format!(" Directories \u{2014} {} ", order.title()),
            theme.border(true),
        ))
        .title_bottom(Span::styled(
            " enter go \u{b7} esc close \u{b7} type to filter ",
            theme.border(false),
        ))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 2 {
        return inner;
    }

    // The filter line, with a block for a cursor: this box takes typing, and it
    // has to look like it does even though the real cursor is elsewhere.
    let filter_line = Line::from(vec![
        Span::styled(" filter ", Style::default().fg(theme.status_fg)),
        Span::styled(
            filter.to_string(),
            Style::default()
                .fg(theme.selected_fg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(theme.selected_fg)),
    ]);
    frame.render_widget(Paragraph::new(filter_line), Rect { height: 1, ..inner });

    let list = Rect {
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
        ..inner
    };
    if list.height == 0 {
        return list;
    }

    if rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                if filter.is_empty() {
                    "  nowhere yet \u{2014} the history fills as you navigate"
                } else {
                    "  nothing matches"
                },
                Style::default().fg(theme.status_fg),
            ))),
            list,
        );
        return list;
    }

    // Keep the selection on screen: scroll only as far as it takes.
    let visible = list.height as usize;
    let top = first_visible(selected, rows.len(), visible);

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(top)
        .take(visible)
        .map(|(i, shown)| row_line(shown, i == selected, order, now, list.width as usize, theme))
        .collect();

    frame.render_widget(Paragraph::new(lines), list);
    list
}

/// Which row sits at the top of the window. Public because the mouse needs the
/// same answer to turn a click into an index.
pub fn first_visible(selected: usize, total: usize, visible: usize) -> usize {
    if total <= visible {
        return 0;
    }
    // Centre the selection once it is past the middle, and stop at the end.
    selected
        .saturating_sub(visible / 2)
        .min(total.saturating_sub(visible))
}

fn row_line<'a>(
    shown: &'a Shown,
    is_selected: bool,
    order: Order,
    now: u64,
    width: usize,
    theme: &Theme,
) -> Line<'a> {
    let base = if is_selected {
        theme.cursor()
    } else {
        Style::default().fg(theme.panel_fg).bg(theme.panel_bg)
    };
    // Matched characters are bold and coloured, so it is obvious *why* a row is
    // in the list — the thing a plain filtered list never tells you.
    let matched = base.fg(theme.selected_fg).add_modifier(Modifier::BOLD);

    // "5d" or "×12", right-aligned: recency for the time-ordered views, the
    // count for the one that ranks by it.
    let tail = match order {
        Order::Frequent => format!("\u{d7}{}", shown.row.hits),
        _ => ago(shown.row.at, now),
    };

    let path = home_relative(&shown.row.path);
    let room = width.saturating_sub(tail.chars().count() + 3);
    let (text, dropped) = elide(&path, room);

    let mut spans = vec![Span::styled(" ", base)];
    for (i, c) in text.chars().enumerate() {
        // `elide` cuts from the left, so a highlight index has to shift with it.
        let original = i + dropped;
        let style = if shown.hit.contains(&original) {
            matched
        } else {
            base
        };
        spans.push(Span::styled(c.to_string(), style));
    }

    let used = text.chars().count() + 1;
    let pad = width.saturating_sub(used + tail.chars().count() + 1);
    spans.push(Span::styled(" ".repeat(pad), base));
    spans.push(Span::styled(tail, base.fg(theme.status_fg)));
    spans.push(Span::styled(" ", base));
    Line::from(spans)
}

/// `/Users/you/prj` shown as `~/prj`. The home prefix is the least informative
/// part of almost every path here.
fn home_relative(path: &str) -> String {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return path.to_string();
    };
    let home = home.to_string_lossy().to_string();
    if home.is_empty() || home == "/" {
        return path.to_string();
    }
    match path.strip_prefix(&home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Cut from the left, which is where the least useful part of a path lives.
/// Returns the text and how many characters were dropped, so highlights can be
/// moved with it.
fn elide(path: &str, room: usize) -> (String, usize) {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= room || room < 2 {
        return (path.to_string(), 0);
    }
    let drop = chars.len() - (room - 1);
    let tail: String = chars[drop..].iter().collect();
    (format!("\u{2026}{tail}"), drop - 1)
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_list_never_scrolls() {
        assert_eq!(first_visible(0, 3, 10), 0);
        assert_eq!(first_visible(2, 3, 10), 0);
    }

    #[test]
    fn the_selection_stays_on_screen() {
        // 100 rows in a 10-row window: the top follows, and stops at the end.
        assert_eq!(first_visible(0, 100, 10), 0);
        assert_eq!(first_visible(3, 100, 10), 0, "no scroll until the middle");
        assert_eq!(first_visible(50, 100, 10), 45, "centred");
        assert_eq!(first_visible(99, 100, 10), 90, "pinned to the bottom");
    }

    #[test]
    fn a_long_path_is_cut_from_the_left() {
        let (text, dropped) = elide("/one/two/three/four", 10);
        assert!(text.starts_with('\u{2026}'));
        assert_eq!(text.chars().count(), 10);
        // The ellipsis stands in for `dropped` characters plus itself, so an
        // index in the original shifts by exactly that much.
        assert_eq!(&"/one/two/three/four"[dropped + 1..], &text[3..]);
    }

    #[test]
    fn a_short_path_is_left_alone() {
        assert_eq!(elide("/a/b", 20), ("/a/b".to_string(), 0));
    }
}

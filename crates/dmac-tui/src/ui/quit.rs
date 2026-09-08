//! "Quit?" — and two things to decide on the way out.
//!
//! A question rather than a bare exit, because leaving has consequences that
//! are decided *now*: whether the windows that are open come back next time,
//! and whether where they are right now is worth remembering for this set of
//! monitors. Both are ticked the way they were last left, so the common case
//! is Enter and nothing else.

use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// The two answers, in the order the rows are drawn.
pub const ROWS: usize = 2;

/// Draw the question. Returns the interior rect of the two rows, so a click
/// maps back to the row it landed on.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    restore_windows: bool,
    remember_positions: bool,
    selected: usize,
    theme: &Theme,
) -> Rect {
    let width = area.width.saturating_sub(6).clamp(44, 72).min(area.width);
    // Border, headline, blank, two rows, footer hint.
    let height = 7_u16.min(area.height);
    let popup = centred(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .title(Span::styled(" Quit? ", theme.border(true)))
        .title_bottom(Span::styled(
            " y quit \u{b7} n stay \u{b7} space toggle ",
            theme.border(false),
        ))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height < 3 {
        return inner;
    }

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Leave DMACommander?",
            Style::default().fg(theme.panel_fg),
        ))),
        Rect { height: 1, ..inner },
    );

    let list = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2).min(ROWS as u16),
        ..inner
    };
    let rows = [
        (restore_windows, "reopen the same windows next time"),
        (
            remember_positions,
            "remember their positions on this set of monitors",
        ),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .take(list.height as usize)
        .map(|(i, (ticked, what))| row(*ticked, what, i == selected, theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
    list
}

fn row<'a>(ticked: bool, what: &'a str, is_selected: bool, theme: &Theme) -> Line<'a> {
    let base = if is_selected {
        theme.cursor()
    } else {
        Style::default().fg(theme.panel_fg).bg(theme.panel_bg)
    };
    // A box, because it is one: something you can change before saying yes.
    let mark = if ticked { "[x]" } else { "[ ]" };
    Line::from(vec![
        Span::styled(format!(" {mark} "), base.add_modifier(Modifier::BOLD)),
        Span::styled(what, base),
    ])
}

fn centred(area: Rect, width: u16, height: u16) -> Rect {
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

//! "Pick up where you left off?"
//!
//! Shown at startup when the last run left something open: agents in shells,
//! and editor windows. It is a question and not a notification. Resuming a
//! conversation reads it back, may act on it, and costs money; reopening five
//! windows puts five windows on your screen. So it lists exactly what it would
//! bring back, every row ticked, and waits — untick the one you do not want.
//!
//! One list for both kinds, on purpose. "Where you left off" is a single
//! question, and the person answering it wants the TimePulse agent and the
//! GreenPulse window and none of the rest — not two prompts that each know
//! half of what was there.

use crate::app::Pending;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Draw the question. Returns the interior rect of the list of agents.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    pending: &[Pending],
    selected: usize,
    theme: &Theme,
) -> Rect {
    let width = area.width.saturating_sub(6).clamp(46, 92).min(area.width);
    // Border, headline, blank, footer hint: five rows that are not the list.
    let height = (pending.len() as u16 + 6).min(area.height);
    let popup = centred(area, width, height);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .title(Span::styled(" Resume? ", theme.border(true)))
        .title_bottom(Span::styled(
            " y resume \u{b7} n not now \u{b7} space pick ",
            theme.border(false),
        ))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height < 3 {
        return inner;
    }

    let agents = pending.iter().filter(|p| !p.is_window()).count();
    let windows = pending.len() - agents;
    let count = |n: usize, one: &str, many: &str| match n {
        1 => format!("1 {one}"),
        n => format!("{n} {many}"),
    };
    let headline = match (agents, windows) {
        (a, 0) => format!(
            " {} going when dmac last exited.",
            count(a, "conversation was", "conversations were")
        ),
        (0, w) => format!(
            " {} open when dmac last exited.",
            count(w, "window was", "windows were")
        ),
        (a, w) => format!(
            " {} and {} open when dmac last exited.",
            count(a, "conversation", "conversations"),
            count(w, "window", "windows")
        ),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            headline,
            Style::default().fg(theme.panel_fg),
        ))),
        Rect { height: 1, ..inner },
    );

    let list = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    let lines: Vec<Line> = pending
        .iter()
        .enumerate()
        .take(list.height as usize)
        .map(|(i, p)| row(p, i == selected, list.width as usize, theme))
        .collect();
    frame.render_widget(Paragraph::new(lines), list);
    list
}

fn row<'a>(p: &'a Pending, is_selected: bool, width: usize, theme: &Theme) -> Line<'a> {
    let base = if is_selected {
        theme.cursor()
    } else {
        Style::default().fg(theme.panel_fg).bg(theme.panel_bg)
    };
    // The tick is the answer for this row. Shown as a box because that is what
    // it is: something you can change before saying yes to the lot.
    let mark = if p.chosen { "[x]" } else { "[ ]" };

    // An agent row shows the command that will run and the first eight
    // characters of its id — enough to tell two apart, short enough to leave
    // room. A window row shows the folder, marked as a window so the two kinds
    // read differently at a glance.
    let (body_text, tail) = match &p.what {
        crate::app::PendingWhat::Agent {
            command,
            conversation,
            ..
        } => {
            let short: String = conversation.chars().take(8).collect();
            (command.as_str(), format!("{short} "))
        }
        crate::app::PendingWhat::Window { dir } => (dir.as_str(), "window ".to_string()),
    };
    let text = format!(
        " {mark} {:<10} {}",
        truncate(&p.session_name, 10),
        body_text
    );

    let room = width.saturating_sub(tail.chars().count());
    let body = truncate(&text, room);
    let pad = room.saturating_sub(body.chars().count());

    Line::from(vec![
        Span::styled(body, base),
        Span::styled(" ".repeat(pad), base),
        Span::styled(tail, base.fg(theme.status_fg).add_modifier(Modifier::DIM)),
    ])
}

fn truncate(s: &str, room: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= room {
        return s.to_string();
    }
    if room < 2 {
        return String::new();
    }
    let head: String = chars[..room - 1].iter().collect();
    format!("{head}\u{2026}")
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
    fn long_text_is_cut_with_an_ellipsis() {
        assert_eq!(truncate("abcdef", 4), "abc\u{2026}");
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("abc", 1), "");
    }
}

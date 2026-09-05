//! The session rail down the left edge.
//!
//! Two states, both always present:
//!
//! - **Collapsed** — three columns of coloured dots. You can always see how many
//!   sessions you have and which one you are in, without opening anything.
//! - **Expanded** — name, position, and what each session is looking at.
//!
//! It *pushes* the panels rather than covering them. Two reasons: you can see
//! both at once, and a file can eventually be dragged from a panel onto a session
//! in the rail, which is the most natural way to reach the cross-session
//! clipboard.

use crate::theme::Theme;
use dmac_session::{SessionColor, SessionManager};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

pub const COLLAPSED_WIDTH: u16 = 3;
pub const EXPANDED_WIDTH: u16 = 22;

/// Rows one session occupies when expanded: name, path, blank.
const ROWS_PER_SESSION: usize = 3;

/// How wide the rail should be. Never more than a third of the screen — the
/// panels are the point of the application.
pub fn width(open: bool, total: u16) -> u16 {
    let want = if open {
        EXPANDED_WIDTH
    } else {
        COLLAPSED_WIDTH
    };
    want.min(total / 3)
}

fn colour(c: SessionColor) -> Color {
    match c {
        SessionColor::Cyan => Color::Cyan,
        SessionColor::Green => Color::LightGreen,
        SessionColor::Yellow => Color::Yellow,
        SessionColor::Magenta => Color::LightMagenta,
        SessionColor::Blue => Color::LightBlue,
        SessionColor::Red => Color::LightRed,
    }
}

/// Which session a click at `row` selects, accounting for scrolling.
pub fn session_at_row(
    sessions: &SessionManager,
    area: Rect,
    open: bool,
    row: u16,
) -> Option<usize> {
    if row < area.y || row >= area.y + area.height {
        return None;
    }
    let offset = scroll_offset(sessions, area.height, open);
    let local = (row - area.y) as usize;
    let index = if open {
        offset + local / ROWS_PER_SESSION
    } else {
        offset + local
    };
    (index < sessions.len()).then_some(index)
}

/// First visible session, chosen so the current one is always on screen.
fn scroll_offset(sessions: &SessionManager, height: u16, open: bool) -> usize {
    let per = if open { ROWS_PER_SESSION } else { 1 };
    let visible = (height as usize / per).max(1);
    let current = sessions.current_index();
    if current < visible {
        0
    } else {
        // Keep the current session on the last visible row rather than centring:
        // the list usually grows downwards and this keeps earlier entries stable.
        current + 1 - visible
    }
}

/// `highlight` is the row the keyboard is on while the rail is being driven,
/// which is not the same as the session on screen — you can move the cursor
/// over a session without switching to it, exactly as in any list.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    sessions: &SessionManager,
    open: bool,
    highlight: Option<usize>,
    theme: &Theme,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }

    let block = Block::default()
        .borders(if open { Borders::RIGHT } else { Borders::NONE })
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(theme.panel_border).bg(theme.panel_bg))
        .style(theme.panel());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return inner;
    }

    let offset = scroll_offset(sessions, inner.height, open);
    let current = sessions.current_index();
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);

    for (i, session) in sessions.all().iter().enumerate().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let is_current = i == current;
        // Filled for the session you are in, hollow for the others — legible even
        // when the terminal has no colour at all.
        let dot = if is_current { '\u{25CF}' } else { '\u{25CB}' };
        let dot_style = Style::default()
            .fg(colour(session.color))
            .bg(theme.panel_bg);

        if !open {
            lines.push(Line::from(vec![
                Span::styled(" ", Style::default().bg(theme.panel_bg)),
                Span::styled(dot.to_string(), dot_style),
            ]));
            continue;
        }

        let on_cursor = highlight == Some(i);
        let name_style = if on_cursor {
            theme.cursor()
        } else if is_current {
            Style::default()
                .fg(theme.selected_fg)
                .bg(theme.panel_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.panel_fg).bg(theme.panel_bg)
        };

        // The number is the Alt-N shortcut. Showing it is how anyone learns the
        // shortcut exists.
        let number = format!("{}", i + 1);
        let room = (inner.width as usize).saturating_sub(4 + number.width());
        lines.push(Line::from(vec![
            Span::styled(" ", Style::default().bg(theme.panel_bg)),
            Span::styled(dot.to_string(), dot_style),
            Span::styled(" ", Style::default().bg(theme.panel_bg)),
            Span::styled(fit(&session.name, room), name_style),
            Span::styled(
                format!(" {number} "),
                Style::default().fg(theme.status_fg).bg(theme.panel_bg),
            ),
        ]));

        if lines.len() < inner.height as usize {
            lines.push(Line::from(Span::styled(
                format!(
                    "   {}",
                    fit_end(
                        &session.subtitle(),
                        (inner.width as usize).saturating_sub(3)
                    )
                ),
                Style::default().fg(theme.status_fg).bg(theme.panel_bg),
            )));
        }
        if lines.len() < inner.height as usize {
            lines.push(Line::from(""));
        }
    }

    frame.render_widget(Paragraph::new(lines).style(theme.panel()), inner);
    inner
}

/// Clip from the end, for names.
fn fit(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('\u{2026}');
    out
}

/// Clip from the front, for paths: the tail of a path is the informative half.
fn fit_end(s: &str, max: usize) -> String {
    if s.width() <= max || max < 2 {
        return s.chars().take(max).collect();
    }
    let tail: String = s
        .chars()
        .rev()
        .take(max - 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("\u{2026}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use dmac_vfs::VfsPath;

    fn mgr(n: usize) -> SessionManager {
        let mut m = SessionManager::new("first", VfsPath::local("/a"), VfsPath::local("/b"));
        for i in 1..n {
            m.create(format!("s{i}"), VfsPath::local("/a"), VfsPath::local("/b"));
        }
        m
    }

    #[test]
    fn the_rail_never_takes_more_than_a_third_of_the_screen() {
        assert!(width(true, 30) <= 10);
        assert_eq!(width(true, 200), EXPANDED_WIDTH);
        assert_eq!(width(false, 200), COLLAPSED_WIDTH);
    }

    /// With more sessions than fit, the current one must still be visible —
    /// otherwise the rail stops telling you where you are.
    #[test]
    fn the_current_session_is_always_scrolled_into_view() {
        let mut m = mgr(20);
        let area = Rect {
            x: 0,
            y: 0,
            width: EXPANDED_WIDTH,
            height: 12,
        };
        for target in [0usize, 5, 12, 19] {
            m.switch_to(target);
            let offset = scroll_offset(&m, area.height, true);
            let visible = area.height as usize / ROWS_PER_SESSION;
            assert!(
                (offset..offset + visible).contains(&target),
                "session {target} not visible (offset {offset}, visible {visible})"
            );
        }
    }

    #[test]
    fn a_click_maps_back_to_the_session_it_landed_on() {
        let m = mgr(4);
        let area = Rect {
            x: 0,
            y: 0,
            width: EXPANDED_WIDTH,
            height: 12,
        };
        assert_eq!(session_at_row(&m, area, true, 0), Some(0));
        assert_eq!(session_at_row(&m, area, true, 3), Some(1));
        assert_eq!(session_at_row(&m, area, true, 6), Some(2));
    }

    #[test]
    fn clicking_past_the_last_session_selects_nothing() {
        let m = mgr(2);
        let area = Rect {
            x: 0,
            y: 0,
            width: EXPANDED_WIDTH,
            height: 30,
        };
        assert_eq!(session_at_row(&m, area, true, 27), None);
        assert_eq!(session_at_row(&m, area, true, 99), None);
    }

    /// The keyboard highlight is independent of which session is on screen:
    /// you move over a session before deciding to switch to it.
    #[test]
    fn the_highlight_is_independent_of_the_live_session() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        // `create` switches to the new session, so start from a known place.
        let mut m = mgr(4);
        m.switch_to(0);
        let theme = Theme::norton();
        let mut term = Terminal::new(TestBackend::new(EXPANDED_WIDTH, 14)).unwrap();
        term.draw(|f| {
            let a = Rect {
                x: 0,
                y: 0,
                width: EXPANDED_WIDTH,
                height: 14,
            };
            draw(f, a, &m, true, Some(3), &theme);
        })
        .unwrap();
        assert_eq!(m.current_index(), 0, "drawing must not switch sessions");
    }

    #[test]
    fn the_collapsed_rail_is_one_row_per_session() {
        let m = mgr(4);
        let area = Rect {
            x: 0,
            y: 0,
            width: COLLAPSED_WIDTH,
            height: 10,
        };
        assert_eq!(session_at_row(&m, area, false, 0), Some(0));
        assert_eq!(session_at_row(&m, area, false, 2), Some(2));
    }

    #[test]
    fn paths_are_clipped_from_the_front_and_names_from_the_end() {
        assert!(fit_end("/very/long/path/to/project", 10).starts_with('\u{2026}'));
        assert!(fit("a-very-long-session-name", 10).ends_with('\u{2026}'));
        assert!(fit_end("/very/long/path/to/project", 10).width() <= 10);
        assert!(fit("a-very-long-session-name", 10).width() <= 10);
    }

    #[test]
    fn drawing_survives_every_size_including_none_at_all() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let m = mgr(6);
        let theme = Theme::norton();
        for (w, h) in [(0u16, 0u16), (1, 1), (3, 4), (22, 20), (22, 2)] {
            let mut term = Terminal::new(TestBackend::new(w.max(1), h.max(1))).unwrap();
            term.draw(|f| {
                let a = Rect {
                    x: 0,
                    y: 0,
                    width: w,
                    height: h,
                };
                draw(f, a, &m, true, Some(2), &theme);
                draw(f, a, &m, false, None, &theme);
            })
            .unwrap_or_else(|e| panic!("rail failed at {w}x{h}: {e}"));
        }
    }
}

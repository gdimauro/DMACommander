//! The session rail down the left edge.
//!
//! Two widths, both always present and both the user's to set:
//!
//! - **Resting** — three columns of coloured dots by default. You can always see
//!   how many sessions you have and which one you are in, without opening
//!   anything.
//! - **Opened** — name, position, and what each session is looking at.
//!
//! What is *drawn* follows from the width alone, never from which of the two is
//! in effect: widen the resting strip past [`DETAILED_FROM`] and it shows names
//! all the time, which is how you ask to keep the sessions in sight.
//!
//! It *pushes* the panels rather than covering them. Two reasons: you can see
//! both at once, and a file can eventually be dragged from a panel onto a session
//! in the rail, which is the most natural way to reach the cross-session
//! clipboard.

use crate::theme::Theme;
use dmac_session::{RailWidths, SessionColor, SessionManager};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Rows one session occupies when it is shown in detail: name, path, blank.
const ROWS_PER_SESSION: usize = 3;

/// The narrowest a rail can be and still fit a dot, a space and a one-letter
/// name. Under this there is no room for anything but the dots, so the rail
/// shows dots however wide it has been made.
pub const DETAILED_FROM: u16 = 8;

/// How wide the rail should be, given the pair of widths the user has set.
/// Never more than a third of the screen — the panels are the point of the
/// application, and a rail that could eat them is a rail that eventually will.
pub fn width(open: bool, widths: RailWidths, total: u16) -> u16 {
    let want = if open {
        widths.expanded
    } else {
        widths.collapsed
    };
    want.min(total / 3)
}

/// The widest the user is allowed to drag it, for the same reason.
pub fn max_width(total: u16) -> u16 {
    total / 3
}

/// Whether a rail this wide has room for names and paths, or only for dots.
///
/// A property of the width and not of the open flag, so widening the resting
/// strip is all it takes to see the sessions all the time — which is the whole
/// reason for being able to widen it.
pub fn detailed(width: u16) -> bool {
    width >= DETAILED_FROM
}

/// The colour a session answers to, here and anywhere else that names one.
///
/// Shared rather than looked up twice: the shell's border carries the session
/// name in this colour, and a second copy of the mapping is a second thing to
/// remember to change.
pub(crate) fn colour(c: SessionColor) -> Color {
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
///
/// `detail` is [`detailed`] of the rail's own width: how tall each entry is
/// depends on how much of it is being shown, and a click has to be read with
/// the same arithmetic that drew it.
pub fn session_at_row(
    sessions: &SessionManager,
    rows: &[usize],
    area: Rect,
    detail: bool,
    row: u16,
) -> Option<usize> {
    if row < area.y || row >= area.y + area.height {
        return None;
    }
    let offset = scroll_offset(sessions, rows, area.height, detail);
    let local = (row - area.y) as usize;
    let nth = if detail {
        offset + local / ROWS_PER_SESSION
    } else {
        offset + local
    };
    // Through the visible list, never straight into the session list: a folded
    // group takes rows away, and a click read against the unfolded list picks
    // whatever has slid up into that row instead.
    rows.get(nth).copied()
}

/// First visible session, chosen so the current one is always on screen.
fn scroll_offset(sessions: &SessionManager, rows: &[usize], height: u16, detail: bool) -> usize {
    let per = if detail { ROWS_PER_SESSION } else { 1 };
    let visible = (height as usize / per).max(1);
    // Counted in drawn rows, so a folded group scrolls as the one row it is —
    // and so a search scrolls through what it found.
    let current = rows
        .iter()
        .position(|&i| i == sessions.current_index())
        .unwrap_or(0);
    if current < visible {
        0
    } else {
        // Keep the current session on the last visible row rather than centring:
        // the list usually grows downwards and this keeps earlier entries stable.
        current + 1 - visible
    }
}

/// An open search over the sessions: what is being typed, and a way to ask
/// which characters of a given session's name it hit.
///
/// A pair rather than two parameters because they are meaningless apart — a
/// needle with no way to ask where it landed cannot highlight anything, and
/// positions with no needle have nothing to label the box with. `None` means no
/// search is open, and then the box is not drawn at all rather than drawn
/// empty: an empty box in a list of sessions looks like a session with no name.
pub struct Search<'a> {
    pub needle: &'a str,
    pub hit: &'a dyn Fn(usize) -> Vec<usize>,
}

/// `highlight` is the row the keyboard is on while the rail is being driven,
/// which is not the same as the session on screen — you can move the cursor
/// over a session without switching to it, exactly as in any list.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    sessions: &SessionManager,
    rows: &[usize],
    highlight: Option<usize>,
    search: Option<Search<'_>>,
    theme: &Theme,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return area;
    }

    // Detail is decided by the room there is, not by whether the list was
    // opened: a resting strip the user has widened shows names, and an opened
    // one squeezed by a narrow terminal falls back to dots rather than to
    // clipped nonsense.
    let detail = detailed(area.width);

    let block = Block::default()
        .borders(if detail {
            Borders::RIGHT
        } else {
            Borders::NONE
        })
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(theme.panel_border).bg(theme.panel_bg))
        .style(theme.panel());
    // On the border rather than as a row: a search box that takes a line takes
    // it from the sessions, which is the thing being searched.
    let block = match (detail, search.as_ref()) {
        (true, Some(s)) => block.title_bottom(Span::styled(
            format!(" / {}\u{2588} ", s.needle),
            Style::default()
                .fg(theme.selected_fg)
                .bg(theme.panel_bg)
                .add_modifier(Modifier::BOLD),
        )),
        _ => block,
    };
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return inner;
    }

    let offset = scroll_offset(sessions, rows, inner.height, detail);
    let current = sessions.current_index();
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);

    for i in rows.iter().copied().skip(offset) {
        if lines.len() >= inner.height as usize {
            break;
        }
        let Some(session) = sessions.get(i) else {
            continue;
        };
        // A group reads as a group at a glance: the marker says whether it is
        // open, the indent says what belongs to it. Both are one column, which
        // is all a rail this narrow can spare — and the marker is a character
        // rather than a colour, so it survives a terminal that has none.
        let nested = sessions.depth(i) == 1;
        let marker = match (sessions.has_children(i), session.collapsed) {
            (true, true) => '\u{25B8}',
            (true, false) => '\u{25BE}',
            (false, _) => ' ',
        };
        let is_current = i == current;
        // Filled for the session you are in, hollow for the others — legible even
        // when the terminal has no colour at all.
        let dot = if is_current { '\u{25CF}' } else { '\u{25CB}' };
        let dot_style = Style::default()
            .fg(colour(session.color))
            .bg(theme.panel_bg);

        if !detail {
            // Even at three columns the shape of the group survives: a child's
            // dot sits one over, which is the whole of what the resting strip
            // has room to say.
            lines.push(Line::from(vec![
                Span::styled(
                    if nested { "  " } else { " " },
                    Style::default().bg(theme.panel_bg),
                ),
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
        let lead = if nested {
            format!("{marker} \u{2514}")
        } else {
            format!("{marker} ")
        };
        let room = (inner.width as usize).saturating_sub(3 + lead.width() + number.width());
        let mut spans = vec![
            Span::styled(
                lead,
                Style::default().fg(theme.status_fg).bg(theme.panel_bg),
            ),
            Span::styled(dot.to_string(), dot_style),
            Span::styled(" ", Style::default().bg(theme.panel_bg)),
        ];
        // The letters the search actually matched, picked out inside the name.
        // Highlighting the whole row would say *that* it matched and hide
        // *where*, which is the half you are looking for when two sessions are
        // called almost the same thing.
        let hits = search.as_ref().map(|s| (s.hit)(i)).unwrap_or_default();
        let shown = fit(&session.name, room);
        spans.extend(name_spans(&shown, &hits, name_style, theme));
        spans.push(Span::styled(
            format!(" {number} "),
            Style::default().fg(theme.status_fg).bg(theme.panel_bg),
        ));
        lines.push(Line::from(spans));

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

/// A name, split so the characters a search matched can be picked out.
///
/// `hits` are character positions in the *whole* name; the name drawn here may
/// have been clipped, so anything past its end is dropped rather than shifted —
/// a highlight on the wrong letter is worse than none.
///
/// Positions are characters and the slicing is by byte, so each span is cut at
/// a boundary the string actually has. A session called `résumé` is why.
fn name_spans(name: &str, hits: &[usize], plain: Style, theme: &Theme) -> Vec<Span<'static>> {
    if hits.is_empty() {
        return vec![Span::styled(name.to_string(), plain)];
    }
    let lit = Style::default()
        .fg(theme.selected_fg)
        .bg(theme.panel_bg)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_lit = false;
    for (i, c) in name.chars().enumerate() {
        let on = hits.contains(&i);
        if on != run_lit && !run.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut run),
                if run_lit { lit } else { plain },
            ));
        }
        run_lit = on;
        run.push(c);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, if run_lit { lit } else { plain }));
    }
    spans
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

    /// The default pair, named so the tests read the way the rail does.
    const COLLAPSED_WIDTH: u16 = 3;
    const EXPANDED_WIDTH: u16 = 22;

    fn widths() -> RailWidths {
        RailWidths::default()
    }

    #[test]
    fn the_rail_never_takes_more_than_a_third_of_the_screen() {
        assert!(width(true, widths(), 30) <= 10);
        assert_eq!(width(true, widths(), 200), EXPANDED_WIDTH);
        assert_eq!(width(false, widths(), 200), COLLAPSED_WIDTH);
    }

    /// The two widths are separate settings, and neither may move the other:
    /// they answer different questions and are set at different moments.
    #[test]
    fn the_resting_and_opened_widths_are_independent() {
        let w = RailWidths {
            collapsed: 14,
            expanded: 30,
        };
        assert_eq!(width(false, w, 200), 14);
        assert_eq!(width(true, w, 200), 30);
        // Still a third of the screen at most, whatever was asked for.
        assert_eq!(width(true, w, 60), 20);
    }

    /// What the rail draws follows from how wide it is, never from whether it
    /// was opened — which is what makes widening the resting strip a way to
    /// keep the session names in sight.
    #[test]
    fn a_wide_enough_rail_shows_names_whether_or_not_it_was_opened() {
        assert!(!detailed(COLLAPSED_WIDTH), "a strip of dots has no room");
        assert!(detailed(DETAILED_FROM));
        assert!(detailed(EXPANDED_WIDTH));
        // An opened rail squeezed by a narrow terminal falls back to dots
        // rather than to clipped nonsense.
        assert!(!detailed(width(true, widths(), 18)));
    }

    /// A rail set to nothing is nothing: someone who wants the columns back
    /// can have them, and Ctrl-T still opens the list.
    #[test]
    fn a_resting_width_of_zero_hides_it_entirely() {
        let w = RailWidths {
            collapsed: 0,
            ..RailWidths::default()
        };
        assert_eq!(width(false, w, 200), 0);
        assert_eq!(width(true, w, 200), EXPANDED_WIDTH);
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
            let offset = scroll_offset(&m, &m.visible(), area.height, true);
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
        assert_eq!(session_at_row(&m, &m.visible(), area, true, 0), Some(0));
        assert_eq!(session_at_row(&m, &m.visible(), area, true, 3), Some(1));
        assert_eq!(session_at_row(&m, &m.visible(), area, true, 6), Some(2));
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
        assert_eq!(session_at_row(&m, &m.visible(), area, true, 27), None);
        assert_eq!(session_at_row(&m, &m.visible(), area, true, 99), None);
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
            draw(f, a, &m, &m.visible(), Some(3), None, &theme);
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
        assert_eq!(session_at_row(&m, &m.visible(), area, false, 0), Some(0));
        assert_eq!(session_at_row(&m, &m.visible(), area, false, 2), Some(2));
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
                draw(f, a, &m, &m.visible(), Some(2), None, &theme);
                draw(f, a, &m, &m.visible(), None, None, &theme);
            })
            .unwrap_or_else(|e| panic!("rail failed at {w}x{h}: {e}"));
        }
    }
}

//! A single-line text prompt.
//!
//! Deliberately small and general: renaming a session is the first user, but
//! F7 (make directory) and F6 (rename a file) want exactly the same thing, and
//! duplicating this for each would be three chances to get the editing keys
//! subtly different.

use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

pub fn draw(frame: &mut Frame, area: Rect, title: &str, value: &str, theme: &Theme) {
    let width = 44.min(area.width);
    let height = 3.min(area.height);
    if width < 8 || height < 3 {
        return;
    }

    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .title(Span::styled(
            title,
            theme.border(true).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(" enter ok · esc cancel ", theme.border(false)))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.width == 0 {
        return;
    }

    // Scroll the text so the end stays visible: a prompt that hides what you are
    // currently typing is worse than one that hides the beginning.
    let room = inner.width as usize;
    let shown = clip_start(value, room);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            shown.clone(),
            Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
        ))),
        inner,
    );

    let cursor_x = inner
        .x
        .saturating_add(shown.width().min(room.saturating_sub(1)) as u16);
    frame.set_cursor_position((cursor_x, inner.y));
}

/// Keep the tail of the string, dropping from the front when it does not fit.
fn clip_start(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    // One column is reserved for the cursor to sit in.
    let room = max.saturating_sub(1);
    if s.width() <= room {
        return s.to_string();
    }
    let mut out: Vec<char> = Vec::new();
    let mut w = 0;
    for c in s.chars().rev() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > room {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.reverse();
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_value_is_shown_whole() {
        assert_eq!(clip_start("build", 20), "build");
    }

    /// The end is what you are typing, so the front is what gets dropped.
    #[test]
    fn a_long_value_keeps_its_tail() {
        let s = "a-very-long-session-name-indeed";
        let out = clip_start(s, 10);
        assert!(s.ends_with(&out), "kept {out:?} of {s:?}");
        assert!(out.width() <= 9);
    }

    #[test]
    fn wide_characters_are_counted_in_columns() {
        let out = clip_start("日本語のセッション", 9);
        assert!(out.width() <= 8, "got width {}", out.width());
    }

    #[test]
    fn a_zero_width_prompt_does_not_panic() {
        assert_eq!(clip_start("anything", 0), "");
        assert_eq!(clip_start("anything", 1), "");
    }

    #[test]
    fn it_renders_at_every_size() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let theme = Theme::norton();
        for (w, h) in [(1u16, 1u16), (10, 3), (80, 24)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, f.area(), " Rename ", "value", &theme))
                .unwrap_or_else(|e| panic!("prompt failed at {w}x{h}: {e}"));
        }
    }
}

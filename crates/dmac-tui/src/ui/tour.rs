//! The tour's overlay: the caption, the key caps, and the pointer.
//!
//! Drawn last, over whatever the program is showing, because the program is
//! the demonstration and the overlay is the narration. It stays out of the
//! middle of the screen for the same reason.

use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

/// What the overlay needs to know, and nothing about how the tour runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub title: &'static str,
    pub caption: &'static str,
    /// The key caps to light, in the order pressed.
    pub caps: Vec<String>,
    pub step: usize,
    pub of: usize,
    /// Where a click is about to land, if this is a click.
    pub pointer: Option<(u16, u16)>,
}

/// Draw the overlay. Returns the box's rect, for anything that wants to avoid
/// drawing under it.
pub fn draw(frame: &mut Frame, area: Rect, view: &View, theme: &Theme) -> Rect {
    // Bottom right, clear of the panels' top halves where the action mostly
    // is, and never more than half the width — the point is to watch the
    // program, not the box.
    let width = (area.width / 2).clamp(30, 64).min(area.width);
    let inner_w = width.saturating_sub(4) as usize;
    let caption_lines = wrapped_height(view.caption, inner_w);
    // Border, caption, blank, caps strip, border.
    let height = (caption_lines as u16 + 4).min(area.height);
    let popup = Rect {
        x: area.x + area.width.saturating_sub(width) - 1.min(area.width.saturating_sub(width)),
        y: area.y + area.height.saturating_sub(height + 2),
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let title = format!(
        " Tour \u{b7} {} \u{b7} {}/{} ",
        view.title, view.step, view.of
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(title, theme.border(true)))
        .title_bottom(Span::styled(" Esc stops ", theme.border(false)))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height == 0 || inner.width == 0 {
        return popup;
    }

    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        height: inner.height.saturating_sub(2).max(1),
        ..inner
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            view.caption,
            Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
        )))
        .wrap(Wrap { trim: true }),
        text,
    );

    // The key caps, drawn as caps: a lit box per key, joined with `+`. This is
    // the part that makes a tour something you can follow rather than
    // something that happens to you — you see the key before its effect.
    if inner.height >= 2 {
        let strip = Rect {
            x: inner.x + 1,
            y: inner.y + inner.height - 1,
            width: inner.width.saturating_sub(2),
            height: 1,
        };
        let cap = theme.cursor().add_modifier(Modifier::BOLD);
        let plain = Style::default().fg(theme.status_fg).bg(theme.panel_bg);
        let mut spans: Vec<Span> = Vec::new();
        for (i, c) in view.caps.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" + ", plain));
            }
            spans.push(Span::styled(format!(" {c} "), cap));
        }
        if spans.is_empty() {
            spans.push(Span::styled("", plain));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), strip);
    }

    // The pointer, where the click is about to land: the cell inverted and a
    // cross on it, big enough to be seen arriving.
    if let Some((x, y)) = view.pointer
        && x >= area.x
        && x < area.x + area.width
        && y >= area.y
        && y < area.y + area.height
    {
        let buf = frame.buffer_mut();
        buf[(x, y)]
            .set_char('\u{271a}')
            .set_style(theme.cursor().add_modifier(Modifier::BOLD));
    }
    popup
}

/// The tours as a menu: one row each, with a letter to press.
pub fn items() -> Vec<crate::ui::menu::Item<'static>> {
    crate::tour::SCENARIOS
        .iter()
        .enumerate()
        .map(|(i, s)| crate::ui::menu::Item::new(s.name, key_of(i)))
        .collect()
}

/// The letter that picks tour `i` in the menu.
pub fn key_of(i: usize) -> &'static str {
    const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz";
    LETTERS.get(i..i + 1).unwrap_or("")
}

/// Which tour a letter picks, if any.
pub fn accelerator(c: char) -> Option<usize> {
    let i = (c as u32 as usize).checked_sub('a' as u32 as usize)?;
    (i < crate::tour::SCENARIOS.len()).then_some(i)
}

/// How many lines `text` takes when wrapped to `width`, for sizing the box.
fn wrapped_height(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut lines = 1;
    let mut col = 0;
    for word in text.split_whitespace() {
        let w = word.chars().count();
        if col > 0 && col + 1 + w > width {
            lines += 1;
            col = w;
        } else {
            col += if col > 0 { 1 + w } else { w };
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn view() -> View {
        View {
            title: "The panels",
            caption: "Tab switches to the other panel. Watch the border.",
            caps: vec!["Ctrl".into(), "T".into()],
            step: 3,
            of: 11,
            pointer: Some((36, 23)),
        }
    }

    fn text_of(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let a = buf.area;
        let mut s = String::new();
        for y in 0..a.height {
            for x in 0..a.width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    /// The caption, the step count and every key cap are on screen, and the
    /// pointer is drawn where the click is going.
    #[test]
    fn the_overlay_shows_the_caption_the_caps_and_the_pointer() {
        let theme = Theme::default();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            draw(f, f.area(), &view(), &theme);
        })
        .unwrap();
        let s = text_of(&term);
        assert!(s.contains("Tab switches"), "{s}");
        assert!(s.contains("3/11"), "{s}");
        assert!(
            s.contains(" Ctrl ") && s.contains(" T "),
            "the caps are missing: {s}"
        );
        assert!(s.contains("Esc stops"), "{s}");
        assert_eq!(term.backend().buffer()[(36, 23)].symbol(), "\u{271a}");
    }

    /// Small terminals get a smaller box, not a panic, and the box never
    /// claims more than the screen has.
    #[test]
    fn a_small_screen_gets_a_small_box() {
        let theme = Theme::default();
        for (w, h) in [(40, 12), (30, 8), (20, 5), (10, 3)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let r = draw(f, f.area(), &view(), &theme);
                assert!(r.width <= w && r.height <= h, "{w}x{h}: {r:?}");
            })
            .unwrap();
        }
    }

    /// The menu lists every tour, each with the letter that picks it, and
    /// the letter picks the same tour back.
    #[test]
    fn the_menu_lists_every_tour_and_its_letter_picks_it() {
        let items = items();
        assert_eq!(items.len(), crate::tour::SCENARIOS.len());
        for (i, (item, s)) in items.iter().zip(crate::tour::SCENARIOS).enumerate() {
            assert_eq!(item.label, s.name);
            assert_eq!(item.hint, key_of(i));
            let c = key_of(i).chars().next().expect("a letter");
            assert_eq!(accelerator(c), Some(i));
        }
        assert_eq!(accelerator('z'), None);
        assert_eq!(accelerator('1'), None);
    }

    #[test]
    fn wrapping_counts_lines_the_way_the_widget_wraps() {
        assert_eq!(wrapped_height("one two three", 40), 1);
        assert_eq!(wrapped_height("one two three", 7), 2);
        assert_eq!(wrapped_height("one two three", 3), 3);
        assert_eq!(wrapped_height("", 10), 1);
    }
}

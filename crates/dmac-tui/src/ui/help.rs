//! The help page, drawn over the panels.
//!
//! A scrolling box of every key, from the table in [`crate::help`] — the same
//! one the `cannon` screensaver shoots at. Nothing here decides what a key
//! does; that is the keymap. Nothing here is typed into; it is read, scrolled,
//! and closed.

use crate::help::{SECTIONS, key_width};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Wider than this and a line of prose is a long way for the eye to travel.
const MAX_WIDTH: u16 = 78;

/// One rendered line, before colour.
enum Entry {
    Blank,
    Title(&'static str),
    /// A key and one line of its description. Continuation lines of a wrapped
    /// description carry an empty key.
    Row {
        keys: &'static str,
        prose: String,
        /// What this row's key does, carried on *every* line of a wrapped
        /// description rather than only the first: someone clicking a row aimed
        /// at the row, not at its top line.
        actions: &'static [crate::action::Action],
    },
}

/// Lay the page out for an interior `width` columns wide.
fn layout(width: usize) -> Vec<Entry> {
    let key_w = key_width();
    // Two of indent, the key, a gap of two.
    let prose_w = width.saturating_sub(key_w + 4).max(12);
    let mut out = Vec::new();
    for (i, section) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push(Entry::Blank);
        }
        out.push(Entry::Title(section.title));
        for row in section.rows {
            for (j, prose) in wrap(row.what, prose_w).into_iter().enumerate() {
                out.push(Entry::Row {
                    keys: if j == 0 { row.keys } else { "" },
                    prose,
                    actions: row.actions,
                });
            }
        }
    }
    out
}

/// What the row at `line` does, if it does anything.
///
/// The help's rows already carry the actions their keys stand for — they are
/// data, so the page can be a list of things to press as well as a list of
/// things to read. That is worth most for exactly the keys this program cannot
/// promise: the ones a hosted program eats, and the ones a terminal cannot
/// spell. A note, or a title, or a blank, answers `None`.
///
/// The first action when a row lists several: those are the several spellings
/// of one key, and they mean the same thing.
pub fn action_at(width: usize, line: usize) -> Option<crate::action::Action> {
    match layout(width).get(line) {
        Some(Entry::Row { actions, .. }) => actions.first().cloned(),
        _ => None,
    }
}

/// How many lines the page takes at this width. The caller needs it to clamp
/// a scroll position without drawing.
pub fn page_len(width: usize) -> usize {
    layout(width).len()
}

/// Draw the page and return the interior rect, so the keys and the wheel can
/// clamp their scrolling to what is actually on screen.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    scroll: usize,
    // The visible row the pointer is over, if any. A row that does something
    // is lit under it, so the page reads as pressable where it is — and a
    // title or a note, which do nothing, stays as it is.
    hover: Option<usize>,
    theme: &Theme,
) -> Rect {
    let width = if area.width < 24 {
        area.width
    } else {
        area.width.saturating_sub(4).min(MAX_WIDTH)
    };
    let inner_w = width.saturating_sub(2) as usize;
    let entries = layout(inner_w);
    let total = entries.len();
    let height = if area.height < 5 {
        area.height
    } else {
        (total as u16 + 2).min(area.height.saturating_sub(2))
    };
    let popup = centred(area, width, height);

    // `Clear` is what stops the panels showing through the overlay.
    frame.render_widget(Clear, popup);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .title(Span::styled(" Help ", theme.border(true)))
        .title_bottom(Span::styled(
            " \u{2191}\u{2193} PgUp PgDn scroll \u{b7} esc close ",
            theme.border(false),
        ))
        .style(theme.panel());
    let inner = block.inner(popup);
    let visible = inner.height as usize;
    let max_scroll = total.saturating_sub(visible);
    let scroll = scroll.min(max_scroll);
    // Where you are, but only when there is somewhere else to be.
    if max_scroll > 0 && visible > 0 {
        let last = (scroll + visible).min(total);
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {}\u{2013}{last} of {total} ", scroll + 1),
                theme.border(false),
            ))
            .right_aligned(),
        );
    }
    frame.render_widget(block, popup);
    if inner.width == 0 || visible == 0 {
        return inner;
    }

    let key_w = key_width();
    let base = Style::default().fg(theme.panel_fg).bg(theme.panel_bg);
    let title = Style::default()
        .fg(theme.selected_fg)
        .bg(theme.panel_bg)
        .add_modifier(Modifier::BOLD);
    let key = base.add_modifier(Modifier::BOLD);
    let lit = theme.cursor();
    let lines: Vec<Line> = entries
        .iter()
        .skip(scroll)
        .take(visible)
        .enumerate()
        .map(|(i, e)| match e {
            Entry::Blank => Line::from(Span::styled("", base)),
            Entry::Title(t) => Line::from(Span::styled(format!(" {t}"), title)),
            Entry::Row {
                keys,
                prose,
                actions,
            } if hover == Some(i) && !actions.is_empty() => Line::from(vec![
                Span::styled(format!("  {keys:<key_w$}  "), lit),
                Span::styled(prose.clone(), lit),
            ]),
            Entry::Row { keys, prose, .. } => Line::from(vec![
                Span::styled(format!("  {keys:<key_w$}  "), key),
                Span::styled(prose.clone(), base),
            ]),
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
    inner
}

/// Break `text` into lines no wider than `width`, at spaces where it can and
/// mid-word where it must. Never returns nothing: an empty text is one empty
/// line, so a row always has a line to sit on.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut line_w = 0;
    for word in text.split(' ') {
        let mut word_w = word.chars().count();
        let mut word = word.to_string();
        // A word wider than the line: hard-cut what does not fit.
        while word_w > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                line_w = 0;
            }
            let head: String = word.chars().take(width).collect();
            word = word.chars().skip(width).collect();
            word_w -= width;
            lines.push(head);
        }
        if line_w == 0 {
            line = word;
            line_w = word_w;
        } else if line_w + 1 + word_w <= width {
            line.push(' ');
            line.push_str(&word);
            line_w += 1 + word_w;
        } else {
            lines.push(std::mem::take(&mut line));
            line = word;
            line_w = word_w;
        }
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
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
    fn wrapping_breaks_at_spaces_and_never_overflows() {
        let lines = wrap("the quick brown fox jumps over the lazy dog", 12);
        assert_eq!(
            lines,
            ["the quick", "brown fox", "jumps over", "the lazy dog"]
        );
        assert!(lines.iter().all(|l| l.chars().count() <= 12));
    }

    #[test]
    fn a_word_wider_than_the_line_is_cut_rather_than_lost() {
        let lines = wrap("docs/TERMINAL-KEYS.md ok", 8);
        assert_eq!(lines, ["docs/TER", "MINAL-KE", "YS.md ok"]);
    }

    #[test]
    fn an_empty_text_is_one_empty_line() {
        assert_eq!(wrap("", 10), [""]);
    }

    #[test]
    fn a_narrower_page_is_a_longer_page() {
        assert!(page_len(30) > page_len(76));
        assert!(page_len(76) > 40);
    }

    /// Every row of the page names its key on its first line only, so a
    /// wrapped description is not read as several bindings.
    #[test]
    fn a_wrapped_row_names_its_key_once() {
        let entries = layout(40);
        let mut prev_key: Option<&str> = None;
        for e in &entries {
            if let Entry::Row { keys, .. } = e {
                if keys.is_empty() {
                    assert!(
                        prev_key.is_some(),
                        "a continuation line with no row above it"
                    );
                }
                prev_key = Some(keys);
            } else {
                prev_key = None;
            }
        }
    }
}

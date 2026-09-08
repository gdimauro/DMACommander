//! One panel: a bordered list of entries with a name/size/date column layout.
//!
//! Rendering cost is O(visible rows). A panel holding a million entries draws in
//! the same time as one holding ten — [`dmac_core::Panel::visible`] hands us
//! only the slice on screen.

use crate::theme::Theme;
use dmac_core::{Entry, EntryKind, Panel, entry::format_size};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Fixed widths for the right-hand columns; the name column takes what is left.
const SIZE_W: usize = 8;
/// `dd/mm/yy hh:mm` is exactly 14 columns. Reserving fewer truncates every
/// dated row, which is subtle enough to ship by accident — the test below pins it.
const DATE_W: usize = 14;

/// The bottom-border text: how many entries, and — when any are marked — how
/// many, with how many files sit inside the marked directories.
///
/// The second number arrives from a task. Until it does the footer says `…`
/// rather than `0`: a zero there would be read as "the folders are empty",
/// which is the one thing it is not allowed to claim before it knows.
fn footer(panel: &Panel) -> String {
    let marked = panel.marked();
    if marked == 0 {
        return format!(" {} items ", panel.len());
    }
    let inside = match (panel.marked_dirs().is_empty(), panel.marked_files) {
        (true, _) => String::new(),
        (false, None) => " (\u{2026})".to_string(),
        (false, Some(n)) if n >= dmac_core::panel::MARKED_FILES_CAP => format!(" ({n}+ files)"),
        (false, Some(n)) => format!(" ({n} files)"),
    };
    format!(" {} items \u{b7} {marked} selected{inside} ", panel.len())
}

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    panel: &mut Panel,
    active: bool,
    focused: bool,
    bordered: bool,
    theme: &Theme,
) {
    // Unbordered, the title stays: without a box around it the path is the only
    // thing saying which directory these rows are, and losing it to save one
    // line would make full screen worse, not cleaner. The item count goes,
    // because the panel is a list and a list you can see the end of counts
    // itself.
    let block = Block::default()
        .borders(if bordered {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_type(BorderType::Plain)
        .border_style(theme.border(active))
        .title(Span::styled(
            // saturating: a drag-resize really can hand us a 1-cell-wide panel.
            format!(
                " {} ",
                truncate_middle(&panel.location, (area.width as usize).saturating_sub(4))
            ),
            theme.border(active),
        ))
        .style(theme.panel());
    let block = if bordered {
        block.title_bottom(Span::styled(footer(panel), theme.border(active)))
    } else {
        block
    };

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Tell the core how many rows fit *before* asking what is visible, so a
    // resize is reflected in this very frame instead of the next one.
    panel.set_viewport(inner.height as usize);
    let cursor = panel.cursor();
    let (start, rows) = panel.visible();

    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, e)| {
            render_row(
                e,
                start + i == cursor && active,
                focused,
                inner.width as usize,
                theme,
            )
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_row(
    e: &Entry,
    under_cursor: bool,
    focused: bool,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let style = match (under_cursor, focused) {
        (true, true) => theme.cursor(),
        (true, false) => theme.cursor_unfocused(),
        (false, _) => theme.entry(e),
    };

    let name_w = width.saturating_sub(SIZE_W + DATE_W + 2);
    let name = truncate_end(&e.name, name_w);

    let size = match e.kind {
        // A directory's byte count means nothing to a user; NC showed <DIR> and
        // it was the right call.
        EntryKind::Dir => "<DIR>".to_string(),
        EntryKind::Parent => "<UP>".to_string(),
        _ => e.size.map(format_size).unwrap_or_else(|| "?".into()),
    };

    let date = e
        .modified
        .and_then(format_mtime)
        .unwrap_or_else(|| " ".repeat(DATE_W));

    let text = format!(
        "{name:<name_w$} {size:>SIZE_W$} {date:>DATE_W$}",
        name = name,
        name_w = name_w,
        size = size,
        date = date,
    );

    // The cursor highlight must span the full row width, or it looks ragged on
    // short filenames.
    let padded = pad_to(&text, width);
    Line::from(Span::styled(padded, style))
}

/// `dd/mm/yy hh:mm`, trimmed to fit. Compact beats precise here: the user is
/// scanning, and a full ISO timestamp would eat the name column.
fn format_mtime(t: std::time::SystemTime) -> Option<String> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let ts = jiff::Timestamp::from_second(secs).ok()?;
    let zoned = ts.to_zoned(jiff::tz::TimeZone::system());
    Some(format!(
        "{:02}/{:02}/{:02} {:02}:{:02}",
        zoned.day(),
        zoned.month(),
        zoned.year() % 100,
        zoned.hour(),
        zoned.minute()
    ))
}

/// Truncate to a display width, counting grapheme columns rather than bytes.
/// Getting this wrong is how a filename with an emoji tears the panel border.
fn truncate_end(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
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
    out.push('~');
    out
}

/// Titles lose their middle, not their end — the directory you are in matters
/// more than the root you came from.
fn truncate_middle(s: &str, max: usize) -> String {
    if s.width() <= max || max < 5 {
        return s.to_string();
    }
    let keep = max - 1;
    let tail: String = s
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

fn pad_to(s: &str, width: usize) -> String {
    let w = s.width();
    if w >= width {
        truncate_end(s, width)
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_counts_columns_not_bytes() {
        // Each CJK glyph is two columns wide: four glyphs must not fit in 5.
        let s = "日本語です";
        let out = truncate_end(s, 5);
        assert!(out.width() <= 5, "got width {}", out.width());
    }

    #[test]
    fn emoji_do_not_overflow_the_row() {
        let s = "👨‍👩‍👧‍👦 family.txt";
        assert!(pad_to(s, 10).width() <= 10);
    }

    #[test]
    fn padding_reaches_exactly_the_requested_width() {
        assert_eq!(pad_to("ab", 6).width(), 6);
        assert_eq!(pad_to("", 4).width(), 4);
    }

    /// The date column must fit the format that produces it. A mismatch here
    /// silently truncates every single row.
    #[test]
    fn date_column_is_wide_enough_for_the_format_it_renders() {
        let rendered = format_mtime(std::time::SystemTime::now()).expect("format");
        assert_eq!(
            rendered.width(),
            DATE_W,
            "DATE_W must match the width of `dd/mm/yy hh:mm`"
        );
    }

    /// A row must consume its full width in columns and no more, or the panel
    /// border is overwritten.
    #[test]
    fn a_rendered_row_exactly_fills_the_width() {
        let theme = crate::theme::Theme::norton();
        let e = Entry {
            name: "Cargo.lock".into(),
            kind: EntryKind::File,
            size: Some(49_152),
            modified: Some(std::time::SystemTime::now()),
            mode: None,
            selected: false,
        };
        for width in [30usize, 46, 60, 120] {
            let line = render_row(&e, false, true, width, &theme);
            let w: usize = line.spans.iter().map(|s| s.content.width()).sum();
            assert_eq!(w, width, "row width mismatch at terminal width {width}");
        }
    }

    #[test]
    fn zero_width_target_does_not_panic() {
        assert_eq!(truncate_end("hello", 0), "");
        assert_eq!(pad_to("hello", 0), "");
    }
}

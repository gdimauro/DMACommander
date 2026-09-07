//! The file viewer: what F3 draws.
//!
//! Painting only. Which lines exist, which bytes they are and where a search
//! matched are all [`dmac_view::Document`]'s answers — this decides how much of
//! them fits and what colour they are, and nothing else. That split is what
//! lets the GPU backend show the same file without a second viewer.

use crate::theme::Theme;
use dmac_view::{Document, Kind};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Draw the document over `area`, returning the interior — which is what a
/// scroll has to be measured against, so it goes back to the caller rather than
/// being worked out twice.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    doc: &Document,
    scroll: usize,
    hex: bool,
    needle: &str,
    theme: &Theme,
) -> Rect {
    // The whole screen. A viewer that leaves the panels showing round the edges
    // is a viewer you have to look past to read the file.
    let name = doc
        .path()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    let mut title = format!(" {name} ");
    if doc.truncated() {
        // Said on the frame, not only in the status line: the status line is
        // one keypress from being replaced by something else, and "you are
        // looking at part of a file" must not be able to scroll away.
        title = format!(
            " {name} \u{2014} first {} bytes of {} ",
            dmac_view::READ_CAP,
            doc.total_bytes()
        );
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border(true))
        .title(Span::styled(title, theme.border(true)))
        .title_bottom(Span::styled(
            match hex || doc.kind() == Kind::Binary {
                true => " \u{2191}\u{2193} PgUp/PgDn \u{b7} / find \u{b7} n next \u{b7} h text \u{b7} Esc ",
                false => " \u{2191}\u{2193} PgUp/PgDn \u{b7} / find \u{b7} n next \u{b7} h hex \u{b7} Esc ",
            },
            theme.border(false),
        ))
        .style(theme.panel());
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return inner;
    }

    let rows = inner.height as usize;
    let lines: Vec<Line> = match hex || doc.kind() == Kind::Binary {
        true => (scroll..scroll + rows)
            .filter_map(|r| doc.hex_row(r))
            .map(|h| {
                Line::from(vec![
                    Span::styled(
                        format!("{:08x}  ", h.offset),
                        Style::default().fg(theme.status_fg).bg(theme.panel_bg),
                    ),
                    Span::styled(
                        h.hex(),
                        Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
                    ),
                    Span::styled(
                        format!(" {}", h.printable()),
                        Style::default().fg(theme.selected_fg).bg(theme.panel_bg),
                    ),
                ])
            })
            .collect(),
        false => doc
            .lines()
            .iter()
            .enumerate()
            .skip(scroll)
            .take(rows)
            .map(|(i, l)| text_line(i, l, needle, theme))
            .collect(),
    };

    frame.render_widget(Paragraph::new(lines).style(theme.panel()), inner);
    inner
}

/// One line of text, with the search term picked out of it.
///
/// The match is highlighted rather than the whole line: a line that is entirely
/// inverted tells you *that* it matched and hides *where*, which is the half of
/// the answer you were looking for.
fn text_line<'a>(index: usize, line: &'a str, needle: &str, theme: &Theme) -> Line<'a> {
    let number = Span::styled(
        format!("{:>6}  ", index + 1),
        Style::default().fg(theme.status_fg).bg(theme.panel_bg),
    );
    let plain = Style::default().fg(theme.panel_fg).bg(theme.panel_bg);
    if needle.is_empty() {
        return Line::from(vec![number, Span::styled(line, plain)]);
    }

    // Case-insensitively, to agree with `Document::search` — a viewer that
    // finds a line and then cannot show you why is worse than one that does
    // not find it.
    let hay = line.to_lowercase();
    let pin = needle.to_lowercase();
    let mut spans = vec![number];
    let mut at = 0;
    while let Some(found) = hay[at..].find(&pin) {
        let start = at + found;
        let end = start + pin.len();
        // Byte offsets from the lowercase copy only line up with the original
        // while both are ASCII. Anything else and the highlight is abandoned
        // rather than being drawn in the wrong place — or panicking on a
        // boundary that is not a character.
        if !line.is_char_boundary(start) || !line.is_char_boundary(end) {
            spans.push(Span::styled(&line[at..], plain));
            return Line::from(spans);
        }
        spans.push(Span::styled(&line[at..start], plain));
        spans.push(Span::styled(
            &line[start..end],
            Style::default()
                .fg(theme.selected_fg)
                .bg(theme.panel_bg)
                .add_modifier(Modifier::REVERSED),
        ));
        at = end;
    }
    spans.push(Span::styled(&line[at..], plain));
    Line::from(spans)
}

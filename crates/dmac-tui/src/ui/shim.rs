//! "The agent shim is not being reached — fix it?"
//!
//! Shown once, at startup, and only when a hosted shell would run some other
//! `claude` than this session's shim. It is a question because the answer is
//! an edit to a file the user owns, and nobody's rc file should be written
//! without them saying so.

use crate::app::ShimShadowed;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, area: Rect, shim: &ShimShadowed, theme: &Theme) {
    let width = area.width.saturating_sub(6).clamp(46, 78).min(area.width);
    // One column of padding each side, and the border: four columns that the
    // text does not get. Wrapped lines start where the first one did, which is
    // why the padding is a rectangle and not a space in front of the string.
    let text_width = width.saturating_sub(4).max(1) as usize;

    let lead = format!("claude here runs {}", shim.by.display());
    let body = "and not this session's shim, so an agent started in it joins no \
                conversation and cannot see the panels it is running inside."
        .to_string();
    let ask = match &shim.rc {
        Some(rc) => format!(
            "{} puts its own directories in front of PATH after DMACommander has \
             had its say. Add a few lines to the end of it that put the shim back \
             in front?",
            rc.display()
        ),
        // Nothing to press y for: this one only says what is wrong and what
        // the shape of the fix is.
        None => "Your shell's configuration runs after DMACommander has had its say \
                 and puts its own directories first. Put $DMAC_SHIM_DIR back at the \
                 front of PATH at the end of it."
            .to_string(),
    };

    let paragraphs = [&lead, &body, &ask];
    let rows: usize = paragraphs.iter().map(|p| wrapped_rows(p, text_width)).sum();
    // The blank line between the diagnosis and the question, and the border.
    let height = (rows as u16 + 3).min(area.height);
    let popup = centred(area, width, height);

    frame.render_widget(Clear, popup);
    let hint = match shim.rc {
        Some(_) => " y fix it \u{b7} n leave it ",
        None => " any key to dismiss ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Double)
        .border_style(theme.border(true))
        .padding(Padding::horizontal(1))
        .title(Span::styled(" Agent shim ", theme.border(true)))
        .title_bottom(Span::styled(hint, theme.border(false)))
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let plain = Style::default().fg(theme.panel_fg);
    let lines = vec![
        Line::from(Span::styled(lead, plain.add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(body, plain)),
        Line::from(""),
        Line::from(Span::styled(ask, plain)),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// How many rows `text` takes once wrapped to `width`, counted the way
/// [`Wrap`] wraps: on spaces, and never mid-word unless the word is longer
/// than the line. Needed before drawing, to give the box a height that fits
/// its contents rather than a guess with dead space under it.
fn wrapped_rows(text: &str, width: usize) -> usize {
    let mut rows = 1;
    let mut used = 0;
    for word in text.split_whitespace() {
        let w = word.chars().count();
        if used == 0 {
            // A word too long for the line is broken across as many as it needs.
            rows += w.saturating_sub(1) / width;
            used = if w % width == 0 && w > 0 {
                width
            } else {
                w % width
            };
            continue;
        }
        if used + 1 + w <= width {
            used += 1 + w;
        } else {
            rows += 1 + w.saturating_sub(1) / width;
            used = if w % width == 0 { width } else { w % width };
        }
    }
    rows
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
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn shadowed(rc: Option<&str>) -> ShimShadowed {
        ShimShadowed {
            by: std::path::PathBuf::from("/Users/x/.local/bin/claude"),
            rc: rc.map(std::path::PathBuf::from),
        }
    }

    /// Including the sizes where it must decline to draw rather than draw badly.
    #[test]
    fn the_question_survives_every_terminal_size() {
        let theme = Theme::norton();
        for (w, h) in [(1u16, 1u16), (10, 4), (30, 8), (80, 24), (300, 90)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, f.area(), &shadowed(Some("/Users/x/.zshrc")), &theme))
                .unwrap_or_else(|e| panic!("failed at {w}x{h}: {e}"));
        }
    }

    /// It has to name what is being run instead, because that name is the whole
    /// diagnosis — without it the question is unanswerable.
    #[test]
    fn the_question_names_what_runs_instead() {
        let theme = Theme::norton();
        let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &shadowed(Some("/Users/x/.zshrc")), &theme))
            .unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("/Users/x/.local/bin/claude"), "{text}");
        assert!(text.contains("/Users/x/.zshrc"), "{text}");
        assert!(text.contains("y fix it"), "{text}");
    }

    /// The box is built around its text, so a change to the wording cannot
    /// leave a band of dead space under it — or, worse, cut the question off.
    #[test]
    fn the_box_is_the_size_of_what_it_says() {
        let theme = Theme::norton();
        for w in [50u16, 66, 90, 140] {
            let mut term = Terminal::new(TestBackend::new(w, 30)).unwrap();
            term.draw(|f| draw(f, f.area(), &shadowed(Some("/Users/x/.zshrc")), &theme))
                .unwrap();
            let buf = term.backend().buffer().clone();
            let rows: Vec<String> = (0..buf.area.height)
                .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
                .filter(|r: &String| r.contains('\u{2551}'))
                .collect();
            let last = rows.last().expect("a box was drawn");
            assert!(
                last.replace('\u{2551}', "").trim() != "",
                "a blank row above the bottom border at {w} columns"
            );
            assert!(
                rows.iter().any(|r| r.contains("in front?")),
                "cut off at {w}"
            );
        }
    }

    /// With no file we know how to write, offering `y` would be offering
    /// something that does nothing.
    #[test]
    fn an_unknown_shell_is_told_and_not_asked() {
        let theme = Theme::norton();
        let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &shadowed(None), &theme))
            .unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("y fix it"), "{text}");
        assert!(text.contains("DMAC_SHIM_DIR"), "{text}");
    }
}

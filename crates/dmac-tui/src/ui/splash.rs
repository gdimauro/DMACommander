//! The startup splash.
//!
//! Shown briefly over the panels — not instead of them, so the app never looks
//! like it is still loading when it is already usable. Any key dismisses it, and
//! that key is swallowed, for the same reason it is on the screensaver: a
//! gesture that clears something off the screen must not also act on a file.

use crate::theme::Theme;
use dmac_config::build_info;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

/// Label column width, so the values line up under each other.
const LABEL_W: usize = 9;

pub fn draw(frame: &mut Frame, area: Rect, theme: &Theme) {
    let rows = build_info::rows();

    let widest = rows
        .iter()
        .map(|(_, v)| LABEL_W + 1 + v.width())
        .max()
        .unwrap_or(24)
        .max(build_info::NAME.width() + 8);

    let width = (widest as u16 + 6).min(area.width);
    // title + blank + rows + blank + hint, inside a border
    let height = (rows.len() as u16 + 6).min(area.height);
    if width < 12 || height < 6 {
        // No room for a splash. Skipping it is strictly better than drawing a
        // mangled one over the panels.
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
        .style(theme.panel());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 4);

    // Letter-spaced and upper-case: spacing out mixed case reads as a stutter,
    // spacing out capitals reads as a logo.
    let title: String = build_info::NAME
        .to_uppercase()
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    lines.push(centre(
        &title,
        inner.width,
        Style::default()
            .fg(theme.selected_fg)
            .bg(theme.panel_bg)
            .add_modifier(Modifier::BOLD),
    ));
    lines.push(Line::from(""));

    for (label, value) in &rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {label:<LABEL_W$} "),
                Style::default().fg(theme.status_fg).bg(theme.panel_bg),
            ),
            Span::styled(
                value.clone(),
                Style::default().fg(theme.panel_fg).bg(theme.panel_bg),
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(centre(
        "press any key",
        inner.width,
        Style::default()
            .fg(theme.status_fg)
            .bg(theme.panel_bg)
            .add_modifier(Modifier::DIM),
    ));

    frame.render_widget(Paragraph::new(lines), inner);
}

fn centre(text: &str, width: u16, style: Style) -> Line<'static> {
    let pad = (width as usize).saturating_sub(text.width()) / 2;
    Line::from(Span::styled(format!("{}{text}", " ".repeat(pad)), style))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Including the sizes where it must decline to draw rather than draw badly.
    #[test]
    fn the_splash_survives_every_terminal_size() {
        let theme = Theme::norton();
        for (w, h) in [(1u16, 1u16), (10, 4), (30, 8), (80, 24), (300, 90)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|f| draw(f, f.area(), &theme))
                .unwrap_or_else(|e| panic!("splash failed at {w}x{h}: {e}"));
        }
    }

    /// The whole point of the splash is the build identity — if the version stops
    /// appearing, the feature is silently doing nothing.
    #[test]
    fn the_splash_shows_the_version() {
        let theme = Theme::norton();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(f, f.area(), &theme)).unwrap();

        let buf = term.backend().buffer();
        let text: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains(build_info::VERSION), "version missing");
        assert!(text.contains(build_info::GIT_SHA), "commit missing");
    }
}

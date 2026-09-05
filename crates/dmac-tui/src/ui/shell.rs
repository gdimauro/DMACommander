//! Rendering a hosted shell, and encoding keys to send back to it.
//!
//! The emulator gives us a grid of cells with attributes; this turns that into
//! a ratatui buffer. Nothing here interprets escape sequences — `vt100` has
//! already done that, which is the whole reason for hosting through a PTY
//! rather than trying to parse a child's output ourselves.

use crate::theme::Theme;
use dmac_pty::Hosted;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};

/// Draw the hosted screen into `area`, returning the interior rect so the caller
/// can keep the PTY the same size as what is visible.
pub fn draw(frame: &mut Frame, area: Rect, shell: &Hosted, focused: bool, theme: &Theme) -> Rect {
    let title = if shell.finished() {
        format!(" {} (exited) ", shell.program())
    } else {
        format!(" {} ", shell.program())
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(theme.border(focused))
        .title(Span::styled(title, theme.border(focused)))
        .title_bottom(Span::styled(" Ctrl-O panels ", theme.border(false)))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return inner;
    }

    let buf = frame.buffer_mut();
    shell.with_screen(|screen| {
        let (rows, cols) = screen.size();
        for y in 0..inner.height.min(rows) {
            for x in 0..inner.width.min(cols) {
                let Some(cell) = screen.cell(y, x) else {
                    continue;
                };
                let target = &mut buf[(inner.x + x, inner.y + y)];
                // A hosted program can emit any character, including width-2
                // ones; `contents()` gives the grapheme for the cell and an
                // empty string for the continuation column of a wide character.
                let text = cell.contents();
                target.set_symbol(if text.is_empty() { " " } else { text });
                target.set_style(style_of(cell));
            }
        }
    });

    // The child's cursor, only when the shell has the keyboard. Two visible
    // cursors is worse than none.
    if focused && !shell.finished() {
        shell.with_screen(|screen| {
            if screen.hide_cursor() {
                return;
            }
            let (row, col) = screen.cursor_position();
            if row < inner.height && col < inner.width {
                frame.set_cursor_position((inner.x + col, inner.y + row));
            }
        });
    }

    inner
}

fn style_of(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(convert(cell.fgcolor(), Color::Gray))
        .bg(convert(cell.bgcolor(), Color::Black));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn convert(c: vt100::Color, default: Color) -> Color {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Encode a key press as the bytes a terminal would send.
///
/// Returns `None` for keys with no representation, which are dropped rather
/// than sent as something approximate — a shell receiving a plausible-but-wrong
/// escape sequence behaves worse than one receiving nothing.
pub fn encode(key: KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let bytes: Vec<u8> = match key.code {
        KeyCode::Char(c) if ctrl => {
            // Ctrl-A..Ctrl-Z and the handful of control codes above them.
            let b = match c.to_ascii_lowercase() {
                'a'..='z' => (c.to_ascii_lowercase() as u8) - b'a' + 1,
                '@' | ' ' => 0,
                '[' => 27,
                '\\' => 28,
                ']' => 29,
                '^' => 30,
                '_' | '?' => 31,
                _ => return None,
            };
            vec![b]
        }
        KeyCode::Char(c) => {
            let mut v = Vec::new();
            // Alt is sent as an ESC prefix, which is what every terminal does
            // and what readline expects for Alt-b, Alt-f and friends.
            if alt {
                v.push(0x1b);
            }
            let mut buf = [0u8; 4];
            v.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            v
        }

        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        // DEL, not BS: this is what a modern terminal sends, and what shells
        // are configured to expect.
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],

        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Insert => b"\x1b[2~".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),

        KeyCode::F(n) => match n {
            1 => b"\x1bOP".to_vec(),
            2 => b"\x1bOQ".to_vec(),
            3 => b"\x1bOR".to_vec(),
            4 => b"\x1bOS".to_vec(),
            5 => b"\x1b[15~".to_vec(),
            6 => b"\x1b[17~".to_vec(),
            7 => b"\x1b[18~".to_vec(),
            8 => b"\x1b[19~".to_vec(),
            9 => b"\x1b[20~".to_vec(),
            10 => b"\x1b[21~".to_vec(),
            11 => b"\x1b[23~".to_vec(),
            12 => b"\x1b[24~".to_vec(),
            _ => return None,
        },

        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn with(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn ordinary_characters_go_through_as_themselves() {
        assert_eq!(encode(key(KeyCode::Char('l'))), Some(b"l".to_vec()));
        assert_eq!(encode(key(KeyCode::Char('/'))), Some(b"/".to_vec()));
    }

    #[test]
    fn non_ascii_characters_are_sent_as_utf8() {
        assert_eq!(
            encode(key(KeyCode::Char('è'))),
            Some("è".as_bytes().to_vec())
        );
        assert_eq!(
            encode(key(KeyCode::Char('日'))),
            Some("日".as_bytes().to_vec())
        );
    }

    /// Ctrl-C has to be a real SIGINT-producing byte, or nothing in the shell
    /// can be interrupted and the pane becomes a trap.
    #[test]
    fn ctrl_c_is_the_interrupt_byte() {
        assert_eq!(
            encode(with(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(vec![0x03])
        );
    }

    #[test]
    fn ctrl_d_ends_input_and_ctrl_z_suspends() {
        assert_eq!(
            encode(with(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(vec![0x04])
        );
        assert_eq!(
            encode(with(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            Some(vec![0x1a])
        );
    }

    #[test]
    fn ctrl_is_case_insensitive() {
        assert_eq!(
            encode(with(KeyCode::Char('C'), KeyModifiers::CONTROL)),
            encode(with(KeyCode::Char('c'), KeyModifiers::CONTROL))
        );
    }

    /// Readline's word motions are Alt-prefixed, and Alt is an ESC prefix.
    #[test]
    fn alt_is_sent_as_an_escape_prefix() {
        assert_eq!(
            encode(with(KeyCode::Char('b'), KeyModifiers::ALT)),
            Some(vec![0x1b, b'b'])
        );
    }

    /// DEL rather than BS: shells are configured for the former, and sending
    /// the latter makes backspace print `^H`.
    #[test]
    fn backspace_is_del() {
        assert_eq!(encode(key(KeyCode::Backspace)), Some(vec![0x7f]));
    }

    #[test]
    fn arrows_are_csi_sequences_so_history_works() {
        assert_eq!(encode(key(KeyCode::Up)), Some(b"\x1b[A".to_vec()));
        assert_eq!(encode(key(KeyCode::Left)), Some(b"\x1b[D".to_vec()));
    }

    #[test]
    fn enter_is_carriage_return_not_newline() {
        // A PTY in canonical mode translates CR to NL; sending NL directly can
        // leave the line uncommitted.
        assert_eq!(encode(key(KeyCode::Enter)), Some(vec![b'\r']));
    }

    /// Dropping is deliberate: a plausible-but-wrong escape sequence is worse
    /// for a shell than nothing at all.
    #[test]
    fn keys_with_no_terminal_representation_are_dropped() {
        assert_eq!(encode(key(KeyCode::F(25))), None);
        assert_eq!(encode(key(KeyCode::CapsLock)), None);
    }
}

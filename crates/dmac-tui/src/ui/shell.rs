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
/// A text selection over the hosted screen, in its own cell coordinates.
///
/// Held in screen coordinates rather than in the scrollback, which is why it is
/// dropped as soon as the screen changes underneath it: a highlight that stayed
/// put while the text scrolled out from under it would be pointing at whatever
/// happened to land there, and copying it would hand you something you never
/// selected. Wrong-looking is recoverable; wrong-and-confident is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    anchor: (u16, u16),
    head: (u16, u16),
}

impl Selection {
    pub fn new(row: u16, col: u16) -> Self {
        Self {
            anchor: (row, col),
            head: (row, col),
        }
    }

    pub fn extend_to(&mut self, row: u16, col: u16) {
        self.head = (row, col);
    }

    /// Start and end in reading order. Tuples compare lexicographically, which
    /// for `(row, col)` *is* reading order — so nothing downstream has to know
    /// which way the drag went.
    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (a, b) = self.ordered();
        (row, col) >= a && (row, col) <= b
    }

    /// A single click selects nothing. Without this, every click would put an
    /// invisible one-cell selection on the clipboard and destroy what was there.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Grow to the whole word under the anchor, for a double-click.
    pub fn expand_to_word(&mut self, shell: &Hosted) {
        let (row, col) = self.anchor;
        let Some((lo, hi)) = shell
            .with_screen(|screen| word_at(screen, row, col))
            .flatten()
        else {
            return;
        };
        self.anchor = (row, lo);
        self.head = (row, hi);
    }

    /// The selected text, as the user would read it.
    pub fn text(&self, shell: &Hosted) -> String {
        let (a, b) = self.ordered();
        shell
            .with_screen(|screen| {
                let (_, cols) = screen.size();
                // `contents_between` stops *before* end_col; the cell under the
                // pointer when the button came up is part of what was selected.
                let end = b.1.saturating_add(1).min(cols);
                screen.contents_between(a.0, a.1, b.0, end)
            })
            .unwrap_or_default()
    }
}

/// The run of word characters containing `col`, as inclusive columns.
fn word_at(screen: &vt100::Screen, row: u16, col: u16) -> Option<(u16, u16)> {
    let (_, cols) = screen.size();
    let is_word = |c: u16| {
        screen
            .cell(row, c)
            .map(|cell| {
                let t = cell.contents();
                !t.is_empty() && !t.chars().all(|ch| ch.is_whitespace())
            })
            .unwrap_or(false)
    };
    if !is_word(col) {
        return None;
    }
    let mut lo = col;
    while lo > 0 && is_word(lo - 1) {
        lo -= 1;
    }
    let mut hi = col;
    while hi + 1 < cols && is_word(hi + 1) {
        hi += 1;
    }
    Some((lo, hi))
}

/// How the pane is presented this frame. A struct rather than six positional
/// arguments, which is how `bordered` and `focused` end up swapped.
#[derive(Debug, Clone, Copy, Default)]
pub struct Chrome {
    /// Whether the shell has the keyboard, which decides the cursor.
    pub focused: bool,
    /// A border and titles, or bare contents for full screen.
    pub bordered: bool,
    /// `Some(on)` when the caller draws its own cursor because the terminal
    /// cannot be trusted to blink one, and `None` for the real one.
    pub software_cursor: Option<bool>,
    pub selection: Option<Selection>,
}

pub fn draw(frame: &mut Frame, area: Rect, shell: &Hosted, c: &Chrome, theme: &Theme) -> Rect {
    let Chrome {
        focused,
        bordered,
        software_cursor,
        selection,
    } = *c;
    let title = if shell.finished() {
        format!(" {} (exited) ", shell.program())
    } else {
        format!(" {} ", shell.program())
    };

    let block = Block::default()
        .borders(if bordered {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_type(BorderType::Plain)
        .border_style(theme.border(focused))
        .style(Style::default().bg(Color::Black));
    let block = if bordered {
        block
            .title(Span::styled(title, theme.border(focused)))
            // The F-key bar is gone in this view, so this line is the only
            // documentation of the keys that still reach the commander from
            // inside a hosted program. It has to name all of them.
            .title_bottom(Span::styled(
                " Ctrl-O DMAC commander · F9 utilities · F12 history ",
                theme.border(false),
            ))
    } else {
        block
    };
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
                let mut style = style_of(cell);
                if selection.is_some_and(|s| s.contains(y, x)) {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                target.set_style(style);
            }
        }
    });

    // The child's cursor, only when the shell has the keyboard. Two visible
    // cursors is worse than none.
    //
    // A hosted CLI without a visible cursor is a CLI you cannot tell is
    // waiting for you, so this matters more here than on the command line.
    // `hide_cursor` is the child's own decision and is always honoured.
    if focused && !shell.finished() {
        shell.with_screen(|screen| {
            if screen.hide_cursor() {
                return;
            }
            let (row, col) = screen.cursor_position();
            if row >= inner.height || col >= inner.width {
                return;
            }
            let at = (inner.x + col, inner.y + row);
            match software_cursor {
                // Drawn, not asked for: some terminals ignore the request to
                // blink, and inverting the cell works in every one of them.
                Some(on) => {
                    if on {
                        frame.buffer_mut()[at].set_style(
                            Style::default().add_modifier(ratatui::style::Modifier::REVERSED),
                        );
                    }
                }
                None => frame.set_cursor_position(at),
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

    /// A drag upward or leftward is the same selection as the drag back.
    #[test]
    fn a_selection_reads_the_same_in_either_direction() {
        let mut forward = Selection::new(1, 2);
        forward.extend_to(3, 4);
        let mut backward = Selection::new(3, 4);
        backward.extend_to(1, 2);
        assert_eq!(forward.ordered(), backward.ordered());

        for (r, c) in [(1, 2), (1, 79), (2, 0), (3, 4)] {
            assert!(forward.contains(r, c), "{r},{c} should be in");
            assert!(backward.contains(r, c), "{r},{c} should be in either way");
        }
        assert!(!forward.contains(1, 1), "before the start");
        assert!(!forward.contains(3, 5), "after the end");
        assert!(!forward.contains(4, 0), "a row past the end");
    }

    /// A click that never moved must not put an empty string on the clipboard:
    /// that silently destroys whatever was there, which is a way to lose work.
    #[test]
    fn a_click_that_never_moved_selects_nothing() {
        assert!(Selection::new(4, 9).is_empty());
        let mut s = Selection::new(4, 9);
        s.extend_to(4, 10);
        assert!(!s.is_empty());
    }

    #[cfg(unix)]
    fn shell_showing(text: &str) -> Hosted {
        let args = ["-c".to_string(), format!("printf '{text}'")];
        let h = Hosted::spawn(dmac_pty::Spawn::new("/bin/sh", &args, 40, 10)).expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !h.finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        h
    }

    /// The cell under the pointer when the button came up is part of what was
    /// selected — off by one here means the last character never gets copied.
    #[cfg(unix)]
    #[test]
    fn the_selected_text_includes_both_ends() {
        let h = shell_showing("hello world");
        let mut sel = Selection::new(0, 0);
        sel.extend_to(0, 4);
        assert_eq!(sel.text(&h), "hello");

        let mut all = Selection::new(0, 0);
        all.extend_to(0, 10);
        assert_eq!(all.text(&h), "hello world");
    }

    #[cfg(unix)]
    #[test]
    fn a_selection_across_rows_keeps_the_line_break() {
        let h = shell_showing("first\\nsecond");
        let mut sel = Selection::new(0, 0);
        sel.extend_to(1, 5);
        assert_eq!(sel.text(&h), "first\nsecond");
    }

    /// Double-click takes the word under the pointer, from anywhere in it.
    #[cfg(unix)]
    #[test]
    fn double_click_takes_the_whole_word_from_anywhere_in_it() {
        let h = shell_showing("alpha beta gamma");
        for col in 6..=9 {
            let mut sel = Selection::new(0, col);
            sel.expand_to_word(&h);
            assert_eq!(sel.text(&h), "beta", "clicking column {col}");
        }
    }

    /// A double-click on empty space should not silently select a run of
    /// spaces and replace the clipboard with them.
    #[cfg(unix)]
    #[test]
    fn double_click_on_blank_space_selects_nothing() {
        let h = shell_showing("hi");
        let mut sel = Selection::new(0, 20);
        sel.expand_to_word(&h);
        assert!(sel.is_empty(), "got {:?}", sel.text(&h));
    }
}

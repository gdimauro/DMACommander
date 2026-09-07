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
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};

/// A position over the hosted pane: a column, and a row counted from the top of
/// what is currently *visible*.
///
/// The row is signed because a selection is allowed to run off the top of the
/// view and keep going — scrolling is how a selection grows past one screenful,
/// and an end that had to be clamped to row 0 would silently stop selecting
/// while the user was still holding the key down.
pub type Cell = (i32, u16);

/// A text selection over the hosted screen.
///
/// Held in *visible* coordinates, which is what makes it survive scrolling
/// without ever pointing at the wrong text. Two things move underneath it, and
/// they are handled differently on purpose:
///
/// - The user scrolls. The text moves by a known number of lines and
///   [`Selection::shift`] moves the selection by exactly the same amount, so
///   the highlight stays glued to the characters it was put on.
/// - The child prints. While the view is scrolled back `vt100` keeps the
///   visible rows where they are, so there is nothing to do; at the live bottom
///   the text scrolls out from under the selection, and the caller drops it.
///   A highlight left behind there would be pointing at whatever happened to
///   land in those cells, and copying it would hand you something you never
///   selected. Wrong-looking is recoverable; wrong-and-confident is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    anchor: Cell,
    head: Cell,
}

impl Selection {
    pub fn new(row: i32, col: u16) -> Self {
        Self {
            anchor: (row, col),
            head: (row, col),
        }
    }

    pub fn extend_to(&mut self, row: i32, col: u16) {
        self.head = (row, col);
    }

    /// Follow the text by `rows` after the view has scrolled that far.
    pub fn shift(&mut self, rows: i32) {
        self.anchor.0 += rows;
        self.head.0 += rows;
    }

    /// Start and end in reading order. Tuples compare lexicographically, which
    /// for `(row, col)` *is* reading order — so nothing downstream has to know
    /// which way the drag went.
    fn ordered(&self) -> (Cell, Cell) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    pub fn contains(&self, row: u16, col: u16) -> bool {
        let (a, b) = self.ordered();
        let at = (i32::from(row), col);
        at >= a && at <= b
    }

    /// A single click selects nothing. Without this, every click would put an
    /// invisible one-cell selection on the clipboard and destroy what was there.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// Grow to the whole word under the anchor, for a double-click.
    pub fn expand_to_word(&mut self, shell: &Hosted) {
        let (row, col) = self.anchor;
        // A double-click always lands on a visible row; anything else is not a
        // word this can find.
        let Ok(row) = u16::try_from(row) else {
            return;
        };
        let Some((lo, hi)) = shell
            .with_screen(|screen| word_at(screen, row, col))
            .flatten()
        else {
            return;
        };
        self.anchor = (i32::from(row), lo);
        self.head = (i32::from(row), hi);
    }

    /// The selected text, as the user would read it — including the parts that
    /// have been scrolled out of view.
    pub fn text(&self, shell: &Hosted) -> String {
        let (a, b) = self.ordered();
        let (cols, _) = shell.size();
        // Visible row 0 is this line of the buffer, so the selection can be
        // asked for in coordinates that do not depend on where the view is.
        let base = shell.scrollback_len() as i64 - shell.scroll_offset() as i64;
        // The far end stops *before* its column, and the cell under the pointer
        // when the button came up is part of what was selected.
        let end = b.1.saturating_add(1).min(cols);
        shell
            .contents_between_buffer((base + i64::from(a.0), a.1), (base + i64::from(b.0), end))
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
pub struct Chrome<'a> {
    /// Whether the shell has the keyboard, which decides the cursor.
    pub focused: bool,
    /// A border and titles, or bare contents for full screen.
    pub bordered: bool,
    /// `Some(on)` when the caller draws its own cursor because the terminal
    /// cannot be trusted to blink one, and `None` for the real one.
    pub software_cursor: Option<bool>,
    pub selection: Option<Selection>,
    /// Where the shell actually is, and where the panels have gone without it.
    /// The only thing this view says about location: the F-key bar is not here,
    /// and a shell whose directory you cannot see is one you have to `pwd` at.
    /// `pending` is `Some` only when the two differ — which happens while
    /// something is running, because a `cd` cannot be typed at it.
    pub cwd: Option<&'a str>,
    pub pending: Option<&'a str>,
    /// Which session this shell belongs to, and the colour the rail gives it.
    /// `None` draws the border the way it was drawn before there were sessions
    /// to name, which is what a caller with nothing to say should get.
    pub session: Option<(&'a str, Color)>,
    /// Where the keyboard is selecting from, while a Shift-selection is being
    /// made. Drawn so it is obvious which end of the highlight moves next.
    pub caret: Option<Cell>,
}

/// What the top border says: which session, which program, and where it is.
///
/// The session first, in the colour the rail gives its dot. In this view the
/// rail is three columns of dots, so the name is nowhere else on the screen —
/// and a command typed into the wrong session is not something you notice while
/// you are typing it. The colour comes from the rail rather than being chosen
/// again here, so the dot and the name cannot drift apart.
///
/// Then the program and the directory, both, because either alone leaves a
/// question. The program without the directory is the state this view was in
/// for months — the one place you type commands and the only one that would not
/// tell you where they would land. The directory without the program would not
/// say what has the keyboard.
///
/// An arrow appears when the panels have gone somewhere the shell could not
/// follow, which is exactly when something is running in it: the answer to
/// "did it come with me?" belongs on the screen, not in the user's head.
///
/// Given the two facts it needs from the shell rather than the shell itself, so
/// that what the border says can be asserted without starting a process — and
/// on a platform where the process would be a different one.
fn title_for(
    program: &str,
    finished: bool,
    session: Option<(&str, Color)>,
    cwd: Option<&str>,
    pending: Option<&str>,
    base: Style,
) -> Line<'static> {
    let mut rest = format!(" {program}");
    if finished {
        rest.push_str(" (exited)");
    }
    if let Some(cwd) = cwd {
        rest.push_str(&format!(" \u{b7} {cwd}"));
    }
    if let Some(pending) = pending {
        rest.push_str(&format!(" \u{2192} {pending}"));
    }
    rest.push(' ');
    match session {
        // Bold as well as coloured: this is the one word on the border that is
        // not about the program, and a terminal with no colour must still be
        // able to tell them apart.
        Some((name, colour)) => Line::from(vec![
            Span::styled(
                format!(" {name}"),
                base.fg(colour).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" \u{b7}{rest}"), base),
        ]),
        None => Line::from(Span::styled(rest, base)),
    }
}

/// Draw the hosted screen into `area`, returning the interior rect so the caller
/// can keep the PTY the same size as what is visible.
pub fn draw(frame: &mut Frame, area: Rect, shell: &Hosted, c: &Chrome<'_>, theme: &Theme) -> Rect {
    let Chrome {
        focused,
        bordered,
        software_cursor,
        selection,
        caret,
        cwd,
        pending,
        session,
    } = *c;
    let title = title_for(
        shell.program(),
        shell.finished(),
        session,
        cwd,
        pending,
        theme.border(focused),
    );

    let block = Block::default()
        .borders(if bordered {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_type(BorderType::Plain)
        .border_style(theme.border(focused))
        .style(Style::default().bg(Color::Black));
    // How far back the view is. Said out loud rather than left to be inferred:
    // a pane that has quietly stopped following its shell looks exactly like a
    // pane whose shell has stopped saying anything.
    let back = shell.scroll_offset();

    let block = if bordered {
        block
            .title(title)
            // The F-key bar is gone in this view, so this line is the only
            // documentation of the keys that still reach the commander from
            // inside a hosted program. It has to name all of them.
            .title_bottom(Span::styled(
                // Ctrl-O is in every one of them. The other hints belong to a
                // state you are in for a moment — selecting, reading back — and
                // they used to replace this line wholesale, so the one key that
                // gets you out of a hosted program went missing exactly when
                // someone lost in one would go looking for it.
                match (selection.is_some(), back) {
                    (true, 0) => " selecting \u{b7} Ctrl-Shift-C copy \u{b7} Esc clear \
                                  \u{b7} Ctrl-O commander "
                        .to_string(),
                    (true, n) => format!(
                        " \u{2191} {n} back \u{b7} Ctrl-Shift-C copy \u{b7} Esc clear \
                         \u{b7} Ctrl-O commander "
                    ),
                    (false, 0) => {
                        " Ctrl-O DMAC commander \u{b7} F9 utilities \u{b7} F12 history ".to_string()
                    }
                    (false, n) => format!(
                        " \u{2191} {n} lines back \u{b7} Esc back to live \
                         \u{b7} Ctrl-O commander "
                    ),
                },
                theme.border(back > 0 || selection.is_some()),
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

    // The end of the selection the keyboard moves next. Drawn under the child's
    // cursor on purpose: while a selection is being made the caret is what the
    // keys act on, and the child's cursor is a bystander.
    if let Some((row, col)) = caret
        && let Ok(row) = u16::try_from(row)
        && row < inner.height
        && col < inner.width
    {
        frame.buffer_mut()[(inner.x + col, inner.y + row)]
            .set_style(theme.cursor().add_modifier(Modifier::BOLD));
    }

    // The child's cursor, only when the shell has the keyboard. Two visible
    // cursors is worse than none.
    //
    // A hosted CLI without a visible cursor is a CLI you cannot tell is
    // waiting for you, so this matters more here than on the command line.
    // `hide_cursor` is the child's own decision and is always honoured.
    if focused && caret.is_none() && !shell.finished() {
        shell.with_screen(|screen| {
            if screen.hide_cursor() {
                return;
            }
            let (row, col) = screen.cursor_position();
            // `cursor_position` is where the child put it on the *live* screen,
            // which is `back` rows further down once the view has been scrolled
            // away from it.
            let Some(row) = u16::try_from(back).ok().and_then(|b| row.checked_add(b)) else {
                return;
            };
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
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

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
        // Terminals disagree about which of the two a shifted Tab arrives as —
        // a bare `BackTab` where modifiers are not reported, `Tab` plus Shift
        // where they are. Both are the same keypress and the child expects the
        // same three bytes for it; sending a plain tab for one of them is how a
        // key works in one terminal and not the next.
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
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

    /// Both spellings of Shift-Tab reach the child as the same key. It is how
    /// `claude` cycles its permission modes and how everything else moves
    /// backwards through its own fields, and a plain tab is not that key.
    #[test]
    fn shift_tab_is_the_same_three_bytes_in_either_spelling() {
        assert_eq!(encode(key(KeyCode::BackTab)), Some(b"\x1b[Z".to_vec()));
        assert_eq!(
            encode(with(KeyCode::Tab, KeyModifiers::SHIFT)),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(encode(key(KeyCode::Tab)), Some(vec![b'\t']), "plain Tab");
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

    /// A shell that has printed `n` numbered lines onto a 6-row screen, so most
    /// of what it said is above the top of the view.
    #[cfg(unix)]
    fn shell_that_counted_to(n: u32) -> Hosted {
        let script = format!("i=1; while [ $i -le {n} ]; do echo line-$i; i=$((i+1)); done");
        let args = ["-c".to_string(), script];
        let h = Hosted::spawn(dmac_pty::Spawn {
            scrollback: 200,
            ..dmac_pty::Spawn::new("/bin/sh", &args, 40, 6)
        })
        .expect("spawn");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            let seen = h.with_screen(|s| s.contents()).unwrap_or_default();
            if seen.contains(&format!("line-{n}")) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        h
    }

    /// Selecting more than one screenful is the reason the rows are signed.
    /// A selection whose top end is above the view has to copy the text that
    /// is up there, not the top line of what happens to be showing.
    #[cfg(unix)]
    #[test]
    fn a_selection_reaching_above_the_view_still_copies_what_is_up_there() {
        let h = shell_that_counted_to(30);
        // Rows -8 .. 0: eight lines that scrolled off, and the top visible one.
        let mut sel = Selection::new(-8, 0);
        sel.extend_to(0, 39);
        let text = sel.text(&h);
        let numbers: Vec<u32> = text
            .lines()
            .filter_map(|l| l.trim().strip_prefix("line-"))
            .filter_map(|n| n.parse().ok())
            .collect();
        assert!(
            numbers.len() >= 8,
            "only {numbers:?} came back from {text:?}"
        );
        for pair in numbers.windows(2) {
            assert_eq!(pair[1], pair[0] + 1, "out of order in {numbers:?}");
        }
    }

    /// The view moving must not move the selection off its own characters:
    /// after scrolling back by n, the same text is n rows further down.
    #[cfg(unix)]
    #[test]
    fn a_selection_stays_on_its_text_when_the_view_scrolls() {
        let h = shell_that_counted_to(30);
        let mut sel = Selection::new(0, 0);
        sel.extend_to(0, 39);
        let before = sel.text(&h);
        assert!(before.contains("line-"), "nothing selected: {before:?}");

        let moved = h.scroll_by(4);
        assert_eq!(moved, 4);
        sel.shift(moved);
        assert_eq!(sel.text(&h), before, "the highlight slid off its own text");

        // And back again, from the other end of the buffer.
        let moved = h.scroll_by(-4);
        sel.shift(moved);
        assert_eq!(sel.text(&h), before);
    }

    /// Shifting is the only thing that may move it. A selection nobody touched
    /// must read the same twice, or copying it twice would give two answers.
    #[test]
    fn shifting_moves_both_ends_by_the_same_amount() {
        let mut sel = Selection::new(2, 3);
        sel.extend_to(5, 7);
        let ordered = sel.ordered();
        sel.shift(4);
        let after = sel.ordered();
        assert_eq!(after.0.0 - ordered.0.0, 4);
        assert_eq!(after.1.0 - ordered.1.0, 4);
        assert_eq!((after.0.1, after.1.1), (ordered.0.1, ordered.1.1));

        sel.shift(-9);
        assert!(sel.ordered().0.0 < 0, "an end is allowed above the view");
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

    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The one thing on this border that is not about the program. The rail is
    /// three columns of dots in this view, so a name that is not here is
    /// nowhere — and a command typed into the wrong session is not something
    /// anyone notices while they are typing it.
    #[test]
    fn the_border_says_which_session_this_is() {
        let title = title_for(
            "/bin/zsh",
            false,
            Some(("DMAC", Color::Cyan)),
            Some("~/prj/DimaCommander"),
            None,
            Style::default(),
        );
        assert_eq!(
            text_of(&title),
            " DMAC \u{b7} /bin/zsh \u{b7} ~/prj/DimaCommander "
        );
        assert_eq!(
            title.spans.first().map(|s| s.style.fg),
            Some(Some(Color::Cyan)),
            "the name wears the colour the rail gives that session"
        );
    }

    /// Adding the session must not cost the border anything it already said.
    #[test]
    fn the_border_still_says_the_program_and_where_it_is() {
        let title = title_for(
            "sh",
            true,
            Some(("MAIN", Color::Green)),
            Some("/tmp"),
            Some("/etc"),
            Style::default(),
        );
        assert_eq!(
            text_of(&title),
            " MAIN \u{b7} sh (exited) \u{b7} /tmp \u{2192} /etc "
        );
    }

    /// A caller with no session to name gets the border it had before there
    /// were sessions to name, and not one with a gap where the name goes.
    #[test]
    fn no_session_leaves_the_border_as_it_was() {
        let title = title_for("sh", false, None, Some("/tmp"), None, Style::default());
        assert_eq!(text_of(&title), " sh \u{b7} /tmp ");
    }
}

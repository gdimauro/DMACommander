//! The help page: what the keys do, in one place, inside the program.
//!
//! The content is data. `F1` draws it over the panels, and the `cannon`
//! screensaver shoots at the same lines, so there is one page and not two that
//! drift apart.
//!
//! A row that names a binding also names the [`Action`] its key must resolve
//! to, and a test presses every such key and checks. A help page that lies is
//! worse than none — and the keymap *will* move the moment nobody is checking.

use crate::action::Action;
use crate::app::Focus;

/// One line of the page.
pub struct Row {
    /// The key, or keys separated by ` / `. Free text for a note.
    pub keys: &'static str,
    pub what: &'static str,
    /// What each spelling of `keys` must resolve to. One entry covers every
    /// spelling; otherwise there is one per spelling. Empty for a note, which
    /// promises nothing the keymap can be asked about.
    pub actions: &'static [Action],
    /// Where the keyboard is when the key means this.
    pub focus: Focus,
}

impl Row {
    /// A binding that holds while a panel has the keyboard.
    pub const fn panel(keys: &'static str, what: &'static str, actions: &'static [Action]) -> Self {
        Self {
            keys,
            what,
            actions,
            focus: Focus::Panel,
        }
    }

    /// A binding that holds while the command line has the keyboard.
    pub const fn line(keys: &'static str, what: &'static str, actions: &'static [Action]) -> Self {
        Self {
            keys,
            what,
            actions,
            focus: Focus::CommandLine,
        }
    }

    /// Something worth knowing that is not a keymap entry: a chord, the mouse,
    /// what a hosted program keeps.
    pub const fn note(keys: &'static str, what: &'static str) -> Self {
        Self {
            keys,
            what,
            actions: &[],
            focus: Focus::Panel,
        }
    }
}

pub struct Section {
    pub title: &'static str,
    pub rows: &'static [Row],
}

use Action::*;
use dmac_core::SortKey;

pub const SECTIONS: &[Section] = &[
    Section {
        title: "Panels",
        rows: &[
            Row::panel("Up / Down", "move the cursor", &[CursorUp, CursorDown]),
            Row::panel("PgUp / PgDn", "a screen at a time", &[PageUp, PageDown]),
            Row::panel(
                "Home / End",
                "the first entry, and the last",
                &[GoTop, GoBottom],
            ),
            Row::panel(
                "Enter",
                "open the directory, or the file, under the cursor",
                &[Activate],
            ),
            Row::panel(
                "Backspace / Alt-Backspace",
                "up one directory; the Alt form even while a quick search is on",
                &[QuickSearchBackspace, GoParent],
            ),
            Row::panel(
                "Tab",
                "the other panel, then the command line",
                &[FocusNext],
            ),
            Row::panel(
                "Esc",
                "undo the last thing: a search, then the command line, then back \
                 to the shell",
                &[FocusToggle],
            ),
            Row::panel("Alt-U", "swap the two panels", &[SwapPanels]),
            Row::panel("Ctrl-R", "re-read the directory", &[Refresh]),
            Row::panel(
                "Alt-H / Alt-.",
                "show and hide hidden files",
                &[ToggleHidden],
            ),
            Row::panel(
                "Ctrl-H / Ctrl-Backspace",
                "the directory history: everywhere the panels have been",
                &[DirectoryHistory],
            ),
            Row::panel(
                "Ctrl-U / Ctrl-Shift-U",
                "the utilities: generators, paths, transforms of the line",
                &[UtilitiesMenu],
            ),
            Row::note(
                "a letter",
                "quick search: the cursor jumps to the entry that starts with it",
            ),
        ],
    },
    Section {
        title: "Marking",
        rows: &[
            Row::panel(
                "Ins",
                "mark or unmark the entry, and move on",
                &[ToggleSelection],
            ),
            Row::panel(
                "+ / - / *",
                "mark everything, unmark everything, invert",
                &[SelectAll, ClearSelection, InvertSelection],
            ),
            Row::panel(
                "Shift-Up / Shift-Down",
                "grow the marked span by a row",
                &[ExtendSelection(-1), ExtendSelection(1)],
            ),
            Row::panel(
                "Shift-PgUp / Shift-PgDn",
                "by a screen",
                &[ExtendSelectionPage(-1), ExtendSelectionPage(1)],
            ),
            Row::panel(
                "Shift-Home / Shift-End",
                "to the top, and to the bottom",
                &[ExtendSelectionToTop, ExtendSelectionToBottom],
            ),
        ],
    },
    Section {
        title: "Sorting",
        rows: &[
            Row::panel("Shift-F3", "by name", &[SortBy(SortKey::Name)]),
            Row::panel("Shift-F4", "by extension", &[SortBy(SortKey::Extension)]),
            Row::panel("Shift-F5", "by date", &[SortBy(SortKey::Modified)]),
            Row::panel("Shift-F6", "by size", &[SortBy(SortKey::Size)]),
            Row::note("the same key again", "reverses the order"),
        ],
    },
    Section {
        title: "The command line",
        rows: &[
            Row::line(
                "Enter",
                "run what is typed, in this session's shell",
                &[CommandSubmit],
            ),
            Row::line(
                "Tab",
                "complete a path; with nothing to complete, move the keyboard on",
                &[FocusNext],
            ),
            Row::line("Ctrl-Y", "clear the line", &[CommandClear]),
            Row::line(
                "Shift-Left / Shift-Right",
                "select what is typed, a character at a time",
                &[ExtendCommandSelection(-1), ExtendCommandSelection(1)],
            ),
            Row::line(
                "Shift-Home / Shift-End",
                "select to either end of the line",
                &[ExtendCommandSelectionToStart, ExtendCommandSelectionToEnd],
            ),
            Row::line(
                "Ctrl-Shift-C / Ctrl-Shift-V",
                "copy the selection, paste the clipboard",
                &[ClipboardCopy, ClipboardPaste],
            ),
            Row::line(
                "Ctrl-Ins / Shift-Ins",
                "the same, for a terminal that cannot send those",
                &[ClipboardCopy, ClipboardPaste],
            ),
        ],
    },
    Section {
        title: "The F-keys",
        rows: &[
            Row::panel("F1", "this page", &[Help]),
            Row::panel("F2", "the user menu (not built yet)", &[UserMenu]),
            Row::panel("F3", "view the file (not built yet)", &[View]),
            Row::panel("F4", "edit the file (not built yet)", &[Edit]),
            Row::panel("F5", "copy to the other panel (not built yet)", &[Copy]),
            Row::panel("F6", "move, or rename (not built yet)", &[Move]),
            Row::panel("F7", "make a directory (not built yet)", &[MakeDir]),
            Row::panel("F8 / Del", "delete (not built yet)", &[Delete]),
            Row::panel(
                "F9",
                "the utilities, and every session by number",
                &[UtilitiesMenu],
            ),
            Row::panel("F10", "quit", &[Quit]),
            Row::panel(
                "F11",
                "full screen: no frame, black behind everything",
                &[ToggleFullscreen],
            ),
            Row::panel("F12", "the screensaver picker", &[ScreensaverMenu]),
            Row::panel(
                "Shift-F10",
                "the context menu for the entry under the cursor",
                &[ContextMenu],
            ),
            Row::panel(
                "Shift-F12",
                "a screensaver, right now; again for the next one",
                &[ScreensaverNext],
            ),
        ],
    },
    Section {
        title: "Sessions",
        rows: &[
            Row::panel(
                "Ctrl-T / Shift-Tab",
                "the session rail: enter switch, n new, r rename, d close, a beside",
                &[ToggleRail],
            ),
            Row::note(
                "Ctrl-F, in the rail",
                "search the sessions \u{2014} space folds a group, Esc clears then closes",
            ),
            Row::panel("Ctrl-N", "a new session on this directory", &[NewSession]),
            Row::panel("Ctrl-W", "close this session", &[CloseSession]),
            Row::panel(
                "Ctrl-Tab / Ctrl-PgDn",
                "the next session",
                &[CycleSession(1)],
            ),
            Row::panel(
                "Ctrl-Shift-Tab / Ctrl-PgUp",
                "the previous one",
                &[CycleSession(-1)],
            ),
            Row::panel(
                "Alt-1 … Alt-9",
                "a session by its number in the rail",
                &[SwitchSession(0)],
            ),
            Row::note("F9, then a digit", "the same, from the menu"),
        ],
    },
    Section {
        title: "The shell",
        rows: &[
            Row::panel(
                "Ctrl-O",
                "this session's shell, and back to the DMAC commander",
                &[ToggleShell],
            ),
            Row::note(
                "Ctrl-O",
                "the way out of anything hosted \u{2014} it is on the shell's bottom border too",
            ),
            Row::note(
                "Ctrl-O h / u / Tab",
                "leave the shell and open the history, the utilities, the next session",
            ),
            Row::note("Ctrl-O F1", "leave the shell and open this page"),
            Row::note(
                "F9 / F12",
                "inside a shell: the utilities, the directory history",
            ),
            Row::note(
                "Ctrl-Shift-U / Ctrl-Shift-H",
                "the same, where the terminal reports modifiers",
            ),
            Row::note("Ctrl-T", "the session rail, from inside a shell"),
            Row::note(
                "Shift-PgUp / Shift-PgDn",
                "read back through what the shell has printed",
            ),
            Row::note(
                "Ctrl-Shift-Up / Down",
                "a line at a time; Ctrl-Shift-Home / End the oldest line, and back",
            ),
            Row::note(
                "Shift + arrows",
                "select text; Ctrl-Shift-C copies it; Esc back to the live screen",
            ),
            Row::note(
                "every other key",
                "belongs to the program running in the shell",
            ),
        ],
    },
    Section {
        title: "Screensavers",
        rows: &[
            Row::note(
                "F12",
                "the picker: Enter starts the highlighted one, and so does F12 again",
            ),
            Row::note(
                "Shift-F12",
                "start one now; while one shows, F12 or Shift-F12 moves to the next",
            ),
            Row::note(
                "any other key",
                "back to work; the key itself is not passed on",
            ),
            Row::note(
                "on its own",
                "after five idle minutes: --screensaver-after sets the seconds, 0 never",
            ),
        ],
    },
    Section {
        title: "The mouse",
        rows: &[
            Row::note(
                "click",
                "focus the panel and move the cursor; double-click opens",
            ),
            Row::note("right-click", "mark the entry; drag with it to sweep marks"),
            Row::note("drag", "sweep a range of marks"),
            Row::note(
                "wheel",
                "scrolls the panel under the pointer, not the active one",
            ),
            Row::note(
                "the rail's edge",
                "drag it to resize the rail, open or closed",
            ),
            Row::note("the F-key bar", "each key is a button"),
        ],
    },
    Section {
        title: "Your terminal",
        rows: &[
            Row::note(
                "Apple Terminal",
                "cannot send Ctrl with Shift: use the Ctrl-O chords, F9 and F12",
            ),
            Row::note("kitty, Ghostty, WezTerm, iTerm2", "everything above works"),
            Row::note("docs/TERMINAL-KEYS.md", "the whole story, key by key"),
        ],
    },
];

/// The key column is as wide as the widest key, up to a limit past which a
/// long spelling is allowed to push its own description along instead of
/// pushing every other row's out.
pub const KEY_COLUMN_MAX: usize = 28;

/// Width of the key column, in characters.
pub fn key_width() -> usize {
    SECTIONS
        .iter()
        .flat_map(|s| s.rows)
        .map(|r| r.keys.chars().count())
        .max()
        .unwrap_or(8)
        .min(KEY_COLUMN_MAX)
}

/// The page as plain text: a title on its own line, then each row indented
/// with the key in a column and the description after a gap of at least two
/// spaces, and a blank line between sections. What the `cannon` screensaver
/// is handed; it tells the keys from the prose by exactly that layout.
pub fn plain_lines() -> Vec<String> {
    let width = key_width();
    let mut out = Vec::new();
    for (i, section) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push(String::new());
        }
        out.push(section.title.to_string());
        for row in section.rows {
            out.push(format!("  {:<width$}  {}", row.keys, row.what));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// Read a key as the page spells it: `Ctrl-Shift-Tab`, `Shift-F3`, `Alt-.`,
    /// `+`. Anything after an ellipsis is a range and is not read.
    fn parse(spelling: &str) -> Option<KeyEvent> {
        let spelling = spelling.split('\u{2026}').next()?.trim();
        let mut chars = spelling.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Some(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let mut parts: Vec<&str> = spelling.split('-').collect();
        let last = parts.pop()?;
        let mut mods = KeyModifiers::NONE;
        for m in parts {
            mods |= match m {
                "Ctrl" => KeyModifiers::CONTROL,
                "Shift" => KeyModifiers::SHIFT,
                "Alt" => KeyModifiers::ALT,
                _ => return None,
            };
        }
        let code = match last {
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "PgUp" => KeyCode::PageUp,
            "PgDn" => KeyCode::PageDown,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "Enter" => KeyCode::Enter,
            "Esc" => KeyCode::Esc,
            "Tab" => KeyCode::Tab,
            "Backspace" => KeyCode::Backspace,
            "Ins" => KeyCode::Insert,
            "Del" => KeyCode::Delete,
            f if f.len() > 1 && f.starts_with('F') => KeyCode::F(f[1..].parse().ok()?),
            c if c.chars().count() == 1 => KeyCode::Char(c.chars().next()?.to_ascii_lowercase()),
            _ => return None,
        };
        Some(KeyEvent::new(code, mods))
    }

    /// Every key the page names does what the page says. This is the test
    /// that makes the page trustworthy: change a binding and it fails here
    /// until the page is changed too.
    #[test]
    fn the_page_does_not_lie() {
        let mut checked = 0;
        for section in SECTIONS {
            for row in section.rows {
                if row.actions.is_empty() {
                    continue;
                }
                let spellings: Vec<&str> = row.keys.split(" / ").collect();
                if row.actions.len() > 1 {
                    assert_eq!(
                        row.actions.len(),
                        spellings.len(),
                        "{:?}: one action per spelling, or one for all",
                        row.keys
                    );
                }
                for (i, spelling) in spellings.iter().enumerate() {
                    let key = parse(spelling)
                        .unwrap_or_else(|| panic!("{:?}: cannot read {spelling:?}", row.keys));
                    let expected = if row.actions.len() == 1 {
                        &row.actions[0]
                    } else {
                        &row.actions[i]
                    };
                    assert_eq!(
                        keymap::resolve(key, row.focus).as_ref(),
                        Some(expected),
                        "{}: {spelling} ({:?})",
                        row.keys,
                        row.focus
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked > 60,
            "only {checked} keys checked — the page shrank?"
        );
    }

    /// The parser reads what the page writes, and not what it does not.
    #[test]
    fn the_spellings_parse_as_intended() {
        assert_eq!(
            parse("Ctrl-Shift-Tab"),
            Some(KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
        );
        assert_eq!(
            parse("Shift-F3"),
            Some(KeyEvent::new(KeyCode::F(3), KeyModifiers::SHIFT))
        );
        assert_eq!(
            parse("Alt-."),
            Some(KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT))
        );
        assert_eq!(
            parse("-"),
            Some(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::NONE))
        );
        assert_eq!(
            parse("Alt-1 \u{2026} Alt-9"),
            Some(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT))
        );
        assert_eq!(parse("Cmd-H"), None, "a modifier the keymap cannot take");
        assert_eq!(parse("a letter"), None);
    }

    /// The plain text keeps the shape the cannon reads: titles flush left,
    /// rows indented, the key and the prose two spaces apart at least.
    #[test]
    fn the_plain_page_keeps_its_shape() {
        let lines = plain_lines();
        assert!(lines.len() > 40);
        let mut titles = 0;
        for line in &lines {
            assert!(!line.contains('\t'));
            if line.is_empty() {
                continue;
            }
            if !line.starts_with(' ') {
                titles += 1;
                continue;
            }
            assert!(line.starts_with("  "), "{line:?}");
            assert!(line.contains("  "), "{line:?}: no gap before the prose");
        }
        assert_eq!(titles, SECTIONS.len());
        assert_eq!(lines[0], SECTIONS[0].title);
    }

    /// Every row's key fits the column, save for the deliberately long ones.
    #[test]
    fn the_key_column_is_bounded() {
        assert!(key_width() <= KEY_COLUMN_MAX);
        assert!(key_width() >= 10);
    }
}

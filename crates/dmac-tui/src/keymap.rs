//! Key to [`Action`] resolution.
//!
//! The default preset is `norton`: the F-key row and the selection keys are
//! exactly where a Norton Commander user's fingers expect them. Modern additions
//! live on keys the canon never claimed.

use crate::action::Action;
use crate::app::Focus;
use dmac_core::SortKey;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Resolve a key press for the current focus.
///
/// Focus is a parameter rather than something the caller patches up afterwards,
/// because the same key genuinely means different things: `Enter` opens a
/// directory on a panel and runs a command on the command line, and a letter is
/// an incremental search in one place and text in the other.
pub fn resolve(key: KeyEvent, focus: Focus) -> Option<Action> {
    use KeyCode::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    let action = match (key.code, ctrl, alt) {
        // --- Modified F-keys must come before the bare arms below, or the
        //     bare arms shadow them and the binding silently never fires. ---

        // Screensaver picker. Ctrl+Shift+F1 is what was asked for; F12 is kept
        // as a plain-key equivalent because a good many terminals do not encode
        // Ctrl+Shift with an F-key at all, and a binding you cannot press is
        // not a binding.
        (F(10), _, _) if shift => Action::ContextMenu,
        (F(1), true, _) if shift => Action::ScreensaverMenu,
        (F(12), _, _) => Action::ScreensaverMenu,

        // Sorting on Shift+F3..F6.
        (F(3), _, _) if shift => Action::SortBy(SortKey::Name),
        (F(4), _, _) if shift => Action::SortBy(SortKey::Extension),
        (F(5), _, _) if shift => Action::SortBy(SortKey::Modified),
        (F(6), _, _) if shift => Action::SortBy(SortKey::Size),

        // --- The F-key row: the bottom bar is the documentation. ---
        (F(1), _, _) => Action::Help,
        (F(2), _, _) => Action::UserMenu,
        (F(3), _, _) => Action::View,
        (F(4), _, _) => Action::Edit,
        (F(5), _, _) => Action::Copy,
        (F(6), _, _) => Action::Move,
        (F(7), _, _) => Action::MakeDir,
        (F(8), _, _) | (Delete, false, false) => Action::Delete,
        (F(9), _, _) => Action::Menu,
        (F(10), _, _) => Action::Quit,

        // --- Navigation ---
        (Up, false, false) => Action::CursorUp,
        (Down, false, false) => Action::CursorDown,
        (PageUp, false, false) => Action::PageUp,
        (PageDown, false, false) => Action::PageDown,
        (KeyCode::Home, false, false) => Action::GoTop,
        (KeyCode::End, false, false) => Action::GoBottom,

        // --- Focus. Tab cycles all three stops; Esc jumps between the current
        //     panel and the command line, ignoring the other panel. ---
        (Tab, _, _) => Action::FocusNext,
        (Esc, _, _) => Action::FocusToggle,

        (Char('u'), true, false) => Action::SwapPanels,
        (Char('o'), true, false) => Action::TogglePanels,
        (Char('r'), true, false) => Action::Refresh,
        // Backspace goes up a directory only with a modifier now, because plain
        // Backspace edits the quick-search buffer while a panel has focus.
        (Backspace, true, false) | (Backspace, false, true) => Action::GoParent,

        // --- Selection. Ins sweeps; Gray +/-/* are the classic mask keys. ---
        (Insert, _, _) => Action::ToggleSelection,
        (Char('*'), false, false) => Action::InvertSelection,
        (Char('+'), false, false) => Action::SelectAll,
        (Char('-'), false, false) => Action::ClearSelection,

        (Char('h'), true, false) => Action::ToggleHidden,

        // --- Modern additions, on keys the canon left free. ---
        (Char('p'), true, true) => Action::Unimplemented("command palette"),
        (Char('f'), true, false) => Action::Unimplemented("fuzzy find"),
        (Char('a'), true, true) => Action::Unimplemented("AI chat window"),

        // --- Everything below depends on where the keyboard is. ---
        _ => return resolve_focused(key, focus),
    };
    Some(action)
}

/// Keys whose meaning depends on focus.
fn resolve_focused(key: KeyEvent, focus: Focus) -> Option<Action> {
    use KeyCode::*;
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let action = match focus {
        Focus::CommandLine => match (key.code, ctrl, alt) {
            (Char('y'), true, false) => Action::CommandClear,
            (Backspace, _, _) => Action::CommandBackspace,
            (Enter, _, _) => Action::CommandSubmit,
            (Char(c), false, false) => Action::CommandChar(c),
            _ => return None,
        },

        Focus::Panel => match (key.code, ctrl, alt) {
            (Enter, false, false) => Action::Activate,
            (Backspace, false, false) => Action::QuickSearchBackspace,
            // Selection keys keep their meaning; everything else printable is a
            // search. `*`, `+` and `-` are matched earlier, above this function.
            (Char(c), false, false) => Action::QuickSearch(c),
            _ => return None,
        },
    };
    Some(action)
}

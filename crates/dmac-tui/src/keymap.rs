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

        // --- Shift+cursor on the command line selects text there. Left and
        //     Right are free everywhere else, so only Home and End have to ask
        //     where the keyboard is. ---
        (Left, false, false) if shift => Action::ExtendCommandSelection(-1),
        (Right, false, false) if shift => Action::ExtendCommandSelection(1),
        (KeyCode::Home, false, false) if shift && focus == Focus::CommandLine => {
            Action::ExtendCommandSelectionToStart
        }
        (KeyCode::End, false, false) if shift && focus == Focus::CommandLine => {
            Action::ExtendCommandSelectionToEnd
        }

        // --- Shift+cursor: anchored multi-selection. Must precede the plain
        //     navigation arms below, or they shadow it. ---
        (Up, false, false) if shift => Action::ExtendSelection(-1),
        (Down, false, false) if shift => Action::ExtendSelection(1),
        (PageUp, false, false) if shift => Action::ExtendSelectionPage(-1),
        (PageDown, false, false) if shift => Action::ExtendSelectionPage(1),
        (KeyCode::Home, false, false) if shift => Action::ExtendSelectionToTop,
        (KeyCode::End, false, false) if shift => Action::ExtendSelectionToBottom,

        // --- Navigation ---
        (Up, false, false) => Action::CursorUp,
        (Down, false, false) => Action::CursorDown,
        (PageUp, false, false) => Action::PageUp,
        (PageDown, false, false) => Action::PageDown,
        (KeyCode::Home, false, false) => Action::GoTop,
        (KeyCode::End, false, false) => Action::GoBottom,

        // Full screen. Global on purpose: it is a property of the display, so
        // it has to work from the shell view too, which costs the hosted
        // program one key it almost never wants.
        (F(11), _, _) => Action::ToggleFullscreen,

        // --- Sessions ---
        // Ctrl+Tab forward, Ctrl+Shift+Tab back, as in every browser and IDE.
        // Both spellings of the shifted one, because terminals disagree about
        // whether it arrives as Tab-with-Shift or as BackTab.
        (Tab, true, _) if shift => Action::CycleSession(-1),
        (BackTab, true, _) => Action::CycleSession(-1),
        (Tab, true, _) => Action::CycleSession(1),
        (BackTab, _, _) => Action::ToggleRail,
        (Tab, _, _) if shift => Action::ToggleRail,
        // Ctrl-T as a plain-key equivalent: Shift+Tab is claimed by a number of
        // terminals (Warp among them) before it ever reaches the application,
        // and a binding you cannot press is not a binding. `t` for the tab-like
        // strip of sessions it opens.
        (Char('t'), true, false) => Action::ToggleRail,
        // Alt+1..9. Digits, not F-keys: they are the numbers shown in the rail.
        (Char(c @ '1'..='9'), false, true) => {
            Action::SwitchSession(c.to_digit(10).unwrap_or(1) as usize - 1)
        }
        (Char('n'), true, false) => Action::NewSession,
        // Ctrl-W closes a session, not Ctrl-X: Ctrl-X is `cut`, and a key that
        // sometimes cuts a file and sometimes closes a workspace will eventually
        // close the wrong thing. See docs/PLAN.md, M4.
        (Char('w'), true, false) => Action::CloseSession,
        (PageUp, true, false) => Action::CycleSession(-1),
        (PageDown, true, false) => Action::CycleSession(1),

        // --- Focus. Tab cycles all three stops; Esc jumps between the current
        //     panel and the command line, ignoring the other panel. ---
        (Tab, _, _) => Action::FocusNext,
        (Esc, _, _) => Action::FocusToggle,

        // Cmd-U as asked for, and Ctrl-U because Cmd never reaches a terminal
        // application unless the terminal is configured to forward it — a
        // binding you cannot press is not a binding. Swapping the panels, which
        // is Ctrl-U in the canon, moves to Alt-U.
        (Char('u'), _, _) if key.modifiers.contains(KeyModifiers::SUPER) => Action::UtilitiesMenu,
        (Char(c), true, false) if shift && c.eq_ignore_ascii_case(&'u') => Action::UtilitiesMenu,
        (Char('u'), true, false) => Action::UtilitiesMenu,
        (Char('u'), false, true) => Action::SwapPanels,
        (Char('o'), true, false) => Action::ToggleShell,
        (Char('r'), true, false) => Action::Refresh,
        // Backspace goes up a directory. Plain Backspace does too, unless a
        // quick search is in progress — then it deletes a character, and goes
        // up once the buffer is empty again.
        //
        // Ctrl-Backspace is the history, not the parent: a terminal that cannot
        // report Ctrl-H sends it as exactly this, and the two must agree or the
        // binding would depend on which terminal you happened to open.
        (Backspace, true, false) => Action::DirectoryHistory,
        (Backspace, false, true) => Action::GoParent,

        // --- The clipboard. Ctrl-Shift-C/V is what every modern terminal uses
        //     and what people try first; Ctrl-Insert / Shift-Insert is what
        //     encodes reliably in terminals that cannot report Ctrl with Shift
        //     at all. Both, because either alone leaves someone stuck.
        //
        //     The Shift is load-bearing: without it Ctrl-C is the interrupt, and
        //     a file manager that swallowed Ctrl-C would make its hosted shell
        //     impossible to get out of. Terminals that cannot distinguish the
        //     two send plain Ctrl-C, which goes to the child, as it must.
        (Char(c), true, false) if shift && c.eq_ignore_ascii_case(&'c') => Action::ClipboardCopy,
        (Char(c), true, false) if shift && c.eq_ignore_ascii_case(&'v') => Action::ClipboardPaste,
        // Cmd-C and Cmd-V, for terminals that forward the Command key rather
        // than keeping it. Most keep it — and when they do, their own paste
        // arrives as a bracketed paste event instead, which is handled too.
        (Char(c), _, _)
            if key.modifiers.contains(KeyModifiers::SUPER) && c.eq_ignore_ascii_case(&'c') =>
        {
            Action::ClipboardCopy
        }
        (Char(c), _, _)
            if key.modifiers.contains(KeyModifiers::SUPER) && c.eq_ignore_ascii_case(&'v') =>
        {
            Action::ClipboardPaste
        }
        (Insert, true, false) => Action::ClipboardCopy,
        (Insert, false, false) if shift => Action::ClipboardPaste,

        // --- Selection. Ins works from anywhere; the classic mask keys are
        //     resolved under panel focus only, because `*`, `+` and `-` are
        //     ordinary characters when you are typing a command. Binding them
        //     here would silently eat every hyphen in a command line. ---
        (Insert, _, _) => Action::ToggleSelection,

        // Ctrl-H is the directory history, and Ctrl-Shift-H is the same thing
        // from inside a hosted shell, where a bare Ctrl-H belongs to the child.
        // Hidden files move to Alt-H and Alt-period, which is where `mc` has
        // always kept them.
        //
        // Ctrl-Shift-H, and Cmd-H or Cmd-Shift-H where the Command key is
        // forwarded rather than kept by the terminal. None of these is the
        // binding to rely on everywhere: plain Ctrl-H is byte 0x08, which is
        // also Backspace, so only a terminal that reports modifiers separately
        // can tell them apart — and Apple's Terminal cannot. Where they cannot
        // arrive, `Ctrl-O h` and F12 do the same job; see `App::on_key`.
        (Char(c), true, false) if shift && c.eq_ignore_ascii_case(&'h') => Action::DirectoryHistory,
        (Char(c), _, _)
            if key.modifiers.contains(KeyModifiers::SUPER) && c.eq_ignore_ascii_case(&'h') =>
        {
            Action::DirectoryHistory
        }
        (Char('h'), true, false) => Action::DirectoryHistory,
        (Char('h'), false, true) | (Char('.'), false, true) => Action::ToggleHidden,

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
            // The Norton mask keys, live only while a panel has the keyboard.
            (Char('*'), false, false) => Action::InvertSelection,
            (Char('+'), false, false) => Action::SelectAll,
            (Char('-'), false, false) => Action::ClearSelection,
            (Backspace, false, false) => Action::QuickSearchBackspace,
            // Selection keys keep their meaning; everything else printable is a
            // search. `*`, `+` and `-` are matched earlier, above this function.
            (Char(c), false, false) => Action::QuickSearch(c),
            _ => return None,
        },
    };
    Some(action)
}

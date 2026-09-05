//! What the user asked for, decoupled from which key they pressed.
//!
//! Keys map to actions in the keymap; nothing outside the keymap resolver ever
//! matches on a key code. This is what makes every binding rebindable and what
//! lets `norton`, `far`, `mc` and `vim` presets coexist.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // Navigation
    CursorUp,
    CursorDown,
    PageUp,
    PageDown,
    GoTop,
    GoBottom,
    /// Enter a directory, or open the file under the cursor.
    Activate,
    GoParent,

    // Focus. The keyboard is either on a panel or on the command line, never
    // ambiguously between them.
    /// Tab: cycle left panel -> right panel -> command line -> left panel.
    FocusNext,
    /// Esc: jump straight between the *current* panel and the command line,
    /// without disturbing which panel is current.
    FocusToggle,

    // Sessions. Several are live at once; switching does not reload anything.
    /// Shift-Tab: show or hide the session rail.
    ToggleRail,
    /// Alt+1..9: jump straight to a session by position.
    SwitchSession(usize),
    /// Ctrl-N: a new session on the current directory.
    NewSession,
    /// Ctrl-W: close the current session. Refused when it is the last one.
    CloseSession,
    /// Ctrl-PageUp / Ctrl-PageDown: previous / next session.
    CycleSession(isize),

    // Panels
    SwitchPanel,
    SwapPanels,
    /// Ctrl-O: swap between the panels and the session's shell. In Norton
    /// Commander this hid the panels to reveal the shell underneath; here there
    /// is a real shell to reveal.
    ToggleShell,

    // Selection
    /// Shift+Up/Down: grow or shrink an anchored selection by rows.
    ExtendSelection(isize),
    /// Shift+PageUp/PageDown: the same by whole screens.
    ExtendSelectionPage(isize),
    /// Shift+Home / Shift+End: extend all the way to one end.
    ExtendSelectionToTop,
    ExtendSelectionToBottom,

    ToggleSelection,
    InvertSelection,
    SelectAll,
    ClearSelection,

    // Sorting and view
    SortBy(dmac_core::SortKey),
    ToggleHidden,
    Refresh,

    // The F-key row. These are a contract with thirty years of muscle memory.
    Help,
    UserMenu,
    View,
    Edit,
    Copy,
    Move,
    MakeDir,
    Delete,
    Menu,
    Quit,

    /// Contextual commands for whatever the cursor is on. Right-click, or
    /// Shift+F10 — the same binding Windows has used since 1995.
    ContextMenu,

    /// Open the screensaver picker: every effect plus the games, and the
    /// rotation. Ctrl+Shift+F1, with F12 as the reliably-encodable equivalent.
    ScreensaverMenu,

    /// Typed text going to the command line — only while it has focus.
    CommandChar(char),
    CommandBackspace,
    CommandSubmit,
    /// Ctrl-Y: wipe the command line, as in Far Manager.
    CommandClear,

    /// A character typed while a panel has focus: incremental search within the
    /// listing. With focus made explicit, letters can finally mean this.
    QuickSearch(char),
    QuickSearchBackspace,

    /// Something is bound but not implemented yet; reported in the status line
    /// rather than silently ignored, so gaps are visible instead of confusing.
    Unimplemented(&'static str),
}

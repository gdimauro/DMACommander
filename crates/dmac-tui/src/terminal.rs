//! Terminal setup and — far more importantly — teardown.
//!
//! A file manager that leaves the shell in raw mode after a crash is a file
//! manager people uninstall. The guard here restores the terminal on drop, on
//! panic, and on a fatal signal, in that order of likelihood.

use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::{
    cursor::SetCursorStyle,
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether we pushed keyboard-enhancement flags, so teardown only pops what it
/// pushed. A stray pop corrupts the stack of whatever runs next in this terminal.
static PUSHED_KEYBOARD_FLAGS: AtomicBool = AtomicBool::new(false);

/// How the text cursor should look on the command line.
///
/// `Software` is the escape hatch: some terminals ignore DECSCUSR entirely, or
/// override it with their own cursor preference, and then nothing the
/// application asks for has any effect. In that mode we stop asking and draw the
/// cursor ourselves, which works everywhere because it is just an inverted cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorStyle {
    #[default]
    BlinkingBlock,
    BlinkingBar,
    BlinkingUnderline,
    SteadyBlock,
    Software,
}

impl CursorStyle {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "blinking-block" | "block" => Self::BlinkingBlock,
            "blinking-bar" | "bar" => Self::BlinkingBar,
            "blinking-underline" | "underline" => Self::BlinkingUnderline,
            "steady-block" | "steady" => Self::SteadyBlock,
            "software" => Self::Software,
            _ => return None,
        })
    }

    pub const NAMES: &'static [&'static str] = &[
        "blinking-block",
        "blinking-bar",
        "blinking-underline",
        "steady-block",
        "software",
    ];

    fn decscusr(self) -> Option<SetCursorStyle> {
        Some(match self {
            Self::BlinkingBlock => SetCursorStyle::BlinkingBlock,
            Self::BlinkingBar => SetCursorStyle::BlinkingBar,
            Self::BlinkingUnderline => SetCursorStyle::BlinkingUnderScore,
            Self::SteadyBlock => SetCursorStyle::SteadyBlock,
            // Nothing to ask the terminal for; we draw it ourselves.
            Self::Software => return None,
        })
    }

    /// Whether the application draws the cursor itself.
    pub fn is_software(self) -> bool {
        self == Self::Software
    }
}

/// Ask the terminal for this cursor shape.
///
/// Called on every frame that shows a cursor, not once at startup: entering the
/// alternate screen, a terminal's own redraw, or a hosted program can all reset
/// DECSCUSR, and a cursor that silently stops blinking is a bug people notice
/// and cannot explain. Re-asking costs a handful of bytes.
pub fn apply_cursor_style(style: CursorStyle) {
    if let Some(seq) = style.decscusr() {
        let _ = execute!(stdout(), seq);
    }
}

/// Whether we asked for the kitty keyboard protocol.
///
/// Note what this does *not* say: whether the terminal honoured the request.
/// Finding that out requires querying the terminal and waiting for a reply, and
/// a terminal that does not implement the protocol never replies — so the query
/// costs a full timeout in exactly the terminals where the answer is "no". That
/// is far too expensive for the startup path; `crate::doctor` runs the query on
/// demand instead.
pub fn requested_enhanced_keyboard() -> bool {
    PUSHED_KEYBOARD_FLAGS.load(Ordering::Relaxed)
}
use std::io::{Stdout, stdout};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Owns the terminal's modified state. Constructing it enters the alternate
/// screen; dropping it leaves, whatever the reason.
pub struct TerminalGuard {
    terminal: Tui,
}

impl TerminalGuard {
    pub fn enter(cursor: CursorStyle) -> anyhow::Result<Self> {
        install_panic_hook();

        enable_raw_mode()?;
        execute!(
            stdout(),
            EnterAlternateScreen,
            // Includes any-motion tracking, which is what lets us draw the
            // pointer the way DOS did — by inverting the cell under it.
            EnableMouseCapture,
            EnableBracketedPaste,
            // Lets us stop animating when the user tabs away — the screensaver
            // and the dock both depend on knowing we are unfocused.
            EnableFocusChange,
        )?;
        apply_cursor_style(cursor);

        // The kitty keyboard protocol. This is what makes modified function keys
        // and Ctrl+Shift combinations arrive at all, and it lets a real Esc be
        // told apart from the start of an escape sequence — which matters here,
        // because Esc switches focus.
        //
        // Pushed without asking first, deliberately. `supports_keyboard_enhancement`
        // sends a query and waits for a reply, and a terminal that does not
        // implement the protocol never replies — so it burns the full 2s timeout
        // in precisely the terminals where the answer is "no". That measured 2.0s
        // to first frame against an 80ms budget. The push itself is an ordinary
        // CSI sequence, which terminals that do not understand it discard.
        if !std::env::var("DMAC_NO_KEYBOARD_ENHANCEMENT").is_ok_and(|v| v != "0") {
            let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES;
            if execute!(stdout(), PushKeyboardEnhancementFlags(flags)).is_ok() {
                PUSHED_KEYBOARD_FLAGS.store(true, Ordering::Relaxed);
            }
        }

        let terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
        Ok(Self { terminal })
    }

    pub fn terminal(&mut self) -> &mut Tui {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Undo everything [`TerminalGuard::enter`] did. Safe to call twice, and safe to
/// call from a panic hook — every step ignores its error, because a half-restored
/// terminal is still better than an early return that restores nothing.
pub fn restore() {
    // Pop before anything else: the flags affect how the terminal encodes keys,
    // and leaving them set breaks whatever the user runs next.
    if PUSHED_KEYBOARD_FLAGS.swap(false, Ordering::Relaxed) {
        let _ = execute!(stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableFocusChange,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
    );
    let _ = disable_raw_mode();
}

/// Restore the terminal *before* the default hook prints the panic message,
/// so the backtrace lands on a usable screen instead of scrolling sideways
/// through the alternate buffer.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        default(info);
    }));
}

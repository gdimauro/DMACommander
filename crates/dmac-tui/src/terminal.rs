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
    terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
        supports_keyboard_enhancement,
    },
};
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether we pushed keyboard-enhancement flags, so teardown only pops what it
/// pushed. A stray pop corrupts the stack of whatever runs next in this terminal.
static PUSHED_KEYBOARD_FLAGS: AtomicBool = AtomicBool::new(false);

/// Did the terminal accept the kitty keyboard protocol?
///
/// Without it, a terminal cannot tell `Shift+F3` from `F3`, or `Ctrl+Shift+F1`
/// from nothing at all — which is why those bindings ship with plain-key
/// equivalents. Reported in the UI so a missing key has a visible explanation
/// instead of looking like a bug.
pub fn enhanced_keyboard() -> bool {
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
    pub fn enter() -> anyhow::Result<Self> {
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
            // A blinking cursor, shown only where text is actually being edited.
            SetCursorStyle::BlinkingBlock,
        )?;

        // The kitty keyboard protocol, where the terminal supports it. This is
        // what makes modified function keys and Ctrl+Shift combinations arrive
        // at all, and it disambiguates a real Esc from the start of an escape
        // sequence — which matters here, because Esc switches focus.
        //
        // Ask first: pushing flags a terminal does not understand leaves visible
        // garbage on the screen.
        if supports_keyboard_enhancement().unwrap_or(false) {
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

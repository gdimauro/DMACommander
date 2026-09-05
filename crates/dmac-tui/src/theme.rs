//! Colours.
//!
//! The default is the Norton Commander palette people remember: white on blue,
//! cyan directories, yellow highlights. It is not nostalgia — a high-contrast
//! two-colour panel is genuinely the fastest thing to scan.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub panel_bg: Color,
    pub panel_fg: Color,
    pub panel_border: Color,
    /// The border of the panel that has focus. The single most important visual
    /// cue in the whole UI: the active panel is the source of every operation.
    pub panel_border_active: Color,
    pub dir_fg: Color,
    pub symlink_fg: Color,
    pub executable_fg: Color,
    pub selected_fg: Color,
    pub cursor_bg: Color,
    pub cursor_fg: Color,
    pub fkey_label_fg: Color,
    pub fkey_label_bg: Color,
    pub fkey_name_fg: Color,
    pub fkey_name_bg: Color,
    pub status_fg: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::norton()
    }
}

impl Theme {
    pub fn norton() -> Self {
        Self {
            panel_bg: Color::Blue,
            panel_fg: Color::White,
            panel_border: Color::White,
            panel_border_active: Color::Yellow,
            dir_fg: Color::White,
            symlink_fg: Color::Cyan,
            executable_fg: Color::LightGreen,
            selected_fg: Color::Yellow,
            cursor_bg: Color::Cyan,
            cursor_fg: Color::Black,
            fkey_label_fg: Color::White,
            fkey_label_bg: Color::Reset,
            fkey_name_fg: Color::Black,
            fkey_name_bg: Color::Cyan,
            status_fg: Color::Gray,
        }
    }

    pub fn panel(&self) -> Style {
        Style::default().fg(self.panel_fg).bg(self.panel_bg)
    }

    pub fn border(&self, active: bool) -> Style {
        let c = if active {
            self.panel_border_active
        } else {
            self.panel_border
        };
        let s = Style::default().fg(c).bg(self.panel_bg);
        if active {
            s.add_modifier(Modifier::BOLD)
        } else {
            s
        }
    }

    /// The colour of one row, before the cursor highlight is applied.
    pub fn entry(&self, e: &dmac_core::Entry) -> Style {
        use dmac_core::EntryKind::*;
        let base = Style::default().bg(self.panel_bg);
        if e.selected {
            return base.fg(self.selected_fg).add_modifier(Modifier::BOLD);
        }
        match e.kind {
            Dir | Parent => base.fg(self.dir_fg).add_modifier(Modifier::BOLD),
            Symlink => base.fg(self.symlink_fg),
            File if is_executable(e) => base.fg(self.executable_fg),
            _ => base.fg(self.panel_fg),
        }
    }

    pub fn cursor(&self) -> Style {
        Style::default().fg(self.cursor_fg).bg(self.cursor_bg)
    }

    /// The cursor row of the active panel while the keyboard is on the command
    /// line. Still visible — you must be able to see what an operation would act
    /// on — but visibly not where the keys are going.
    pub fn cursor_unfocused(&self) -> Style {
        Style::default()
            .fg(self.panel_fg)
            .bg(self.cursor_bg)
            .add_modifier(Modifier::DIM)
    }

    /// The cell under the mouse pointer. Text mode had no sprite: DOS drew the
    /// pointer by inverting the attribute of the cell it was over, and that is
    /// exactly what this reproduces.
    pub fn mouse_pointer(&self, under: Style) -> Style {
        Style::default()
            .fg(under.bg.unwrap_or(self.panel_bg))
            .bg(under.fg.unwrap_or(self.panel_fg))
    }
}

/// Any execute bit. Windows entries have no mode, so nothing is highlighted
/// there rather than guessing from the extension.
fn is_executable(e: &dmac_core::Entry) -> bool {
    e.mode.is_some_and(|m| m & 0o111 != 0)
}

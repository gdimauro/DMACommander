//! Rendering. Every function here is a pure function of [`App`] state — nothing
//! is stored in the widgets, which is what lets a second backend draw the exact
//! same frame from the exact same state.

mod fkeybar;
pub(crate) mod help;
pub(crate) mod history;
pub(crate) mod menu;
mod panel;
pub(crate) mod picker;
pub(crate) mod prompt;
pub(crate) mod rail;
pub(crate) mod reattach;
mod screen;
pub(crate) mod shell;
mod splash;
pub(crate) mod view;

pub use screen::draw_canvas;

use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

/// Below this a panel cannot show a name together with its size and date, and
/// two of them side by side show neither.
///
/// Deliberately low. Norton Commander drew two panels side by side on an 80x25
/// screen and so must this: 80 columns is the canonical size, not a narrow
/// window, and stacking there would be a surprise rather than a rescue. The
/// size and date field is about 22 columns, so at this width a name still gets
/// ten — cramped, which is what 80 columns has always been, and legible.
const MIN_PANEL_WIDTH: u16 = 34;

/// ...but stacking costs rows, and below this there are not enough to split.
const MIN_STACK_HEIGHT: u16 = 16;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // The screensaver owns the whole screen when it is running — no panels, no
    // F-key bar. Anything less is a screensaver that does not save the screen.
    if let Some(canvas) = app.screensaver_canvas(area.width, area.height) {
        draw_canvas(frame, area, canvas);
        return;
    }

    // Full screen takes the frame off and leaves the contents: no borders, no
    // F-key bar, black behind everything. The command line stays either way —
    // it is not decoration, it is where you type.
    let full = app.fullscreen;
    let theme = if full {
        app.theme.blacked_out()
    } else {
        app.theme.clone()
    };
    if full {
        // The buffer starts unstyled, so anything not covered would show the
        // host terminal's own background through it.
        frame.render_widget(
            ratatui::widgets::Block::default()
                .style(Style::default().bg(ratatui::style::Color::Black)),
            area,
        );
    }

    // In the shell view both bottom rows go. A second prompt under a shell's
    // own prompt is two places to type with no way to tell which is listening,
    // and an F-key legend for keys the shell has taken is a legend that lies.
    // What stays is one row of status, and only when there is something to say.
    let in_shell = app.sessions.current().view == dmac_session::View::Shell;
    let command_rows = u16::from(!in_shell);
    let status_rows = u16::from(in_shell && !app.status.is_empty());
    // The history puts its three orders on the F-keys, so its bar is shown even
    // where there would normally be none: over a shell, and in full screen.
    let history_open = matches!(app.mode, crate::app::Mode::History { .. });
    let fkey_rows = u16::from(history_open || (!full && !in_shell));

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),                             // panels or shell
            Constraint::Length(command_rows + status_rows), // command line
            Constraint::Length(fkey_rows),                  // F-key bar
        ])
        .split(area);

    // The rail pushes the panels rather than covering them, so both stay
    // readable and a file can eventually be dragged from a panel onto a session.
    let rail_open = app.rail_open;
    // The collapsed strip is a hint, and a hint is frame. Opened deliberately,
    // the rail is contents and stays.
    let rail_w = if full && !rail_open {
        0
    } else {
        rail::width(rail_open, app.sessions.rail, rows[0].width)
    };
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(rail_w), Constraint::Min(0)])
        .split(rows[0]);

    // The rail reads *every* session, so it is drawn first, from a shared
    // borrow — before the current session is borrowed mutably below.
    // While the rail is being driven, the highlight is where the keyboard is —
    // which may be a session you have not switched to yet.
    let rail_cursor = match app.mode {
        crate::app::Mode::Rail { selected } => Some(selected),
        _ => None,
    };
    // The outer rect as well as the interior: the last column of it is the grip
    // the pointer drags to resize, and the interior does not include it.
    app.layout.rail_outer = body[0];
    app.layout.rail = rail::draw(frame, body[0], &app.sessions, rail_cursor, &theme);
    let software_cursor = app.software_cursor();
    let shell_selection = app.shell_selection;
    let shell_caret = app.shell_caret;

    // Split the borrow: the current session's panels are drawn mutably (they
    // record their viewport height) while the theme is read.
    let App {
        sessions, layout, ..
    } = app;
    let theme = &theme;
    let session = sessions.current_mut();
    let (active, focus, panels_hidden) = (session.active, session.focus, session.panels_hidden);

    layout.fkeys = rows[2];
    layout.command = rows[1];
    layout.screen = area;

    // The shell replaces the panels rather than sitting beside them: it is the
    // same session seen a different way, and splitting the screen would give
    // both halves too little room to be useful.
    if session.view == dmac_session::View::Shell {
        if let Some(sh) = session.shell.as_ref() {
            // Asked of the system, not remembered: the commander only knows
            // about the `cd`s it typed itself, and the whole point of a shell
            // is that you type your own.
            let here = sh
                .cwd()
                .map(|p| dmac_session::abbreviate_home(&p.display().to_string()));
            let panel = dmac_session::abbreviate_home(
                &session.cwd[dmac_session::Session::index_of(session.active)].display(),
            );
            // Only when they differ, which is only when something is running:
            // a shell at a prompt has already been sent after the panels.
            let pending = (here.as_deref() != Some(panel.as_str())).then_some(panel.as_str());
            layout.shell = shell::draw(
                frame,
                body[1],
                sh,
                &shell::Chrome {
                    focused: true,
                    bordered: !full,
                    software_cursor,
                    selection: shell_selection,
                    caret: shell_caret,
                    cwd: here.as_deref(),
                    pending,
                    // The rail is a strip of dots in this view; the border is
                    // where the session gets to say its name.
                    session: Some((session.name.as_str(), rail::colour(session.color))),
                },
                theme,
            );
            layout.panels = [ratatui::layout::Rect::default(); 2];
        }
    } else if !panels_hidden {
        let panels = &mut session.panels;
        // Narrow enough and the two panels stop being two panels: a name, a
        // size and a date do not fit in half of a small window, so both columns
        // show truncated names and nothing else. Stacked, each keeps the full
        // width and pays in rows instead — the cheaper thing to lose, because a
        // listing scrolls and a filename does not.
        //
        // Only while there are rows to spend, though: two panels three rows
        // tall are worse than two narrow ones.
        let stacked = body[1].width < MIN_PANEL_WIDTH * 2 && body[1].height >= MIN_STACK_HEIGHT;
        let cols = Layout::default()
            .direction(if stacked {
                Direction::Vertical
            } else {
                Direction::Horizontal
            })
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(body[1]);

        for (i, id) in [dmac_core::PanelId::Left, dmac_core::PanelId::Right]
            .into_iter()
            .enumerate()
        {
            // Record the *interior* rect so a click maps straight to a row with
            // no border arithmetic at the call site.
            // Without a border the interior still starts one row down: the
            // title keeps its line, so a click maps to the same row either way.
            layout.panels[i] = if full {
                let mut r = cols[i];
                r.y = r.y.saturating_add(1);
                r.height = r.height.saturating_sub(1);
                r
            } else {
                inner(cols[i])
            };
            let is_active = active == id;
            panel::draw(
                frame,
                cols[i],
                &mut panels[i],
                is_active,
                is_active && focus == crate::app::Focus::Panel,
                !full,
                theme,
            );
        }
    } else {
        layout.panels = [ratatui::layout::Rect::default(); 2];
    }

    if in_shell {
        if status_rows > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", app.status),
                    Style::default().fg(theme.status_fg),
                ))),
                rows[1],
            );
        }
    } else {
        draw_command_line(frame, rows[1], app);
    }
    if fkey_rows > 0 {
        let (keys, active) = if history_open {
            (&fkeybar::HISTORY[..], Some(app.history_order.index()))
        } else {
            (&fkeybar::NORMAL[..], None)
        };
        fkeybar::draw(frame, rows[2], keys, active, theme);
    }

    match app.mode {
        crate::app::Mode::Picker { selected } => {
            app.layout.picker = picker::draw(frame, area, dmac_fx::catalog(), selected, theme);
        }
        crate::app::Mode::UserMenu { selected } => {
            // The rows were built when the menu opened. Drawing from that list
            // rather than rebuilding one here is what keeps a click, a letter
            // and the highlight all pointing at the same command.
            let items: Vec<menu::Item<'_>> = app
                .menu_rows
                .iter()
                .map(|r| menu::Item {
                    label: &r.label,
                    hint: &r.hint,
                    separator: r.separator,
                    inert: r.inert,
                })
                .collect();
            let anchor = (area.x + 2, area.y + 2);
            app.layout.menu = menu::draw(frame, area, anchor, &items, selected, theme);
        }
        crate::app::Mode::View { scroll, hex } => {
            if let Some(doc) = app.document.as_ref() {
                app.layout.view =
                    view::draw(frame, area, doc, scroll, hex, &app.view_search, theme);
            }
        }
        crate::app::Mode::Help { scroll } => {
            // The page arrives before it is a page. While the characters are
            // still flying in, what is drawn is the effect; the moment they
            // land, the real one takes over — scrollable, and exactly where the
            // animation left it.
            app.layout.help = help::draw(frame, area, scroll, theme);
            if let Some(canvas) = app.help_canvas(area.width, area.height) {
                screen::draw_canvas(frame, area, canvas);
            }
        }
        crate::app::Mode::Context { selected, anchor } => {
            let items = app.context_items();
            app.layout.menu = menu::draw(frame, area, anchor, &items, selected, theme);
        }
        crate::app::Mode::Prompt { intent } => {
            prompt::draw(frame, area, intent.title(), &app.prompt_value, theme);
        }
        crate::app::Mode::Utilities { selected } => {
            // Anchored at the command line, because that is where its output
            // lands — the menu should point at what it is about to change.
            let anchor = (rows[1].x + 2, rows[1].y);
            app.layout.menu = menu::draw(
                frame,
                area,
                anchor,
                &crate::utilities::items(&app.session_menu_rows()),
                selected,
                theme,
            );
        }
        crate::app::Mode::History { selected } => {
            let shown = app.history_rows();
            app.layout.history = history::draw(
                frame,
                area,
                &shown,
                &history::State {
                    filter: &app.history_filter,
                    order: app.history_order,
                    selected,
                    now: dmac_core::history::now(),
                },
                theme,
            );
        }
        crate::app::Mode::Reattach { selected } => {
            reattach::draw(frame, area, &app.pending, selected, theme);
        }
        crate::app::Mode::Rail { .. } | crate::app::Mode::Normal => {
            app.layout.picker = ratatui::layout::Rect::default();
            app.layout.menu = ratatui::layout::Rect::default();
            app.layout.help = ratatui::layout::Rect::default();
        }
    }

    // Over the panels rather than instead of them: the app is already usable
    // behind it, and it should look that way.
    if app.splash_visible() {
        splash::draw(frame, area, theme);
    }

    // Last, so it sits on top of everything: DOS text mode had no pointer
    // sprite — it inverted the attribute of the cell under the mouse, and that
    // is exactly what this does.
    draw_mouse_pointer(frame, area, app);
}

/// The bottom command line: a prompt, whatever the user has typed, and — when
/// there is something to say — a transient status message on the right.
///
/// The real terminal cursor is placed here, and *only* here. When the keyboard
/// is on a panel no cursor is set, and ratatui hides it — so a blinking cursor
/// always means "text goes here", with no exceptions to remember.
fn draw_command_line(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let theme = &app.theme;
    let focused = app.ses().focus == crate::app::Focus::CommandLine;

    let prompt = format!("{}> ", short_path(&app.cwd_display(app.ses().active)));
    let mut spans = vec![Span::styled(
        prompt.clone(),
        if focused {
            // The prompt brightens when it has the keyboard: a second cue
            // for anyone who cannot see the cursor blink.
            Style::default()
                .fg(theme.selected_fg)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            Style::default().fg(theme.status_fg)
        },
    )];

    // Split the typed text around the selection so it is visible for what it
    // is. A selection you cannot see is one you cannot trust, and this one
    // decides what Copy puts on the clipboard.
    let line = app.ses().command_line.as_str();
    match app.command_selection_span() {
        Some((lo, hi)) => {
            let take = |from: usize, to: usize| -> String {
                line.chars()
                    .skip(from)
                    .take(to.saturating_sub(from))
                    .collect()
            };
            let end = line.chars().count();
            spans.push(Span::raw(take(0, lo)));
            spans.push(Span::styled(
                take(lo, hi),
                Style::default().add_modifier(ratatui::style::Modifier::REVERSED),
            ));
            spans.push(Span::raw(take(hi, end)));
        }
        None => spans.push(Span::raw(line.to_string())),
    }

    if !app.status.is_empty() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            app.status.as_str(),
            Style::default().fg(theme.status_fg),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    if focused {
        // Width in columns, not bytes: a multi-byte or wide character typed into
        // the command line must not push the cursor off its own text.
        let col = prompt.width() + app.ses().command_line.width();
        let x = area
            .x
            .saturating_add(col.min(u16::MAX as usize) as u16)
            .min(area.x + area.width.saturating_sub(1));

        if app.cursor_style.is_software() {
            // Drawn as an inverted cell rather than asked of the terminal. Works
            // everywhere, including terminals that ignore DECSCUSR or override
            // it with their own cursor preference.
            if app.software_cursor_on() {
                frame.buffer_mut()[(x, area.y)].set_style(theme.cursor());
            }
        } else {
            frame.set_cursor_position((x, area.y));
        }
    }
}

/// Invert the cell under the pointer, the way DOS text mode drew a mouse.
fn draw_mouse_pointer(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let Some((x, y)) = app.mouse else { return };
    if x < area.x || y < area.y || x >= area.x + area.width || y >= area.y + area.height {
        return;
    }
    let buf = frame.buffer_mut();
    let cell = &buf[(x, y)];
    let (fg, bg) = (cell.fg, cell.bg);
    // Swap foreground and background. Whatever the theme, the pointer is always
    // visible and never hides the character it is over.
    buf[(x, y)].set_fg(bg).set_bg(fg);
}

/// The area inside a one-cell border.
fn inner(area: ratatui::layout::Rect) -> ratatui::layout::Rect {
    ratatui::layout::Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

/// Keep the prompt short so the typed command has room. Long paths collapse from
/// the left, which is where the least useful part of a path lives.
fn short_path(p: &str) -> String {
    const MAX: usize = 28;
    let chars: Vec<char> = p.chars().collect();
    if chars.len() <= MAX {
        return p.to_string();
    }
    let tail: String = chars[chars.len() - (MAX - 1)..].iter().collect();
    format!("…{tail}")
}

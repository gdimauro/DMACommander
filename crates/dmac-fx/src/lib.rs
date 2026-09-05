//! Screensavers, the dock and visual effects.
//!
//! Owned by the `fx-engineer` agent.
//!
//! Effects are backend-agnostic: each one draws into a [`Canvas`] of logical
//! cells, which the TUI rasterizes to a ratatui buffer and the GPU backend can
//! rasterize to a texture. The effect list is never forked per backend.
// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod audio;
pub mod canvas;
pub mod effects;
pub mod screensaver;

pub use canvas::{Canvas, Cell, Rgb};
pub use screensaver::{Screensaver, ScreensaverConfig, Wake};

use std::time::Duration;

/// A key press, in terms an effect can use without knowing what a terminal is.
///
/// `dmac-fx` sits below `dmac-tui` in the layering, so it cannot see a
/// crossterm `KeyEvent`. The frontend translates; effects stay portable to the
/// GPU backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Space,
    Char(char),
    Other,
}

/// What an effect wants to happen after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectControl {
    /// Dismiss and return to the file manager. What every screensaver does on
    /// any key.
    Exit,
    /// The effect used the key. Games do this — otherwise pressing an arrow to
    /// steer would quit the game.
    Consumed,
}

/// What something in the catalogue is. Games are opt-in curiosities, so they are
/// never picked by `random` and never started by the idle timer — being dropped
/// into a game because you went for coffee is not a feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Screensaver,
    /// Plays itself. Safe for the idle timer and the rotation, because it needs
    /// nobody watching — and a key still gives the file manager back, unless the
    /// user deliberately takes over.
    Demo,
    Game,
}

/// One entry in the picker.
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub name: &'static str,
    pub kind: Kind,
    /// Shown in the picker. One line, lowercase, no trailing period.
    pub blurb: &'static str,
}

/// One animated effect.
///
/// The contract: `tick` is a function of elapsed time, never of frame count. The
/// terminal *will* stall — under load, on a resize, when the user's machine
/// swaps — and an effect that assumes 60fps visibly speeds up and slows down.
pub trait Effect: Send {
    /// Stable identifier, used in config and on the command line.
    fn name(&self) -> &'static str;

    /// The canvas changed size. Effects holding per-column or per-cell state
    /// must rebuild it here.
    fn resize(&mut self, width: u16, height: u16);

    /// Advance by `dt` and draw. The canvas persists between calls, so an effect
    /// that wants trails fades it, and one that wants a clean frame clears it.
    fn tick(&mut self, dt: Duration, canvas: &mut Canvas);

    /// Whether this effect needs a GPU backend. TUI-only sessions hide the ones
    /// that answer `true` rather than rendering them badly.
    fn requires_gpu(&self) -> bool {
        false
    }

    /// React to a key press.
    ///
    /// The default is what a screensaver wants: any key dismisses. A game
    /// overrides this to steer, and returns [`EffectControl::Exit`] only for the
    /// keys that mean "I am done".
    fn on_input(&mut self, _key: EffectKey) -> EffectControl {
        EffectControl::Exit
    }
}

/// Everything in the picker: screensavers first, then games.
pub fn catalog() -> &'static [CatalogEntry] {
    use Kind::*;
    &[
        CatalogEntry {
            name: "matrix",
            kind: Screensaver,
            blurb: "digital rain",
        },
        CatalogEntry {
            name: "starfield",
            kind: Screensaver,
            blurb: "flying through stars",
        },
        CatalogEntry {
            name: "plasma",
            kind: Screensaver,
            blurb: "demoscene sine fields",
        },
        CatalogEntry {
            name: "life",
            kind: Screensaver,
            blurb: "conway, coloured by age",
        },
        CatalogEntry {
            name: "pipes",
            kind: Screensaver,
            blurb: "the 1995 office classic",
        },
        CatalogEntry {
            name: "spectrum",
            kind: Screensaver,
            blurb: "listens to the room and draws it",
        },
        CatalogEntry {
            name: "asteroids",
            kind: Demo,
            blurb: "the computer plays; space to take over",
        },
        CatalogEntry {
            name: "snake",
            kind: Game,
            blurb: "arrows to steer, esc to leave",
        },
    ]
}

/// What `random` and the rotation draw from: everything except games.
///
/// Derived from [`catalog`] rather than kept as a second list. They were two
/// lists briefly, and they immediately disagreed — an effect was advertised in
/// one and missing from the other, which is a whole class of bug that simply
/// cannot happen once one is computed from the other.
pub fn available() -> &'static [&'static str] {
    static NAMES: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        catalog()
            .iter()
            .filter(|e| e.kind != Kind::Game)
            .map(|e| e.name)
            .collect()
    });
    &NAMES
}

/// The entry for a name, if it exists.
pub fn entry(name: &str) -> Option<&'static CatalogEntry> {
    catalog().iter().find(|e| e.name == name)
}

/// Build an effect by name. `None` for an unknown name, so a typo in the config
/// is reported to the user instead of silently falling back.
pub fn build(name: &str) -> Option<Box<dyn Effect>> {
    match name {
        "matrix" => Some(Box::new(effects::matrix::Matrix::new())),
        "starfield" => Some(Box::new(effects::starfield::Starfield::new())),
        "plasma" => Some(Box::new(effects::plasma::Plasma::new())),
        "life" => Some(Box::new(effects::life::Life::new())),
        "pipes" => Some(Box::new(effects::pipes::Pipes::new())),
        "spectrum" => Some(Box::new(effects::spectrum::Spectrum::new())),
        "asteroids" => Some(Box::new(effects::asteroids::Asteroids::new())),
        "snake" => Some(Box::new(effects::snake::Snake::new())),
        _ => None,
    }
}

/// Pick one at random. What `screensaver = "random"` resolves to.
pub fn build_random() -> Box<dyn Effect> {
    let names = available();
    let pick = names[fastrand::usize(..names.len())];
    // Every name in `available()` is buildable; the fallback keeps the lint
    // rules satisfied without an unwrap.
    build(pick).unwrap_or_else(|| Box::new(effects::matrix::Matrix::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogued_effect_can_actually_be_built() {
        for e in catalog() {
            assert!(
                build(e.name).is_some(),
                "{} is listed but not buildable",
                e.name
            );
        }
    }

    /// Games must never be started by the idle timer or by `random`. Coming
    /// back from coffee to find yourself mid-Snake is not a feature. A demo
    /// plays itself, so it is allowed.
    #[test]
    fn games_are_excluded_from_the_random_rotation() {
        for name in available() {
            let kind = entry(name).expect("in catalog").kind;
            assert_ne!(kind, Kind::Game, "{name} must not be in the rotation");
        }
        assert!(
            catalog().iter().any(|e| e.kind == Kind::Game),
            "there should be games"
        );
        assert!(catalog().iter().any(|e| e.kind == Kind::Demo), "and demos");
    }

    /// A screensaver dismisses on any key; a game does not, or you could not
    /// steer it.
    #[test]
    fn screensavers_exit_on_any_key_and_games_do_not() {
        for e in catalog() {
            let mut fx = build(e.name).unwrap();
            fx.resize(40, 20);
            let control = fx.on_input(EffectKey::Left);
            match e.kind {
                Kind::Screensaver | Kind::Demo => {
                    assert_eq!(control, EffectControl::Exit, "{}", e.name)
                }
                Kind::Game => assert_eq!(control, EffectControl::Consumed, "{}", e.name),
            }
        }
    }

    /// Esc must always get you out, including out of a game.
    #[test]
    fn esc_always_exits() {
        for e in catalog() {
            let mut fx = build(e.name).unwrap();
            fx.resize(40, 20);
            assert_eq!(
                fx.on_input(EffectKey::Esc),
                EffectControl::Exit,
                "{}",
                e.name
            );
        }
    }

    #[test]
    fn an_unknown_name_is_reported_not_silently_substituted() {
        assert!(build("nope").is_none());
    }

    /// Every effect must survive the sizes a real terminal produces during a
    /// drag-resize, including degenerate ones.
    #[test]
    fn effects_survive_absurd_canvas_sizes() {
        for e in catalog() {
            let mut fx = build(e.name).unwrap();
            for (w, h) in [(0u16, 0u16), (1, 1), (1, 40), (200, 1), (80, 24)] {
                let mut canvas = Canvas::new(w, h);
                fx.resize(w, h);
                for _ in 0..5 {
                    fx.tick(Duration::from_millis(16), &mut canvas);
                }
            }
        }
    }

    /// A stalled terminal hands us a huge `dt`. An effect that integrates
    /// position without clamping teleports or overflows; this pins that it does not.
    #[test]
    fn a_huge_time_step_does_not_break_any_effect() {
        for e in catalog() {
            let mut fx = build(e.name).unwrap();
            let mut canvas = Canvas::new(80, 24);
            fx.resize(80, 24);
            fx.tick(Duration::from_secs(30), &mut canvas);
            fx.tick(Duration::ZERO, &mut canvas);
        }
    }
}

//! Idle detection and the screensaver lifecycle.
//!
//! The rule that shapes this whole file: **zero cost when not visible.** No
//! polling, no timer ticking in the background, nothing that shows up in
//! `powermetrics` while you are reading a directory listing.
//!
//! That is achieved by [`Screensaver::deadline`]: the caller's event loop blocks
//! on input with *at most one* timer, whose deadline is either "when we would go
//! idle" or "when the next frame is due". When the screensaver is disabled there
//! is no timer at all.

use crate::{Canvas, Effect, EffectControl, EffectKey};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct ScreensaverConfig {
    pub enabled: bool,
    /// Idle time before it starts. Zero disables it as surely as `enabled = false`.
    pub idle: Duration,
    /// An effect name, `"random"` to pick a different one each time, or
    /// `"rotation"` to cycle through the screensavers in order. Games are never
    /// picked by either — you should have to choose one deliberately.
    pub effect: String,
    pub fps: u32,
    /// Whether the keypress that dismisses the screensaver also reaches the app.
    /// Default `false`: waking the screen should not accidentally delete a file.
    pub dismiss_swallows_key: bool,
}

impl Default for ScreensaverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle: Duration::from_secs(300),
            effect: "random".into(),
            fps: 30,
            dismiss_swallows_key: false,
        }
    }
}

struct Running {
    effect: Box<dyn Effect>,
    canvas: Canvas,
    last_tick: Instant,
    /// Size the effect was last told about, so a resize is detected without the
    /// caller having to report one.
    size: (u16, u16),
}

/// What happened to an input that reached the screensaver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Nothing was running — deliver the input to the application as normal.
    Passthrough,
    /// The input dismissed the screensaver and must NOT reach the application.
    /// Waking a screen is not the same gesture as confirming a delete.
    Dismissed,
    /// A running effect used the input. Games do this; without it you could not
    /// steer one.
    Consumed,
}

pub struct Screensaver {
    config: ScreensaverConfig,
    running: Option<Running>,
    last_input: Instant,
    /// Position in the rotation, so `"rotation"` advances rather than repeating.
    rotation: usize,
}

impl Screensaver {
    pub fn new(config: ScreensaverConfig) -> Self {
        Self {
            config,
            running: None,
            last_input: Instant::now(),
            rotation: 0,
        }
    }

    pub fn config(&self) -> &ScreensaverConfig {
        &self.config
    }

    /// Change what starts on the next idle period — `"random"`, `"rotation"` or
    /// an effect name. Set by the picker so a choice made once persists.
    pub fn set_effect(&mut self, effect: &str) {
        self.config.effect = effect.to_string();
    }

    pub fn is_active(&self) -> bool {
        self.running.is_some()
    }

    /// The name of the running effect, for the status line.
    pub fn current(&self) -> Option<&'static str> {
        self.running.as_ref().map(|r| r.effect.name())
    }

    /// Route a key press.
    ///
    /// A running effect gets first refusal: a game keeps its arrow keys, a
    /// screensaver dismisses on anything.
    pub fn on_key(&mut self, key: EffectKey) -> Wake {
        self.last_input = Instant::now();

        let Some(running) = &mut self.running else {
            return Wake::Passthrough;
        };

        match running.effect.on_input(key) {
            EffectControl::Consumed => Wake::Consumed,
            EffectControl::Exit => {
                self.running = None;
                if self.config.dismiss_swallows_key {
                    Wake::Passthrough
                } else {
                    Wake::Dismissed
                }
            }
        }
    }

    /// Route non-key activity: mouse movement, a click, regaining focus.
    ///
    /// This never reaches an effect — a game should not be dismissed because the
    /// mouse was nudged, but the idle clock still resets.
    pub fn on_activity(&mut self) -> Wake {
        self.last_input = Instant::now();
        Wake::Passthrough
    }

    /// Start immediately, regardless of idle time. Bound to a key so it can be
    /// invoked deliberately — and so it is testable without waiting five minutes.
    pub fn start_now(&mut self, width: u16, height: u16) {
        let effect = match self.config.effect.as_str() {
            "random" => crate::build_random(),
            "rotation" => self.next_in_rotation(),
            // An unknown name in the config is the user's typo, not ours: fall
            // back so the feature still works, and let the caller report it.
            name => crate::build(name).unwrap_or_else(crate::build_random),
        };
        self.start_with(effect, width, height);
    }

    /// The next screensaver in sequence. Games are not in `available()`, so the
    /// rotation cannot land on one.
    fn next_in_rotation(&mut self) -> Box<dyn Effect> {
        let names = crate::available();
        if names.is_empty() {
            return crate::build_random();
        }
        let name = names[self.rotation % names.len()];
        self.rotation = self.rotation.wrapping_add(1);
        crate::build(name).unwrap_or_else(crate::build_random)
    }

    pub fn start_with(&mut self, mut effect: Box<dyn Effect>, width: u16, height: u16) {
        let mut canvas = Canvas::new(width, height);
        canvas.clear();
        effect.resize(width, height);
        self.running = Some(Running {
            effect,
            canvas,
            last_tick: Instant::now(),
            size: (width, height),
        });
    }

    pub fn stop(&mut self) {
        self.running = None;
        self.last_input = Instant::now();
    }

    /// When the event loop should wake up next.
    ///
    /// `None` means "never on our account" — block on input alone. This is the
    /// method that delivers the zero-idle-cost promise: when the screensaver is
    /// disabled, we contribute no timer whatsoever.
    pub fn deadline(&self) -> Option<Instant> {
        if let Some(r) = &self.running {
            return Some(r.last_tick + self.frame_interval());
        }
        if !self.config.enabled || self.config.idle.is_zero() {
            return None;
        }
        Some(self.last_input + self.config.idle)
    }

    fn frame_interval(&self) -> Duration {
        Duration::from_secs_f32(1.0 / self.config.fps.clamp(1, 240) as f32)
    }

    /// Advance the clock. Call when the deadline expires *or* on any redraw.
    ///
    /// Returns the canvas to draw when the screensaver is showing, `None` when
    /// the normal UI should be drawn.
    pub fn update(&mut self, width: u16, height: u16) -> Option<&Canvas> {
        // Not running: has it been idle long enough to start?
        if self.running.is_none() {
            if !self.config.enabled || self.config.idle.is_zero() {
                return None;
            }
            if self.last_input.elapsed() < self.config.idle {
                return None;
            }
            self.start_now(width, height);
        }

        let interval = self.frame_interval();
        let r = self.running.as_mut()?;

        if r.size != (width, height) {
            r.canvas.resize(width, height);
            r.effect.resize(width, height);
            r.size = (width, height);
        }

        let now = Instant::now();
        let dt = now.duration_since(r.last_tick);
        // Only advance on a due frame, so an unrelated redraw (a resize, a
        // background job finishing) does not fast-forward the animation.
        if dt >= interval {
            r.effect.tick(dt, &mut r.canvas);
            r.last_tick = now;
        }

        Some(&r.canvas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(idle_ms: u64) -> ScreensaverConfig {
        ScreensaverConfig {
            enabled: true,
            idle: Duration::from_millis(idle_ms),
            effect: "matrix".into(),
            fps: 60,
            dismiss_swallows_key: false,
        }
    }

    /// The zero-idle-cost guarantee, expressed as a test: a disabled screensaver
    /// must contribute no wakeup at all.
    #[test]
    fn a_disabled_screensaver_schedules_no_timer() {
        let s = Screensaver::new(ScreensaverConfig {
            enabled: false,
            ..cfg(1000)
        });
        assert!(s.deadline().is_none());

        let s = Screensaver::new(ScreensaverConfig {
            idle: Duration::ZERO,
            ..cfg(0)
        });
        assert!(
            s.deadline().is_none(),
            "zero idle must mean disabled, not instant"
        );
    }

    #[test]
    fn an_enabled_screensaver_schedules_exactly_one_wakeup() {
        let s = Screensaver::new(cfg(1000));
        let d = s.deadline().expect("must schedule the idle deadline");
        assert!(d > Instant::now());
    }

    #[test]
    fn it_starts_only_after_the_idle_period() {
        let mut s = Screensaver::new(cfg(50));
        assert!(s.update(80, 24).is_none(), "must not start immediately");
        std::thread::sleep(Duration::from_millis(60));
        assert!(s.update(80, 24).is_some(), "must start once idle");
        assert!(s.is_active());
    }

    /// The dismissing keypress must not reach the app. This is the difference
    /// between waking the screen and confirming a delete.
    #[test]
    fn the_dismissing_key_is_swallowed_by_default() {
        let mut s = Screensaver::new(cfg(0));
        s.start_now(80, 24);
        assert!(s.is_active());
        assert_eq!(s.on_key(EffectKey::Char('x')), Wake::Dismissed);
        assert!(!s.is_active(), "any key must dismiss a screensaver");
    }

    #[test]
    fn ordinary_input_while_inactive_passes_straight_through() {
        let mut s = Screensaver::new(cfg(10_000));
        assert_eq!(s.on_key(EffectKey::Char('x')), Wake::Passthrough);
    }

    #[test]
    fn opting_in_lets_the_waking_key_through() {
        let mut s = Screensaver::new(ScreensaverConfig {
            dismiss_swallows_key: true,
            ..cfg(0)
        });
        s.start_now(80, 24);
        assert_eq!(s.on_key(EffectKey::Char('x')), Wake::Passthrough);
    }

    /// A game must keep its steering keys, and Esc must still get you out.
    #[test]
    fn a_game_consumes_its_keys_instead_of_being_dismissed() {
        let mut s = Screensaver::new(cfg(0));
        s.start_with(crate::build("snake").expect("snake"), 60, 20);
        assert_eq!(s.on_key(EffectKey::Left), Wake::Consumed);
        assert!(s.is_active(), "steering must not quit the game");
        assert_eq!(s.on_key(EffectKey::Esc), Wake::Dismissed);
        assert!(!s.is_active(), "esc must always leave");
    }

    /// Nudging the mouse must not end a game, but must reset the idle clock.
    #[test]
    fn mouse_activity_never_reaches_a_running_effect() {
        let mut s = Screensaver::new(cfg(0));
        s.start_with(crate::build("snake").expect("snake"), 60, 20);
        assert_eq!(s.on_activity(), Wake::Passthrough);
        assert!(s.is_active());
    }

    #[test]
    fn rotation_advances_instead_of_repeating() {
        let mut s = Screensaver::new(ScreensaverConfig {
            effect: "rotation".into(),
            ..cfg(0)
        });
        let mut seen = Vec::new();
        for _ in 0..crate::available().len() {
            s.start_now(40, 12);
            seen.push(s.current().expect("running"));
            s.stop();
        }
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), seen.len(), "rotation repeated: {seen:?}");
    }

    #[test]
    fn input_postpones_the_idle_deadline() {
        let mut s = Screensaver::new(cfg(1000));
        let first = s.deadline().unwrap();
        std::thread::sleep(Duration::from_millis(20));
        s.on_key(EffectKey::Char('x'));
        assert!(s.deadline().unwrap() > first);
    }

    #[test]
    fn a_resize_while_running_is_handled_without_restarting() {
        let mut s = Screensaver::new(cfg(0));
        s.start_now(80, 24);
        let name = s.current();
        let canvas = s.update(40, 12).expect("still running");
        assert_eq!(canvas.width(), 40);
        assert_eq!(canvas.height(), 12);
        assert_eq!(s.current(), name, "a resize must not swap the effect");
    }

    #[test]
    fn an_unknown_effect_name_still_produces_a_screensaver() {
        let mut s = Screensaver::new(ScreensaverConfig {
            effect: "definitely-not-an-effect".into(),
            ..cfg(0)
        });
        s.start_now(80, 24);
        assert!(s.is_active(), "a config typo must not disable the feature");
    }
}

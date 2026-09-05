//! Asteroids, played by the computer until you take over.
//!
//! This is an arcade attract mode: it runs itself, and the machine is showing
//! off rather than waiting. That makes it a legitimate screensaver — unlike
//! Snake, it needs nobody — while still being a game the moment you want one.
//!
//! The rule for the first keypress matters. Coming back to your desk and
//! pressing a key must give you your file manager back, not a spaceship: any
//! key dismisses. Taking over is deliberate, on Space or Enter — the arcade's
//! "insert coin".
//!
//! Rendered as real wireframe vectors through [`Canvas::line`], because
//! Asteroids drawn with block characters is just not Asteroids.

use crate::{Canvas, Cell, Effect, EffectControl, EffectKey, Rgb};
use std::f32::consts::TAU;
use std::time::Duration;

const SHIP: Rgb = Rgb(220, 240, 255);
const SHIP_HIT: Rgb = Rgb(255, 120, 120);
const ROCK: Rgb = Rgb(150, 165, 190);
const SHOT: Rgb = Rgb(255, 240, 150);
const FLAME: Rgb = Rgb(255, 170, 60);
const TEXT: Rgb = Rgb(200, 210, 230);
const DEBRIS: Rgb = Rgb(255, 200, 120);

/// Character cells are about twice as tall as wide. The simulation runs in
/// square world units and divides y on the way to the screen, so a circle is
/// round and an asteroid drifting diagonally does not appear to change speed.
const CELL_ASPECT: f32 = 2.0;

const TURN_RATE: f32 = 3.4; // radians/sec
const THRUST: f32 = 46.0;
const DRAG: f32 = 0.66;
const MAX_SPEED: f32 = 42.0;
const BULLET_SPEED: f32 = 70.0;
const BULLET_LIFE: f32 = 1.15;
const FIRE_INTERVAL: f32 = 0.22;
const INVULNERABLE: f32 = 1.6;

#[derive(Clone, Copy, PartialEq)]
struct V2 {
    x: f32,
    y: f32,
}

impl V2 {
    fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn from_angle(a: f32, len: f32) -> Self {
        Self::new(a.cos() * len, a.sin() * len)
    }

    fn add(self, o: V2) -> V2 {
        V2::new(self.x + o.x, self.y + o.y)
    }

    fn scale(self, k: f32) -> V2 {
        V2::new(self.x * k, self.y * k)
    }

    fn len(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    fn angle(self) -> f32 {
        self.y.atan2(self.x)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Size {
    Large,
    Medium,
    Small,
}

impl Size {
    fn radius(self) -> f32 {
        match self {
            Size::Large => 7.0,
            Size::Medium => 4.2,
            Size::Small => 2.4,
        }
    }

    fn smaller(self) -> Option<Size> {
        match self {
            Size::Large => Some(Size::Medium),
            Size::Medium => Some(Size::Small),
            Size::Small => None,
        }
    }

    fn score(self) -> u32 {
        match self {
            Size::Large => 20,
            Size::Medium => 50,
            Size::Small => 100,
        }
    }
}

struct Rock {
    pos: V2,
    vel: V2,
    size: Size,
    angle: f32,
    spin: f32,
    /// Per-vertex radius multipliers. Rocks are lumpy; perfect circles read as
    /// bubbles and kill the whole look.
    shape: Vec<f32>,
}

impl Rock {
    fn new(pos: V2, vel: V2, size: Size) -> Self {
        let n = 8 + fastrand::usize(..4);
        Self {
            pos,
            vel,
            size,
            angle: fastrand::f32() * TAU,
            spin: (fastrand::f32() - 0.5) * 1.6,
            shape: (0..n).map(|_| 0.66 + fastrand::f32() * 0.5).collect(),
        }
    }

    fn radius(&self) -> f32 {
        self.size.radius()
    }
}

struct Bullet {
    pos: V2,
    vel: V2,
    ttl: f32,
}

struct Spark {
    pos: V2,
    vel: V2,
    ttl: f32,
}

/// Who is flying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pilot {
    /// The attract mode. Nobody is watching, and nobody needs to be.
    Computer,
    Human,
}

pub struct Asteroids {
    w: f32,
    h: f32,
    pilot: Pilot,

    pos: V2,
    vel: V2,
    heading: f32,
    thrusting: bool,
    cooldown: f32,
    invulnerable: f32,

    /// Human controls, held between frames — a terminal sends key presses, not
    /// key states, so a held key is emulated by a short decay.
    turn_left: f32,
    turn_right: f32,
    thrust_held: f32,

    rocks: Vec<Rock>,
    bullets: Vec<Bullet>,
    sparks: Vec<Spark>,

    score: u32,
    wave: u32,
    lives: u32,
}

impl Asteroids {
    pub fn new() -> Self {
        Self {
            w: 0.0,
            h: 0.0,
            pilot: Pilot::Computer,
            pos: V2::new(0.0, 0.0),
            vel: V2::new(0.0, 0.0),
            heading: 0.0,
            thrusting: false,
            cooldown: 0.0,
            invulnerable: 0.0,
            turn_left: 0.0,
            turn_right: 0.0,
            thrust_held: 0.0,
            rocks: Vec::new(),
            bullets: Vec::new(),
            sparks: Vec::new(),
            score: 0,
            wave: 0,
            lives: 3,
        }
    }

    fn reset(&mut self) {
        self.pos = V2::new(self.w / 2.0, self.h / 2.0);
        self.vel = V2::new(0.0, 0.0);
        self.heading = -TAU / 4.0;
        self.bullets.clear();
        self.rocks.clear();
        self.sparks.clear();
        self.score = 0;
        self.wave = 0;
        self.lives = 3;
        self.invulnerable = INVULNERABLE;
        self.next_wave();
    }

    fn next_wave(&mut self) {
        self.wave += 1;
        let n = (3 + self.wave).min(9);
        for _ in 0..n {
            // Spawn away from the ship, so a new wave never lands on top of you.
            //
            // Bounded retries, not `loop`: on a playfield smaller than the safe
            // radius no such position exists, and an unbounded search there hangs
            // the whole application. A test at 1x1 found exactly that.
            let mut pos = V2::new(fastrand::f32() * self.w, fastrand::f32() * self.h);
            for _ in 0..24 {
                if self.wrapped_delta(pos, self.pos).len() > 22.0 {
                    break;
                }
                pos = V2::new(fastrand::f32() * self.w, fastrand::f32() * self.h);
            }
            let speed = 4.0 + fastrand::f32() * 7.0 + self.wave as f32 * 0.6;
            let dir = fastrand::f32() * TAU;
            self.rocks
                .push(Rock::new(pos, V2::from_angle(dir, speed), Size::Large));
        }
    }

    /// Shortest vector from `a` to `b` on a wrapping playfield.
    ///
    /// Everything that measures a distance has to use this. Without it the AI
    /// ignores the rock about to hit it from the far edge, and bullets appear to
    /// miss targets they visibly pass through.
    fn wrapped_delta(&self, a: V2, b: V2) -> V2 {
        let mut dx = b.x - a.x;
        let mut dy = b.y - a.y;
        if dx > self.w / 2.0 {
            dx -= self.w;
        } else if dx < -self.w / 2.0 {
            dx += self.w;
        }
        if dy > self.h / 2.0 {
            dy -= self.h;
        } else if dy < -self.h / 2.0 {
            dy += self.h;
        }
        V2::new(dx, dy)
    }

    fn wrap(&self, p: V2) -> V2 {
        V2::new(
            p.x.rem_euclid(self.w.max(1.0)),
            p.y.rem_euclid(self.h.max(1.0)),
        )
    }

    fn fire(&mut self) {
        if self.cooldown > 0.0 || self.bullets.len() >= 6 {
            return;
        }
        self.cooldown = FIRE_INTERVAL;
        let dir = V2::from_angle(self.heading, 1.0);
        self.bullets.push(Bullet {
            pos: self.pos.add(dir.scale(2.5)),
            // Inherit ship velocity: firing while drifting backwards should not
            // produce bullets that hang in space.
            vel: dir.scale(BULLET_SPEED).add(self.vel.scale(0.4)),
            ttl: BULLET_LIFE,
        });
    }

    /// The attract-mode pilot.
    ///
    /// Deliberately good but not perfect: it leads its shots and dodges, but it
    /// has a reaction threshold and no lookahead beyond one target, so it still
    /// loses ships occasionally. A demo that never dies looks like a recording.
    fn autopilot(&mut self, dt: f32) {
        // Pick the target: nearest by surface distance, biased towards whatever
        // is closing on us fastest.
        let mut best: Option<(usize, f32)> = None;
        for (i, r) in self.rocks.iter().enumerate() {
            let d = self.wrapped_delta(self.pos, r.pos);
            let surface = (d.len() - r.radius()).max(0.1);
            let closing = -(d.x * r.vel.x + d.y * r.vel.y) / d.len().max(0.1);
            let threat = surface - closing.max(0.0) * 1.4;
            if best.is_none_or(|(_, b)| threat < b) {
                best = Some((i, threat));
            }
        }
        let Some((idx, _)) = best else {
            return;
        };

        let target = &self.rocks[idx];
        let to = self.wrapped_delta(self.pos, target.pos);
        let dist = to.len();

        // Lead the shot: aim where the rock will be when the bullet arrives.
        // One iteration is enough at these speeds.
        let flight = dist / BULLET_SPEED;
        let lead = to.add(target.vel.scale(flight));
        let want = lead.angle();

        let mut err = (want - self.heading).rem_euclid(TAU);
        if err > std::f32::consts::PI {
            err -= TAU;
        }
        let step = TURN_RATE * dt;
        self.heading += err.clamp(-step, step);

        // Fire when roughly on target. The tolerance widens with distance
        // because the angular size of the rock does too.
        let tolerance = (target.radius() / dist.max(1.0)).clamp(0.05, 0.5);
        if err.abs() < tolerance {
            self.fire();
        }

        // Evade whatever is genuinely close, not whatever we are shooting at.
        let danger = self
            .rocks
            .iter()
            .map(|r| {
                let d = self.wrapped_delta(self.pos, r.pos);
                (d.len() - r.radius(), d)
            })
            .fold(None::<(f32, V2)>, |acc, x| match acc {
                Some(a) if a.0 <= x.0 => Some(a),
                _ => Some(x),
            });

        self.thrusting = false;
        if let Some((gap, d)) = danger
            && gap < 13.0
        {
            // Thrust directly away. Turning to face away first would take longer
            // than the rock does to arrive.
            let away = d.scale(-1.0).angle();
            let mut turn = (away - self.heading).rem_euclid(TAU);
            if turn > std::f32::consts::PI {
                turn -= TAU;
            }
            self.heading += turn.clamp(-step, step);
            self.thrusting = true;
        } else if self.vel.len() < 3.0 && fastrand::f32() < dt * 0.6 {
            // Drift a little when safe, so the demo is not a stationary turret.
            self.thrusting = true;
        }
    }

    fn human(&mut self, dt: f32) {
        // Keys arrive as presses, not as held states; each one keeps its input
        // alive briefly so steering feels continuous rather than stuttering.
        let decay = |v: &mut f32| *v = (*v - dt).max(0.0);
        if self.turn_left > 0.0 {
            self.heading -= TURN_RATE * dt;
        }
        if self.turn_right > 0.0 {
            self.heading += TURN_RATE * dt;
        }
        self.thrusting = self.thrust_held > 0.0;
        decay(&mut self.turn_left);
        decay(&mut self.turn_right);
        decay(&mut self.thrust_held);
    }

    fn physics(&mut self, dt: f32) {
        if self.thrusting {
            self.vel = self.vel.add(V2::from_angle(self.heading, THRUST * dt));
        }
        // Drag, so the ship is controllable in a terminal-sized playfield.
        let damp = (1.0 - DRAG * dt).clamp(0.0, 1.0);
        self.vel = self.vel.scale(damp);
        let speed = self.vel.len();
        if speed > MAX_SPEED {
            self.vel = self.vel.scale(MAX_SPEED / speed);
        }
        self.pos = self.wrap(self.pos.add(self.vel.scale(dt)));

        for r in &mut self.rocks {
            r.pos = V2::new(
                (r.pos.x + r.vel.x * dt).rem_euclid(self.w.max(1.0)),
                (r.pos.y + r.vel.y * dt).rem_euclid(self.h.max(1.0)),
            );
            r.angle += r.spin * dt;
        }

        for b in &mut self.bullets {
            b.pos = V2::new(
                (b.pos.x + b.vel.x * dt).rem_euclid(self.w.max(1.0)),
                (b.pos.y + b.vel.y * dt).rem_euclid(self.h.max(1.0)),
            );
            b.ttl -= dt;
        }
        self.bullets.retain(|b| b.ttl > 0.0);

        let (w, h) = (self.w.max(1.0), self.h.max(1.0));
        for s in &mut self.sparks {
            s.pos = V2::new(
                (s.pos.x + s.vel.x * dt).rem_euclid(w),
                (s.pos.y + s.vel.y * dt).rem_euclid(h),
            );
            s.ttl -= dt;
        }
        self.sparks.retain(|s| s.ttl > 0.0);

        self.cooldown = (self.cooldown - dt).max(0.0);
        self.invulnerable = (self.invulnerable - dt).max(0.0);
    }

    fn burst(&mut self, at: V2, n: usize) {
        for _ in 0..n {
            let a = fastrand::f32() * TAU;
            self.sparks.push(Spark {
                pos: at,
                vel: V2::from_angle(a, 8.0 + fastrand::f32() * 18.0),
                ttl: 0.25 + fastrand::f32() * 0.4,
            });
        }
    }

    fn collisions(&mut self) {
        // Bullets against rocks.
        let mut hit: Option<(usize, usize)> = None;
        'outer: for (bi, b) in self.bullets.iter().enumerate() {
            for (ri, r) in self.rocks.iter().enumerate() {
                if self.wrapped_delta(b.pos, r.pos).len() < r.radius() {
                    hit = Some((bi, ri));
                    break 'outer;
                }
            }
        }
        if let Some((bi, ri)) = hit {
            self.bullets.remove(bi);
            let rock = self.rocks.remove(ri);
            self.score += rock.size.score();
            self.burst(rock.pos, 7);
            if let Some(smaller) = rock.size.smaller() {
                for _ in 0..2 {
                    let dir = fastrand::f32() * TAU;
                    let speed = rock.vel.len() * 1.25 + 3.0 + fastrand::f32() * 5.0;
                    self.rocks
                        .push(Rock::new(rock.pos, V2::from_angle(dir, speed), smaller));
                }
            }
        }

        // Rocks against the ship.
        if self.invulnerable <= 0.0 {
            let struck = self
                .rocks
                .iter()
                .any(|r| self.wrapped_delta(self.pos, r.pos).len() < r.radius() + 1.6);
            if struck {
                let at = self.pos;
                self.burst(at, 18);
                self.lives = self.lives.saturating_sub(1);
                self.invulnerable = INVULNERABLE;
                self.pos = V2::new(self.w / 2.0, self.h / 2.0);
                self.vel = V2::new(0.0, 0.0);
                if self.lives == 0 {
                    // The demo restarts rather than sitting on a game-over
                    // screen: a screensaver that stops moving has stopped working.
                    let pilot = self.pilot;
                    self.reset();
                    self.pilot = pilot;
                }
            }
        }

        if self.rocks.is_empty() {
            self.next_wave();
        }
    }

    /// World point to cell coordinates.
    fn cell(&self, p: V2) -> (i32, i32) {
        (p.x.round() as i32, (p.y / CELL_ASPECT).round() as i32)
    }

    fn draw_ship(&self, canvas: &mut Canvas) {
        // Blink while invulnerable, so a respawn is legible.
        if self.invulnerable > 0.0 && ((self.invulnerable * 12.0) as i32) % 2 == 0 {
            return;
        }
        let colour = if self.invulnerable > 0.0 {
            SHIP_HIT
        } else {
            SHIP
        };

        let nose = self.pos.add(V2::from_angle(self.heading, 3.4));
        let left = self.pos.add(V2::from_angle(self.heading + 2.5, 2.6));
        let right = self.pos.add(V2::from_angle(self.heading - 2.5, 2.6));
        let pts = [self.cell(nose), self.cell(left), self.cell(right)];
        canvas.polygon(&pts, colour);

        if self.thrusting {
            let tail = self
                .pos
                .add(V2::from_angle(self.heading + std::f32::consts::PI, 2.2));
            let flame = self.pos.add(V2::from_angle(
                self.heading + std::f32::consts::PI,
                4.0 + fastrand::f32() * 1.8,
            ));
            let (a, b) = (self.cell(tail), self.cell(flame));
            canvas.line(a.0, a.1, b.0, b.1, FLAME);
        }
    }

    fn draw_rock(&self, canvas: &mut Canvas, r: &Rock) {
        let n = r.shape.len();
        let pts: Vec<(i32, i32)> = r
            .shape
            .iter()
            .enumerate()
            .map(|(i, jitter)| {
                let a = r.angle + TAU * i as f32 / n as f32;
                self.cell(r.pos.add(V2::from_angle(a, r.radius() * jitter)))
            })
            .collect();
        canvas.polygon(&pts, ROCK);
    }

    fn write(canvas: &mut Canvas, x: i32, y: i32, text: &str, fg: Rgb) {
        for (i, ch) in text.chars().enumerate() {
            canvas.set(
                x + i as i32,
                y,
                Cell {
                    ch,
                    fg,
                    bg: Rgb::BLACK,
                },
            );
        }
    }
}

impl Default for Asteroids {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Asteroids {
    fn name(&self) -> &'static str {
        "asteroids"
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.w = width.max(1) as f32;
        self.h = height.max(1) as f32 * CELL_ASPECT;
        let pilot = self.pilot;
        self.reset();
        self.pilot = pilot;
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        if canvas.width() < 24 || canvas.height() < 8 {
            canvas.clear();
            Self::write(canvas, 0, 0, "terminal too small", TEXT);
            return;
        }

        // Clamp: a stalled terminal hands us seconds, and integrating that in one
        // step teleports every rock through the ship.
        let dt = dt.as_secs_f32().min(0.05);

        match self.pilot {
            Pilot::Computer => self.autopilot(dt),
            Pilot::Human => self.human(dt),
        }
        self.physics(dt);
        self.collisions();

        canvas.clear();
        for r in &self.rocks {
            self.draw_rock(canvas, r);
        }
        for b in &self.bullets {
            let (x, y) = self.cell(b.pos);
            canvas.set(
                x,
                y,
                Cell {
                    ch: '\u{2022}',
                    fg: SHOT,
                    bg: Rgb::BLACK,
                },
            );
        }
        for s in &self.sparks {
            let (x, y) = self.cell(s.pos);
            let ch = if s.ttl > 0.3 { '\u{2217}' } else { '\u{00B7}' };
            canvas.set(
                x,
                y,
                Cell {
                    ch,
                    fg: DEBRIS,
                    bg: Rgb::BLACK,
                },
            );
        }
        self.draw_ship(canvas);

        Self::write(
            canvas,
            1,
            0,
            &format!(
                "score {:<6} wave {}  ships {}",
                self.score, self.wave, self.lives
            ),
            TEXT,
        );

        let hint = match self.pilot {
            Pilot::Computer => " demo \u{2014} space to play, any key to leave ",
            Pilot::Human => " \u{2190}\u{2192} turn  \u{2191} thrust  space fire  esc leave ",
        };
        let x = ((canvas.width() as i32 - hint.chars().count() as i32) / 2).max(0);
        Self::write(canvas, x, canvas.height() as i32 - 1, hint, TEXT);
    }

    fn on_input(&mut self, key: EffectKey) -> EffectControl {
        // Esc always leaves, from either mode.
        if key == EffectKey::Esc {
            return EffectControl::Exit;
        }

        if self.pilot == Pilot::Computer {
            // Insert coin. Anything else gives the user their file manager back,
            // which is what pressing a key on a screensaver has to mean.
            return match key {
                EffectKey::Space | EffectKey::Enter => {
                    self.pilot = Pilot::Human;
                    self.thrusting = false;
                    EffectControl::Consumed
                }
                _ => EffectControl::Exit,
            };
        }

        // How long one press keeps its input alive. Long enough that tapping
        // steers smoothly, short enough that the ship stops when you stop.
        const HOLD: f32 = 0.14;
        match key {
            EffectKey::Left | EffectKey::Char('a') => self.turn_left = HOLD,
            EffectKey::Right | EffectKey::Char('d') => self.turn_right = HOLD,
            EffectKey::Up | EffectKey::Char('w') => self.thrust_held = HOLD,
            EffectKey::Space => self.fire(),
            EffectKey::Char('q') => return EffectControl::Exit,
            _ => {}
        }
        EffectControl::Consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game() -> Asteroids {
        let mut a = Asteroids::new();
        a.resize(80, 24);
        a
    }

    fn run(a: &mut Asteroids, seconds: f32) {
        let step = Duration::from_millis(33);
        let mut canvas = Canvas::new(80, 24);
        for _ in 0..((seconds / 0.033) as u32) {
            a.tick(step, &mut canvas);
        }
    }

    #[test]
    fn it_starts_in_attract_mode() {
        assert_eq!(game().pilot, Pilot::Computer);
    }

    /// The rule that protects the user: a key pressed on a screensaver gives
    /// back the file manager. Only an explicit "insert coin" takes over.
    #[test]
    fn an_ordinary_key_leaves_rather_than_handing_you_a_spaceship() {
        let mut a = game();
        assert_eq!(a.on_input(EffectKey::Char('x')), EffectControl::Exit);
        assert_eq!(a.on_input(EffectKey::Down), EffectControl::Exit);
    }

    #[test]
    fn space_takes_over_and_then_keys_steer_instead_of_leaving() {
        let mut a = game();
        assert_eq!(a.on_input(EffectKey::Space), EffectControl::Consumed);
        assert_eq!(a.pilot, Pilot::Human);
        assert_eq!(a.on_input(EffectKey::Left), EffectControl::Consumed);
        assert_eq!(a.on_input(EffectKey::Char('x')), EffectControl::Consumed);
    }

    #[test]
    fn esc_leaves_from_either_mode() {
        let mut a = game();
        assert_eq!(a.on_input(EffectKey::Esc), EffectControl::Exit);
        let mut b = game();
        b.on_input(EffectKey::Space);
        assert_eq!(b.on_input(EffectKey::Esc), EffectControl::Exit);
    }

    /// The demo has to actually play: over a few seconds it must hit something.
    #[test]
    fn the_autopilot_scores() {
        // Seeded, and several games rather than one. Wave placement is random,
        // so a single unseeded game is a coin toss about whether anything comes
        // into range — this failed roughly one run in five, and a test that
        // fails one run in five is not a test, it is a rumour. Each libtest
        // case runs on its own thread, so seeding the thread-local generator
        // here cannot disturb another test.
        let mut scoreless = Vec::new();
        for seed in 0..5u64 {
            fastrand::seed(seed);
            let mut a = game();
            run(&mut a, 30.0);
            if a.score == 0 {
                scoreless.push(seed);
            }
        }
        assert!(
            scoreless.is_empty(),
            "the autopilot went scoreless on seeds {scoreless:?}"
        );
    }

    /// And it must not simply die repeatedly instead of playing.
    #[test]
    fn the_autopilot_survives_a_while() {
        let mut a = game();
        run(&mut a, 12.0);
        assert!(a.wave >= 1);
        assert!(a.lives > 0 || a.score > 0, "it neither survived nor scored");
    }

    #[test]
    fn shooting_a_large_rock_splits_it() {
        let mut a = game();
        a.rocks.clear();
        a.rocks.push(Rock::new(
            V2::new(40.0, 24.0),
            V2::new(0.0, 0.0),
            Size::Large,
        ));
        a.bullets.push(Bullet {
            pos: V2::new(40.0, 24.0),
            vel: V2::new(0.0, 0.0),
            ttl: 1.0,
        });
        a.collisions();
        assert_eq!(a.rocks.len(), 2, "a large rock splits into two mediums");
        assert!(a.rocks.iter().all(|r| r.size == Size::Medium));
        assert_eq!(a.score, Size::Large.score());
    }

    #[test]
    fn a_small_rock_leaves_nothing_behind() {
        let mut a = game();
        a.rocks.clear();
        a.rocks.push(Rock::new(
            V2::new(40.0, 24.0),
            V2::new(0.0, 0.0),
            Size::Small,
        ));
        a.bullets.push(Bullet {
            pos: V2::new(40.0, 24.0),
            vel: V2::new(0.0, 0.0),
            ttl: 1.0,
        });
        a.collisions();
        // Cleared, so a fresh wave arrives rather than an empty screen.
        assert!(a.rocks.iter().all(|r| r.size == Size::Large));
        assert_eq!(a.wave, 2);
    }

    /// Distances must be measured across the wrap, or the AI ignores the rock
    /// about to hit it from the opposite edge.
    #[test]
    fn distance_is_measured_the_short_way_round() {
        let a = game();
        let d = a.wrapped_delta(V2::new(1.0, 1.0), V2::new(79.0, 1.0));
        assert!(d.len() < 5.0, "expected the short way, got {}", d.len());
        assert!(d.x < 0.0, "and in the right direction");
    }

    #[test]
    fn losing_the_last_ship_restarts_the_demo_rather_than_stopping() {
        let mut a = game();
        a.lives = 1;
        a.invulnerable = 0.0;
        a.rocks.clear();
        a.rocks
            .push(Rock::new(a.pos, V2::new(0.0, 0.0), Size::Large));
        a.collisions();
        assert!(a.lives > 0, "a screensaver must never stop moving");
        assert_eq!(
            a.pilot,
            Pilot::Computer,
            "restarting must not steal the pilot"
        );
    }

    #[test]
    fn a_huge_time_step_does_not_teleport_rocks_through_the_ship() {
        let mut a = game();
        let mut canvas = Canvas::new(80, 24);
        a.tick(Duration::from_secs(30), &mut canvas);
        a.tick(Duration::ZERO, &mut canvas);
    }

    #[test]
    fn absurd_sizes_do_not_panic() {
        let mut a = Asteroids::new();
        for (w, h) in [(0u16, 0u16), (1, 1), (24, 8), (200, 60)] {
            a.resize(w, h);
            let mut c = Canvas::new(w, h);
            a.tick(Duration::from_millis(33), &mut c);
        }
    }
}

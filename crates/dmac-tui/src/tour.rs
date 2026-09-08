//! Guided tours: the help that shows you, rather than tells you.
//!
//! A tour is a script that drives the *real* program the way a person would —
//! the same keys through the same `on_key`, the same clicks through the same
//! `on_mouse` — while a caption box says what is about to happen and a strip of
//! key caps shows exactly what is being pressed. Nothing is faked: the panels
//! move because the keys moved them, and if a key stopped working the tour
//! would visibly stop working with it. That is the point of driving the real
//! thing, and it is why every scenario is also a test that replays end to end.
//!
//! # Two things a tour must never do
//!
//! **Touch your files.** Scenarios that copy, move, delete and make folders run
//! in a session of their own, on a scratch tree this module creates and removes
//! — see [`Sandbox`]. A demonstration of F8 that deleted something of yours
//! would be a demonstration of why people do not trust demonstrations.
//!
//! **Launch things behind your back.** A step that would open your editor or
//! start an agent is *shown* — the key cap lights, the caption explains — and
//! not pressed. See [`Step::Show`].
//!
//! # How it runs
//!
//! [`Tour::tick`] is called before every frame with the time, and answers with
//! the input to inject when a step's moment has come. It never sleeps and never
//! blocks: the loop wakes on [`Tour::deadline`], the way it wakes for a
//! screensaver. Esc stops a tour at once; every other key is held back while
//! one plays, so a stray keystroke cannot wander into the demonstration.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A key as a scenario names it: enough to build the event and to label the
/// key cap, and nothing terminal-specific.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    F(u8),
    Enter,
    Esc,
    Tab,
    BackTab,
    Space,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
}

/// A key with its modifiers, which is what a person actually presses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub key: Key,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    pub const fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }
    pub const fn ctrl(key: Key) -> Self {
        Self {
            ctrl: true,
            ..Self::plain(key)
        }
    }
    pub const fn shift(key: Key) -> Self {
        Self {
            shift: true,
            ..Self::plain(key)
        }
    }
    pub const fn alt(key: Key) -> Self {
        Self {
            alt: true,
            ..Self::plain(key)
        }
    }

    /// The event the application receives — exactly what the terminal would
    /// have sent for this chord.
    pub fn event(self) -> KeyEvent {
        let code = match self.key {
            Key::Char(c) => KeyCode::Char(c),
            Key::F(n) => KeyCode::F(n),
            Key::Enter => KeyCode::Enter,
            Key::Esc => KeyCode::Esc,
            Key::Tab => KeyCode::Tab,
            Key::BackTab => KeyCode::BackTab,
            Key::Space => KeyCode::Char(' '),
            Key::Backspace => KeyCode::Backspace,
            Key::Delete => KeyCode::Delete,
            Key::Up => KeyCode::Up,
            Key::Down => KeyCode::Down,
            Key::Left => KeyCode::Left,
            Key::Right => KeyCode::Right,
            Key::PageUp => KeyCode::PageUp,
            Key::PageDown => KeyCode::PageDown,
            Key::Home => KeyCode::Home,
            Key::End => KeyCode::End,
            Key::Insert => KeyCode::Insert,
        };
        let mut m = KeyModifiers::NONE;
        if self.ctrl {
            m |= KeyModifiers::CONTROL;
        }
        if self.shift {
            m |= KeyModifiers::SHIFT;
        }
        if self.alt {
            m |= KeyModifiers::ALT;
        }
        KeyEvent::new(code, m)
    }

    /// The key caps, in the order a hand presses them: `Ctrl`, `Shift`, `Alt`,
    /// then the key. What the strip on screen shows.
    pub fn caps(self) -> Vec<String> {
        let mut out = Vec::new();
        if self.ctrl {
            out.push("Ctrl".to_string());
        }
        if self.shift {
            out.push("Shift".to_string());
        }
        if self.alt {
            out.push("Alt".to_string());
        }
        out.push(match self.key {
            Key::Char(c) => c.to_uppercase().to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Enter => "Enter".into(),
            Key::Esc => "Esc".into(),
            Key::Tab => "Tab".into(),
            Key::BackTab => "Shift+Tab".into(),
            Key::Space => "Space".into(),
            Key::Backspace => "Backspace".into(),
            Key::Delete => "Del".into(),
            Key::Up => "\u{2191}".into(),
            Key::Down => "\u{2193}".into(),
            Key::Left => "\u{2190}".into(),
            Key::Right => "\u{2192}".into(),
            Key::PageUp => "PgUp".into(),
            Key::PageDown => "PgDn".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::Insert => "Ins".into(),
        });
        out
    }
}

/// Where a mouse step lands. Symbolic, because a tour cannot know the
/// terminal's size: the application turns each of these into a cell with the
/// same arithmetic its own hit-tests use, so the pointer is drawn on exactly
/// the thing that gets pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A cell of the F-key bar, 1-based like the keys.
    FKey(u8),
    /// A command on the shell's bottom border, by its label.
    ShellCommand(&'static str),
    /// A row of the active panel, by index.
    PanelRow(usize),
}

/// One thing that happens in a tour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Say something, do nothing. How a scenario sets the scene.
    Say { caption: &'static str, hold: u64 },
    /// Press a chord and let the program react.
    Press {
        chord: Chord,
        caption: &'static str,
        hold: u64,
    },
    /// Type a run of characters, one per beat, so a name or a search appears
    /// the way a person types it.
    Type {
        text: &'static str,
        caption: &'static str,
        hold: u64,
    },
    /// Click somewhere, with the pointer shown landing there first.
    Click {
        at: Target,
        caption: &'static str,
        hold: u64,
    },
    /// Light the key caps and explain, without pressing: for the keys that
    /// would open your editor or start an agent, which a tour has no business
    /// doing behind your back.
    Show {
        chord: Chord,
        caption: &'static str,
        hold: u64,
    },
}

impl Step {
    pub fn caption(&self) -> &'static str {
        match self {
            Step::Say { caption, .. }
            | Step::Press { caption, .. }
            | Step::Type { caption, .. }
            | Step::Click { caption, .. }
            | Step::Show { caption, .. } => caption,
        }
    }

    /// How long the step stays on screen, in milliseconds.
    pub fn hold(&self) -> u64 {
        match self {
            Step::Say { hold, .. }
            | Step::Press { hold, .. }
            | Step::Type { hold, .. }
            | Step::Click { hold, .. }
            | Step::Show { hold, .. } => *hold,
        }
    }

    /// The key caps to light for this step, if any.
    pub fn caps(&self) -> Vec<String> {
        match self {
            Step::Press { chord, .. } | Step::Show { chord, .. } => chord.caps(),
            Step::Type { text, .. } => text.chars().map(|c| c.to_string()).collect(),
            Step::Click { .. } => vec!["\u{1f5b1}".to_string(), "click".to_string()],
            Step::Say { .. } => Vec::new(),
        }
    }
}

/// A scenario: a name, what it is about, and the steps.
#[derive(Debug, Clone, Copy)]
pub struct Scenario {
    pub name: &'static str,
    pub about: &'static str,
    /// Whether it needs the scratch session. Everything that touches files
    /// does; a tour of the help does not.
    pub sandboxed: bool,
    pub steps: &'static [Step],
}

// Shorthands, so the scenarios below read as scripts rather than as data.
const fn say(caption: &'static str) -> Step {
    Step::Say {
        caption,
        hold: 2600,
    }
}
const fn press(chord: Chord, caption: &'static str) -> Step {
    Step::Press {
        chord,
        caption,
        hold: 1800,
    }
}
const fn key(k: Key, caption: &'static str) -> Step {
    press(Chord::plain(k), caption)
}
const fn quick(k: Key) -> Step {
    Step::Press {
        chord: Chord::plain(k),
        caption: "",
        hold: 450,
    }
}
const fn type_in(text: &'static str, caption: &'static str) -> Step {
    Step::Type {
        text,
        caption,
        hold: 320,
    }
}
const fn click(at: Target, caption: &'static str) -> Step {
    Step::Click {
        at,
        caption,
        hold: 2000,
    }
}
const fn show(chord: Chord, caption: &'static str) -> Step {
    Step::Show {
        chord,
        caption,
        hold: 2800,
    }
}

/// Every tour there is, in the order the help lists them.
///
/// The captions say what is *about* to happen, in the present tense, before
/// the key is pressed — a caption that describes what just happened is a
/// caption you read after the screen already changed, which is too late to
/// watch for it.
pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "The panels",
        about: "two panels, and how to move between and inside them",
        sandboxed: true,
        steps: &[
            say(
                "Two panels. The left one is active: its border is brighter, and the cursor bar is in it.",
            ),
            key(Key::Down, "The arrows move the cursor. Down, twice."),
            quick(Key::Down),
            key(
                Key::Tab,
                "Tab switches to the other panel. Watch the border.",
            ),
            key(Key::Tab, "And back."),
            key(Key::Enter, "Enter on a folder goes into it."),
            say("The title on the border is where you are now."),
            key(Key::Backspace, "Backspace goes back up."),
            key(Key::End, "End and Home jump to the last and first entry."),
            quick(Key::Home),
            say("That is the whole of moving around. Everything else is a key on top of this."),
        ],
    },
    Scenario {
        name: "Marking files",
        about: "Space and Shift, and what the footer says about it",
        sandboxed: true,
        steps: &[
            say("Marking is how you say which files an operation is about."),
            key(Key::Down, "Onto a file."),
            key(
                Key::Space,
                "Space marks it — and moves on, so a run of files is one key held down.",
            ),
            quick(Key::Space),
            say(
                "Look at the bottom border: how many are marked, and for marked folders, how many files sit inside them.",
            ),
            press(
                Chord::shift(Key::Down),
                "Shift with an arrow extends a selection as you go.",
            ),
            quick(Key::Down),
            say("Ins does what Space does, for the orthodox hand."),
            key(
                Key::Esc,
                "Esc clears the marks. It is a ladder: it undoes the most recent thing first.",
            ),
            say("With nothing marked, an operation acts on the file under the cursor."),
        ],
    },
    Scenario {
        name: "Copying and moving",
        about: "F5 and F6, and why the other panel is already filled in",
        sandboxed: true,
        steps: &[
            say("This runs in a scratch folder made for the tour. Nothing of yours is touched."),
            key(Key::Down, "Mark two files."),
            quick(Key::Space),
            quick(Key::Space),
            key(
                Key::F(5),
                "F5 copies. It asks where — and the other panel's folder is already filled in.",
            ),
            say(
                "That is what having two panels means: one is the source, the other the destination. You can still edit it.",
            ),
            key(
                Key::Enter,
                "Enter, and the copy runs on its own thread. The status line reports as it goes.",
            ),
            say(
                "A copy verifies what it wrote. A move verifies with a hash before it deletes the original — always, whatever the options say.",
            ),
            key(Key::Tab, "Over to the other panel to see what arrived."),
            say(
                "F6 is the same dialog, for a move. F7 makes a folder. F8 deletes — to the trash, and it asks first.",
            ),
        ],
    },
    Scenario {
        name: "Deleting, carefully",
        about: "F8 asks, and goes to the trash",
        sandboxed: true,
        steps: &[
            say("Still in the scratch folder."),
            key(Key::Down, "Onto a file, and mark it."),
            quick(Key::Space),
            key(
                Key::F(8),
                "F8. It asks before anything happens, and says how many.",
            ),
            say(
                "It goes to the trash, not to nothing. A delete you can undo is worth the seconds it costs.",
            ),
            key(Key::Char('y'), "y confirms."),
            say(
                "If you had marked a folder, the question would have said so, and the whole tree would have gone together.",
            ),
        ],
    },
    Scenario {
        name: "Making a folder",
        about: "F7, and a name several levels deep",
        sandboxed: true,
        steps: &[
            key(Key::F(7), "F7 asks for a name."),
            type_in(
                "notes/2026",
                "Type a path with a slash and you get every level — that is what every file manager does.",
            ),
            key(Key::Enter, "Enter."),
            say("The panel reloads and the new folder is there."),
        ],
    },
    Scenario {
        name: "Looking at a file",
        about: "F3, the viewer: text, hex, and finding things",
        sandboxed: true,
        steps: &[
            key(Key::Down, "Onto README.md."),
            key(
                Key::F(3),
                "F3 views it. Never all of it: there is a cap, and a file past it says so on the frame.",
            ),
            key(Key::Char('/'), "Slash finds."),
            type_in("scratch", "Type what you are looking for."),
            key(
                Key::Enter,
                "Enter lands on the first match, highlighted inside the line — not the whole line.",
            ),
            key(Key::Char('n'), "n walks to the next, and wraps."),
            key(
                Key::Char('h'),
                "h switches to hex. A file with a NUL in its first kilobytes opens this way by itself.",
            ),
            key(Key::Char('h'), "And back."),
            key(
                Key::Esc,
                "Esc closes it. F4 would open the file in your editor instead.",
            ),
        ],
    },
    Scenario {
        name: "Sessions",
        about: "Ctrl-T, typing to find one, and groups",
        sandboxed: false,
        steps: &[
            press(
                Chord::ctrl(Key::Char('t')),
                "Ctrl-T opens the session rail. Each session is its own workspace: directories, shell, history.",
            ),
            say("Just start typing to find one. No key to press first."),
            type_in(
                "tou",
                "The cursor goes to the closest match. The list keeps its order, so the digits stay true.",
            ),
            key(
                Key::Esc,
                "Esc clears the search. A second Esc would close the rail.",
            ),
            say(
                "F2 renames. Del closes. Ctrl-N opens a new session here. Space folds a group away.",
            ),
            show(
                Chord::ctrl(Key::Char('a')),
                "Ctrl-A opens a session beside this one, with an agent that starts by knowing what this one knows. Not pressed here: it would start one.",
            ),
            key(Key::Esc, "Esc."),
            say("Alt with a digit jumps straight to a session by its number in the rail."),
        ],
    },
    Scenario {
        name: "The shell",
        about: "Ctrl-O, and the four keys that still reach the commander from inside",
        sandboxed: true,
        steps: &[
            press(
                Chord::ctrl(Key::Char('o')),
                "Ctrl-O switches to this session's shell. It starts where the panel is.",
            ),
            say(
                "Inside, the keyboard belongs to whatever runs here. The bottom border names the keys that still reach the commander — and each is clickable.",
            ),
            click(
                Target::ShellCommand("F12 history"),
                "Click F12 history on the border.",
            ),
            key(Key::Esc, "Esc closes the history."),
            press(
                Chord::ctrl(Key::Char('o')),
                "Ctrl-O again brings the panels back. Esc does the same when it has nothing else to undo.",
            ),
            say(
                "Shift-PgUp reads back through what the shell printed; Shift with the arrows selects it.",
            ),
        ],
    },
    Scenario {
        name: "Your own commands",
        about: "F2, and why a project's commands do not run until you say so",
        sandboxed: true,
        steps: &[
            key(
                Key::F(2),
                "F2 opens your commands. This scratch folder carries a .dmac-menu.toml, the way a project can.",
            ),
            say(
                "Its rows are shown and inert: a folder you did not write is not trusted until you say so. Cloning a repository must never mean running its author's commands by habit.",
            ),
            say(
                "T would trust it, against a hash of its contents — edit the file and it asks again. Not pressed here: that is your call, not the tour's.",
            ),
            key(Key::Esc, "Esc."),
            say(
                "Every value a command substitutes — {name}, {paths}, {dir} — is shell-quoted, with no way to ask for it unquoted. A file can legally be called '; rm -rf ~'.",
            ),
        ],
    },
    Scenario {
        name: "Agents and the editor",
        about: "F9: an agent that can see the panels, and the editor beside them",
        sandboxed: false,
        steps: &[
            key(
                Key::F(9),
                "F9 is the utilities: things to insert, and things to do.",
            ),
            say(
                "c starts claude in this session's shell, already connected — it can see both panels, the history and the screen it runs in. Not pressed: it would start one.",
            ),
            show(
                Chord::plain(Key::Char('c')),
                "It rejoins the same conversation every time, and is watched while it runs, so a crash does not lose the way back.",
            ),
            say(
                "o opens this folder in your editor, beside the commander. Windows come back where you left them — per set of monitors.",
            ),
            key(Key::Esc, "Esc. And x, or F10, is the way out."),
        ],
    },
    Scenario {
        name: "Leaving, and coming back",
        about: "F10 asks two things; the next start asks one",
        sandboxed: false,
        steps: &[
            key(Key::F(10), "F10 does not leave. It asks."),
            say(
                "Two ticks. Reopen the same windows next time — and remember their positions on this set of monitors.",
            ),
            say(
                "Untick the second one to leave with a layout you made a mess of, without it becoming the memory. Both are kept as you leave them.",
            ),
            say(
                "On the next start, one list: every agent and every window the last run had, each ticked. Untick what you do not want that morning.",
            ),
            key(
                Key::Esc,
                "Esc stays. This tour is not going to quit on you.",
            ),
        ],
    },
];

/// The tour engine: which scenario, which step, and when the next one is due.
#[derive(Debug)]
pub struct Tour {
    pub scenario: &'static Scenario,
    /// Index into `steps`, and — for a `Type` step — how many characters of it
    /// have been typed so far.
    step: usize,
    typed: usize,
    /// When the current beat started.
    since: Instant,
    /// Whether the current step's input has been handed out yet. A step is
    /// shown for a moment *before* its key is pressed, so the caption can be
    /// read, and then holds for a moment after.
    fired: bool,
}

/// What the engine wants the application to do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(KeyEvent),
    Click(Target),
}

/// How long a caption is shown before its key is pressed. Long enough to read
/// one line; the press is then what the reader is waiting for.
pub const LEAD: Duration = Duration::from_millis(1400);

impl Tour {
    pub fn start(scenario: &'static Scenario, now: Instant) -> Self {
        Self {
            scenario,
            step: 0,
            typed: 0,
            since: now,
            fired: false,
        }
    }

    pub fn current(&self) -> Option<&'static Step> {
        self.scenario.steps.get(self.step)
    }

    pub fn finished(&self) -> bool {
        self.step >= self.scenario.steps.len()
    }

    /// `(this step, of how many)`, 1-based for a person.
    pub fn progress(&self) -> (usize, usize) {
        (
            (self.step + 1).min(self.scenario.steps.len()),
            self.scenario.steps.len(),
        )
    }

    /// The key caps to light right now. For a `Type` step, only what has been
    /// typed so far, so the strip fills in as the text does.
    pub fn caps(&self) -> Vec<String> {
        match self.current() {
            Some(Step::Type { text, .. }) => text
                .chars()
                .take(self.typed)
                .map(|c| c.to_string())
                .collect(),
            Some(s) if self.fired => s.caps(),
            _ => Vec::new(),
        }
    }

    /// Advance to `now`, and say what to inject, if anything.
    ///
    /// Each step has a lead — the caption alone — then its input, then a hold.
    /// A `Type` step fires one character per hold. Called before every frame;
    /// cheap when nothing is due.
    pub fn tick(&mut self, now: Instant) -> Option<Input> {
        let step = *self.current()?;
        let elapsed = now.saturating_duration_since(self.since);

        match step {
            Step::Say { hold, .. } | Step::Show { hold, .. } => {
                // Show has caps lit from the start: there is nothing to wait for.
                self.fired = true;
                if elapsed >= Duration::from_millis(hold) {
                    self.advance(now);
                }
                None
            }
            Step::Press { chord, hold, .. } => {
                if !self.fired && elapsed >= LEAD {
                    self.fired = true;
                    self.since = now;
                    return Some(Input::Key(chord.event()));
                }
                if self.fired && elapsed >= Duration::from_millis(hold) {
                    self.advance(now);
                }
                None
            }
            Step::Click { at, hold, .. } => {
                if !self.fired && elapsed >= LEAD {
                    self.fired = true;
                    self.since = now;
                    return Some(Input::Click(at));
                }
                if self.fired && elapsed >= Duration::from_millis(hold) {
                    self.advance(now);
                }
                None
            }
            Step::Type { text, hold, .. } => {
                let total = text.chars().count();
                let wait = if self.typed == 0 {
                    LEAD
                } else {
                    Duration::from_millis(hold)
                };
                if self.typed < total && elapsed >= wait {
                    let c = text.chars().nth(self.typed)?;
                    self.typed += 1;
                    self.fired = true;
                    self.since = now;
                    return Some(Input::Key(Chord::plain(Key::Char(c)).event()));
                }
                if self.typed >= total && elapsed >= Duration::from_millis(hold * 3) {
                    self.advance(now);
                }
                None
            }
        }
    }

    fn advance(&mut self, now: Instant) {
        self.step += 1;
        self.typed = 0;
        self.fired = false;
        self.since = now;
    }

    /// When the loop should wake to move the tour on, so it plays without
    /// anyone pressing anything.
    pub fn deadline(&self) -> Option<Instant> {
        let step = self.current()?;
        let next = match step {
            Step::Say { hold, .. } | Step::Show { hold, .. } => Duration::from_millis(*hold),
            Step::Press { hold, .. } | Step::Click { hold, .. } => {
                if self.fired {
                    Duration::from_millis(*hold)
                } else {
                    LEAD
                }
            }
            Step::Type { text, hold, .. } => {
                if self.typed == 0 {
                    LEAD
                } else if self.typed < text.chars().count() {
                    Duration::from_millis(*hold)
                } else {
                    Duration::from_millis(*hold * 3)
                }
            }
        };
        Some(self.since + next)
    }
}

/// A scratch tree for the tours that touch files, removed when dropped.
///
/// Small and legible on purpose: a handful of files with names a caption can
/// point at, a folder to enter, a second folder to copy into, and a
/// `.dmac-menu.toml` so the F2 tour has something to be untrusting about.
/// Removed on drop, so a tour stopped with Esc halfway through still cleans up.
#[derive(Debug)]
pub struct Sandbox {
    pub root: PathBuf,
}

impl Sandbox {
    /// Make the tree. `None` if the temp directory cannot be written, in which
    /// case the file tours are simply not offered rather than run on nothing.
    pub fn create() -> Option<Self> {
        let root = std::env::temp_dir().join(format!("dmac-tour-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).ok()?;
        std::fs::create_dir_all(root.join("photos")).ok()?;
        std::fs::create_dir_all(root.join("out")).ok()?;
        let files: &[(&str, &str)] = &[
            (
                "README.md",
                "# A scratch folder\n\nMade by the tour, removed when it ends.\nNothing here is yours.\n\nThis is a scratch tree with a few files in it,\nso the keys have something to act on.\n",
            ),
            ("notes.txt", "one\ntwo\nthree\n"),
            ("todo.md", "- try F5\n- try F8\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("src/lib.rs", "pub fn hello() {}\n"),
            ("photos/one.jpg", "not really a photo\n"),
            ("photos/two.jpg", "not really a photo\n"),
            (
                ".dmac-menu.toml",
                "[[entry]]\nkey = \"b\"\ntitle = \"build\"\nrun = \"echo build\"\n\n[[entry]]\nkey = \"t\"\ntitle = \"test\"\nrun = \"echo test\"\n",
            ),
        ];
        for (name, body) in files {
            std::fs::write(root.join(name), body).ok()?;
        }
        Some(Self { root })
    }

    pub fn left(&self) -> &Path {
        &self.root
    }

    pub fn right(&self) -> PathBuf {
        self.root.join("out")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every scenario is well-formed: named, described, at least a few steps,
    /// and every step has a caption a person can read — except the quick
    /// repeats, which deliberately say nothing.
    #[test]
    fn every_scenario_is_something_a_person_can_follow() {
        assert!(
            SCENARIOS.len() >= 8,
            "the tours are supposed to cover the program"
        );
        let mut names = std::collections::BTreeSet::new();
        for s in SCENARIOS {
            assert!(!s.name.is_empty() && !s.about.is_empty());
            assert!(names.insert(s.name), "{} is listed twice", s.name);
            assert!(
                s.steps.len() >= 3,
                "{} is too short to show anything",
                s.name
            );
            for step in s.steps {
                let quick_repeat = matches!(step, Step::Press { caption: "", .. });
                assert!(
                    quick_repeat || !step.caption().is_empty(),
                    "{}: a step with nothing to say",
                    s.name
                );
                assert!(step.hold() >= 300, "{}: a step nobody could see", s.name);
            }
        }
    }

    /// A tour must never leave the program in a dialog that would quit, and
    /// must never *press* the keys that would launch something. This is the
    /// lint that keeps a demonstration from being the thing it warns about.
    #[test]
    fn no_tour_quits_or_launches_anything() {
        for s in SCENARIOS {
            let mut open_quit = false;
            for step in s.steps {
                if let Step::Press { chord, .. } = step {
                    // F10 opens the quit dialog; the next press had better be Esc.
                    if chord.key == Key::F(10) {
                        open_quit = true;
                        continue;
                    }
                    if open_quit {
                        assert_eq!(
                            chord.key,
                            Key::Esc,
                            "{}: after F10 the only safe key is Esc",
                            s.name
                        );
                        open_quit = false;
                    }
                    // The keys that start things are shown, never pressed.
                    let launches = chord.ctrl && chord.key == Key::Char('a');
                    assert!(
                        !launches,
                        "{}: presses Ctrl-A, which starts an agent",
                        s.name
                    );
                }
            }
            assert!(!open_quit, "{} ends inside the quit dialog", s.name);
        }
    }

    /// The engine hands out inputs in order, one per beat, and finishes. Driven
    /// with a clock rather than a sleep, which is also how the loop drives it.
    #[test]
    fn a_tour_plays_its_steps_in_order_and_finishes() {
        let scenario = &SCENARIOS[0];
        let t0 = Instant::now();
        let mut tour = Tour::start(scenario, t0);
        let mut inputs = Vec::new();
        let mut now = t0;
        let mut guard = 0;
        while !tour.finished() {
            now += Duration::from_millis(200);
            if let Some(i) = tour.tick(now) {
                inputs.push(i);
            }
            guard += 1;
            assert!(guard < 10_000, "the tour never ends");
        }
        let presses = scenario
            .steps
            .iter()
            .filter(|s| matches!(s, Step::Press { .. } | Step::Click { .. }))
            .count();
        let typed: usize = scenario
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Type { text, .. } => Some(text.chars().count()),
                _ => None,
            })
            .sum();
        assert_eq!(inputs.len(), presses + typed, "{inputs:?}");
        // The first input is the first Press's chord.
        let first = scenario
            .steps
            .iter()
            .find_map(|s| match s {
                Step::Press { chord, .. } => Some(chord.event()),
                _ => None,
            })
            .expect("a press");
        assert_eq!(inputs[0], Input::Key(first));
    }

    /// A caption shows for a moment before its key is pressed: the reader has
    /// to be told what to watch for, then see it.
    #[test]
    fn the_caption_leads_and_the_key_follows() {
        let scenario = &SCENARIOS[0];
        let t0 = Instant::now();
        let mut tour = Tour::start(scenario, t0);
        // Skip the opening Say.
        let say = scenario.steps[0].hold();
        assert!(tour.tick(t0 + Duration::from_millis(say)).is_none());
        assert!(matches!(tour.current(), Some(Step::Press { .. })));
        assert!(
            tour.caps().is_empty(),
            "caps lit before the key was pressed"
        );
        // Before the lead: still only the caption.
        assert!(tour.tick(t0 + Duration::from_millis(say + 200)).is_none());
        // At the lead: the key.
        let now = t0 + Duration::from_millis(say) + LEAD;
        assert!(matches!(tour.tick(now), Some(Input::Key(_))));
        assert!(!tour.caps().is_empty(), "the pressed key is not lit");
        assert!(tour.deadline().is_some(), "nothing would wake the loop");
    }

    /// Typing goes one character a beat, and the strip fills in as it does.
    #[test]
    fn a_type_step_types_one_character_at_a_time() {
        let scenario = SCENARIOS
            .iter()
            .find(|s| s.name == "Making a folder")
            .expect("the folder tour");
        let t0 = Instant::now();
        let mut tour = Tour::start(scenario, t0);
        // Past the F7 press.
        let mut now = t0;
        let mut seen = String::new();
        let mut guard = 0;
        while !matches!(tour.current(), Some(Step::Type { .. })) {
            now += Duration::from_millis(250);
            tour.tick(now);
            guard += 1;
            assert!(guard < 200);
        }
        while matches!(tour.current(), Some(Step::Type { .. })) {
            now += Duration::from_millis(250);
            if let Some(Input::Key(k)) = tour.tick(now)
                && let KeyCode::Char(c) = k.code
            {
                seen.push(c);
                assert_eq!(
                    tour.caps().len(),
                    seen.chars().count(),
                    "the strip lags the typing"
                );
            }
        }
        assert_eq!(seen, "notes/2026");
    }

    /// Every key labels itself the way a person would say it, and builds the
    /// event the terminal would have sent.
    #[test]
    fn chords_label_themselves_and_build_the_right_event() {
        let c = Chord::ctrl(Key::Char('t'));
        assert_eq!(c.caps(), ["Ctrl", "T"]);
        assert_eq!(
            c.event(),
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)
        );
        assert_eq!(Chord::shift(Key::Down).caps(), ["Shift", "\u{2193}"]);
        assert_eq!(Chord::plain(Key::F(5)).caps(), ["F5"]);
        assert_eq!(Chord::plain(Key::Space).event().code, KeyCode::Char(' '));
    }

    /// The scratch tree exists while the sandbox does, holds what the captions
    /// point at, and is gone when it is dropped — Esc halfway through included.
    #[test]
    fn the_sandbox_is_made_and_removed() {
        let root = {
            let sb = Sandbox::create().expect("a scratch tree");
            assert!(sb.root.join("README.md").is_file());
            assert!(sb.root.join("src/main.rs").is_file());
            assert!(sb.root.join(".dmac-menu.toml").is_file());
            assert!(sb.right().is_dir(), "the other panel has nowhere to be");
            sb.root.clone()
        };
        assert!(!root.exists(), "the scratch tree outlived the sandbox");
    }
}

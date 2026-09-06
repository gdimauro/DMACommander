//! Sessions on disk.
//!
//! One file holding every session, written atomically. Not a directory per
//! session: the whole set changes together — closing one shifts the rest — and
//! a single atomic rename cannot leave half a workspace on disk the way a
//! multi-file update can.
//!
//! Everything here is forgiving by design. A missing file is a first run. A
//! corrupt one is backed up and replaced rather than blocking startup: being
//! locked out of your file manager by a bad JSON file is not an acceptable
//! failure mode, and the backup means nothing is lost while it is investigated.

use crate::{Focus, Session, SessionColor, SessionId, SessionManager, View};
use dmac_core::{SortKey, SortOrder};
use dmac_vfs::VfsPath;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bumped when the shape changes incompatibly. An older binary reading a newer
/// file refuses it and keeps the file, rather than rewriting it as something the
/// newer binary can no longer read.
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("could not determine a config directory for this platform")]
    NoConfigDir,
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: not valid session data ({source})")]
    Corrupt {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error(
        "{path} was written by a newer version of DMACommander (format {found}, we speak {ours})"
    )]
    TooNew { path: String, found: u32, ours: u32 },
}

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Serialize, Deserialize)]
struct Persisted {
    version: u32,
    /// `false` while running, `true` once we shut down cleanly. A `false` found
    /// at startup means the last run crashed, which the user is told about
    /// rather than left to wonder why their layout looks stale.
    clean_exit: bool,
    /// Which session was on screen.
    current: usize,
    sessions: Vec<PersistedSession>,
    /// Every directory visited, so the history survives a restart. Absent in
    /// files written before it existed, which is a first run for the history
    /// and nothing else.
    #[serde(default)]
    history: Vec<dmac_core::history::Visit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSession {
    name: String,
    #[serde(default)]
    color: u8,
    left: String,
    right: String,
    #[serde(default)]
    active_right: bool,
    #[serde(default)]
    focus_command_line: bool,
    #[serde(default)]
    view_shell: bool,
    #[serde(default)]
    panels: [PersistedPanel; 2],
    /// The agent conversation this session owns. Kept across restarts on
    /// purpose: it is the one thing that lets a hosted `claude` come back to
    /// what you were talking about rather than starting beside it.
    #[serde(default)]
    conversation: Option<String>,
    /// What was typed on the command line and not yet run. Half a command is
    /// still work, and losing it on restart is losing work.
    #[serde(default)]
    command_line: String,
    /// The attached agent that was running when this was saved, so the next run
    /// can start it again in the same conversation.
    #[serde(default)]
    agent: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedPanel {
    #[serde(default)]
    sort_key: Option<SortKey>,
    #[serde(default)]
    sort_descending: bool,
    #[serde(default)]
    show_hidden: bool,
}

/// Where sessions live, and how to read and write them.
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// The default location for this platform.
    pub fn platform_default() -> Result<Self> {
        // `~/.config/dmac` when it already exists, even on macOS: people who
        // keep a dotfiles repo expect it there, and quietly using the Apple
        // location instead means their sessions are not in their backup.
        if let Some(home) = home_dir() {
            let xdg = home.join(".config").join("dmac");
            if xdg.exists() {
                return Ok(Self::at(xdg.join("sessions.json")));
            }
        }
        let dirs = directories::ProjectDirs::from("", "", "DMACommander")
            .ok_or(StoreError::NoConfigDir)?;
        Ok(Self::at(dirs.config_dir().join("sessions.json")))
    }

    /// A store at an explicit path. Tests use this so they never touch `$HOME`.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load every session.
    ///
    /// `Ok(None)` means there is nothing saved yet — a first run, which is not
    /// an error. The bool says whether the previous run exited cleanly.
    pub fn load(&self) -> Result<Option<(SessionManager, bool)>> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(StoreError::Io {
                    path: self.path.display().to_string(),
                    source: e,
                });
            }
        };

        let saved: Persisted =
            serde_json::from_str(&text).map_err(|source| StoreError::Corrupt {
                path: self.path.display().to_string(),
                source,
            })?;

        if saved.version > FORMAT_VERSION {
            return Err(StoreError::TooNew {
                path: self.path.display().to_string(),
                found: saved.version,
                ours: FORMAT_VERSION,
            });
        }
        if saved.sessions.is_empty() {
            return Ok(None);
        }

        let mut it = saved.sessions.into_iter();
        // There is always at least one; the check above guarantees it.
        let Some(first) = it.next() else {
            return Ok(None);
        };
        let mut manager = SessionManager::new(
            first.name.clone(),
            VfsPath::local(&first.left),
            VfsPath::local(&first.right),
        );
        apply(manager.current_mut(), &first);

        for s in it {
            let i = manager.create(
                s.name.clone(),
                VfsPath::local(&s.left),
                VfsPath::local(&s.right),
            );
            apply(manager.at_mut(i), &s);
        }

        manager.history = dmac_core::history::History::from_visits(saved.history);
        manager.switch_to(saved.current.min(manager.len() - 1));
        Ok(Some((manager, saved.clean_exit)))
    }

    /// Write every session, atomically.
    ///
    /// Temp file plus rename: a `kill -9` half way through must never leave a
    /// truncated file, because the next start would then find corrupt data and
    /// the user would lose a layout they never asked to change.
    pub fn save(&self, manager: &SessionManager, clean_exit: bool) -> Result<()> {
        let saved = Persisted {
            version: FORMAT_VERSION,
            clean_exit,
            current: manager.current_index(),
            sessions: manager.all().iter().map(persist).collect(),
            history: manager.history.visits().to_vec(),
        };
        let json = serde_json::to_string_pretty(&saved).map_err(|source| StoreError::Corrupt {
            path: self.path.display().to_string(),
            source,
        })?;

        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|source| StoreError::Io {
                path: dir.display().to_string(),
                source,
            })?;
        }

        // The temp file must be in the same directory, or the rename crosses a
        // filesystem boundary and stops being atomic.
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|source| StoreError::Io {
            path: tmp.display().to_string(),
            source,
        })?;
        std::fs::rename(&tmp, &self.path).map_err(|source| StoreError::Io {
            path: self.path.display().to_string(),
            source,
        })
    }

    /// Move a file we could not parse out of the way, so startup can continue
    /// without destroying whatever it held.
    pub fn quarantine(&self) -> Option<PathBuf> {
        let backup = self.path.with_extension("json.broken");
        std::fs::rename(&self.path, &backup).ok().map(|()| backup)
    }
}

fn persist(s: &Session) -> PersistedSession {
    PersistedSession {
        name: s.name.clone(),
        color: s.color as u8,
        left: s.cwd[0].display(),
        right: s.cwd[1].display(),
        active_right: s.active == dmac_core::PanelId::Right,
        focus_command_line: s.focus == Focus::CommandLine,
        // The shell is deliberately not persisted: restoring a *view* of a
        // process that no longer exists would show a dead screen. The shell is
        // respawned on demand, in the directory that was restored.
        view_shell: s.view == View::Shell,
        conversation: s.conversation.clone(),
        command_line: s.command_line.clone(),
        agent: s.agent.clone(),
        panels: [
            PersistedPanel {
                sort_key: Some(s.panels[0].sort_key),
                sort_descending: s.panels[0].sort_order == SortOrder::Descending,
                show_hidden: s.panels[0].show_hidden,
            },
            PersistedPanel {
                sort_key: Some(s.panels[1].sort_key),
                sort_descending: s.panels[1].sort_order == SortOrder::Descending,
                show_hidden: s.panels[1].show_hidden,
            },
        ],
    }
}

fn apply(session: &mut Session, saved: &PersistedSession) {
    session.color = colour_from(saved.color);
    session.active = if saved.active_right {
        dmac_core::PanelId::Right
    } else {
        dmac_core::PanelId::Left
    };
    session.focus = if saved.focus_command_line {
        Focus::CommandLine
    } else {
        Focus::Panel
    };
    // The process is gone, the conversation is not. This is what makes the
    // next `claude` in this session pick up where the last one left off.
    session.conversation = saved.conversation.clone();
    // Nothing has been listed yet; the first visit does that.
    session.loaded = false;
    session.command_line = saved.command_line.clone();
    // Not started here — the store does not spawn processes. Recorded so the
    // caller, which owns the event loop and the waker, can put it back.
    session.reattach = saved.agent.clone();
    // A session that was showing its shell comes back showing its shell. This
    // used to be refused because there was nothing to show; now there is.
    session.view = if saved.view_shell {
        View::Shell
    } else {
        View::Panels
    };

    for (i, p) in saved.panels.iter().enumerate() {
        if let Some(key) = p.sort_key {
            session.panels[i].sort_key = key;
        }
        session.panels[i].sort_order = if p.sort_descending {
            SortOrder::Descending
        } else {
            SortOrder::Ascending
        };
        session.panels[i].show_hidden = p.show_hidden;
    }
}

fn colour_from(i: u8) -> SessionColor {
    SessionColor::nth(i as usize)
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
    #[cfg(windows)]
    {
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    }
}

/// Suppresses the unused warning for `SessionId` in this module's public shape.
const _: fn(SessionId) = |_| {};

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test owns a TempDir; none of them may touch $HOME.
    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SessionStore::at(dir.path().join("sessions.json"));
        (dir, store)
    }

    fn manager() -> SessionManager {
        let mut m = SessionManager::new("work", VfsPath::local("/a"), VfsPath::local("/b"));
        m.create("build", VfsPath::local("/c"), VfsPath::local("/d"));
        m
    }

    #[test]
    fn a_first_run_finds_nothing_and_that_is_not_an_error() {
        let (_d, s) = store();
        assert!(s.load().expect("load").is_none());
    }

    #[test]
    fn sessions_survive_a_round_trip() {
        let (_d, s) = store();
        let mut m = manager();
        m.switch_to(1);
        m.current_mut().focus = Focus::CommandLine;
        m.current_mut().panels[0].sort_key = SortKey::Size;
        m.current_mut().panels[0].show_hidden = true;
        s.save(&m, true).expect("save");

        let (loaded, clean) = s.load().expect("load").expect("some");
        assert!(clean);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.all()[0].name, "work");
        assert_eq!(loaded.current_index(), 1, "the visible session is restored");
        assert_eq!(loaded.current().focus, Focus::CommandLine);
        assert_eq!(loaded.current().panels[0].sort_key, SortKey::Size);
        assert!(loaded.current().panels[0].show_hidden);
        assert_eq!(loaded.current().cwd[0].display(), "/c");
    }

    /// An unclean exit has to be visible, or a stale layout looks like a bug.
    #[test]
    fn an_unclean_exit_is_reported_on_the_next_load() {
        let (_d, s) = store();
        s.save(&manager(), false).expect("save");
        let (_m, clean) = s.load().expect("load").expect("some");
        assert!(!clean, "the crash must be visible on the next start");
    }

    /// A session that was showing its shell comes back showing its shell.
    ///
    /// This used to be refused, on the grounds that the pane would be empty —
    /// which was true when nothing respawned the shell. Now something does, and
    /// dropping the user back on the panels loses where they actually were.
    #[test]
    fn a_session_comes_back_to_the_view_it_was_left_in() {
        let (_d, s) = store();
        let mut m = manager();
        m.current_mut().view = View::Shell;
        s.save(&m, true).expect("save");
        let (loaded, _) = s.load().expect("load").expect("some");
        assert_eq!(loaded.current().view, View::Shell);

        let mut m = manager();
        m.current_mut().view = View::Panels;
        s.save(&m, true).expect("save");
        let (loaded, _) = s.load().expect("load").expect("some");
        assert_eq!(loaded.current().view, View::Panels);
    }

    /// Half a command is still work, and losing it on restart is losing work.
    #[test]
    fn what_was_typed_and_not_run_comes_back() {
        let (_d, s) = store();
        let mut m = manager();
        m.current_mut().command_line = "rsync -av --dry-run ".to_string();
        s.save(&m, true).expect("save");
        let (loaded, _) = s.load().expect("load").expect("some");
        assert_eq!(loaded.current().command_line, "rsync -av --dry-run ");
    }

    /// The conversation id and what was running in it are the two halves of
    /// coming back to the same agent; one without the other is no use.
    #[test]
    fn the_agent_and_its_conversation_both_come_back() {
        let (_d, s) = store();
        let mut m = manager();
        m.current_mut().conversation = Some("11111111-2222-4333-8444-555555555555".into());
        m.current_mut().agent = Some("claude --session-id 1234".into());
        s.save(&m, true).expect("save");

        let (loaded, _) = s.load().expect("load").expect("some");
        assert_eq!(
            loaded.current().conversation.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
        assert_eq!(
            loaded.current().reattach.as_deref(),
            Some("claude --session-id 1234"),
            "the agent's own arguments have to come back with it"
        );
    }

    /// A session with no agent must not have one started in it. Launching a
    /// program the user never ran is worse than not restoring one they did.
    #[test]
    fn a_session_without_an_agent_asks_for_nothing() {
        let (_d, s) = store();
        s.save(&manager(), true).expect("save");
        let (loaded, _) = s.load().expect("load").expect("some");
        assert!(loaded.current().reattach.is_none());
    }

    #[test]
    fn a_corrupt_file_is_reported_and_can_be_quarantined() {
        let (_d, s) = store();
        std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
        std::fs::write(s.path(), "{ this is not json").unwrap();
        assert!(matches!(s.load(), Err(StoreError::Corrupt { .. })));

        let backup = s.quarantine().expect("quarantined");
        assert!(backup.exists(), "the original must be kept, not deleted");
        assert!(!s.path().exists());
        assert!(
            s.load().expect("load").is_none(),
            "and startup can continue"
        );
    }

    /// An older binary must not rewrite a newer file into something the newer
    /// binary can no longer read.
    #[test]
    fn a_file_from_a_newer_version_is_refused_rather_than_downgraded() {
        let (_d, s) = store();
        std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
        std::fs::write(
            s.path(),
            r#"{"version":9999,"clean_exit":true,"current":0,"sessions":[]}"#,
        )
        .unwrap();
        assert!(matches!(s.load(), Err(StoreError::TooNew { .. })));
        assert!(s.path().exists(), "and the file is left alone");
    }

    /// Fields added in a later version must not make an older file unreadable.
    #[test]
    fn a_file_missing_optional_fields_still_loads() {
        let (_d, s) = store();
        std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
        std::fs::write(
            s.path(),
            r#"{"version":1,"clean_exit":true,"current":0,
                "sessions":[{"name":"old","left":"/x","right":"/y"}]}"#,
        )
        .unwrap();
        let (m, _) = s.load().expect("load").expect("some");
        assert_eq!(m.all()[0].name, "old");
        assert_eq!(m.all()[0].cwd[0].display(), "/x");
    }

    /// A kill mid-save must never leave a truncated file in place.
    #[test]
    fn saving_never_leaves_a_partial_file_behind() {
        let (_d, s) = store();
        s.save(&manager(), true).expect("first save");
        for _ in 0..20 {
            s.save(&manager(), true).expect("save");
            let text = std::fs::read_to_string(s.path()).expect("read");
            serde_json::from_str::<serde_json::Value>(&text)
                .expect("the file on disk is always complete JSON");
        }
        assert!(
            !s.path().with_extension("json.tmp").exists(),
            "the temp file must not survive a successful save"
        );
    }

    #[test]
    fn an_empty_session_list_is_treated_as_no_sessions() {
        let (_d, s) = store();
        std::fs::create_dir_all(s.path().parent().unwrap()).unwrap();
        std::fs::write(
            s.path(),
            r#"{"version":1,"clean_exit":true,"current":0,"sessions":[]}"#,
        )
        .unwrap();
        assert!(s.load().expect("load").is_none());
    }
}

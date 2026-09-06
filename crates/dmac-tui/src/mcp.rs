//! The commander, as tools an agent can call.
//!
//! [`dmac_mcp`] owns the protocol; this owns the answers. Every tool here runs
//! on the UI thread, in between frames, with the whole application in hand — no
//! locks, no snapshots, no chance of reporting a panel that has since moved.
//!
//! The rule these follow: **read freely, write visibly.** Anything that changes
//! what the user is looking at says so in its result and shows on screen, and
//! the one tool that can run a command defaults to typing it rather than
//! running it. An agent that quietly moved someone's panels would be a poltergeist.

use crate::app::App;
use dmac_core::{EntryKind, PanelId};
use dmac_mcp::Commander;
use dmac_vfs::VfsPath;
use serde_json::{Value, json};

/// How many entries a single `list` returns before it stops.
const DEFAULT_LIST: usize = 200;
const MAX_LIST: usize = 2000;
const DEFAULT_HISTORY: usize = 30;

impl Commander for App {
    fn call(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "state" => Ok(self.mcp_state()),
            "list" => self.mcp_list(args),
            "navigate" => self.mcp_navigate(args),
            "select" => self.mcp_select(args),
            "command" => self.mcp_command(args),
            "history" => Ok(self.mcp_history(args)),
            "sessions" => Ok(self.mcp_sessions()),
            "switch_session" => self.mcp_switch_session(args),
            "notify" => self.mcp_notify(args),
            "screen" => Ok(self.mcp_screen()),
            // Unreachable: the protocol layer checks the catalogue first. Said
            // out loud anyway, because "silently did nothing" is the worst
            // possible answer to give a model.
            other => Err(format!("no such tool: {other}")),
        }
    }
}

impl App {
    /// Which session a call is about: the one the calling agent is hosted in,
    /// falling back to whatever is on screen.
    ///
    /// This is the difference between an agent that can see its own panels and
    /// one that reports on whichever session the user last clicked.
    fn mcp_index(&self) -> usize {
        self.mcp_session
            .and_then(|id| self.sessions.all().iter().position(|s| s.id == id))
            .unwrap_or_else(|| self.sessions.current_index())
    }

    fn mcp_state(&self) -> Value {
        let index = self.mcp_index();
        let s = self.sessions.at(index);
        json!({
            "session": { "name": s.name, "index": index, "of": self.sessions.len() },
            "on_screen": index == self.sessions.current_index(),
            "view": view_name(s.view),
            "active": panel_name(s.active),
            "panels": {
                "left": panel_state(s, PanelId::Left),
                "right": panel_state(s, PanelId::Right),
            },
        })
    }

    fn mcp_list(&mut self, args: &Value) -> Result<Value, String> {
        let index = self.mcp_index();
        let limit = arg_usize(args, "limit").unwrap_or(DEFAULT_LIST).min(MAX_LIST);

        // No path: answer from the panel itself. It is already loaded, it is
        // what the user is looking at, and it costs nothing.
        //
        // Unless it is empty — a listing is asynchronous, so a `list` that
        // follows a `navigate` closely arrives while the walk is still running.
        // Answering "nothing here" would be a lie with no way to tell it apart
        // from an empty directory, so that case falls through to reading the
        // directory directly.
        let mut from_panel = None;
        if arg_str(args, "path").is_none() {
            let s = self.sessions.at(index);
            let i = dmac_session::Session::index_of(s.active);
            if s.panels[i].entries.is_empty() {
                from_panel = Some(s.cwd[i].display());
            } else {
                let entries: Vec<Value> = s.panels[i]
                    .entries
                    .iter()
                    .take(limit)
                    .map(|e| {
                        json!({
                            "name": e.name,
                            "kind": kind_name(e.kind),
                            "size": e.size,
                            "selected": e.selected,
                        })
                    })
                    .collect();
                let total = s.panels[i].entries.len();
                return Ok(json!({
                    "path": s.cwd[i].display(),
                    "from": "the active panel",
                    "entries": entries,
                    "total": total,
                    "truncated": total > limit,
                }));
            }
        }
        let owned;
        let path = match (&from_panel, arg_str(args, "path")) {
            (Some(p), _) => {
                owned = p.clone();
                owned.as_str()
            }
            (None, Some(p)) => p,
            // Unreachable: one of the two is always set by the block above.
            (None, None) => return Err("list needs a path".to_string()),
        };

        // A path we are not showing has to be read. Blocking, on the UI thread,
        // deliberately: a local directory read is sub-millisecond, and doing it
        // asynchronously would mean answering a tool call before knowing the
        // answer. A path on a stalled network mount will stall a frame; that is
        // the trade, and it is the same one the shell makes.
        let resolved = self.mcp_resolve(index, path)?;
        let dir = std::path::PathBuf::from(&resolved);
        let read = std::fs::read_dir(&dir).map_err(|e| format!("{resolved}: {e}"))?;

        let mut entries: Vec<Value> = Vec::new();
        let mut total = 0usize;
        for item in read {
            let Ok(item) = item else { continue };
            total += 1;
            if entries.len() >= limit {
                continue;
            }
            let meta = item.metadata().ok();
            let kind = match &meta {
                Some(m) if m.is_dir() => "dir",
                Some(m) if m.is_symlink() => "symlink",
                Some(m) if m.is_file() => "file",
                _ => "other",
            };
            entries.push(json!({
                "name": item.file_name().to_string_lossy(),
                "kind": kind,
                "size": meta.as_ref().filter(|m| m.is_file()).map(std::fs::Metadata::len),
            }));
        }
        Ok(json!({
            "path": resolved,
            "entries": entries,
            "total": total,
            "truncated": total > entries.len(),
        }))
    }

    fn mcp_navigate(&mut self, args: &Value) -> Result<Value, String> {
        let index = self.mcp_index();
        let panel = arg_panel(args, self.sessions.at(index).active)?;
        let path = arg_str(args, "path").ok_or("navigate needs a path")?;
        let resolved = self.mcp_resolve_for(index, panel, path)?;

        if !std::path::Path::new(&resolved).is_dir() {
            return Err(format!("{resolved} is not a directory"));
        }
        self.go_to_panel(index, panel, VfsPath::local(&resolved));
        Ok(json!({
            "path": resolved,
            "panel": panel_name(panel),
            "session": self.sessions.at(index).name,
        }))
    }

    fn mcp_select(&mut self, args: &Value) -> Result<Value, String> {
        let index = self.mcp_index();
        let panel = arg_panel(args, self.sessions.at(index).active)?;
        let mode = arg_str(args, "mode").unwrap_or("set");
        let names: Vec<String> = args
            .get("names")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        if names.is_empty() && mode != "clear" {
            return Err("select needs names, or mode \"clear\"".to_string());
        }

        let i = dmac_session::Session::index_of(panel);
        let p = &mut self.sessions.at_mut(index).panels[i];
        match mode {
            "clear" => p.clear_selection(),
            "set" => {
                p.clear_selection();
                mark(p, &names);
            }
            "add" => mark(p, &names),
            other => return Err(format!("mode must be set, add or clear, not {other}")),
        }

        // Report what is actually marked, not what was asked for: a name that
        // is not in the directory is silently nothing, and the model has to
        // know that happened.
        let selected: Vec<String> = p
            .entries
            .iter()
            .filter(|e| e.selected)
            .map(|e| e.name.clone())
            .collect();
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| !p.entries.iter().any(|e| &&e.name == n))
            .collect();
        self.touch_sessions();
        Ok(json!({
            "panel": panel_name(panel),
            "selected": selected,
            "not_found": missing,
        }))
    }

    fn mcp_command(&mut self, args: &Value) -> Result<Value, String> {
        let index = self.mcp_index();
        let line = arg_str(args, "line").ok_or("command needs a line")?.to_string();
        let run = arg_bool(args, "run").unwrap_or(false);

        self.sessions.at_mut(index).command_line = line.clone();
        self.touch_sessions();
        if !run {
            return Ok(json!({
                "line": line,
                "ran": false,
                "note": "typed onto the command line; the user presses Enter",
            }));
        }

        // Only the session on screen may be made to run something. Starting a
        // process in a session nobody is looking at is how output ends up
        // somewhere it will never be read.
        if index != self.sessions.current_index() {
            return Err("that session is not on screen; switch to it first".to_string());
        }
        self.run_line(&line).map_err(|e| format!("shell: {e}"))?;
        Ok(json!({ "line": line, "ran": true, "view": "shell" }))
    }

    fn mcp_history(&self, args: &Value) -> Value {
        use dmac_core::history::Order;
        let order = match arg_str(args, "order").unwrap_or("recent") {
            "frequent" | "most_used" => Order::Frequent,
            "session" => Order::Session,
            _ => Order::Recent,
        };
        let limit = arg_usize(args, "limit").unwrap_or(DEFAULT_HISTORY);
        let filter = arg_str(args, "filter").unwrap_or("");
        let now = dmac_core::history::now();
        let session = self.sessions.at(self.mcp_index()).id.0;

        let mut rows: Vec<(i32, Value)> = self
            .sessions
            .history
            .view(order, session)
            .into_iter()
            .filter_map(|row| {
                let m = dmac_core::fuzzy::score(filter, &row.path)?;
                Some((
                    m.score,
                    json!({
                        "path": row.path,
                        "age": dmac_core::history::ago(row.at, now),
                        "hits": row.hits,
                    }),
                ))
            })
            .collect();
        if !filter.is_empty() {
            rows.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        }
        let total = rows.len();
        let rows: Vec<Value> = rows.into_iter().take(limit).map(|(_, v)| v).collect();
        json!({ "order": order.label(), "rows": rows, "total": total })
    }

    fn mcp_sessions(&self) -> Value {
        let list: Vec<Value> = self
            .sessions
            .all()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({
                    "index": i,
                    "name": s.name,
                    "left": s.cwd[0].display(),
                    "right": s.cwd[1].display(),
                    "view": view_name(s.view),
                    "conversation": s.conversation,
                })
            })
            .collect();
        json!({ "current": self.sessions.current_index(), "sessions": list })
    }

    fn mcp_switch_session(&mut self, args: &Value) -> Result<Value, String> {
        let target = match (arg_str(args, "name"), arg_usize(args, "index")) {
            (Some(name), _) => self
                .sessions
                .all()
                .iter()
                .position(|s| s.name == name)
                .ok_or_else(|| format!("no session called {name}"))?,
            (None, Some(i)) => i,
            (None, None) => return Err("switch_session needs a name or an index".to_string()),
        };
        if target >= self.sessions.len() {
            return Err(format!("no session at index {target}"));
        }
        // `switch_to` reports whether anything moved. Asking for the session
        // already on screen is not a failure — it is a request that was already
        // satisfied, and answering it with an error would make an agent retry
        // something that is already true.
        if self.sessions.switch_to(target) {
            self.after_session_switch();
        }
        Ok(json!({
            "current": self.sessions.current_index(),
            "name": self.sessions.current().name,
        }))
    }

    fn mcp_notify(&mut self, args: &Value) -> Result<Value, String> {
        let message = arg_str(args, "message").ok_or("notify needs a message")?;
        // One line: the status bar is one line, and a message that is silently
        // cut in half is a message that lied.
        let message: String = message.lines().next().unwrap_or("").chars().take(200).collect();
        self.status = message.clone();
        Ok(json!({ "shown": message }))
    }

    /// The screen, rendered on demand.
    ///
    /// Not a copy kept from the last frame: that would either cost a string per
    /// row sixty times a second for nobody, or be missing exactly when it is
    /// first asked for. Drawing again into an off-screen buffer costs one frame
    /// and only when someone actually wants to look.
    fn mcp_screen(&mut self) -> Value {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let (width, height) = self.screen_size();
        // Infallible for this backend, but the type says otherwise and the
        // workspace forbids unwrapping.
        let Ok(mut term) = Terminal::new(TestBackend::new(width, height));
        if term.draw(|f| crate::ui::draw(f, self)).is_err() {
            return json!({ "screen": "", "rows": 0, "note": "could not render" });
        }
        let buffer = term.backend().buffer();
        let area = buffer.area;
        let rows: Vec<String> = (0..area.height)
            .map(|y| {
                let row: String = (0..area.width)
                    .map(|x| buffer[(area.x + x, area.y + y)].symbol())
                    .collect();
                row.trim_end().to_string()
            })
            .collect();
        json!({
            "screen": rows.join("\n"),
            "rows": rows.len(),
            "size": { "width": width, "height": height },
        })
    }

    /// Turn a possibly-relative path into an absolute one, against the active
    /// panel of the calling session.
    fn mcp_resolve(&self, index: usize, path: &str) -> Result<String, String> {
        let panel = self.sessions.at(index).active;
        self.mcp_resolve_for(index, panel, path)
    }

    fn mcp_resolve_for(&self, index: usize, panel: PanelId, path: &str) -> Result<String, String> {
        let expanded = expand_home(path)?;
        if std::path::Path::new(&expanded).is_absolute() {
            return Ok(normalise(&expanded));
        }
        let i = dmac_session::Session::index_of(panel);
        let base = self.sessions.at(index).cwd[i].display();
        Ok(normalise(&format!("{}/{expanded}", base.trim_end_matches('/'))))
    }
}

fn mark(p: &mut dmac_core::Panel, names: &[String]) {
    for e in &mut p.entries {
        // `..` is never selectable, here as everywhere else: an operation that
        // included the parent directory would act on the wrong tree.
        if e.kind != EntryKind::Parent && names.iter().any(|n| n == &e.name) {
            e.selected = true;
        }
    }
}

fn panel_state(s: &dmac_session::Session, id: PanelId) -> Value {
    let i = dmac_session::Session::index_of(id);
    let p = &s.panels[i];
    json!({
        "path": s.cwd[i].display(),
        "entries": p.len(),
        "cursor": p.current().map(|e| json!({ "name": e.name, "kind": kind_name(e.kind) })),
        "selected": p.entries.iter().filter(|e| e.selected).map(|e| e.name.clone()).collect::<Vec<_>>(),
    })
}

fn panel_name(id: PanelId) -> &'static str {
    match id {
        PanelId::Left => "left",
        PanelId::Right => "right",
    }
}

fn view_name(v: dmac_session::View) -> &'static str {
    match v {
        dmac_session::View::Panels => "panels",
        dmac_session::View::Shell => "shell",
    }
}

fn kind_name(k: EntryKind) -> &'static str {
    match k {
        EntryKind::Parent => "parent",
        EntryKind::Dir => "dir",
        EntryKind::File => "file",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    }
}

fn arg_str<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn arg_usize(args: &Value, name: &str) -> Option<usize> {
    args.get(name)
        .and_then(Value::as_u64)
        .map(|n| n.min(usize::MAX as u64) as usize)
}

fn arg_bool(args: &Value, name: &str) -> Option<bool> {
    args.get(name).and_then(Value::as_bool)
}

fn arg_panel(args: &Value, active: PanelId) -> Result<PanelId, String> {
    match arg_str(args, "panel") {
        None | Some("active") => Ok(active),
        Some("left") => Ok(PanelId::Left),
        Some("right") => Ok(PanelId::Right),
        Some("other") => Ok(active.other()),
        Some(other) => Err(format!("panel must be left, right or active, not {other}")),
    }
}

/// `~` and `~/x`, because a model writes paths the way a person does.
fn expand_home(path: &str) -> Result<String, String> {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME").map_err(|_| "no home directory".to_string())?;
        return Ok(match path.strip_prefix("~/") {
            Some(rest) => format!("{home}/{rest}"),
            None => home,
        });
    }
    Ok(path.to_string())
}

/// Resolve `.` and `..` textually, and collapse repeated separators.
///
/// Textually on purpose: this must work for a directory that does not exist
/// yet, so `navigate` can say "that is not a directory" rather than "that is
/// not a path".
fn normalise(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dots_are_resolved_without_touching_the_disk() {
        assert_eq!(normalise("/a/b/../c"), "/a/c");
        assert_eq!(normalise("/a/./b//c/"), "/a/b/c");
        assert_eq!(normalise("/a/../.."), "/", "cannot climb past the root");
        assert_eq!(normalise("a/../b"), "b");
        assert_eq!(normalise("../b"), "../b", "a relative path may climb");
        assert_eq!(normalise("."), ".");
    }

    #[test]
    fn a_panel_argument_names_a_panel() {
        assert_eq!(
            arg_panel(&json!({}), PanelId::Right),
            Ok(PanelId::Right),
            "absent means the active one"
        );
        assert_eq!(arg_panel(&json!({"panel":"left"}), PanelId::Right), Ok(PanelId::Left));
        assert_eq!(arg_panel(&json!({"panel":"other"}), PanelId::Right), Ok(PanelId::Left));
        assert!(arg_panel(&json!({"panel":"middle"}), PanelId::Left).is_err());
    }
}

// --- Listening ---------------------------------------------------------------

/// Start listening for agents that want to drive this commander.
///
/// One socket per process, named by pid, so two commanders on one machine never
/// fight over it. A stale socket left by a crash is removed rather than fatal:
/// refusing to start because of a file from a run that is already dead would be
/// a very silly way to lose a file manager.
#[cfg(unix)]
pub(crate) fn listen(
    socket: std::path::PathBuf,
    tx: tokio::sync::mpsc::UnboundedSender<crate::app::Update>,
) {
    tokio::spawn(async move {
        if let Some(dir) = socket.parent() {
            let _ = tokio::fs::create_dir_all(dir).await;
        }
        let _ = tokio::fs::remove_file(&socket).await;
        let Ok(listener) = tokio::net::UnixListener::bind(&socket) else {
            return;
        };
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move { serve(stream, tx).await });
        }
    });
}

/// One connection: read lines, hand each to the UI thread, write back what it
/// says. The connection never touches application state itself.
#[cfg(unix)]
async fn serve(
    stream: tokio::net::UnixStream,
    tx: tokio::sync::mpsc::UnboundedSender<crate::app::Update>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let mut session: Option<dmac_session::SessionId> = None;
    let mut first = true;

    while let Ok(Some(line)) = lines.next_line().await {
        // The first line may be the bridge's handshake, saying which session
        // the agent is hosted in. A client that speaks straight JSON-RPC sends
        // no handshake, and its first line is treated as a request.
        if first {
            first = false;
            if let Some(named) = dmac_mcp::attach_session(&line) {
                session = named
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(dmac_session::SessionId);
                continue;
            }
        }

        let (reply, answer) = tokio::sync::oneshot::channel();
        if tx
            .send(crate::app::Update::Mcp {
                session,
                line,
                reply,
            })
            .is_err()
        {
            break; // the application is going away
        }
        match answer.await {
            // A notification. Answering one is a protocol violation.
            Ok(None) => {}
            Ok(Some(out)) => {
                if write.write_all(out.as_bytes()).await.is_err()
                    || write.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn listen(
    _socket: std::path::PathBuf,
    _tx: tokio::sync::mpsc::UnboundedSender<crate::app::Update>,
) {
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    use dmac_mcp::Commander;

    /// Every tool the catalogue advertises must be dispatched. A tool a model
    /// can see and cannot call is worse than one that does not exist.
    #[tokio::test]
    async fn every_advertised_tool_is_implemented() {
        let mut app = crate::app::App::for_test();
        for name in dmac_mcp::tools::names() {
            let answer = app.call(name, &json!({}));
            if let Err(e) = &answer {
                assert!(
                    !e.starts_with("no such tool"),
                    "{name} is advertised but not dispatched"
                );
            }
        }
    }

    #[tokio::test]
    async fn state_describes_both_panels() {
        let app = crate::app::App::for_test();
        let s = app.mcp_state();
        assert_eq!(s["panels"]["left"]["path"], "/left");
        assert_eq!(s["panels"]["right"]["path"], "/right");
        assert_eq!(s["active"], "left");
        assert_eq!(s["view"], "panels");
        assert_eq!(s["panels"]["left"]["cursor"]["kind"], "parent");
    }

    #[tokio::test]
    async fn navigate_moves_the_panel_the_user_can_see() {
        let mut app = crate::app::App::for_test();
        let out = app
            .call("navigate", &json!({ "path": "/tmp" }))
            .expect("/tmp exists everywhere this runs");
        assert_eq!(out["path"], "/tmp");
        assert_eq!(out["panel"], "left");
        assert_eq!(app.ses().cwd[0], VfsPath::local("/tmp"));
    }

    #[tokio::test]
    async fn navigate_says_no_rather_than_moving_somewhere_that_is_not_there() {
        let mut app = crate::app::App::for_test();
        let e = app
            .call("navigate", &json!({ "path": "/definitely/not/here" }))
            .expect_err("should refuse");
        assert!(e.contains("not a directory"), "{e}");
        assert_eq!(app.ses().cwd[0], VfsPath::local("/left"), "and did not move");
    }

    #[tokio::test]
    async fn navigate_can_move_the_other_panel_without_stealing_focus() {
        let mut app = crate::app::App::for_test();
        app.call("navigate", &json!({ "path": "/tmp", "panel": "right" }))
            .expect("right panel");
        assert_eq!(app.ses().cwd[1], VfsPath::local("/tmp"));
        assert_eq!(app.ses().cwd[0], VfsPath::local("/left"), "left is untouched");
        assert_eq!(app.ses().active, PanelId::Left, "and still has the keyboard");
    }

    /// A name that is not in the directory is silently nothing, so the answer
    /// has to say which ones those were.
    #[tokio::test]
    async fn select_reports_what_it_actually_marked() {
        let mut app = crate::app::App::for_test();
        let out = app
            .call("select", &json!({ "names": ["src", "nope", ".."] }))
            .expect("select");
        assert_eq!(out["selected"], json!(["src"]));
        assert_eq!(out["not_found"], json!(["nope"]));
        assert_eq!(
            app.ses().panels[0]
                .entries
                .iter()
                .filter(|e| e.selected)
                .count(),
            1,
            "`..` is never selectable"
        );

        app.call("select", &json!({ "mode": "clear" })).expect("clear");
        assert!(app.ses().panels[0].entries.iter().all(|e| !e.selected));
    }

    /// Typing a command is not running it. The default has to be the harmless
    /// one: this is the user's shell, in the user's directory.
    #[tokio::test]
    async fn command_types_by_default_and_does_not_run() {
        let mut app = crate::app::App::for_test();
        let out = app
            .call("command", &json!({ "line": "rm -rf /" }))
            .expect("typed");
        assert_eq!(out["ran"], false);
        assert_eq!(app.ses().command_line, "rm -rf /");
        assert!(app.ses().hosted().is_none(), "no shell was started");
    }

    #[tokio::test]
    async fn history_comes_back_ordered_and_filtered() {
        let mut app = crate::app::App::for_test();
        let now = dmac_core::history::now();
        app.sessions.history.record("/home/me/prj/dmac-tui", 0, now);
        app.sessions.history.record("/var/log", 0, now - 100);

        let all = app.mcp_history(&json!({}));
        assert_eq!(all["rows"].as_array().map(Vec::len), Some(2));
        assert_eq!(all["rows"][0]["path"], "/home/me/prj/dmac-tui", "newest first");

        let filtered = app.mcp_history(&json!({ "filter": "log" }));
        assert_eq!(filtered["rows"].as_array().map(Vec::len), Some(1));
        assert_eq!(filtered["rows"][0]["path"], "/var/log");
    }

    #[tokio::test]
    async fn notify_puts_one_line_in_the_status_bar() {
        let mut app = crate::app::App::for_test();
        app.call("notify", &json!({ "message": "build finished\nand more" }))
            .expect("notify");
        assert_eq!(app.status, "build finished", "one line, and it says so");
    }

    #[tokio::test]
    async fn sessions_can_be_listed_and_switched_by_name() {
        let mut app = crate::app::App::for_test();
        app.handle(crate::action::Action::NewSession);
        app.sessions.at_mut(1).name = "docs".to_string();

        let listed = app.call("sessions", &json!({})).expect("sessions");
        assert_eq!(listed["sessions"].as_array().map(Vec::len), Some(2));

        // Already on it: satisfied, not an error.
        let out = app
            .call("switch_session", &json!({ "name": "docs" }))
            .expect("already there is not a failure");
        assert_eq!(out["name"], "docs");

        app.call("switch_session", &json!({ "index": 0 }))
            .expect("back to the first");
        assert_eq!(app.sessions.current_index(), 0);
        app.call("switch_session", &json!({ "name": "docs" }))
            .expect("and forward again");
        assert_eq!(app.sessions.current().name, "docs");

        let e = app
            .call("switch_session", &json!({ "index": 9 }))
            .expect_err("out of range");
        assert!(e.contains("no session at index 9"), "{e}");

        let e = app
            .call("switch_session", &json!({ "name": "nowhere" }))
            .expect_err("should refuse");
        assert!(e.contains("no session called nowhere"), "{e}");
    }

    /// The agent is hosted in one session; the user may be looking at another.
    /// Its tools must answer about its own.
    #[tokio::test]
    async fn a_call_answers_about_the_session_it_came_from() {
        let mut app = crate::app::App::for_test();
        let first = app.sessions.current().id;
        app.handle(crate::action::Action::NewSession);
        app.sessions.at_mut(1).name = "elsewhere".to_string();
        assert_eq!(app.sessions.current_index(), 1, "the user moved on");

        app.mcp_session = Some(first);
        let s = app.mcp_state();
        assert_eq!(s["session"]["index"], 0);
        assert_eq!(s["on_screen"], false, "and it says it is not on screen");

        app.mcp_session = None;
        assert_eq!(app.mcp_state()["session"]["name"], "elsewhere");
    }

    #[tokio::test]
    async fn a_relative_path_is_resolved_against_the_panel() {
        let app = crate::app::App::for_test();
        assert_eq!(app.mcp_resolve(0, "sub/dir").as_deref(), Ok("/left/sub/dir"));
        assert_eq!(app.mcp_resolve(0, "../other").as_deref(), Ok("/other"));
        assert_eq!(app.mcp_resolve(0, "/abs").as_deref(), Ok("/abs"));
    }
}

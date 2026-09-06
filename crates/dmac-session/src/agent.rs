//! Keeping a hosted agent — Claude Code, in practice — attached to the session
//! it belongs to, across restarts.
//!
//! The mechanism is a shim: a small `claude` script early on the hosted shell's
//! `PATH`, which adds the session's own conversation id to every invocation.
//! A shim rather than rewriting what the user types, because they mostly do not
//! type it here at all — they type it *inside* the hosted shell, where
//! DMACommander never sees the keystrokes. It also means `ps` shows the id and
//! the directory, so which conversation a process belongs to is visible from
//! outside.

use std::path::{Path, PathBuf};

/// Programs worth attaching to a session.
///
/// One entry today. It is a list because the next one — any agent CLI with
/// resumable conversations — needs exactly the same treatment, and a list makes
/// that obvious to whoever adds it.
const ATTACHED: &[Attach] = &[Attach {
    program: "claude",
    // Flags that mean "I have decided which conversation this is". The shim
    // must not add a second opinion.
    explicit: &[
        "--session-id",
        "--resume",
        "-r",
        "-c",
        "--continue",
        "--fork-session",
        "--from-pr",
        "--cloud",
    ],
    new_flag: "--session-id",
    resume_flag: "--resume",
    mcp_flag: Some("--mcp-config"),
}];

struct Attach {
    program: &'static str,
    explicit: &'static [&'static str],
    new_flag: &'static str,
    resume_flag: &'static str,
    /// How this program is told about an extra MCP server, if it can be. The
    /// commander describes itself to whatever it hosts: an agent that has to be
    /// told in prose where the panels are is working from a blurred photograph.
    mcp_flag: Option<&'static str>,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not write the agent shim: {0}")]
    Io(#[from] std::io::Error),
}

/// The agent a session hosts, named once. The menu that offers to start it and
/// the shim that intercepts it have to agree, and the way to guarantee that is
/// for there to be one name. The first of [`ATTACHED`] because that is the one
/// that exists; a second agent needs a second menu entry, and whoever adds it
/// will find this.
pub fn attached_program() -> &'static str {
    ATTACHED[0].program
}

/// Which attached program, if any, is among these running commands.
///
/// Asked at shutdown so the next run knows what to start again — this is what
/// turns "the conversation id was saved" into "the conversation came back".
pub fn running_agent(commands: &[String]) -> Option<String> {
    commands.iter().find(|line| is_attached(line)).cloned()
}

/// Interpreters that run a program without being it.
///
/// A native install shows up as `/path/claude`, but one installed through npm
/// or a package manager shows up as `node /path/cli.js` or `/bin/sh /path/x` —
/// the program the user thinks they are running is the *second* word.
const INTERPRETERS: &[&str] = &["sh", "bash", "zsh", "dash", "fish", "node", "bun", "deno"];

/// Whether a command line starts one of the attached programs.
///
/// Basenames only, and only in argv[0] or — behind an interpreter — argv[1].
/// Searching the whole line instead would match `vim claude.md`, and
/// reattaching someone's text editor because of what they were editing is
/// worse than not reattaching anything.
fn is_attached(line: &str) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    let base = |w: &str| w.rsplit('/').next().unwrap_or(w).to_string();
    let Some(first) = words.first().map(|w| base(w)) else {
        return false;
    };
    if ATTACHED.iter().any(|a| a.program == first) {
        return true;
    }
    if INTERPRETERS.contains(&first.as_str())
        && let Some(second) = words.get(1).map(|w| base(w))
    {
        // `node /path/claude.js` and `sh /path/claude` both count; the
        // extension is not part of the name people know it by.
        let stem = second.split('.').next().unwrap_or(&second);
        return ATTACHED.iter().any(|a| a.program == stem);
    }
    false
}

/// What to run to get this conversation back, as something a shell can be
/// handed safely.
///
/// The input is not a command line. It is an `argv` observed through `ps` and
/// rejoined with spaces, so every quote its author wrote is already gone —
/// which matters enormously, because the shim's `--mcp-config` argument is a
/// JSON object full of braces and containing a path with a space in it. Handed
/// back to a shell it is not one argument any more: `zsh` word-splits it,
/// tries to glob `{"mcpServers":{...}}`, and refuses the whole line with "bad
/// pattern". That is not a thing to fix by quoting it again — the quoting that
/// was lost cannot be recovered — so what comes back here is the *command*, not
/// the expansion of it:
///
/// - the program by its bare name, so the shim on `PATH` is what runs, rather
///   than the absolute path the shim itself resolved to last time;
/// - nothing of what the shim added — the socket in an old `--mcp-config` died
///   with the run that printed it, and the conversation is the shim's to name,
///   from the session, correctly quoted;
/// - everything the *user* chose, untouched: a model, a permission mode, a
///   directory. That is what they set up, and it is not ours to drop.
///
/// Above all, never a bare `--resume`. A resume flag whose id went missing does
/// not fail — it silently opens whichever conversation was most recent, which
/// is how you end up somewhere you have never been with no idea why.
pub fn as_resume(line: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut words = line.split_whitespace().peekable();

    if let Some(first) = words.next() {
        out.push(first.rsplit('/').next().unwrap_or(first).to_string());
    }

    while let Some(w) = words.next() {
        match w {
            // Ours. The value is JSON that has already lost its quotes, so it
            // is not one word any more: skip until the braces balance, or the
            // remains of it end up on the command line as globs.
            "--mcp-config" => {
                let mut depth = 0i32;
                for v in words.by_ref() {
                    depth += v.matches('{').count() as i32 - v.matches('}').count() as i32;
                    if depth <= 0 {
                        break;
                    }
                }
            }
            // Ours as well, and the value with it. Dropping the flag and
            // keeping what came after it would leave the id standing on the
            // command line as a positional argument — which for an agent is
            // not a stray word, it is a prompt.
            "--session-id" | "--resume" | "-r" => {
                if words.peek().is_some_and(|v| !v.starts_with('-')) {
                    words.next();
                }
            }
            _ if w.starts_with("--mcp-config=")
                || w.starts_with("--session-id=")
                || w.starts_with("--resume=") => {}
            _ => out.push(w.to_string()),
        }
    }
    out.join(" ")
}

/// Kill anything still holding `conversation`, left over from a previous run.
///
/// Called at startup, where by definition nothing of ours is running yet — so a
/// process carrying one of our conversation ids is an orphan from a run that
/// did not get to clean up, and it is still holding the conversation the user
/// is about to ask for. Leaving it there is what produces "that session is
/// already in use" on a fresh start, which reads as data loss to anyone who
/// does not know to go hunting in `ps`.
///
/// Returns how many were cleared.
#[cfg(unix)]
pub fn clear_orphans(conversation: &str) -> usize {
    // A conversation id is a UUID we generated: nothing else on the machine has
    // it in its arguments, so matching on it cannot reach an unrelated process.
    if conversation.len() < 32 {
        return 0;
    }
    let Ok(output) = std::process::Command::new("ps")
        .args(["-Ao", "pid=,args="])
        .output()
    else {
        return 0;
    };
    let me = std::process::id() as i32;
    let mut killed = 0;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut it = line.trim().splitn(2, char::is_whitespace);
        let (Some(pid), Some(args)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(pid) = pid.parse::<i32>() else {
            continue;
        };
        if pid == me || !args.contains(conversation) {
            continue;
        }
        // SAFETY: two integers in, one out.
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, libc::SIGTERM);
            libc::kill(pid, libc::SIGKILL);
        }
        killed += 1;
    }
    killed
}

#[cfg(not(unix))]
pub fn clear_orphans(_conversation: &str) -> usize {
    0
}

/// Where one commander keeps a session's shims.
///
/// Under this process's own pid, because the session ids are indices: two
/// commanders both have a session `0`, and a shared directory means the second
/// one to start rewrites the first one's `mcp.json` to name *its* socket. The
/// first commander is still running and still listening, but every agent it
/// hosts is now pointed at a socket that dies with the other commander — the
/// server is configured, and answers nothing. The socket is already named by
/// pid for exactly this reason; the shims have to be too.
/// Marks a shim directory as belonging to one run. Spelled out rather than
/// left as a bare number because the old layout named these directories by
/// session id — `0`, `1`, `2` — which read as perfectly good pids. Worse, pid
/// `0` is not a process at all: `kill(0, 0)` asks about the caller's whole
/// process group and says yes, so `shims/0` looked permanently alive and was
/// never swept.
const RUN_PREFIX: &str = "run-";

fn shim_dir(root: &Path, session_id: &str) -> PathBuf {
    root.join("shims")
        .join(format!("{RUN_PREFIX}{}", std::process::id()))
        .join(session_id)
}

/// Where the "this conversation has been started once" marker lives.
///
/// Keyed by conversation and kept out of the per-commander directory, because
/// it describes the conversation and not the run: it has to outlive both. A
/// marker that vanished on restart would send the next `claude` in with
/// `--session-id` for a conversation that already exists, which is refused.
fn marker_path(root: &Path, conversation: &str, program: &str) -> PathBuf {
    root.join("agents")
        .join(format!("{conversation}.{program}.started"))
}

/// Remove the shim directories of commanders that are no longer running.
///
/// One directory per run accumulates otherwise. Returns how many were removed.
#[cfg(unix)]
pub fn clear_stale_shims(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root.join("shims")) else {
        return 0;
    };
    let me = std::process::id() as i32;
    let mut removed = 0;
    for e in entries.flatten() {
        let path = e.path();
        let Some(pid) = path
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix(RUN_PREFIX))
            .and_then(|s| s.parse::<i32>().ok())
        else {
            // Anything else is the older layout, where the directory was named
            // by session id. Nothing reads those any more.
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
            }
            continue;
        };
        // SAFETY: two integers in, one out; signal 0 delivers nothing.
        #[allow(unsafe_code)]
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        if pid == me || alive {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(not(unix))]
pub fn clear_stale_shims(_root: &Path) -> usize {
    0
}

/// The shim directory for one session, created if needed.
///
/// Returns `None` when there is nothing to attach — no `claude` on `PATH` means
/// no shim, rather than a script that shadows a program the user might install
/// later and then fails to find it.
pub fn prepare(root: &Path, session_id: &str, conversation: &str) -> Option<PathBuf> {
    let dir = shim_dir(root, session_id);
    // The shim writes its marker here with `: >`, which does not create
    // directories; without this the marker silently never appears and every
    // run looks like the first one.
    let _ = std::fs::create_dir_all(root.join("agents"));
    let mcp = mcp_config(root, session_id);
    let mut wrote_any = false;
    for a in ATTACHED {
        let Some(real) = resolve(a.program, &dir) else {
            continue;
        };
        let marker = marker_path(root, conversation, a.program);
        let script = shim_script(a, &real, conversation, &marker, mcp.as_deref());
        if write_executable(&dir.join(a.program), &script).is_ok() {
            wrote_any = true;
        }
    }
    wrote_any.then_some(dir)
}

/// Describe this commander as an MCP server the hosted agent can call.
///
/// Returned as the JSON `--mcp-config` takes directly — it accepts strings as
/// well as files. Nothing is written to disk: a file would have to live
/// somewhere, and wherever that somewhere is, a second commander wants it too.
/// The description belongs to the run, so it travels inside the run's own shim
/// rather than in a file both runs can see.
///
/// `None` when there is nothing to describe — no socket, or no binary to point
/// at. Absent, everything else still works: the agent simply cannot see the
/// panels.
fn mcp_config(root: &Path, session_id: &str) -> Option<String> {
    let binary = std::env::current_exe().ok()?;
    // A test binary is not a commander. Without this, a `cargo test` run would
    // hand a real agent something in `target/debug/deps` that the next build
    // deletes — and the agent would report a broken MCP server it was never
    // meant to have.
    if binary.file_stem().is_none_or(|n| n != "dmac") {
        return None;
    }
    let socket = dmac_mcp::socket_path(root, std::process::id());
    // Only advertise a server that is actually there. Binding can fail — a path
    // too long for a socket address, a read-only directory — and it fails
    // silently, so describing it anyway hands the agent a path nothing answers
    // on. An MCP server that is absent is better than one that is present and
    // dead: the second wastes the user's time working out why the tools do not
    // respond.
    if !socket.exists() {
        return None;
    }
    serde_json::to_string(&serde_json::json!({
        "mcpServers": {
            "dmac": {
                "command": binary,
                "args": ["--mcp", socket, "--mcp-session", session_id],
            }
        }
    }))
    .ok()
}

/// The environment a hosted shell needs so the shim is found and the id is
/// visible to anything that wants it.
pub fn environment(
    shim_dir: &Path,
    session_name: &str,
    conversation: &str,
) -> Vec<(String, String)> {
    let path = match std::env::var("PATH") {
        Ok(p) => format!("{}:{p}", shim_dir.display()),
        Err(_) => shim_dir.display().to_string(),
    };
    let mut env = vec![
        ("PATH".to_string(), path),
        // Named as well as prepended, because prepending is not the last word:
        // the shell reads its rc files after this, and an rc file that puts its
        // own directory in front of `PATH` puts it in front of ours too. A
        // shell that can name the directory can put it back — which is what
        // `repair` writes, and what `check` looks for the absence of.
        (SHIM_DIR_VAR.to_string(), shim_dir.display().to_string()),
        ("DMAC_SESSION".to_string(), session_name.to_string()),
        ("DMAC_CONVERSATION".to_string(), conversation.to_string()),
    ];
    // Also in the environment, not only in the agent's configuration file: a
    // script, or an agent this program has never heard of, can find the
    // commander without anyone having taught it about shims.
    if let Some(root) = crate::agent_root() {
        let socket = dmac_mcp::socket_path(&root, std::process::id());
        env.push(("DMAC_MCP_SOCKET".to_string(), socket.display().to_string()));
    }
    env
}

/// The variable naming the shim directory to a hosted shell.
///
/// Also the marker that says the rc file has already been repaired: a file
/// that mentions it at all is left alone, so running the repair twice writes
/// nothing the second time.
const SHIM_DIR_VAR: &str = "DMAC_SHIM_DIR";

/// Marks off the one line of a shell's output that is an answer to us.
const FENCE: &str = "--dmac--";

/// Whether a hosted shell would actually reach the shim.
///
/// It is worth asking because the failure is silent. The shim is only reached
/// while it is the first `claude` on `PATH`, and `PATH` is not ours to keep:
/// what we hand the shell is read before its rc files, and the usual
/// `PATH="$HOME/.local/bin:$PATH"` in a `.zshrc` puts that directory in front
/// of ours. The agent then starts perfectly well, knowing nothing about which
/// conversation it belongs to and unable to see the panels it is running
/// inside — and nothing anywhere says why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimCheck {
    /// The shim wins. Nothing to do.
    Reached,
    /// Something else wins. Names it, and the file that would have to change
    /// for it not to — `None` when the shell is one whose configuration we do
    /// not know how to write, where offering to edit it would be worse than
    /// saying nothing.
    Shadowed { by: PathBuf, rc: Option<PathBuf> },
    /// No answer worth acting on: no shell, or one that would not say.
    Unknown,
}

/// Ask the user's shell, the way the user's shell will be asked.
///
/// Not by reading `PATH` here: the answer depends on what the rc files do
/// after we hand the environment over, and the only thing that knows that is
/// the shell itself. So it is started the way a session starts it —
/// interactive, same environment — and asked where the agent resolves.
///
/// Costs a whole shell startup, rc files and all, so it belongs on a thread
/// that is not drawing anything.
#[cfg(unix)]
pub fn check(shim_dir: &Path) -> ShimCheck {
    let Some(program) = ATTACHED.first().map(|a| a.program) else {
        return ShimCheck::Unknown;
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let path = match std::env::var("PATH") {
        Ok(p) => format!("{}:{p}", shim_dir.display()),
        Err(_) => shim_dir.display().to_string(),
    };
    let out = std::process::Command::new(&shell)
        .arg("-i")
        .arg("-c")
        // Fenced, because an interactive shell is not a quiet one: rc files
        // greet, print tips, and restore sessions, and the first line of that
        // is not the answer to anything. `echo` and `;` are the two pieces of
        // syntax every shell worth asking agrees on, fish included.
        .arg(format!("echo {FENCE}; command -v {program}; echo {FENCE}"))
        .env("PATH", path)
        .env("DMAC", "1")
        .env(SHIM_DIR_VAR, shim_dir)
        // An rc file that reads from its input gets end of file rather than
        // the user's keyboard: this runs behind their back and must not be
        // able to sit there waiting for them. Complaints about job control go
        // the same way — a shell that is interactive without a terminal says
        // so, and it is not an answer to anything.
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return ShimCheck::Unknown;
    };
    let answer = String::from_utf8_lossy(&out.stdout);
    let Some(found) = answer
        .lines()
        .skip_while(|l| l.trim() != FENCE)
        .skip(1)
        .take_while(|l| l.trim() != FENCE)
        .map(str::trim)
        .find(|l| !l.is_empty())
    else {
        // Nothing between the fences, or no fences at all: nothing found, or a
        // shell that would not answer. Neither is a shadowed shim — there is
        // no agent here to shadow, and `prepare` would not have written one.
        return ShimCheck::Unknown;
    };
    let found = PathBuf::from(found);
    if found.parent() == Some(shim_dir) {
        return ShimCheck::Reached;
    }
    ShimCheck::Shadowed {
        by: found,
        rc: rc_file(&shell),
    }
}

#[cfg(not(unix))]
pub fn check(_shim_dir: &Path) -> ShimCheck {
    ShimCheck::Unknown
}

/// The file that gets the last word on `PATH`, for the shells whose answer we
/// know how to write.
fn rc_file(shell: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    match Path::new(shell).file_name()?.to_str()? {
        // Not `$HOME` blindly: a `ZDOTDIR` is where that user's zsh actually
        // reads from, and writing to the other file would change nothing while
        // looking like it had.
        "zsh" => Some(
            std::env::var_os("ZDOTDIR")
                .map(PathBuf::from)
                .unwrap_or(home)
                .join(".zshrc"),
        ),
        "bash" => Some(home.join(".bashrc")),
        "fish" => Some(home.join(".config/fish/config.fish")),
        _ => None,
    }
}

/// Put the shim directory back in front, from inside the user's own rc file.
///
/// Appended, never inserted: it has to run after whatever else the file does
/// to `PATH`, and that is the whole point of it. Written in terms of the
/// variable rather than the directory, because the directory is named after
/// this run's pid and will not exist tomorrow — so the line is correct for
/// every future run, and does nothing at all in a shell DMACommander did not
/// start.
///
/// Idempotent: a file that already mentions the variable is left alone.
pub fn repair(rc: &Path) -> Result<(), AgentError> {
    let existing = std::fs::read_to_string(rc).unwrap_or_default();
    if existing.contains(SHIM_DIR_VAR) {
        return Ok(());
    }
    let fish = rc.extension().is_some_and(|e| e == "fish");
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(if fish { FISH_REPAIR } else { POSIX_REPAIR });
    if let Some(parent) = rc.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(rc, out)?;
    Ok(())
}

/// Why the line is there, in the file the user will find it in one day.
const POSIX_REPAIR: &str = r#"
# Added by DMACommander. It puts its own directory in front of PATH before this
# file runs, and this file then puts yours in front of that — so without these
# lines the `claude` started here is the one from your PATH, which knows nothing
# about which conversation this session is or that there are panels to look at.
# Outside DMACommander it does nothing: nothing else sets DMAC_SHIM_DIR.
if [ -n "$DMAC_SHIM_DIR" ] && [ "${PATH%%:*}" != "$DMAC_SHIM_DIR" ]; then
  PATH="$DMAC_SHIM_DIR:$PATH"
  export PATH
fi
"#;

const FISH_REPAIR: &str = r#"
# Added by DMACommander. It puts its own directory in front of PATH before this
# file runs, and this file then puts yours in front of that — so without these
# lines the `claude` started here is the one from your PATH, which knows nothing
# about which conversation this session is or that there are panels to look at.
# Outside DMACommander it does nothing: nothing else sets DMAC_SHIM_DIR.
if set -q DMAC_SHIM_DIR
    fish_add_path --path --prepend --move $DMAC_SHIM_DIR
end
"#;

/// The first `program` on `PATH` that is not our own shim.
///
/// Skipping the shim directory matters: without it a shim written into a
/// directory already on `PATH` would find itself and recurse until the process
/// runs out of file descriptors.
fn resolve(program: &str, shim_dir: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir == shim_dir {
            continue;
        }
        let candidate = dir.join(program);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// The shim itself.
///
/// `exec` rather than a call, so the shim leaves no process of its own between
/// the shell and the agent: signals, job control and `ps` all then read the way
/// they would without it.
fn shim_script(
    a: &Attach,
    real: &Path,
    conversation: &str,
    marker: &Path,
    mcp_config: Option<&str>,
) -> String {
    let (real, marker) = (real.display(), marker.display());
    let (program, new_flag, resume_flag) = (a.program, a.new_flag, a.resume_flag);
    // Empty when there is nothing to add, so the exec lines below read the same
    // either way rather than needing two versions of each. Quoted properly and
    // not just wrapped in apostrophes: this is JSON carrying filesystem paths,
    // and a home directory can be called `O'Brien`.
    let mcp = match (a.mcp_flag, mcp_config) {
        (Some(flag), Some(json)) => {
            format!("{flag} {} ", dmac_core::tools::shell_quote(json))
        }
        _ => String::new(),
    };
    let explicit = a
        .explicit
        .iter()
        .map(|f| format!("{f}|{f}=*"))
        .collect::<Vec<_>>()
        .join("|");
    format!(
        r#"#!/bin/sh
# Written by DMACommander. Every `{program}` started from this session joins the
# same conversation, so closing DMACommander and coming back reopens it where it
# was. To start a fresh conversation instead:
#   rm '{marker}'
#
# The commander also describes itself to {program} as an MCP server, so it can
# see the panels, the history and the sessions it is running inside. The
# description is passed inline rather than through a file, so several
# commanders can be open at once without sharing anything writable.
#
# An explicit choice on the command line always wins; this only fills in a gap.
for arg in "$@"; do
  case "$arg" in
    {explicit}) exec '{real}' {mcp}"$@" ;;
  esac
done
# Ask {program}'s own store whether this conversation exists. That store is the
# truth: the conversation outlives our marker whenever the marker is cleaned up
# or never written, and the marker outlives the conversation whenever one was
# reserved and never used. Resuming on the marker alone gets both wrong, in
# opposite directions.
existing=$(ls "$HOME"/.claude/projects/*/'{conversation}'.jsonl 2>/dev/null | head -1)
if [ -n "$existing" ]; then
  exec '{real}' {mcp}{resume_flag} '{conversation}' "$@"
fi
# Only when the store cannot be consulted at all does the marker get a say.
# It used to have one whenever it existed, and that is a resume of a
# conversation that is not there — which does not start empty, it refuses to
# start. The marker is written the moment a conversation is *reserved*, and a
# reserved conversation nobody ever typed into leaves no transcript behind.
if [ ! -d "$HOME/.claude/projects" ] && [ -e '{marker}' ]; then
  exec '{real}' {mcp}{resume_flag} '{conversation}' "$@"
fi
: > '{marker}'
exec '{real}' {mcp}{new_flag} '{conversation}' "$@"
"#
    )
}

fn write_executable(path: &Path, contents: &str) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> &'static Attach {
        &ATTACHED[0]
    }

    /// The description is JSON carrying filesystem paths, and a home directory
    /// can be called `O'Brien`. Wrapped in bare apostrophes that would end the
    /// quoting early and hand the rest of the JSON to the shell as code.
    #[test]
    fn an_apostrophe_in_the_description_cannot_escape_its_quotes() {
        let json = r#"{"command":"/Users/O'Brien/dmac"}"#;
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "abc",
            Path::new("/tmp/m"),
            Some(json),
        );
        assert!(
            s.contains(r#"'{"command":"/Users/O'\''Brien/dmac"}'"#),
            "the apostrophe was not closed and reopened: {s}"
        );
        // And what a shell would actually read back is the JSON we started with.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("printf %s {}", dmac_core::tools::shell_quote(json)))
            .output()
            .expect("sh");
        assert_eq!(String::from_utf8_lossy(&out.stdout), json);
    }

    /// Two commanders both have a session `0`. When they shared a directory the
    /// second one to start rewrote the first one's `mcp.json` to name its own
    /// socket, and the first commander — still running, still listening — had
    /// every agent it hosted pointed at a socket that died with the other one.
    /// The server was configured and answered nothing.
    #[test]
    fn one_commander_never_writes_over_another_commanders_shims() {
        let root = tempfile::tempdir().expect("a temp dir");
        // Where the shared layout put it, and where a second commander would
        // therefore land.
        let theirs = root.path().join("shims").join("0");
        std::fs::create_dir_all(&theirs).expect("their directory");
        std::fs::write(theirs.join("claude"), b"theirs").expect("their shim");

        let _ = prepare(root.path(), "0", "11111111-2222-4333-8444-555555555555");

        assert_eq!(
            std::fs::read(theirs.join("claude")).expect("still there"),
            b"theirs",
            "another commander's shim was overwritten"
        );
        assert!(
            shim_dir(root.path(), "0")
                .to_string_lossy()
                .contains(&format!("{RUN_PREFIX}{}", std::process::id())),
            "the directory must be named by the run that owns it"
        );
    }

    /// The old layout named these directories by session id, and `0`, `1`, `2`
    /// parse as pids. Pid `0` is the caller's own process group, which is
    /// always alive, so `shims/0` was kept for ever.
    #[test]
    fn the_sweep_removes_the_old_layout_and_keeps_this_run() {
        let root = tempfile::tempdir().expect("a temp dir");
        for old in ["0", "1", "2"] {
            std::fs::create_dir_all(root.path().join("shims").join(old)).expect("old layout");
        }
        let mine = shim_dir(root.path(), "0");
        std::fs::create_dir_all(&mine).expect("ours");
        clear_stale_shims(root.path());

        for old in ["0", "1", "2"] {
            assert!(
                !root.path().join("shims").join(old).exists(),
                "the old layout survived the sweep"
            );
        }
        assert!(mine.exists(), "this run's own directory was swept away");
    }

    /// The marker says a conversation has been started once, so it has to
    /// outlive the run that started it: kept inside a per-commander directory
    /// it would vanish on restart, and the next `claude` would go in with
    /// `--session-id` for a conversation that already exists — which is refused
    /// with "Session ID ... is already in use".
    #[test]
    fn the_marker_outlives_the_commander_that_wrote_it() {
        let root = tempfile::tempdir().expect("a temp dir");
        let marker = marker_path(root.path(), "abc", "claude");
        assert!(
            !marker.starts_with(shim_dir(root.path(), "0")),
            "the marker must not live in a directory swept away on restart"
        );
        prepare(root.path(), "0", "abc");
        assert!(
            marker.parent().is_some_and(std::path::Path::is_dir),
            "the shim writes the marker with `: >`, which creates no directories"
        );
    }

    #[test]
    fn the_shim_adds_the_conversation_on_a_first_run_and_resumes_after() {
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "11111111-2222-4333-8444-555555555555",
            Path::new("/tmp/shims/s1/claude.started"),
            None,
        );
        assert!(
            s.contains("--session-id '11111111-2222-4333-8444-555555555555'"),
            "{s}"
        );
        assert!(
            s.contains("--resume '11111111-2222-4333-8444-555555555555'"),
            "{s}"
        );
        assert!(s.starts_with("#!/bin/sh\n"), "{s}");
    }

    /// A user who says which conversation they want must get that one. The shim
    /// fills in a gap; it does not hold an opinion.
    #[test]
    fn an_explicit_choice_on_the_command_line_wins() {
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "abc",
            Path::new("/tmp/m"),
            None,
        );
        for flag in claude().explicit {
            assert!(s.contains(&format!("{flag}|{flag}=*")), "{flag} missing");
        }
    }

    /// `exec`, so the shim leaves nothing of its own between the shell and the
    /// agent — otherwise signals and job control read differently through it.
    #[test]
    fn the_shim_execs_rather_than_calling() {
        let s = shim_script(
            claude(),
            Path::new("/bin/true"),
            "abc",
            Path::new("/tmp/m"),
            None,
        );
        for line in s.lines() {
            let line = line.trim();
            if line.contains("/bin/true") {
                assert!(line.contains("exec "), "not an exec: {line}");
            }
        }
    }

    /// The real program is quoted, because it is a path from the environment and
    /// a directory on PATH can contain a space.
    #[test]
    fn paths_in_the_shim_are_quoted() {
        let s = shim_script(
            claude(),
            Path::new("/opt/my tools/claude"),
            "abc",
            Path::new("/tmp/my markers/m"),
            None,
        );
        assert!(s.contains("'/opt/my tools/claude'"), "{s}");
        assert!(s.contains("'/tmp/my markers/m'"), "{s}");
    }

    /// The conversation outlives our marker — it is cleaned up, moved, or never
    /// written because a first run was killed early — and asking for it with
    /// --session-id then fails with "already in use". The agent's own store is
    /// the thing that actually knows.
    #[test]
    fn the_shim_asks_the_agent_store_not_just_its_own_marker() {
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "abcd-1234",
            Path::new("/tmp/m"),
            None,
        );
        assert!(s.contains(".claude/projects/"), "{s}");
        assert!(s.contains("'abcd-1234'.jsonl"), "{s}");
        // And the marker is still consulted, for a store that is not there.
        assert!(s.contains("[ -e '/tmp/m' ]"), "{s}");
    }

    /// `cargo test` must not leave a configuration behind that points a real
    /// agent at a binary in `target/debug/deps`. This test asserts the guard by
    /// being one: it *is* running from a test binary.
    #[test]
    fn a_test_binary_never_advertises_itself_as_the_commander() {
        let root = tempfile::tempdir().expect("a temp dir");
        assert!(
            mcp_config(root.path(), "s1").is_none(),
            "current_exe() here is a test binary"
        );
    }

    /// The commander describes itself to the agent it hosts. Without this the
    /// agent is in the file manager and cannot see it.
    #[test]
    fn the_shim_hands_the_agent_our_own_mcp_configuration() {
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "abcd-1234",
            Path::new("/tmp/m"),
            Some(r#"{"mcpServers":{"dmac":{"command":"/usr/bin/dmac"}}}"#),
        );
        assert!(
            s.contains(r#"--mcp-config '{"mcpServers":{"dmac":{"command":"/usr/bin/dmac"}}}'"#),
            "the description travels inline, not as a path: {s}"
        );
        // On every route out, including the one the user's own flags take:
        // choosing a conversation is not choosing to be blind. Counted against
        // the `exec`s themselves, so adding a route cannot quietly add a blind
        // one.
        assert_eq!(
            s.matches("--mcp-config").count(),
            s.matches("exec '/usr/local/bin/claude'").count(),
            "every exec should carry it: {s}"
        );
        assert!(s.matches("--mcp-config").count() >= 4, "{s}");
    }

    /// ...and without one, the shim is exactly what it was.
    #[test]
    fn no_configuration_means_no_extra_flag() {
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "abcd-1234",
            Path::new("/tmp/m"),
            None,
        );
        assert!(!s.contains("--mcp-config"), "{s}");
    }

    /// A directory named by pid, so two runs of the suite cannot collide.
    fn scratch(what: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dmac-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// The shim's one decision, exercised by running it. All of resuming lives
    /// in a few lines of `sh`, and asserting on the text of them proves nothing
    /// about what `sh` does with it.
    #[cfg(unix)]
    #[test]
    fn the_shim_resumes_only_a_conversation_that_is_really_there() {
        const CONV: &str = "d0754a32-dd64-4d19-891b-d5bcf3de3d4b";
        let dir = scratch("decide");
        let home = dir.join("home");
        let argv = dir.join("argv");
        let marker = dir.join("marker");

        // Stands in for the agent: writes down what it was handed, and stops.
        let real = dir.join("agent");
        write_executable(
            &real,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", argv.display()),
        )
        .expect("agent");

        let shim = dir.join("claude");
        write_executable(&shim, &shim_script(claude(), &real, CONV, &marker, None)).expect("shim");

        let transcript = home
            .join(".claude/projects/somewhere")
            .join(format!("{CONV}.jsonl"));
        let run = || {
            let _ = std::fs::remove_file(&argv);
            let ok = std::process::Command::new(&shim)
                .env("HOME", &home)
                .status()
                .expect("the shim runs")
                .success();
            assert!(ok, "the shim exited badly");
            std::fs::read_to_string(&argv).expect("the agent recorded nothing")
        };

        // A conversation with a transcript is resumed.
        std::fs::create_dir_all(transcript.parent().expect("parent")).expect("projects");
        std::fs::write(&transcript, "{}").expect("transcript");
        assert!(
            run().contains("--resume"),
            "a real conversation was not resumed"
        );

        // One that was reserved and never typed into leaves a marker and no
        // transcript. Resuming that does not start empty — it refuses to start.
        std::fs::remove_file(&transcript).expect("remove");
        std::fs::write(&marker, "").expect("marker");
        let args = run();
        assert!(args.contains("--session-id"), "a ghost was resumed: {args}");

        // Unless the store cannot be looked at at all, which is the one case
        // the marker was ever for.
        std::fs::remove_dir_all(home.join(".claude")).expect("remove store");
        assert!(
            run().contains("--resume"),
            "with no store to consult, the marker has the say"
        );
    }

    /// Running it twice must not write it twice: the offer is made once per
    /// run, and a user who says yes on three mornings should not find three
    /// copies of the same block in their `.zshrc`.
    #[test]
    fn the_repair_is_written_once() {
        let rc = scratch("rc").join(".zshrc");
        std::fs::write(&rc, "export PATH=\"$HOME/.local/bin:$PATH\"").expect("write");
        repair(&rc).expect("first");
        repair(&rc).expect("second");
        let text = std::fs::read_to_string(&rc).expect("read");
        assert_eq!(
            text.matches(SHIM_DIR_VAR).count(),
            POSIX_REPAIR.matches(SHIM_DIR_VAR).count(),
            "{text}"
        );
        // And what was there before is still there, untouched and still first:
        // the whole point is to run after it.
        assert!(
            text.starts_with("export PATH=\"$HOME/.local/bin:$PATH\"\n"),
            "{text}"
        );
    }

    /// fish is not a POSIX shell and `${PATH%%:*}` is a syntax error in it.
    #[test]
    fn a_fish_rc_is_written_in_fish() {
        let rc = scratch("fish").join("config.fish");
        repair(&rc).expect("write");
        let text = std::fs::read_to_string(&rc).expect("read");
        assert!(text.contains("fish_add_path"), "{text}");
        assert!(!text.contains("${PATH%%:*}"), "{text}");
    }

    /// The shell is told the directory as well as given it, because being
    /// given it is not enough — the rc files run afterwards.
    #[test]
    fn the_shell_is_told_where_the_shim_is() {
        let env = environment(Path::new("/tmp/shims/s1"), "work", "abc");
        assert!(
            env.iter()
                .any(|(k, v)| k == SHIM_DIR_VAR && v == "/tmp/shims/s1"),
            "{env:?}"
        );
    }

    /// What this machine's shell would really do, which no assertion can know.
    /// Ignored because it starts a whole interactive shell:
    ///
    ///     cargo test -p dmac-session -- --ignored --nocapture
    #[test]
    #[ignore]
    fn what_this_shell_would_run() {
        let dir = scratch("check");
        write_executable(&dir.join("claude"), "#!/bin/sh\nexit 0\n").expect("write");
        println!("{:?}", check(&dir));
    }

    #[test]
    fn the_shim_directory_goes_in_front_of_the_path() {
        let env = environment(Path::new("/tmp/shims/s1"), "work", "abc");
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .expect("a PATH");
        assert!(path.starts_with("/tmp/shims/s1"), "{path}");
        assert!(
            env.iter()
                .any(|(k, v)| k == "DMAC_CONVERSATION" && v == "abc"),
            "the id should be visible to the shell too"
        );
    }

    /// Without this the shim finds itself and recurses until the process runs
    /// out of file descriptors.
    // SAFETY: `set_var` is unsound only when another thread is reading the
    // environment at the same time. This test is the only thing touching PATH
    // and the suite gives it no concurrent reader of its own.
    #[allow(unsafe_code)]
    #[test]
    fn resolving_skips_our_own_shim_directory() {
        let dir = std::env::temp_dir().join(format!("dmac-shim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        write_executable(&dir.join("claude"), "#!/bin/sh\nexit 0\n").expect("write");

        let old = std::env::var_os("PATH");
        // SAFETY-adjacent: this test is single-threaded with respect to PATH.
        unsafe { std::env::set_var("PATH", &dir) };
        let found = resolve("claude", &dir);
        if let Some(old) = old {
            unsafe { std::env::set_var("PATH", old) };
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert!(found.is_none(), "resolved to our own shim: {found:?}");
    }

    #[test]
    fn only_the_program_name_counts_as_an_agent() {
        assert!(is_attached("claude"));
        assert!(is_attached("/opt/homebrew/bin/claude --session-id abc"));
        assert!(
            !is_attached("vim claude.md"),
            "an argument is not the program"
        );
        assert!(!is_attached("grep claude /etc/hosts"));
        assert!(!is_attached(""));
    }

    /// A native install is `/path/claude`; an npm one is `node /path/cli.js`.
    /// Both are the program the user thinks they are running.
    #[test]
    fn an_agent_behind_an_interpreter_still_counts() {
        assert!(is_attached(
            "node /usr/local/lib/node_modules/claude.js --session-id abc"
        ));
        assert!(is_attached("/bin/sh /home/me/bin/claude"));
        assert!(is_attached("bun /opt/claude.mjs"));
        // The interpreter alone is not an agent, whatever it is running.
        assert!(!is_attached("node /usr/lib/server.js"));
        assert!(!is_attached("/bin/sh"));
    }

    /// A saved command line names the conversation it created; running it again
    /// verbatim asks for one that already exists, which is the "already in use"
    /// the user actually hits.
    #[test]
    fn what_is_replayed_is_the_command_and_not_its_expansion() {
        // The line exactly as `ps` hands it back: one `argv` rejoined with
        // spaces, so the quoting around the JSON — which itself contains a path
        // with a space in it — is already gone.
        let seen = concat!(
            "/Users/x/.local/bin/claude --mcp-config ",
            "{\"mcpServers\":{\"dmac\":{\"args\":[\"--mcp\",\"/Users/x/Library/Application ",
            "Support/DMACommander/mcp/42891.sock\",\"--mcp-session\",\"2\"],",
            "\"command\":\"/Users/x/dmac\"}}} --resume d0754a32-dd64-4d19-891b-d5bcf3de3d4b"
        );
        // Not a fragment of that object survives: handed to a shell, the braces
        // are globs and the whole line is refused with "bad pattern".
        assert_eq!(as_resume(seen), "claude");
    }

    /// What the *user* chose is theirs, and is not ours to drop.
    #[test]
    fn the_arguments_the_user_chose_survive() {
        assert_eq!(
            as_resume("claude --session-id 1234 --model opus"),
            "claude --model opus"
        );
        assert_eq!(
            as_resume("/opt/bin/claude --resume=abc --permission-mode auto"),
            "claude --permission-mode auto"
        );
    }

    /// The one that cost an evening. A resume flag whose id has gone missing
    /// does not fail — it opens whichever conversation was most recent, and
    /// lands someone in a conversation they have never seen with no clue why.
    #[test]
    fn a_resume_flag_never_comes_back_without_its_id() {
        for line in [
            "claude --resume",
            "claude --mcp-config {\"a\":1} --resume",
            "claude --session-id",
            "/abs/claude -r",
        ] {
            let out = as_resume(line);
            assert!(!out.contains("resume"), "{line:?} became {out:?}");
            assert!(!out.contains("session-id"), "{line:?} became {out:?}");
            assert!(!out.contains(" -r"), "{line:?} became {out:?}");
        }
    }

    /// Everything the user chose is part of what they set up, and giving back
    /// something that only looks like it is worse than giving back nothing.
    #[test]
    fn every_other_argument_survives_the_rewrite() {
        let line = "/opt/bin/claude --session-id abc --model opus --permission-mode plan -n work";
        let out = as_resume(line);
        for keep in ["--model", "opus", "--permission-mode", "plan", "-n", "work"] {
            assert!(out.contains(keep), "{keep} was dropped from {out}");
        }
        assert!(!out.contains("--session-id"), "{out}");
    }

    #[test]
    fn the_running_agent_is_found_among_other_processes() {
        let commands = vec![
            "node /usr/lib/something.js".to_string(),
            "/opt/bin/claude --session-id abc".to_string(),
            "sleep 300".to_string(),
        ];
        assert_eq!(
            running_agent(&commands).as_deref(),
            Some("/opt/bin/claude --session-id abc")
        );
        assert!(running_agent(&["sleep 300".to_string()]).is_none());
    }
}

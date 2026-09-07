//! Recognising a hosted agent — Claude Code, in practice — and bringing it back
//! with the session it belongs to.
//!
//! There used to be a shim here: a `claude` script planted early on the hosted
//! shell's `PATH`, which added the session's conversation id to every
//! invocation. It is gone. `PATH` is not ours to keep — the shell reads its rc
//! files after we set it, and an rc file that puts its own directory in front
//! puts it in front of ours — so the shim worked or did not depending on
//! someone else's dotfiles, and when it did not it failed in silence.
//!
//! What is left works on what can actually be observed: the command line of
//! whatever is running in the shell, read through `ps`. If the user starts an
//! agent with a conversation id, that id is in its arguments, and this module
//! saves the line, hands it back as a resume on the next run, and clears
//! anything still holding the conversation. Nothing is written to the user's
//! machine, and nothing depends on being found first on a search path.

/// Programs worth recognising as an agent, by the name people run them under.
///
/// One entry today. It is a list because the next one — any agent CLI with
/// resumable conversations — needs exactly the same treatment, and a list makes
/// that obvious to whoever adds it.
const ATTACHED: &[&str] = &["claude"];

/// The agent a session hosts, named once. The menu that offers to start it and
/// the code that recognises it running have to agree, and the way to guarantee
/// that is for there to be one name.
pub fn attached_program() -> &'static str {
    ATTACHED[0]
}

/// Which attached program, if any, is among these running commands.
///
/// Asked at shutdown so the next run knows what to start again — this is what
/// turns "the conversation id was saved" into "the conversation came back".
pub fn running_agent(commands: &[String]) -> Option<String> {
    commands.iter().find(|line| is_attached(line)).cloned()
}

/// What each of these shells is hosting, from one read of the process table.
///
/// One `ps` for every session rather than one per session. That is what makes
/// it affordable to ask this *while the commander runs* instead of only on the
/// way out — and a record written only on the way out is a record that a
/// `kill -9`, a closed terminal window or a panic erases completely, taking
/// with it any chance of putting the agent back.
#[cfg(unix)]
pub fn scan<Id: Copy>(shells: &[(Id, u32)]) -> Vec<(Id, Option<String>)> {
    let table = dmac_pty::ProcessTable::read();
    shells
        .iter()
        .map(|(id, pid)| (*id, running_agent(&table.commands_under(*pid as i32))))
        .collect()
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
    if ATTACHED.contains(&first.as_str()) {
        return true;
    }
    if INTERPRETERS.contains(&first.as_str())
        && let Some(second) = words.get(1).map(|w| base(w))
    {
        // `node /path/claude.js` and `sh /path/claude` both count; the
        // extension is not part of the name people know it by.
        let stem = second.split('.').next().unwrap_or(&second);
        return ATTACHED.contains(&stem);
    }
    false
}

/// Whether a word is a conversation id: a UUID, which is what the agent's own
/// store names its files by and what `--resume` takes.
///
/// Strict on purpose. This decides whether an id observed in someone else's
/// command line is adopted as the session's, and adopting a word that is not an
/// id would point the session at a conversation that does not exist — which
/// fails at the worst moment, on the restart where the user expects their work
/// back.
fn looks_like_conversation(w: &str) -> bool {
    let b = w.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// The conversation a command line names, if it names one.
///
/// This is the only thing that knows, for certain, which conversation a running
/// agent belongs to. The alternative — matching the newest transcript in the
/// project directory — is a guess, and it is wrong in exactly the case this
/// program is built for: several commanders open on the same repository, each
/// with its own agent, all appending into the same directory. A guess that is
/// usually right is not good enough for something whose failure mode is "you
/// are in someone else's conversation".
///
/// The transcript is not held open, so the file the agent is writing cannot be
/// read off its file descriptors either. That was checked, not assumed.
///
/// `None` is an honest and common answer: `claude --resume` opens a picker and
/// `claude -c` continues the most recent, and neither says on the command line
/// where it ended up. `--fork-session` is `None` too — it resumes under a *new*
/// id, so the one on the line is the parent and not what is running.
pub fn conversation_of(line: &str) -> Option<String> {
    // `--session-id` first, and on its own terms: it names the conversation the
    // agent will *be in*, which is not always the one `--resume` names. A fork
    // is exactly that case — `--resume <parent> --fork-session --session-id
    // <new>` starts from the parent's history and becomes `<new>` — and it is
    // how a session opened beside another one is started, so getting this
    // precedence wrong would resume every sibling into its mother.
    if let Some(id) = dictated(line) {
        return Some(id);
    }
    // Nothing dictated, and it forks: the id on the line is the parent's, and
    // what is actually running has an id nobody outside has been told.
    if forks(line) {
        return None;
    }
    let mut words = line.split_whitespace().peekable();
    while let Some(w) = words.next() {
        let named = match w.split_once('=') {
            Some(("--resume" | "-r", v)) => Some(v.to_string()),
            _ if matches!(w, "--resume" | "-r") => words.peek().map(|v| (*v).to_string()),
            _ => None,
        };
        if let Some(v) = named
            && looks_like_conversation(&v)
        {
            return Some(v);
        }
    }
    None
}

/// The conversation id the line *dictates*, with `--session-id`.
///
/// Split out because it outranks everything else on the line: whatever else is
/// there, this is the conversation the agent ends up in.
fn dictated(line: &str) -> Option<String> {
    let mut words = line.split_whitespace().peekable();
    while let Some(w) = words.next() {
        let v = match w.split_once('=') {
            Some(("--session-id", v)) => Some(v.to_string()),
            _ if w == "--session-id" => words.peek().map(|v| (*v).to_string()),
            _ => None,
        };
        if let Some(v) = v
            && looks_like_conversation(&v)
        {
            return Some(v);
        }
    }
    None
}

/// Whether the line asks for a new id on the way in.
///
/// `--fork-session` resumes a conversation and immediately becomes a different
/// one, so nothing on the line names what is actually running.
fn forks(line: &str) -> bool {
    line.split_whitespace()
        .any(|w| w == "--fork-session" || w.starts_with("--fork-session="))
}

/// Whether the line settles the conversation in a way that still belongs to
/// this session.
///
/// `-c` continues the most recent conversation *in this directory* and
/// `--fork-session` makes a new one: both are rules whose answer is this
/// session's, so replaying them is faithful.
///
/// A **bare `--resume` is not**, and that is a correction. It opens a picker
/// whose default is the most recent conversation anywhere. Replaying one used
/// to be justified as handing the user back their own choice; restarting with
/// nine sessions puts nine pickers on screen, the obvious thing to do with each
/// is press Enter, and nine sessions land in whichever conversation was touched
/// last. One belongs there. The other eight are in somebody else's.
///
/// It is silent and it perpetuates itself: the running process still shows only
/// `claude --resume`, so nothing out here can learn where that session went,
/// its stored conversation stays unrelated to what is on screen, and the next
/// restart does it again.
fn picks_its_own(line: &str) -> bool {
    // A fork that was told which id to become has not chosen anything — we did,
    // and we know it. Only an undirected fork is out of our reach, and a fresh
    // fork is at worst a new conversation, never somebody else's.
    if forks(line) && dictated(line).is_none() {
        return true;
    }
    if dictated(line).is_some() {
        return false;
    }
    // `-c` continues the most recent conversation *in this directory* — a rule
    // that produces an answer belonging to this session. A bare `--resume` is
    // not that, and is deliberately absent here: see the note above.
    line.split_whitespace().any(|w| {
        let head = w.split_once('=').map_or(w, |(k, _)| k);
        matches!(head, "-c" | "--continue")
    })
}

/// What to run to get this conversation back, as something a shell can be
/// handed safely.
///
/// The input is not a command line. It is an `argv` observed through `ps` and
/// rejoined with spaces, so every quote its author wrote is already gone —
/// which matters enormously, because a `--mcp-config` argument is a JSON
/// object full of braces and containing a path with a space in it. Handed
/// back to a shell it is not one argument any more: `zsh` word-splits it,
/// tries to glob `{"mcpServers":{...}}`, and refuses the whole line with "bad
/// pattern". That is not a thing to fix by quoting it again — the quoting that
/// was lost cannot be recovered — so what comes back here is the *command*, not
/// the expansion of it:
///
/// - the program by its bare name, so what runs is whatever the user's shell
///   resolves today, not the absolute path `ps` happened to show last time;
/// - nothing that belonged to the run that is over — a socket named in an old
///   `--mcp-config` died with the commander that printed it;
/// - everything the *user* chose, untouched: a model, a permission mode, a
///   directory. That is what they set up, and it is not ours to drop.
///
/// The conversation the line named is *kept*, and that is a correction. It used
/// to be stripped and replaced with the session's own, on the reasoning that
/// the two are always the same — which holds only while the commander is the
/// one that started the agent. Type `claude --resume <something-else>` in a
/// hosted shell and they are not the same, and the restart silently put the
/// user back in a conversation they had left. What is on the line is what was
/// running; the session follows it, not the other way round.
pub fn as_resume(line: &str) -> String {
    // A fork happens once, at birth. The conversation it produced exists now
    // and has an id of its own, so coming back to it is an ordinary resume —
    // replaying the fork would start a *new* conversation every restart, each
    // one branching from the same mother and none of them the one the user was
    // in yesterday.
    let born = dictated(line).is_some();
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
            // A named conversation is the one fact on this line worth more
            // than anything the commander remembers — but it does not travel
            // *here*. It is put back by [`start_command`] as the variable the
            // hosted shell already carries, so the typed line stays something
            // a person can read and correct before pressing Enter, and no id
            // has to survive being quoted through a shell. What this does is
            // take it off the line, so there is only ever one selector.
            //
            // Dropping the flag and keeping the value would leave the value
            // standing on the command line as a positional argument, which for
            // an agent is not a stray word, it is a prompt.
            "--session-id" | "--resume" | "-r" => match words.peek() {
                Some(v) if looks_like_conversation(v) => {
                    words.next();
                }
                // A picker, with or without a search term. It does *not* come
                // back — see [`picks_its_own`] — and the session's own
                // conversation goes on instead, which is the one this program
                // can actually name.
                Some(v) if w != "--session-id" && !v.starts_with('-') => {
                    words.next();
                }
                _ if w != "--session-id" => {}
                // `--session-id` takes a UUID and nothing else, so anything
                // else is a line that could not have started. Both halves go.
                Some(v) if !v.starts_with('-') => {
                    words.next();
                }
                _ => {}
            },
            "--fork-session" if born => {}
            _ if w.starts_with("--fork-session=") && born => {}
            _ if w.starts_with("--mcp-config=") || w.starts_with("--session-id=") => {}
            // An id of ours goes back on as the variable; a picker's search
            // term is dropped, for the reason above.
            _ if w.starts_with("--resume=") || w.starts_with("-r=") => {}
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
/// The environment a hosted shell is given: the ids, in plain sight.
///
/// No `PATH` surgery. Nothing here changes what a command resolves to — these
/// are facts a script or an agent can read if it wants them, and ignore if it
/// does not. `DMAC_MCP_SOCKET` is the one that earns its place: it is how
/// anything running in this shell can find the commander hosting it without
/// having been configured to.
pub fn environment(
    session_id: &str,
    session_name: &str,
    conversation: &str,
    parent_conversation: Option<&str>,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("DMAC_SESSION".to_string(), session_name.to_string()),
        // The id as well as the name, because it is what the bridge takes:
        // `--mcp-session` is how a tool call is answered about the session the
        // agent is hosted in rather than whichever one is on screen. Without
        // it the documented one-liner is subtly worse than what it replaces.
        ("DMAC_SESSION_ID".to_string(), session_id.to_string()),
        ("DMAC_CONVERSATION".to_string(), conversation.to_string()),
    ];
    if let Some(root) = crate::agent_root() {
        let socket = dmac_mcp::socket_path(&root, std::process::id());
        env.push(("DMAC_MCP_SOCKET".to_string(), socket.display().to_string()));
    }
    if let Some(config) = mcp_config(session_id) {
        env.push((MCP_CONFIG_VAR.to_string(), config));
    }
    if let Some(parent) = parent_conversation {
        env.push((PARENT_VAR.to_string(), parent.to_string()));
    }
    env
}

/// The variable carrying the commander's own MCP description to a hosted shell.
///
/// Named once because two things have to agree about it: [`environment`], which
/// sets it, and [`start_command`], which spends it. A line that expands to
/// nothing would hand the agent `--mcp-config ""`, which is not a server that
/// is absent — it is a server that is malformed, and it fails on startup.
const MCP_CONFIG_VAR: &str = "DMAC_MCP_CONFIG";

/// The variable carrying this session's conversation id. Set by [`environment`]
/// and spent by [`start_command`], for the same reason as [`MCP_CONFIG_VAR`].
const CONVERSATION_VAR: &str = "DMAC_CONVERSATION";

/// What to type at a hosted shell to start the agent in it.
///
/// The program by its bare name, so the user's own shell resolves it — their
/// aliases, their functions, their `PATH`. Then what the commander knows and
/// the user should not have to retype: its own description, and which
/// conversation this session is.
///
/// Both by the *variable* rather than the value. The description is JSON full
/// of braces wrapped around a path with a space in it, and a command line
/// carrying it is one nobody can read — which defeats the point of typing it
/// where it can be seen and corrected before Enter.
///
/// `theirs` is whatever the user chose themselves — a model, a permission mode,
/// a directory — kept, and put where it reads first.
///
/// `--mcp-config` goes **last**, and that is not a matter of taste: it takes
/// *several* values, space-separated, so it swallows every following word that
/// does not begin with a dash. With it earlier, a plain argument of theirs — a
/// directory, a prompt — would be read as another configuration file, and the
/// agent would refuse to start over a path nobody wrote. At the end of the line
/// there is nothing left for it to take.
pub fn start_command(session_id: &str, conversation: &str, theirs: &str) -> String {
    let mut line = attached_program().to_string();
    if !conversation.is_empty() {
        line.push_str(&format!(
            " {} \"${CONVERSATION_VAR}\"",
            conversation_flag(conversation)
        ));
    }
    let theirs = theirs.trim();
    if !theirs.is_empty() {
        line.push(' ');
        line.push_str(theirs);
    }
    if mcp_config(session_id).is_some() {
        line.push_str(&format!(" --mcp-config \"${MCP_CONFIG_VAR}\""));
    }
    line
}

/// The variable carrying the conversation a sibling is forked *from*.
///
/// Set only on a session that was opened beside another one, and spent only by
/// [`fork_command`]. Like the others it travels as a variable rather than as
/// thirty-six characters on a line nobody can check by eye.
const PARENT_VAR: &str = "DMAC_PARENT_CONVERSATION";

/// What to type to start an agent that begins where another one is.
///
/// `claude --resume <mother> --fork-session --session-id <ours>`: it reads the
/// mother's whole history, then becomes a conversation of its own and diverges.
/// That is what "beside this one" has to mean — a second agent that starts by
/// knowing everything the first one knows, rather than an empty one sitting
/// next to it that has to be told the problem again.
///
/// The `--session-id` is not optional and is the difference between a good idea
/// and a working one. Without it the fork picks an id nobody outside is told,
/// and the sibling is unresumable: tomorrow there is a session in the rail whose
/// conversation cannot be named. With it we choose the id up front, so the
/// sibling comes back exactly like any other session. The combination is
/// accepted — checked against the real CLI, which parses all three and then
/// complains only about a mother that does not exist.
pub fn fork_command(session_id: &str, theirs: &str) -> String {
    let mut line = format!(
        "{} --resume \"${PARENT_VAR}\" --fork-session --session-id \"${CONVERSATION_VAR}\"",
        attached_program()
    );
    let theirs = theirs.trim();
    if !theirs.is_empty() {
        line.push(' ');
        line.push_str(theirs);
    }
    // Last, as always: it takes several values and swallows every following
    // word that does not begin with a dash.
    if mcp_config(session_id).is_some() {
        line.push_str(&format!(" --mcp-config \"${MCP_CONFIG_VAR}\""));
    }
    line
}

/// The same, for an agent the last run was hosting: the saved line stripped of
/// what belonged to that run, with this run's own put back.
///
/// `conversation` is the session's, and it is only used when the saved line
/// settles nothing. A line that names a conversation has already answered the
/// question, and a line that says *how* to choose one — `-c`, or a bare
/// `--resume` and its picker — has answered it too, in the user's own terms.
/// Adding ours on top of either is two answers to one question, and the agent
/// acts on the wrong one.
pub fn resume_command(session_id: &str, conversation: &str, saved: &str) -> String {
    let stripped = as_resume(saved);
    let theirs = stripped.split_once(' ').map_or("", |(_, rest)| rest);
    if picks_its_own(saved) {
        return start_command(session_id, "", theirs);
    }
    start_command(session_id, conversation, theirs)
}

/// `--session-id` for a conversation that does not exist yet, `--resume` for one
/// that does.
///
/// There is no flag that means "either". `--session-id` is refused for a
/// conversation that already exists — *"Session ID … is already in use"* — and
/// `--resume` for one that does not, so the choice has to be made by looking,
/// and the only thing that really knows is the agent's own store.
///
/// A marker file of ours was the alternative, and it was wrong exactly when it
/// mattered: cleaned up, moved, or never written because the first run was
/// killed before it got that far. The conversation outlives our bookkeeping,
/// and asking the store is the answer that cannot drift from the truth.
fn conversation_flag(conversation: &str) -> &'static str {
    match agent_store().is_some_and(|s| conversation_in(&s, conversation)) {
        true => "--resume",
        false => "--session-id",
    }
}

/// Where Claude Code keeps its conversations.
fn agent_store() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        return Some(std::path::PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".claude"))
}

/// Whether `store` already holds this conversation.
///
/// Split from the lookup so it can be tested against a directory laid out by
/// hand: the alternative is writing `HOME` or `CLAUDE_CONFIG_DIR`, which is
/// process-wide and races every other test in the binary.
///
/// One file per conversation, under a directory per project — and which project
/// is not ours to guess, so every one of them is looked in.
fn conversation_in(store: &std::path::Path, conversation: &str) -> bool {
    let Ok(projects) = std::fs::read_dir(store.join("projects")) else {
        return false;
    };
    let name = format!("{conversation}.jsonl");
    projects.flatten().any(|p| p.path().join(&name).is_file())
}

/// This commander described as an MCP server, in the JSON `--mcp-config` takes.
///
/// Put in the environment rather than on a command line, which is what makes
/// the command line worth looking at: `claude --mcp-config "$DMAC_MCP_CONFIG"`
/// is a line you can read, and the JSON — braces, a socket path with a space
/// in it — never has to survive being quoted through a shell to get there.
///
/// `None` when there is nothing to describe: no socket bound, or no commander
/// to point at. Absent, an agent starts perfectly well and simply cannot see
/// the panels; a server that is described but dead is worse, because it wastes
/// the user's time working out why the tools never answer.
pub fn mcp_config(session_id: &str) -> Option<String> {
    let binary = std::env::current_exe().ok()?;
    // A test binary is not a commander. Without this a `cargo test` run would
    // describe something in `target/debug/deps` that the next build deletes.
    if binary.file_stem().is_none_or(|n| n != "dmac") {
        return None;
    }
    let root = crate::agent_root()?;
    let socket = dmac_mcp::socket_path(&root, std::process::id());
    // Only advertise a server that is actually there. Binding can fail — a path
    // too long for a socket address, a read-only directory — and it fails
    // quietly, so describing it anyway hands the agent a path nothing answers
    // on.
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

/// Remove the shim directories earlier versions wrote.
///
/// Nothing creates them any more, so this is a one-way sweep rather than the
/// per-run housekeeping it replaces. Worth doing rather than leaving behind:
/// each of those directories holds an executable called `claude` that resolves
/// to a path from a run that is long gone, and a stray `claude` on disk is the
/// kind of thing that is eventually found by something.
///
/// Returns how many were removed.
pub fn clear_shims(root: &std::path::Path) -> usize {
    let shims = root.join("shims");
    let Ok(entries) = std::fs::read_dir(&shims) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        if std::fs::remove_dir_all(e.path()).is_ok() {
            removed += 1;
        }
    }
    let _ = std::fs::remove_dir(&shims);
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shim directories are the one thing an older version left on the
    /// user's disk, and each holds an executable called `claude`. Upgrading
    /// has to take them away.
    #[test]
    fn the_sweep_takes_away_what_older_versions_left_behind() {
        let root = std::env::temp_dir().join(format!("dmac-sweep-{}", std::process::id()));
        let dir = root.join("shims").join("run-1234").join("0");
        std::fs::create_dir_all(&dir).expect("scratch");
        std::fs::write(dir.join("claude"), "#!/bin/sh\nexit 0\n").expect("write");

        assert_eq!(clear_shims(&root), 1);
        assert!(
            !root.join("shims").exists(),
            "the directory itself has to go too, not only its contents"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The line and the environment have to agree about the variable. If they
    /// drift apart the line expands to nothing and the agent is handed
    /// `--mcp-config ""` — not a missing server but a malformed one, which
    /// fails at startup for a reason that names neither of them.
    #[test]
    fn the_typed_line_spends_only_what_the_environment_sets() {
        let line = start_command("3", "11111111-2222-3333-4444-555555555555", "");
        if let Some(rest) = line.strip_prefix(attached_program()) {
            if rest.contains("--mcp-config") {
                let named = format!("${MCP_CONFIG_VAR}");
                assert!(rest.contains(&named), "{line} does not use {named}");
                let env = environment("3", "work", "11111111-2222-3333-4444-555555555555", None);
                assert!(
                    env.iter().any(|(k, _)| k == MCP_CONFIG_VAR),
                    "the line spends {MCP_CONFIG_VAR}, which nothing sets"
                );
            }
        } else {
            panic!("the line must start with the program: {line}");
        }
        // The same coupling for the conversation, which is always spent.
        let env = environment("3", "work", "11111111-2222-3333-4444-555555555555", None);
        assert!(line.contains(&format!("${CONVERSATION_VAR}")), "{line}");
        assert!(
            env.iter().any(|(k, _)| k == CONVERSATION_VAR),
            "the line spends {CONVERSATION_VAR}, which nothing sets"
        );
    }

    /// The description is JSON: braces, quotes, and a socket path that on macOS
    /// contains a space. None of it may reach a command line — it would have to
    /// survive being quoted through a shell to get there, and it does not.
    #[test]
    fn the_description_never_reaches_the_command_line() {
        let line = start_command("3", "11111111-2222-3333-4444-555555555555", "");
        for c in ['{', '}', '\''] {
            assert!(!line.contains(c), "{c:?} in the typed line: {line}");
        }
    }

    /// `--mcp-config` takes several values, so it eats every following word that
    /// does not start with a dash. A plain argument of the user's after it — a
    /// directory, a prompt — is read as another configuration file, and the
    /// agent refuses to start over a path nobody wrote. It has to be last.
    #[test]
    fn nothing_follows_the_configuration_that_it_could_swallow() {
        for theirs in ["", "--model opus", "/some/directory", "--verbose /a/dir"] {
            let line = start_command("3", "ffffffff-0000-4000-8000-000000000000", theirs);
            let Some(at) = line.find("--mcp-config") else {
                continue;
            };
            let after: Vec<&str> = line[at..].split_whitespace().skip(2).collect();
            assert!(
                after.is_empty(),
                "{after:?} would be read as more configuration files: {line}"
            );
        }
    }

    /// There is no flag that means "either". `--session-id` is refused for a
    /// conversation that already exists and `--resume` for one that does not,
    /// so this has to be decided by looking — and the agent's own store is the
    /// only thing that knows.
    #[test]
    fn the_store_decides_which_flag() {
        let store = std::env::temp_dir().join(format!("dmac-store-{}", std::process::id()));
        let project = store.join("projects").join("-Users-someone-code");
        std::fs::create_dir_all(&project).expect("scratch");
        let known = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        std::fs::write(project.join(format!("{known}.jsonl")), "{}\n").expect("write");

        assert!(
            conversation_in(&store, known),
            "a conversation the store holds has to be found in whichever project it is under"
        );
        assert!(
            !conversation_in(&store, "ffffffff-0000-4000-8000-000000000000"),
            "and one it does not hold must not be"
        );
        // A store that is not there at all is not an error, it is a first run.
        assert!(!conversation_in(&store.join("nope"), known));
        let _ = std::fs::remove_dir_all(&store);
    }

    /// A conversation nothing has ever heard of is a first start, and a first
    /// start is `--session-id`. Never a bare `--resume`: that does not fail,
    /// it silently opens whichever conversation was most recent.
    #[test]
    fn an_unknown_conversation_is_started_and_not_resumed() {
        let line = start_command("3", "ffffffff-0000-4000-8000-000000000000", "");
        assert!(line.contains("--session-id"), "{line}");
        assert!(!line.contains("--resume"), "{line}");
    }

    /// What the user chose is theirs and comes back untouched; what belonged to
    /// the run that ended does not — its socket died with it.
    ///
    /// The conversation is neither: it belongs to *the agent*, and it outlives
    /// both runs. It used to be stripped and replaced with the session's own,
    /// which is a no-op while the commander is the one that started the agent
    /// and silently wrong the moment it is not — type `claude --resume <other>`
    /// in a hosted shell and every restart afterwards put you back in the
    /// commander's conversation instead of the one you were in.
    #[test]
    fn a_resumed_line_keeps_the_conversation_that_was_running() {
        let ran = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let ours = "ffffffff-0000-4000-8000-000000000000";
        let saved = format!(
            "/opt/homebrew/bin/claude --mcp-config {{\"mcpServers\":{{\"dmac\":{{}}}}}} \
             --session-id {ran} --model opus --permission-mode plan"
        );

        // The conversation that was running is read off the line, and it is
        // this — not the session's own — that the session is moved onto before
        // the command is built. Ask for the wrong one here and every restart
        // afterwards puts the user somewhere they left.
        assert_eq!(conversation_of(&saved).as_deref(), Some(ran));

        // Built with what was adopted, which is how the caller must use it: the
        // line spends `$DMAC_CONVERSATION`, and the environment sets it from
        // the same field, so the two cannot disagree.
        let line = resume_command("3", ran, &saved);
        assert!(line.starts_with(attached_program()), "{line}");
        assert!(line.contains("--model opus"), "{line}");
        assert!(line.contains("--permission-mode plan"), "{line}");
        assert!(
            !line.contains("mcpServers"),
            "the dead run's description came back: {line}"
        );
        assert!(
            !line.contains(ran) && !line.contains(ours),
            "an id on the command line is an id quoted through a shell: {line}"
        );
        assert!(line.contains("\"$DMAC_CONVERSATION\""), "{line}");
        // One selector, whichever it is. Two is one too many.
        assert_eq!(
            line.matches("--session-id").count() + line.matches("--resume").count(),
            1,
            "{line}"
        );
    }

    /// A session opened beside another starts *in* its history: that is what
    /// "beside" means, and an empty agent sitting next to a full one has to be
    /// told the problem all over again.
    ///
    /// The `--session-id` is the difference between a good idea and a working
    /// one. Without it the fork becomes an id nobody outside is told, and the
    /// sibling is unresumable — tomorrow there is a session in the rail whose
    /// conversation cannot be named.
    #[test]
    fn a_forked_sibling_starts_in_its_mother_and_keeps_an_id_of_its_own() {
        let line = fork_command("3", "--model opus");
        assert!(line.starts_with(attached_program()), "{line}");
        assert!(line.contains("--fork-session"), "{line}");
        assert!(
            line.contains("--resume \"$DMAC_PARENT_CONVERSATION\""),
            "{line}"
        );
        assert!(
            line.contains("--session-id \"$DMAC_CONVERSATION\""),
            "{line}"
        );
        assert!(line.contains("--model opus"), "{line}");
        // Both ids travel as variables: neither has to survive being quoted
        // through a shell, and the line stays one a person can read.
        assert!(!line.contains('{'), "{line}");
        // The environment sets both of the variables the line spends.
        let env = environment(
            "3",
            "work",
            "11111111-2222-4333-8444-555555555555",
            Some("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
        );
        for var in ["DMAC_CONVERSATION", "DMAC_PARENT_CONVERSATION"] {
            assert!(
                env.iter().any(|(k, _)| k == var),
                "{var} is spent and unset"
            );
        }
        // And a session that was not opened beside anything is not given one.
        let plain = environment("3", "work", "11111111-2222-4333-8444-555555555555", None);
        assert!(!plain.iter().any(|(k, _)| k == "DMAC_PARENT_CONVERSATION"));
    }

    /// The one that would have been found in a week's time. A fork happens
    /// once, at birth; replaying it would branch a *new* conversation on every
    /// restart, each from the same mother and none of them yesterday's.
    #[test]
    fn a_fork_is_resumed_and_never_forked_again() {
        let born = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let mother = "ffffffff-0000-4000-8000-000000000000";
        let seen = format!(
            "/opt/bin/claude --resume {mother} --fork-session --session-id {born} --model opus"
        );

        // The conversation that is running is the one it *became*, not the one
        // it came from. Getting this backwards resumes every sibling into its
        // mother, which is the same conversation twice and neither of them
        // where the user was.
        assert_eq!(conversation_of(&seen).as_deref(), Some(born));

        let line = resume_command("3", born, &seen);
        assert!(
            !line.contains("--fork-session"),
            "it would branch a new conversation on every restart: {line}"
        );
        assert!(line.contains("--model opus"), "{line}");
        assert!(!line.contains(mother), "the mother is on the line: {line}");
        assert!(line.contains("\"$DMAC_CONVERSATION\""), "{line}");
        assert_eq!(
            line.matches("--session-id").count() + line.matches("--resume").count(),
            1,
            "{line}"
        );
    }

    /// `-c` continues the most recent conversation in that directory. It is a
    /// complete answer on its own, so nothing of ours goes with it: a line
    /// carrying both `-c` and a conversation id asks the agent two things at
    /// once.
    #[test]
    fn continuing_is_an_answer_and_is_not_argued_with() {
        let ours = "ffffffff-0000-4000-8000-000000000000";
        for saved in ["claude -c", "claude --continue --model opus"] {
            let line = resume_command("3", ours, saved);
            assert!(
                line.contains("-c") || line.contains("--continue"),
                "{saved:?} became {line:?}"
            );
            assert!(
                !line.contains(ours),
                "{saved:?} came back with the commander's own conversation: {line}"
            );
        }
    }

    /// `--fork-session` resumes a conversation and immediately becomes a
    /// *different* one, so the id on the line is the parent and not what is
    /// running. Adopting it would point the session at a conversation the user
    /// left behind on purpose.
    #[test]
    fn a_forked_session_names_a_parent_and_not_what_is_running() {
        let parent = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
        let saved = format!("claude --resume {parent} --fork-session");
        assert_eq!(
            conversation_of(&saved),
            None,
            "the parent was mistaken for the live conversation"
        );
        // Replayed as the user wrote it: another fork is at worst a new
        // conversation, which is never someone else's.
        let line = resume_command("3", "ffffffff-0000-4000-8000-000000000000", &saved);
        assert!(line.contains("--fork-session"), "{line}");
        assert!(
            !line.contains("ffffffff-0000-4000-8000-000000000000"),
            "{line}"
        );
    }

    /// The id is read from the line only when it is shaped like one. A word
    /// that is not a UUID is a search term or a mistake, and adopting it would
    /// point the session at a conversation that does not exist — which fails on
    /// exactly the restart where the user expects their work back.
    #[test]
    fn only_something_shaped_like_a_conversation_is_taken_for_one() {
        let good = "d0754a32-dd64-4d19-891b-d5bcf3de3d4b";
        assert_eq!(
            conversation_of(&format!("claude --resume {good}")).as_deref(),
            Some(good)
        );
        assert_eq!(
            conversation_of(&format!("claude --session-id={good} --model opus")).as_deref(),
            Some(good)
        );
        for line in [
            "claude",
            "claude --resume",
            "claude --resume auth",
            "claude -c",
            "claude --session-id 1234",
            "claude --model d0754a32-dd64-4d19-891b-d5bcf3de3d4b",
        ] {
            assert_eq!(conversation_of(line), None, "{line:?}");
        }
    }

    /// Nothing this sets may change what a command resolves to. That was the
    /// shim's whole trouble, and the environment is what is left of it.
    #[test]
    fn the_environment_never_touches_the_path() {
        let env = environment("3", "work", "11111111-2222-3333-4444-555555555555", None);
        assert!(
            !env.iter().any(|(k, _)| k == "PATH"),
            "the hosted shell's PATH is the user's, not ours"
        );
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"DMAC_SESSION"), "{names:?}");
        assert!(names.contains(&"DMAC_CONVERSATION"), "{names:?}");
        assert!(names.contains(&"DMAC_SESSION_ID"), "{names:?}");
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
        let out = as_resume(seen);
        assert!(!out.contains('{'), "the description came back: {out}");
        assert!(!out.contains("mcp"), "something of ours came back: {out}");
        // The id comes off the line too — it goes back on as the variable the
        // hosted shell already carries, not as thirty-six characters nobody
        // can check by eye.
        assert_eq!(out, "claude");
        // But it is not *lost*: this is what says which conversation was
        // running, and the session is moved onto it.
        assert_eq!(
            conversation_of(seen).as_deref(),
            Some("d0754a32-dd64-4d19-891b-d5bcf3de3d4b")
        );
    }

    /// What the *user* chose is theirs, and is not ours to drop.
    #[test]
    fn the_arguments_the_user_chose_survive() {
        // `--session-id` takes a UUID and nothing else, so a line carrying
        // anything else could not have started. Both halves go, and the
        // session's own conversation is used instead.
        assert_eq!(
            as_resume("claude --session-id 1234 --model opus"),
            "claude --model opus"
        );
        // A `--resume` whose value is not an id is a search term for the
        // picker, and a picker does not come back — see
        // `a_picker_never_comes_back_because_its_default_is_somebody_else`.
        assert_eq!(
            as_resume("/opt/bin/claude --resume=abc --permission-mode auto"),
            "claude --permission-mode auto"
        );
    }

    /// `--session-id` is not optional: it takes a UUID, so a line where the
    /// value is missing or is not one is a line that never started. Both halves
    /// go rather than leaving the flag to be answered by the wrong thing, or
    /// the value to stand on the command line — where, for an agent, a stray
    /// word is not a stray word, it is a prompt.
    #[test]
    fn a_session_id_without_a_usable_id_is_dropped_entirely() {
        for line in [
            "claude --session-id",
            "claude --session-id 1234",
            "claude --session-id=nope --model opus",
        ] {
            let out = as_resume(line);
            assert!(!out.contains("session-id"), "{line:?} became {out:?}");
            assert!(!out.contains("1234"), "{line:?} became {out:?}");
            assert!(!out.contains("nope"), "{line:?} became {out:?}");
        }
    }

    /// The one that got found in the field, and it was mine.
    ///
    /// A bare `--resume` opens a picker whose default is the most recent
    /// conversation anywhere. Replaying one used to be justified as handing the
    /// user back their own choice. Restarting with nine sessions puts nine
    /// pickers on screen, the obvious thing to do with each is press Enter, and
    /// nine sessions land in whichever conversation was touched last: one of
    /// them belongs there and the other eight are in somebody else's.
    ///
    /// It is also silent and self-perpetuating — the running process still
    /// shows only `claude --resume`, so nothing out here can ever learn where
    /// that session went.
    #[test]
    fn a_picker_never_comes_back_because_its_default_is_somebody_else() {
        let ours = "ffffffff-0000-4000-8000-000000000000";
        for saved in [
            "claude --resume",
            "/abs/claude -r",
            "claude --resume auth",
            "claude --resume=auth --model opus",
            // Exactly as `ps` reported it in the field, with the commander's
            // own configuration alongside it.
            "claude --resume --mcp-config {\"mcpServers\":{}}",
        ] {
            let line = resume_command("3", ours, saved);
            assert!(
                line.contains("\"$DMAC_CONVERSATION\""),
                "{saved:?} came back without a conversation this session owns: {line}"
            );
            // Exactly one selector, whichever of the two it is: which flag
            // depends on whether the agent's own store already holds that
            // conversation, and that is not what this test is about.
            assert_eq!(
                line.matches("--resume").count() + line.matches("--session-id").count(),
                1,
                "{saved:?} became {line:?}, which asks twice or not at all"
            );
        }
        // What the user chose *besides* the picker is still theirs.
        assert!(
            resume_command("3", ours, "claude --resume=auth --model opus").contains("--model opus")
        );
    }

    /// `-c` is different and is kept: it continues the most recent conversation
    /// *in this directory*, a rule whose answer belongs to this session rather
    /// than to whatever was last touched anywhere.
    #[test]
    fn continuing_here_is_a_rule_and_survives() {
        let line = resume_command("3", "ffffffff-0000-4000-8000-000000000000", "claude -c");
        assert!(line.contains("-c"), "{line}");
        assert!(!line.contains("$DMAC_CONVERSATION"), "two answers: {line}");
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

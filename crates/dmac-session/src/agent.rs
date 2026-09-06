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
/// a directory — kept and put last, so it is the part that reads like theirs.
pub fn start_command(session_id: &str, conversation: &str, theirs: &str) -> String {
    let mut line = attached_program().to_string();
    if mcp_config(session_id).is_some() {
        line.push_str(&format!(" --mcp-config \"${MCP_CONFIG_VAR}\""));
    }
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
    line
}

/// The same, for an agent the last run was hosting: the saved line stripped of
/// what belonged to that run, with this run's own put back.
pub fn resume_command(session_id: &str, conversation: &str, saved: &str) -> String {
    let stripped = as_resume(saved);
    let theirs = stripped.split_once(' ').map_or("", |(_, rest)| rest);
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
                let env = environment("3", "work", "11111111-2222-3333-4444-555555555555");
                assert!(
                    env.iter().any(|(k, _)| k == MCP_CONFIG_VAR),
                    "the line spends {MCP_CONFIG_VAR}, which nothing sets"
                );
            }
        } else {
            panic!("the line must start with the program: {line}");
        }
        // The same coupling for the conversation, which is always spent.
        let env = environment("3", "work", "11111111-2222-3333-4444-555555555555");
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
    /// the run that ended does not — its socket died with it, and its
    /// `--session-id` names a conversation that now exists.
    #[test]
    fn a_resumed_line_keeps_their_arguments_and_replaces_ours() {
        let saved = "/opt/homebrew/bin/claude --mcp-config {\"mcpServers\":{\"dmac\":{}}} \
                     --session-id aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee --model opus \
                     --permission-mode plan";
        let line = resume_command("3", "ffffffff-0000-4000-8000-000000000000", saved);

        assert!(line.starts_with(attached_program()), "{line}");
        assert!(line.contains("--model opus"), "{line}");
        assert!(line.contains("--permission-mode plan"), "{line}");
        assert!(
            !line.contains("mcpServers"),
            "the dead run's description came back: {line}"
        );
        assert!(
            !line.contains("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"),
            "the old conversation id is on the line: {line}"
        );
        assert_eq!(line.matches("--session-id").count(), 1, "{line}");
    }

    /// Nothing this sets may change what a command resolves to. That was the
    /// shim's whole trouble, and the environment is what is left of it.
    #[test]
    fn the_environment_never_touches_the_path() {
        let env = environment("3", "work", "11111111-2222-3333-4444-555555555555");
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

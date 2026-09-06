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
    env
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

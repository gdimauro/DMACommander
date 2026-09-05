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
}];

struct Attach {
    program: &'static str,
    explicit: &'static [&'static str],
    new_flag: &'static str,
    resume_flag: &'static str,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not write the agent shim: {0}")]
    Io(#[from] std::io::Error),
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

/// The same command line, but resuming rather than creating.
///
/// A saved command line names the conversation it *created*; running it again
/// verbatim asks for a conversation that already exists, which is exactly the
/// "already in use" the user hits. Everything else — the model, the permission
/// mode, whatever else they chose — is left alone, because it is part of what
/// they set up.
pub fn as_resume(line: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut words = line.split_whitespace().peekable();
    while let Some(w) = words.next() {
        match w {
            "--session-id" => {
                out.push("--resume".to_string());
                if let Some(id) = words.next() {
                    out.push(id.to_string());
                }
            }
            _ if w.starts_with("--session-id=") => {
                out.push(w.replacen("--session-id=", "--resume=", 1));
            }
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

/// The shim directory for one session, created if needed.
///
/// Returns `None` when there is nothing to attach — no `claude` on `PATH` means
/// no shim, rather than a script that shadows a program the user might install
/// later and then fails to find it.
pub fn prepare(root: &Path, session_id: &str, conversation: &str) -> Option<PathBuf> {
    let dir = root.join("shims").join(session_id);
    let mut wrote_any = false;
    for a in ATTACHED {
        let Some(real) = resolve(a.program, &dir) else {
            continue;
        };
        let marker = dir.join(format!("{}.started", a.program));
        let script = shim_script(a, &real, conversation, &marker);
        if write_executable(&dir.join(a.program), &script).is_ok() {
            wrote_any = true;
        }
    }
    wrote_any.then_some(dir)
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
    vec![
        ("PATH".to_string(), path),
        ("DMAC_SESSION".to_string(), session_name.to_string()),
        ("DMAC_CONVERSATION".to_string(), conversation.to_string()),
    ]
}

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
fn shim_script(a: &Attach, real: &Path, conversation: &str, marker: &Path) -> String {
    let (real, marker) = (real.display(), marker.display());
    let (program, new_flag, resume_flag) = (a.program, a.new_flag, a.resume_flag);
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
# was. Delete this file's marker to start a fresh conversation:
#   rm '{marker}'
#
# An explicit choice on the command line always wins; this only fills in a gap.
for arg in "$@"; do
  case "$arg" in
    {explicit}) exec '{real}' "$@" ;;
  esac
done
if [ -e '{marker}' ]; then
  exec '{real}' {resume_flag} '{conversation}' "$@"
fi
: > '{marker}'
exec '{real}' {new_flag} '{conversation}' "$@"
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

    #[test]
    fn the_shim_adds_the_conversation_on_a_first_run_and_resumes_after() {
        let s = shim_script(
            claude(),
            Path::new("/usr/local/bin/claude"),
            "11111111-2222-4333-8444-555555555555",
            Path::new("/tmp/shims/s1/claude.started"),
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
        );
        for flag in claude().explicit {
            assert!(s.contains(&format!("{flag}|{flag}=*")), "{flag} missing");
        }
    }

    /// `exec`, so the shim leaves nothing of its own between the shell and the
    /// agent — otherwise signals and job control read differently through it.
    #[test]
    fn the_shim_execs_rather_than_calling() {
        let s = shim_script(claude(), Path::new("/bin/true"), "abc", Path::new("/tmp/m"));
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
        );
        assert!(s.contains("'/opt/my tools/claude'"), "{s}");
        assert!(s.contains("'/tmp/my markers/m'"), "{s}");
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
    fn replaying_a_command_line_resumes_instead_of_creating() {
        assert_eq!(
            as_resume("claude --session-id 1234 --model opus"),
            "claude --resume 1234 --model opus"
        );
        assert_eq!(
            as_resume("claude --session-id=1234"),
            "claude --resume=1234"
        );
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

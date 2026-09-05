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
}

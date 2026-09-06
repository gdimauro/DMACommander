//! DMACommander: a next-generation orthodox file manager.
//!
//! The binary is deliberately thin: parse arguments, resolve which session to
//! open, hand control to the UI. Everything else lives in a crate with an owner
//! (see `.claude/agents/`).

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
use anyhow::Result;
use clap::Parser;
use dmac_vfs::VfsPath;
use std::path::{Path, PathBuf};

#[derive(Parser, Debug)]
#[command(
    name = "dmac",
    version = dmac_config::build_info::VERSION,
    long_version = dmac_config::build_info::clap_long_version(),
    about = "DMACommander — an orthodox file manager with a VFS, agents and a dock",
    long_about = None
)]
struct Cli {
    /// Open a named session, restoring its panels, tabs, windows and attached
    /// agent sessions. Highest-priority source of the session choice.
    #[arg(short = 's', long, value_name = "NAME")]
    session: Option<String>,

    /// Skip session resolution entirely and start a throwaway workspace.
    #[arg(long, conflicts_with = "session")]
    no_session: bool,

    /// Starting directory for the left panel. Defaults to the current directory.
    #[arg(value_name = "LEFT")]
    left: Option<PathBuf>,

    /// Starting directory for the right panel. Defaults to the home directory.
    #[arg(value_name = "RIGHT")]
    right: Option<PathBuf>,

    /// Which screensaver to run: an effect name, `random` for a different one
    /// each time, or `rotation` to cycle through them in order.
    #[arg(long, value_name = "NAME", default_value = "random")]
    screensaver: String,

    /// Idle seconds before the screensaver starts. `0` disables it entirely —
    /// and disabled means no timer at all, not a timer that never fires.
    #[arg(long, value_name = "SECS", default_value_t = 300)]
    screensaver_after: u64,

    /// Print the available screensavers and exit.
    #[arg(long)]
    list_screensavers: bool,

    /// Skip the startup splash.
    #[arg(long)]
    no_splash: bool,

    /// Text cursor on the command line. Use `software` when your terminal
    /// ignores the cursor shape the application asks for — DMACommander then
    /// draws and blinks the cursor itself, which works everywhere.
    #[arg(long, value_name = "STYLE", default_value = "blinking-block")]
    cursor: String,

    /// Print the full build identity and exit. The first thing to paste into a
    /// bug report.
    #[arg(long)]
    build_info: bool,

    /// Speak Model Context Protocol on stdin and stdout, relaying to the
    /// DMACommander listening on this socket.
    ///
    /// Not a mode you run by hand: an MCP client starts its servers itself, as
    /// child processes with pipes, and this is the few lines of pipe that puts
    /// one of them in touch with the commander already on screen. DMACommander
    /// writes the flag into the configuration it hands a hosted agent.
    #[arg(long, value_name = "SOCKET")]
    mcp: Option<PathBuf>,

    /// Which session an `--mcp` bridge belongs to, so its tools answer about
    /// that session's panels rather than whichever one is on screen.
    #[arg(long, value_name = "ID", requires = "mcp")]
    mcp_session: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Before anything else, and before the terminal is touched: this process is
    // a pipe, not a file manager. Any stray byte on stdout would be read by the
    // client as a malformed protocol message.
    if let Some(socket) = &cli.mcp {
        return dmac_mcp::bridge::run(socket, cli.mcp_session.as_deref()).map_err(Into::into);
    }

    let Some(cursor) = dmac_tui::terminal::CursorStyle::parse(&cli.cursor) else {
        anyhow::bail!(
            "unknown cursor style {:?}; available: {}",
            cli.cursor,
            dmac_tui::terminal::CursorStyle::NAMES.join(", ")
        );
    };

    if cli.build_info {
        print!("{}", dmac_config::build_info::long());
        return Ok(());
    }

    if cli.list_screensavers {
        println!("{:<11} a different one every time", "random");
        println!("{:<11} each one in turn", "rotation");
        for e in dmac_fx::catalog() {
            let kind = match e.kind {
                dmac_fx::Kind::Game => "  [game]",
                dmac_fx::Kind::Demo => "  [demo]",
                dmac_fx::Kind::Screensaver => "",
            };
            println!("{:<11} {}{}", e.name, e.blurb, kind);
        }
        return Ok(());
    }

    // A typo here should be reported now, not silently swapped for something
    // else five minutes into an idle period.
    let is_mode = matches!(cli.screensaver.as_str(), "random" | "rotation");
    if !is_mode && dmac_fx::build(&cli.screensaver).is_none() {
        anyhow::bail!(
            "unknown screensaver {:?}; available: random, rotation, {}",
            cli.screensaver,
            dmac_fx::catalog()
                .iter()
                .map(|e| e.name)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let screensaver = dmac_fx::ScreensaverConfig {
        enabled: cli.screensaver_after > 0,
        idle: std::time::Duration::from_secs(cli.screensaver_after),
        effect: cli.screensaver.clone(),
        ..Default::default()
    };

    // Resolution order is a contract (see `.claude/agents/session-engineer.md`):
    // --session, then $DMAC_SESSION, then a `.dmac-session` file walking up
    // from the cwd, then auto-resume, then the picker. Only the first two steps
    // exist today; the rest is the session-engineer's work.
    let session = if cli.no_session {
        None
    } else {
        cli.session
            .clone()
            .or_else(|| std::env::var("DMAC_SESSION").ok())
    };

    let cwd = std::env::current_dir()?;
    let left = start_dir(cli.left, &cwd, cwd.clone());
    let right = start_dir(cli.right, &cwd, home_dir().unwrap_or_else(|| cwd.clone()));

    // Multi-threaded on purpose: directory walks, hashing and network VFS all
    // want to run while the UI keeps drawing.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // Several sessions can be live at once; this names the first one. Persisting
    // them across runs is still to come, so a named session that does not exist
    // yet is simply created rather than restored.
    let session_name = session.unwrap_or_else(|| "main".to_string());

    // Sessions live in one file, written atomically. `--no-session` opts out
    // entirely rather than writing to a throwaway location: "do not persist"
    // should mean exactly that.
    let (store, restored) = if cli.no_session {
        (None, None)
    } else {
        match dmac_session::store::SessionStore::platform_default() {
            Ok(store) => match store.load() {
                Ok(found) => (Some(store), found),
                Err(e) => {
                    // Never let a bad file block startup: keep it, say where it
                    // went, and carry on with a fresh set.
                    eprintln!("dmac: {e}");
                    if let Some(backup) = store.quarantine() {
                        eprintln!("dmac: moved it to {}", backup.display());
                    }
                    (Some(store), None)
                }
            },
            Err(e) => {
                eprintln!("dmac: sessions will not be saved: {e}");
                (None, None)
            }
        }
    };

    runtime.block_on(async move {
        dmac_tui::run(dmac_tui::app::Startup {
            session_name,
            left,
            right,
            screensaver,
            splash: !cli.no_splash,
            cursor,
            store,
            restored,
        })
        .await
    })
}

/// Resolve a starting directory to an absolute path.
///
/// `dmac docs` must behave the same as `dmac /full/path/to/docs`. A relative
/// path has no parent that `Path` will admit to, so a panel opened on one had no
/// `..` row and no way to navigate upwards — you could go down and never come
/// back. Absolute from the start avoids the whole class of problem, and it is
/// what a file manager should be showing anyway.
fn start_dir(arg: Option<PathBuf>, cwd: &Path, fallback: PathBuf) -> VfsPath {
    let raw = arg.unwrap_or(fallback);
    let joined = if raw.is_absolute() {
        raw
    } else {
        cwd.join(raw)
    };
    // Canonicalise where possible, so `.` and `..` in the argument are resolved
    // and symlinks are followed once, up front. A path that does not exist is
    // left as given: the listing will report why, which is more useful than
    // refusing to start.
    VfsPath::local(joined.canonicalize().unwrap_or(joined))
}

/// The user's home directory, without pulling in a dependency for one lookup.
/// `dmac-config` will replace this with `directories` once it exists, so the
/// same logic serves config, cache and session paths on all three platforms.
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

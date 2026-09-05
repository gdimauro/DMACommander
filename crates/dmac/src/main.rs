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
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "dmac",
    version,
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list_screensavers {
        println!("{:<11} a different one every time", "random");
        println!("{:<11} each one in turn", "rotation");
        for e in dmac_fx::catalog() {
            let kind = match e.kind {
                dmac_fx::Kind::Game => "  [game]",
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
    let left = VfsPath::local(cli.left.unwrap_or_else(|| cwd.clone()));
    let right = VfsPath::local(cli.right.unwrap_or_else(|| home_dir().unwrap_or(cwd)));

    // Multi-threaded on purpose: directory walks, hashing and network VFS all
    // want to run while the UI keeps drawing.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        if let Some(name) = &session {
            // Sessions are not persisted yet; say so rather than pretending the
            // flag worked and silently losing the user's workspace.
            eprintln!("session {name:?} requested — persistence is not implemented yet");
        }
        dmac_tui::run(left, right, screensaver).await
    })
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

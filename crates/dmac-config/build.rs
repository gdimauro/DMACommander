//! Capture build identity at compile time.
//!
//! Everything here lands in `cargo:rustc-env` variables that `build_info.rs`
//! reads with `env!`, so the values are baked into the binary as `&'static str`
//! with no runtime cost and no files to ship alongside.
//!
//! Every value degrades to something honest when it cannot be determined — a
//! source tarball with no `.git` still builds, and says `unknown` rather than
//! inventing a commit.

use std::process::Command;

fn main() {
    // Regenerate when the checkout moves. Deliberately *not* on every build:
    // this crate sits at the bottom of the layering, so invalidating it rebuilds
    // the whole workspace, and paying that on every `cargo build` to refresh a
    // timestamp is a bad trade. See docs/VERSIONING.md.
    for path in [
        ".git/HEAD",
        ".git/index",
        "../../.git/HEAD",
        "../../.git/index",
    ] {
        if std::path::Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    println!("cargo:rerun-if-changed=build.rs");

    // The commit count is a monotonic build number: it only ever goes up, it is
    // the same for everyone who has the same history, and it needs no state file.
    emit("DMAC_BUILD_NUMBER", git(&["rev-list", "--count", "HEAD"]));
    emit("DMAC_GIT_SHA", git(&["rev-parse", "--short=7", "HEAD"]));
    emit(
        "DMAC_GIT_BRANCH",
        git(&["rev-parse", "--abbrev-ref", "HEAD"]),
    );

    // A dirty build is one whose source does not match any commit. Saying so is
    // the difference between a bug report we can reproduce and one we cannot.
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    println!("cargo:rustc-env=DMAC_GIT_DIRTY={dirty}");

    println!("cargo:rustc-env=DMAC_BUILT_AT={}", now_utc());
    emit("DMAC_RUSTC", rustc_version());

    // Cargo hands us these for free.
    println!(
        "cargo:rustc-env=DMAC_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".into())
    );
    println!(
        "cargo:rustc-env=DMAC_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".into())
    );
}

fn emit(key: &str, value: Option<String>) {
    println!(
        "cargo:rustc-env={key}={}",
        value.unwrap_or_else(|| "unknown".into())
    );
}

/// Run a git command, returning `None` if git is missing, this is not a
/// repository, or the command failed. Never panics: a build must not depend on
/// git being installed.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

fn rustc_version() -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let out = Command::new(rustc).arg("--version").output().ok()?;
    let s = String::from_utf8(out.stdout).ok()?;
    // "rustc 1.96.0 (ac68faa20 2026-05-25)" -> "1.96.0"
    s.split_whitespace().nth(1).map(str::to_string)
}

/// `YYYY-MM-DD HH:MM UTC`. Minute resolution: the second a build started is
/// noise, and a shorter string keeps the splash box narrow.
fn now_utc() -> String {
    let ts = jiff::Timestamp::now();
    let z = ts.to_zoned(jiff::tz::TimeZone::UTC);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        z.year(),
        z.month(),
        z.day(),
        z.hour(),
        z.minute()
    )
}

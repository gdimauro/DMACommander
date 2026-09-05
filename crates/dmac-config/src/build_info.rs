//! Who this binary is.
//!
//! Baked in by `build.rs`, so every field is a `&'static str` costing nothing at
//! runtime. Shown on the splash screen, by `--version`, and in the settings
//! screen — and it is the first thing to ask for in a bug report.

/// The semantic version from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Commits in the history at build time. Monotonic, shared by everyone with the
/// same history, and needs no state file — which is why it beats a counter.
pub const BUILD_NUMBER: &str = env!("DMAC_BUILD_NUMBER");

pub const GIT_SHA: &str = env!("DMAC_GIT_SHA");
pub const GIT_BRANCH: &str = env!("DMAC_GIT_BRANCH");

/// Whether tracked files differed from `HEAD` when this was built. A `true` here
/// means the commit does not describe the source, so a bug report quoting the
/// SHA alone would be misleading.
pub const GIT_DIRTY: bool = matches!(env!("DMAC_GIT_DIRTY").as_bytes(), b"true");

pub const BUILT_AT: &str = env!("DMAC_BUILT_AT");
pub const RUSTC: &str = env!("DMAC_RUSTC");
pub const TARGET: &str = env!("DMAC_TARGET");
pub const PROFILE: &str = env!("DMAC_PROFILE");

/// The product name as people should see it written.
pub const NAME: &str = "DMACommander";

/// One line, for a status bar or a log header.
///
/// `0.1.0 (build 3, 92640a3)` — or with a `+` when the tree was dirty.
pub fn short() -> String {
    format!(
        "{VERSION} (build {BUILD_NUMBER}, {GIT_SHA}{})",
        if GIT_DIRTY { "+" } else { "" }
    )
}

/// The full identity, as `--version` prints it and the splash shows it.
///
/// Ordered by how often someone actually needs each line: version first, then
/// what commit it came from, then the environment it was built in.
pub fn rows() -> Vec<(&'static str, String)> {
    let mut v = vec![
        ("version", VERSION.to_string()),
        (
            "build",
            format!(
                "{BUILD_NUMBER} · {GIT_SHA}{} ({GIT_BRANCH})",
                if GIT_DIRTY { " dirty" } else { "" }
            ),
        ),
        ("compiled", BUILT_AT.to_string()),
        ("rustc", RUSTC.to_string()),
        ("target", TARGET.to_string()),
    ];
    // The profile is only worth a line when it is not the one people expect from
    // a shipped binary.
    if PROFILE != "release" {
        v.push(("profile", PROFILE.to_string()));
    }
    v
}

/// Multi-line block for `--version` and for pasting into a bug report.
pub fn long() -> String {
    let mut s = format!("{NAME} {VERSION}\n");
    for (k, v) in rows().into_iter().skip(1) {
        s.push_str(&format!("{k:<9} {v}\n"));
    }
    s
}

/// The block clap should print after the binary name for `--version`.
///
/// Starts with the bare version rather than the product name, because clap
/// already prints the binary name in front of it — including it here yields
/// "dmac DMACommander 0.1.0".
pub fn clap_long_version() -> &'static str {
    static V: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let mut s = format!("{VERSION}\n");
        for (k, v) in rows().into_iter().skip(1) {
            s.push_str(&format!("{k:<9} {v}\n"));
        }
        s
    });
    &V
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field must be populated. A build that quietly emits `unknown`
    /// everywhere produces bug reports nobody can act on.
    #[test]
    fn build_metadata_is_present() {
        assert!(!VERSION.is_empty());
        assert!(!BUILT_AT.is_empty());
        assert_ne!(
            BUILT_AT, "unknown",
            "the build timestamp must always be real"
        );
        assert_ne!(TARGET, "unknown", "cargo always provides TARGET");
    }

    /// Built inside the repository, so git data must have been found. This is
    /// the test that catches a build.rs that silently stopped working.
    #[test]
    fn git_metadata_was_captured() {
        assert_ne!(GIT_SHA, "unknown", "git sha missing — did build.rs break?");
        assert_ne!(BUILD_NUMBER, "unknown");
        assert!(
            BUILD_NUMBER.parse::<u64>().is_ok(),
            "the build number must be a number, got {BUILD_NUMBER:?}"
        );
    }

    #[test]
    fn the_short_form_carries_version_and_commit() {
        let s = short();
        assert!(s.contains(VERSION));
        assert!(s.contains(GIT_SHA));
    }

    #[test]
    fn a_dirty_tree_is_marked_so_a_sha_is_never_misleading() {
        if GIT_DIRTY {
            assert!(short().contains('+'));
            assert!(long().contains("dirty"));
        }
    }

    #[test]
    fn the_long_form_names_the_product() {
        assert!(long().starts_with(NAME));
    }

    /// clap prints the binary name itself, so ours must not repeat it — that is
    /// how you get "dmac DMACommander 0.1.0".
    #[test]
    fn the_clap_version_does_not_repeat_the_product_name() {
        assert!(clap_long_version().starts_with(VERSION));
        assert!(!clap_long_version().contains(NAME));
    }
}

//! Configuration, keymaps and themes for DMACommander.
//!
//! Owned by the `dmac-config` agent (see `.claude/agents/`).
//!
//! This is the bottom of the stack: nothing of ours is below it, so anything
//! here can be used by everything. That is why [`shell_quote`] lives here
//! rather than somewhere more obvious — see its own note.

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod build_info;
pub mod menu;

/// Wrap a string so a shell reads it as exactly one argument, whatever is in it.
///
/// Single quotes, with `'` written as `'\''` — the only quoting a POSIX shell
/// treats as entirely literal. Inside single quotes there is no expansion, no
/// splitting, no globbing and no command substitution, so `$(rm -rf ~)` is four
/// words of text and nothing else.
///
/// # Why it is here
///
/// It is the same function as `dmac_core::tools::shell_quote`, and it is here
/// because [`menu::expand`] must not be able to skip it, and `dmac-config` sits
/// below `dmac-core` — so it cannot reach the other one. **Two implementations
/// of quoting is one too many**, and the intent is that `dmac-core`'s becomes a
/// re-export of this rather than a second copy. That has not happened yet only
/// because that crate's manifest is being edited elsewhere as this is written;
/// the test below pins the exact output so the two cannot drift apart in the
/// meantime.
pub fn shell_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned exactly, because a second copy of this function exists in
    /// `dmac-core` until they are merged, and "quoting that is nearly the same"
    /// is how one of them ends up wrong.
    #[test]
    fn quoting_is_literal_and_survives_everything() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("a b.txt"), "'a b.txt'");
        assert_eq!(shell_quote("O'Brien"), r"'O'\''Brien'");
        assert_eq!(shell_quote("$(rm -rf ~)"), "'$(rm -rf ~)'");
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
        assert_eq!(shell_quote("`whoami`"), "'`whoami`'");
        assert_eq!(shell_quote(""), "''", "an empty argument is still one");
    }
}

//! Completing what is being typed on the command line.
//!
//! Pure: it is handed a word and a list of candidates and decides what the line
//! should become. Finding the candidates means touching the filesystem, which
//! happens off the UI thread — this module never does, so it can be tested
//! exhaustively without one.

/// Where the word under completion starts, and the word itself.
///
/// Splits on unescaped whitespace, so `ls /tmp/my\ file` completes the path and
/// not just `file`. Quotes are deliberately not parsed: half-understood quoting
/// is worse than none, because it completes confidently in the wrong place.
pub fn word_at_end(line: &str) -> (usize, &str) {
    let bytes = line.as_bytes();
    let mut start = line.len();
    while start > 0 {
        let i = start - 1;
        if bytes[i].is_ascii_whitespace() {
            // An escaped space is part of the word, not a separator.
            let escaped = i > 0 && bytes[i - 1] == b'\\';
            if !escaped {
                break;
            }
        }
        start = i;
    }
    (start, &line[start..])
}

/// Whether the word being completed is the command itself rather than an
/// argument. Only then are the executables on `PATH` worth offering.
pub fn is_first_word(line: &str, start: usize) -> bool {
    line[..start].trim().is_empty()
}

/// The longest string every candidate starts with.
pub fn common_prefix(items: &[String]) -> String {
    let Some(first) = items.first() else {
        return String::new();
    };
    let mut end = first.len();
    for other in &items[1..] {
        end = end.min(
            first
                .char_indices()
                .zip(other.char_indices())
                .take_while(|((_, a), (_, b))| a == b)
                .last()
                .map_or(0, |((i, c), _)| i + c.len_utf8()),
        );
    }
    first[..end].to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Nothing matched: the line is left exactly as it was.
    None,
    /// One candidate, or a prefix that is already the whole of one.
    Single(String),
    /// Several. `prefix` is as far as the word can be extended without
    /// choosing, which may be no further than it already is.
    Many { prefix: String, items: Vec<String> },
}

/// Decide what `word` should become, given everything it could become.
///
/// `candidates` are the full replacement words, already filtered to those
/// starting with `word`.
pub fn resolve(word: &str, mut candidates: Vec<String>) -> Completion {
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => Completion::None,
        1 => Completion::Single(candidates.remove(0)),
        _ => {
            let prefix = common_prefix(&candidates);
            // The common prefix can be shorter than what is typed when the
            // match was case-insensitive; never shorten the user's own text.
            let prefix = if prefix.len() > word.len() {
                prefix
            } else {
                word.to_string()
            };
            Completion::Many {
                prefix,
                items: candidates,
            }
        }
    }
}

/// Escape a candidate so a shell reads it as one word.
///
/// Backslashes rather than quotes, because the result is spliced into a line
/// the user is still editing and may already have opened a quote of their own.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if " \t\n'\"\\$`&|;<>()*?[]{}!#~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Undo [`escape`]: what the user typed, as the filesystem spells it.
pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splice `replacement` in place of the word starting at `start`.
pub fn splice(line: &str, start: usize, replacement: &str) -> String {
    let mut out = String::with_capacity(start + replacement.len());
    out.push_str(&line[..start]);
    out.push_str(replacement);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_is_whatever_follows_the_last_space() {
        assert_eq!(word_at_end(""), (0, ""));
        assert_eq!(word_at_end("ls"), (0, "ls"));
        assert_eq!(word_at_end("ls "), (3, ""));
        assert_eq!(word_at_end("ls /tm"), (3, "/tm"));
        assert_eq!(word_at_end("  ls   /tm"), (7, "/tm"));
    }

    /// `ls /tmp/my\ fi` is completing a path with a space in it, not `fi`.
    #[test]
    fn an_escaped_space_belongs_to_the_word() {
        assert_eq!(word_at_end(r"ls /tmp/my\ fi"), (3, r"/tmp/my\ fi"));
        assert_eq!(word_at_end(r"cp a\ b c\ d"), (8, r"c\ d"));
    }

    #[test]
    fn only_the_first_word_is_a_command() {
        assert!(is_first_word("ls", 0));
        assert!(is_first_word("   ls", 3));
        assert!(!is_first_word("ls /tm", 3));
        assert!(!is_first_word("ls ", 3));
    }

    #[test]
    fn the_common_prefix_is_as_far_as_completion_can_go() {
        let items = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(common_prefix(&items(&["cargo", "cat", "cd"])), "c");
        assert_eq!(common_prefix(&items(&["config", "confuse"])), "conf");
        assert_eq!(common_prefix(&items(&["config", "confirm"])), "confi");
        assert_eq!(common_prefix(&items(&["same", "same"])), "same");
        assert_eq!(common_prefix(&items(&["a", "b"])), "");
        assert_eq!(common_prefix(&[]), "");
    }

    /// Multi-byte characters must not be split down the middle.
    #[test]
    fn the_common_prefix_respects_character_boundaries() {
        let items = vec!["日本語".to_string(), "日本人".to_string()];
        assert_eq!(common_prefix(&items), "日本");
        let mixed = vec!["café-one".to_string(), "café-two".to_string()];
        assert_eq!(common_prefix(&mixed), "café-");
    }

    #[test]
    fn one_candidate_completes_and_none_leaves_it_alone() {
        assert_eq!(
            resolve("ca", vec!["cargo".into()]),
            Completion::Single("cargo".into())
        );
        assert_eq!(resolve("zzz", vec![]), Completion::None);
    }

    #[test]
    fn several_candidates_extend_as_far_as_they_agree() {
        let c = resolve("c", vec!["config".into(), "confuse".into()]);
        assert_eq!(
            c,
            Completion::Many {
                prefix: "conf".into(),
                items: vec!["config".into(), "confuse".into()],
            }
        );
    }

    /// Completion must never take away what the user typed.
    #[test]
    fn a_shorter_common_prefix_never_shortens_the_line() {
        let c = resolve("READ", vec!["README".into(), "READY".into()]);
        match c {
            Completion::Many { prefix, .. } => assert!(prefix.starts_with("READ"), "{prefix}"),
            other => panic!("expected several, got {other:?}"),
        }
    }

    #[test]
    fn duplicates_do_not_make_a_unique_match_ambiguous() {
        assert_eq!(
            resolve("s", vec!["src".into(), "src".into()]),
            Completion::Single("src".into())
        );
    }

    #[test]
    fn escaping_makes_a_name_survive_the_shell() {
        assert_eq!(escape("plain"), "plain");
        assert_eq!(escape("with space"), r"with\ space");
        assert_eq!(escape("it's"), r"it\'s");
        assert_eq!(escape("a;b"), r"a\;b");
        assert_eq!(escape("$(x)"), r"\$\(x\)");
    }

    #[test]
    fn escaping_round_trips() {
        for s in ["plain", "with space", "it's", "a;b", "$(x)", r"back\\slash"] {
            assert_eq!(unescape(&escape(s)), s, "{s:?}");
        }
    }

    #[test]
    fn splicing_replaces_only_the_word() {
        assert_eq!(splice("ls /tm", 3, "/tmp/"), "ls /tmp/");
        assert_eq!(splice("ca", 0, "cargo"), "cargo");
        assert_eq!(splice("cp a b", 5, "build/"), "cp a build/");
    }
}

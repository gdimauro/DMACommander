//! Subsequence matching, scored the way an editor's quick-open scores it.
//!
//! Typing `dtu` should find `crates/dmac-tui` and rank it above something that
//! merely contains those three letters, far apart, in the middle of words. Two
//! bonuses do that work: one for a character that starts a word, and one for a
//! character that follows the previous match. A run beats a scatter, and a
//! boundary beats a middle.
//!
//! The shape is fzf's dynamic programme, `O(needle × haystack)`, rather than the
//! exponential "try every alignment" a naive search would be. That is what lets
//! it run over a whole history on every keystroke.

/// A match, and where it landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub score: i32,
    /// Character positions in the haystack, ascending. What to highlight.
    pub positions: Vec<usize>,
}

const MATCHED: i32 = 16;
/// Per character skipped. Small, but enough that of two equal matches the
/// earlier and tighter one wins.
const GAP: i32 = -1;
const CONSECUTIVE: i32 = 8;
const BOUNDARY: i32 = 10;
const FIRST: i32 = 6;
/// Not `i32::MIN`: scores are added to, and this must stay far from overflow.
const NONE: i32 = i32::MIN / 4;

/// One character, case-folded, staying one character.
///
/// `char::to_lowercase` can yield several — folding through it would shift every
/// index after it and highlight the wrong letters.
fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Score `needle` against `haystack`, or `None` if it is not a subsequence.
///
/// An empty needle matches everything with score zero, which is what lets a
/// filter box show the unfiltered list before anything is typed.
pub fn score(needle: &str, haystack: &str) -> Option<Match> {
    let n: Vec<char> = needle.chars().map(fold).collect();
    if n.is_empty() {
        return Some(Match {
            score: 0,
            positions: Vec::new(),
        });
    }
    let h: Vec<char> = haystack.chars().collect();
    if n.len() > h.len() {
        return None;
    }
    let folded: Vec<char> = h.iter().copied().map(fold).collect();

    // Position bonuses, computed once: they depend on the haystack alone.
    let bonus: Vec<i32> = (0..h.len())
        .map(|j| {
            if j == 0 {
                return BOUNDARY + FIRST;
            }
            let prev = h[j - 1];
            if matches!(prev, '/' | '\\' | '_' | '-' | '.' | ' ' | ':' | '@') {
                BOUNDARY
            } else if !prev.is_uppercase() && h[j].is_uppercase() {
                BOUNDARY - 2
            } else if !prev.is_alphanumeric() {
                BOUNDARY - 4
            } else {
                0
            }
        })
        .collect();

    let (rows, cols) = (n.len(), h.len());
    // `m`: the best score with needle[i] matched *at* haystack[j].
    // `d`: the best score for needle[..=i] anywhere within haystack[..=j].
    let mut m = vec![NONE; rows * cols];
    let mut d = vec![NONE; rows * cols];
    let mut m_from = vec![usize::MAX; rows * cols];
    let mut d_from = vec![usize::MAX; rows * cols];

    for (i, needle_char) in n.iter().enumerate() {
        for j in 0..cols {
            let at = i * cols + j;
            if *needle_char == folded[j] {
                let base = MATCHED + bonus[j];
                if i == 0 {
                    // A leading gap costs, so an early match wins a tie.
                    m[at] = base + GAP * j as i32;
                } else if j > 0 {
                    let prev = (i - 1) * cols + (j - 1);
                    let run = if m[prev] > NONE {
                        m[prev] + base + CONSECUTIVE
                    } else {
                        NONE
                    };
                    let apart = if d[prev] > NONE { d[prev] + base } else { NONE };
                    if run >= apart {
                        m[at] = run;
                        m_from[at] = j - 1;
                    } else {
                        m[at] = apart;
                        m_from[at] = d_from[prev];
                    }
                }
            }
            // Carry the best-so-far along the row, paying for the gap.
            let carried = if j > 0 && d[at - 1] > NONE {
                d[at - 1] + GAP
            } else {
                NONE
            };
            if m[at] >= carried {
                d[at] = m[at];
                d_from[at] = j;
            } else {
                d[at] = carried;
                d_from[at] = d_from[at - 1];
            }
        }
    }

    // Where the last needle character landed, best case.
    let last = (rows - 1) * cols;
    let (end, best) = (0..cols)
        .map(|j| (j, m[last + j]))
        .max_by_key(|&(j, s)| (s, std::cmp::Reverse(j)))?;
    if best <= NONE {
        return None;
    }

    let mut positions = vec![0usize; rows];
    let mut j = end;
    for i in (0..rows).rev() {
        positions[i] = j;
        if i > 0 {
            j = m_from[i * cols + j];
            if j == usize::MAX {
                return None;
            }
        }
    }
    Some(Match {
        score: best,
        positions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(needle: &str, hay: &str) -> i32 {
        score(needle, hay).expect("should match").score
    }

    #[test]
    fn an_empty_needle_matches_anything() {
        let m = score("", "whatever").unwrap();
        assert_eq!(m.score, 0);
        assert!(m.positions.is_empty());
    }

    #[test]
    fn a_non_subsequence_does_not_match() {
        assert!(score("xyz", "abc").is_none());
        assert!(score("cba", "abc").is_none(), "order matters");
        assert!(score("abcd", "abc").is_none(), "too long to fit");
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert!(score("DMAC", "dmac-tui").is_some());
        assert!(score("dmac", "DMAC-TUI").is_some());
    }

    #[test]
    fn positions_point_at_the_characters_that_matched() {
        let m = score("dt", "dmac-tui").unwrap();
        assert_eq!(m.positions, vec![0, 5], "the d and the t");
    }

    /// The property the whole thing exists for: a run of characters at word
    /// boundaries must beat the same characters scattered mid-word.
    #[test]
    fn boundaries_and_runs_outrank_scatter() {
        assert!(
            s("dt", "dmac-tui") > s("dt", "abcdefghijklmnopqrstuvwxyz"),
            "two word starts beat two letters far apart"
        );
        assert!(
            s("dma", "dmac-core") > s("dma", "a-d-m-a-y"),
            "consecutive beats interrupted"
        );
    }

    /// Between two files with the same name, the shallower path wins, because
    /// the leading gap is smaller.
    #[test]
    fn an_earlier_match_beats_a_later_one() {
        assert!(s("core", "core/lib.rs") > s("core", "a/b/c/d/core/lib.rs"));
    }

    /// Path separators start a word: `tui` should find `dmac-tui/src` through
    /// the segment, not through stray letters earlier in the path.
    #[test]
    fn a_path_segment_is_a_word() {
        let m = score("tui", "crates/dmac-tui/src").unwrap();
        assert_eq!(m.positions, vec![12, 13, 14]);
    }

    /// Long inputs must not blow up: the DP is quadratic in area, and a runaway
    /// allocation on a long path would be felt on every keystroke.
    #[test]
    fn long_inputs_are_handled() {
        let hay = "a/".repeat(400);
        assert!(score("aaa", &hay).is_some());
        assert!(score(&"z".repeat(50), &hay).is_none());
    }
}

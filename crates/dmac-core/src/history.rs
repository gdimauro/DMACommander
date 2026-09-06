//! Where you have been.
//!
//! Every directory the panels land in is recorded, across every session and
//! across restarts. The list is worth having because it can be read three ways,
//! and the three answer different questions:
//!
//! * *Recent* — where was I just now? Everything, newest first.
//! * *Most used* — where do I always end up? Everything, by how often.
//! * *Session* — where has **this** session been? Only its own.
//!
//! The store keeps one row per directory *per session*, so switching the view to
//! "session" is a filter rather than a second history to keep in step.

use serde::{Deserialize, Serialize};

/// One directory, as seen by one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Visit {
    pub path: String,
    /// Which session went there. Not stable across restarts — sessions are
    /// identified on disk by name — but stable for as long as the list is used.
    pub session: u64,
    /// Unix seconds of the last visit.
    pub at: u64,
    pub hits: u32,
}

/// How to read the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Recent,
    Frequent,
    Session,
}

impl Order {
    pub const ALL: [Order; 3] = [Order::Recent, Order::Frequent, Order::Session];

    /// What the F-key bar calls it.
    pub fn label(self) -> &'static str {
        match self {
            Order::Recent => "Recent",
            Order::Frequent => "MostUsed",
            Order::Session => "Session",
        }
    }

    /// The heading, which has room for a sentence the bar does not.
    pub fn title(self) -> &'static str {
        match self {
            Order::Recent => "recent, newest first",
            Order::Frequent => "most used, everywhere",
            Order::Session => "this session only",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Order::Recent => 0,
            Order::Frequent => 1,
            Order::Session => 2,
        }
    }

    /// Cycle, so one key can reach all three without three keys.
    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }
}

/// A row as shown: one directory, with the totals for the chosen order already
/// folded in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub path: String,
    pub at: u64,
    pub hits: u32,
}

#[derive(Debug, Default, Clone)]
pub struct History {
    visits: Vec<Visit>,
}

impl History {
    /// Enough to cover months of work, small enough to load and sort without
    /// anyone noticing.
    pub const CAP: usize = 512;

    pub fn from_visits(visits: Vec<Visit>) -> Self {
        let mut this = Self { visits };
        this.trim();
        this
    }

    pub fn visits(&self) -> &[Visit] {
        &self.visits
    }

    pub fn is_empty(&self) -> bool {
        self.visits.is_empty()
    }

    /// Record a visit.
    ///
    /// One row per (path, session): going back to a directory bumps its clock
    /// and its count rather than filling the list with the same name.
    pub fn record(&mut self, path: &str, session: u64, at: u64) {
        if path.is_empty() {
            return;
        }
        if let Some(v) = self
            .visits
            .iter_mut()
            .find(|v| v.path == path && v.session == session)
        {
            v.at = at;
            v.hits = v.hits.saturating_add(1);
            return;
        }
        self.visits.push(Visit {
            path: path.to_string(),
            session,
            at,
            hits: 1,
        });
        self.trim();
    }

    /// Forget the least *recently* visited, not the first recorded: a list that
    /// evicted by insertion order would throw away the directory you have used
    /// every day since Tuesday.
    fn trim(&mut self) {
        while self.visits.len() > Self::CAP {
            let Some(oldest) = self
                .visits
                .iter()
                .enumerate()
                .min_by_key(|(i, v)| (v.at, *i))
                .map(|(i, _)| i)
            else {
                return;
            };
            self.visits.remove(oldest);
        }
    }

    /// The rows to show, in the order asked for.
    ///
    /// `Recent` and `Frequent` fold the per-session rows together, so a
    /// directory two sessions have both used appears once, with both their
    /// visits counted.
    pub fn view(&self, order: Order, session: u64) -> Vec<Row> {
        let mut rows: Vec<Row> = match order {
            Order::Session => self
                .visits
                .iter()
                .filter(|v| v.session == session)
                .map(|v| Row {
                    path: v.path.clone(),
                    at: v.at,
                    hits: v.hits,
                })
                .collect(),
            Order::Recent | Order::Frequent => {
                let mut folded: Vec<Row> = Vec::new();
                for v in &self.visits {
                    match folded.iter_mut().find(|r| r.path == v.path) {
                        Some(r) => {
                            r.at = r.at.max(v.at);
                            r.hits = r.hits.saturating_add(v.hits);
                        }
                        None => folded.push(Row {
                            path: v.path.clone(),
                            at: v.at,
                            hits: v.hits,
                        }),
                    }
                }
                folded
            }
        };

        // The path is the final tiebreak everywhere, so the same history always
        // produces the same list — a list that reshuffles under the cursor is
        // one you cannot use by muscle memory.
        match order {
            Order::Frequent => rows.sort_by(|a, b| {
                b.hits
                    .cmp(&a.hits)
                    .then(b.at.cmp(&a.at))
                    .then(a.path.cmp(&b.path))
            }),
            Order::Recent | Order::Session => {
                rows.sort_by(|a, b| b.at.cmp(&a.at).then(a.path.cmp(&b.path)))
            }
        }
        rows
    }
}

/// Now, in Unix seconds. Before the epoch is not a time this program runs at.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// "3m", "2h", "5d" — short enough for a column, long enough to mean something.
pub fn ago(at: u64, now: u64) -> String {
    let secs = now.saturating_sub(at);
    match secs {
        0..=59 => "now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        86_400..=2_591_999 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|r| r.path.as_str()).collect()
    }

    #[test]
    fn revisiting_bumps_rather_than_repeats() {
        let mut h = History::default();
        h.record("/a", 0, 100);
        h.record("/a", 0, 200);
        assert_eq!(h.visits().len(), 1, "one row, not two");
        assert_eq!(h.visits()[0].hits, 2);
        assert_eq!(h.visits()[0].at, 200, "the clock moves to the latest visit");
    }

    #[test]
    fn the_same_path_from_two_sessions_is_two_rows() {
        let mut h = History::default();
        h.record("/a", 0, 100);
        h.record("/a", 1, 110);
        assert_eq!(h.visits().len(), 2);
    }

    #[test]
    fn recent_folds_the_sessions_together_newest_first() {
        let mut h = History::default();
        h.record("/old", 0, 100);
        h.record("/new", 1, 300);
        h.record("/mid", 0, 200);
        assert_eq!(paths(&h.view(Order::Recent, 0)), ["/new", "/mid", "/old"]);
    }

    #[test]
    fn most_used_counts_every_session() {
        let mut h = History::default();
        for _ in 0..5 {
            h.record("/rare-but-recent", 0, 900);
        }
        for _ in 0..3 {
            h.record("/from-here", 0, 100);
        }
        for _ in 0..4 {
            h.record("/from-there", 1, 100);
        }
        // Seven visits from two sessions beat five from one.
        assert_eq!(
            h.view(Order::Frequent, 0)[0].path,
            "/rare-but-recent",
            "five is still five"
        );
        h.record("/from-here", 1, 100);
        h.record("/from-here", 1, 100);
        h.record("/from-here", 1, 100);
        assert_eq!(
            h.view(Order::Frequent, 0)[0].path,
            "/from-here",
            "three here plus three there beats five"
        );
    }

    #[test]
    fn the_session_view_shows_only_its_own() {
        let mut h = History::default();
        h.record("/mine", 7, 100);
        h.record("/theirs", 8, 200);
        assert_eq!(paths(&h.view(Order::Session, 7)), ["/mine"]);
    }

    /// Eviction drops what you stopped using, never what you use daily.
    #[test]
    fn the_cap_forgets_the_least_recent() {
        let mut h = History::default();
        h.record("/ancient", 0, 1);
        for i in 0..History::CAP {
            h.record(&format!("/p{i}"), 0, 1000 + i as u64);
        }
        assert_eq!(h.visits().len(), History::CAP);
        assert!(
            !h.visits().iter().any(|v| v.path == "/ancient"),
            "the oldest went first"
        );
    }

    #[test]
    fn an_empty_path_is_not_history() {
        let mut h = History::default();
        h.record("", 0, 100);
        assert!(h.is_empty());
    }

    #[test]
    fn ages_read_as_durations() {
        assert_eq!(ago(1000, 1000), "now");
        assert_eq!(ago(1000, 1000 + 120), "2m");
        assert_eq!(ago(1000, 1000 + 7200), "2h");
        assert_eq!(ago(1000, 1000 + 3 * 86_400), "3d");
        assert_eq!(ago(1000, 1000 + 30 * 86_400), "4w");
        assert_eq!(ago(2000, 1000), "now", "a clock that went backwards is now");
    }

    #[test]
    fn order_cycles_through_all_three() {
        let mut o = Order::Recent;
        for _ in 0..3 {
            o = o.next();
        }
        assert_eq!(o, Order::Recent);
    }
}

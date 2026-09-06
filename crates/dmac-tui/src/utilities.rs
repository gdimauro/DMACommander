//! The utilities menu: the small things you want halfway through typing a
//! command, without leaving the file manager to get them.
//!
//! Every entry either *produces* text, *reads* something the panels already
//! know, *transforms* what is on the command line, or names a deed for the
//! application to perform. Nothing here touches the filesystem or the network
//! — the entry that opens an editor returns the deed rather than doing it — so
//! the menu can never make a frame late.

use crate::ui::menu::Item;
use dmac_core::tools;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Utility {
    Uuid,
    TimestampIso,
    TimestampUnix,
    DateStamp,
    RandomToken,
    Password,
    ThisPath,
    OtherPath,
    SelectedNames,
    SelectedPaths,
    Base64Encode,
    Base64Decode,
    QuoteLine,
    EditorHere,
    AgentHere,
}

/// Something for the application to do. The utilities name it; performing it —
/// which means launching a program and waiting for its window — belongs to the
/// caller, on a thread that is not drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deed {
    /// Open the active panel's directory in the editor, beside the commander.
    OpenEditorHere,
    /// Start this session's agent in its shell, and show the shell.
    StartAgentHere,
}

/// What choosing an entry does to the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Add this to what is already typed.
    Insert(String),
    /// Replace the whole line — the transforms act on what is there.
    Replace(String),
    /// Nothing to do, and why.
    Nothing(&'static str),
    /// Not text at all: something for the application to go and do.
    Do(Deed),
}

impl Utility {
    /// The menu, in order. Separators group the three kinds so the list can be
    /// read at a glance rather than scanned.
    pub const MENU: &'static [Option<Self>] = &[
        Some(Self::Uuid),
        Some(Self::TimestampIso),
        Some(Self::TimestampUnix),
        Some(Self::DateStamp),
        None,
        Some(Self::RandomToken),
        Some(Self::Password),
        None,
        Some(Self::ThisPath),
        Some(Self::OtherPath),
        Some(Self::SelectedNames),
        Some(Self::SelectedPaths),
        None,
        Some(Self::Base64Encode),
        Some(Self::Base64Decode),
        Some(Self::QuoteLine),
        None,
        Some(Self::EditorHere),
        Some(Self::AgentHere),
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Uuid => "UUID v4",
            Self::TimestampIso => "Timestamp, ISO 8601",
            Self::TimestampUnix => "Timestamp, epoch seconds",
            Self::DateStamp => "Date stamp, yyyy-mm-dd",
            Self::RandomToken => "Random token, 32 hex",
            Self::Password => "Password, 20 characters",
            Self::ThisPath => "Path of this panel",
            Self::OtherPath => "Path of the other panel",
            Self::SelectedNames => "Selected names, quoted",
            Self::SelectedPaths => "Selected paths, quoted",
            Self::Base64Encode => "Base64-encode the line",
            Self::Base64Decode => "Base64-decode the line",
            Self::QuoteLine => "Shell-quote the line",
            Self::EditorHere => "Open this panel in the editor",
            Self::AgentHere => "Start claude in this session",
        }
    }

    /// The single key that picks this entry. Shown in the hint column, which is
    /// how a menu teaches its own shortcuts.
    pub fn key(self) -> char {
        match self {
            Self::Uuid => 'u',
            Self::TimestampIso => 't',
            Self::TimestampUnix => 'e',
            Self::DateStamp => 'y',
            Self::RandomToken => 'k',
            Self::Password => 'p',
            Self::ThisPath => '.',
            Self::OtherPath => ',',
            Self::SelectedNames => 'n',
            Self::SelectedPaths => 'f',
            Self::Base64Encode => 'b',
            Self::Base64Decode => 'd',
            Self::QuoteLine => 'q',
            Self::EditorHere => 'o',
            Self::AgentHere => 'c',
        }
    }

    pub fn hint(self) -> String {
        self.key().to_string()
    }

    /// Look an entry up by its accelerator.
    pub fn from_key(c: char) -> Option<Self> {
        Self::MENU.iter().flatten().copied().find(|u| u.key() == c)
    }

    /// The entry at a menu row, if that row is not a separator.
    pub fn at(row: usize) -> Option<Self> {
        Self::MENU.get(row).copied().flatten()
    }
}

/// One of the other live sessions, as the menu needs it: where to jump, and
/// what to call it.
#[derive(Debug, Clone)]
pub struct Elsewhere {
    /// Position in the session list — the number the rail shows, and the one
    /// `Alt`+digit already jumps to. Kept as the real index rather than the row
    /// number, so the digit in this menu and the digit everywhere else are the
    /// same digit.
    pub index: usize,
    pub name: String,
}

/// What a row of this menu means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Do(Utility),
    /// Switch to the session at this index.
    GoTo(usize),
}

/// The menu as the widget wants it: the fixed entries, then the sessions you
/// are not in.
///
/// The sessions are here because this is the menu people reach for. The rail
/// has had them all along, on a key that is one more thing to know — and a list
/// that exists in one place and not in the obvious one is a list nobody finds.
pub fn items(elsewhere: &[Elsewhere]) -> Vec<Item<'_>> {
    let mut items: Vec<Item<'_>> = Utility::MENU
        .iter()
        .map(|slot| match slot {
            None => Item::SEPARATOR,
            Some(u) => Item::new(u.label(), hint_of(*u)),
        })
        .collect();
    if !elsewhere.is_empty() {
        items.push(Item::SEPARATOR);
        items.extend(elsewhere.iter().map(|s| Item::new(&s.name, digit(s.index))));
    }
    items
}

/// The digit that jumps to a session, as a `&'static str` — the menu takes
/// borrowed text and there are only nine of them.
fn digit(index: usize) -> &'static str {
    const DIGITS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];
    DIGITS.get(index).copied().unwrap_or("")
}

/// What the row at `row` does, if it does anything.
pub fn at(row: usize, elsewhere: &[Elsewhere]) -> Option<Choice> {
    if row < Utility::MENU.len() {
        return Utility::at(row).map(Choice::Do);
    }
    // One separator between the two halves.
    let below = row.checked_sub(Utility::MENU.len() + 1)?;
    elsewhere.get(below).map(|s| Choice::GoTo(s.index))
}

/// The same, by the accelerator shown in the hint column. Letters are the
/// utilities; digits are the sessions, and they are the digits `Alt` already
/// answers to.
pub fn from_key(c: char, elsewhere: &[Elsewhere]) -> Option<Choice> {
    if let Some(d) = c.to_digit(10).filter(|d| *d > 0) {
        let index = d as usize - 1;
        return elsewhere
            .iter()
            .any(|s| s.index == index)
            .then_some(Choice::GoTo(index));
    }
    Utility::from_key(c).map(Choice::Do)
}

/// The accelerator as a `&'static str`, from a fixed table — the menu widget
/// takes static strings and there are thirteen possible values, so a table is
/// both simpler and cheaper than leaking a string per frame.
fn hint_of(u: Utility) -> &'static str {
    match u.key() {
        'u' => "u",
        't' => "t",
        'e' => "e",
        'y' => "y",
        'k' => "k",
        'p' => "p",
        '.' => ".",
        ',' => ",",
        'n' => "n",
        'f' => "f",
        'b' => "b",
        'd' => "d",
        'q' => "q",
        'o' => "o",
        'c' => "c",
        _ => "",
    }
}

/// Everything the utilities can need to know about where the user is.
pub struct Context<'a> {
    pub line: &'a str,
    /// Text the user has selected, which the transforms prefer over the command
    /// line. Selecting something and then having a menu act on something else
    /// is the kind of surprise that costs trust in the whole menu.
    pub selection: Option<&'a str>,
    pub this_path: String,
    pub other_path: String,
    /// Names of the selected entries, or of the one under the cursor when
    /// nothing is selected — which is what every file manager means by "the
    /// selection" and what the F-keys already do here.
    pub selected: Vec<String>,
    pub selected_paths: Vec<String>,
}

impl Context<'_> {
    /// What a transform acts on, and whether it came from a selection.
    ///
    /// A selection can be anywhere — the hosted shell's screen most of all —
    /// so its result cannot replace anything. It is added to the command line
    /// instead, which is somewhere the user can then do something with it.
    fn subject(&self) -> (&str, bool) {
        match self.selection {
            Some(s) if !s.trim().is_empty() => (s, true),
            _ => (self.line, false),
        }
    }
}

fn transformed(from_selection: bool, text: String) -> Outcome {
    if from_selection {
        Outcome::Insert(text)
    } else {
        Outcome::Replace(text)
    }
}

pub fn run(u: Utility, cx: &Context<'_>) -> Outcome {
    match u {
        Utility::Uuid => Outcome::Insert(tools::uuid_v4()),
        Utility::TimestampIso => Outcome::Insert(tools::timestamp_iso()),
        Utility::TimestampUnix => Outcome::Insert(tools::timestamp_unix()),
        Utility::DateStamp => Outcome::Insert(tools::date_stamp()),
        Utility::RandomToken => Outcome::Insert(tools::random_hex(16)),
        Utility::Password => Outcome::Insert(tools::password(20)),
        Utility::ThisPath => Outcome::Insert(tools::shell_quote(&cx.this_path)),
        Utility::OtherPath => Outcome::Insert(tools::shell_quote(&cx.other_path)),
        Utility::SelectedNames => join_quoted(&cx.selected, "nothing is selected"),
        Utility::SelectedPaths => join_quoted(&cx.selected_paths, "nothing is selected"),
        Utility::EditorHere => Outcome::Do(Deed::OpenEditorHere),
        Utility::AgentHere => Outcome::Do(Deed::StartAgentHere),
        Utility::Base64Encode => {
            let (text, from_sel) = cx.subject();
            if text.is_empty() {
                Outcome::Nothing("nothing selected and the command line is empty")
            } else {
                transformed(from_sel, tools::base64_encode(text.as_bytes()))
            }
        }
        Utility::Base64Decode => {
            let (text, from_sel) = cx.subject();
            match tools::base64_decode(text.trim()) {
                // Decoded bytes that are not text would put control characters
                // on a command line, where they are invisible and still sent.
                Some(bytes) => match String::from_utf8(bytes) {
                    Ok(s) if !s.is_empty() => transformed(from_sel, s),
                    Ok(_) => Outcome::Nothing("that decodes to nothing"),
                    Err(_) => Outcome::Nothing("that decodes to bytes, not text"),
                },
                None => Outcome::Nothing("that is not base64"),
            }
        }
        Utility::QuoteLine => {
            let (text, from_sel) = cx.subject();
            if text.is_empty() {
                Outcome::Nothing("nothing selected and the command line is empty")
            } else {
                transformed(from_sel, tools::shell_quote(text))
            }
        }
    }
}

fn join_quoted(items: &[String], empty: &'static str) -> Outcome {
    if items.is_empty() {
        return Outcome::Nothing(empty);
    }
    Outcome::Insert(
        items
            .iter()
            .map(|s| tools::shell_quote(s))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cx(line: &str) -> Context<'_> {
        Context {
            line,
            selection: None,
            this_path: "/tmp/here".into(),
            other_path: "/tmp/there".into(),
            selected: vec!["one.txt".into(), "two files.txt".into()],
            selected_paths: vec!["/tmp/here/one.txt".into()],
        }
    }

    fn inserted(u: Utility, line: &str) -> String {
        match run(u, &cx(line)) {
            Outcome::Insert(s) | Outcome::Replace(s) => s,
            Outcome::Nothing(why) => panic!("{u:?} produced nothing: {why}"),
            Outcome::Do(d) => panic!("{u:?} is a deed, not text: {d:?}"),
        }
    }

    /// Starting the agent is named, not done, for the same reason: this module
    /// promises to touch nothing, and a shell is something.
    #[test]
    fn starting_the_agent_is_named_and_not_done() {
        assert_eq!(
            run(Utility::AgentHere, &cx("")),
            Outcome::Do(Deed::StartAgentHere)
        );
    }

    /// The one entry that does something instead of producing something. It
    /// must stay a deed: performing it here would launch an editor from a
    /// module whose whole promise is that it touches nothing.
    #[test]
    fn opening_the_editor_is_named_and_not_done() {
        assert_eq!(
            run(Utility::EditorHere, &cx("")),
            Outcome::Do(Deed::OpenEditorHere)
        );
    }

    /// Every accelerator must be unique, or one entry becomes unreachable and
    /// the menu quietly lies about how to get to it.
    #[test]
    fn no_two_entries_share_a_key() {
        let mut seen = std::collections::HashSet::new();
        for u in Utility::MENU.iter().flatten() {
            assert!(seen.insert(u.key()), "{:?} reuses {:?}", u, u.key());
            assert_eq!(Utility::from_key(u.key()), Some(*u));
        }
    }

    /// The hint column is what teaches the shortcut; a blank one is a lie.
    #[test]
    fn every_entry_shows_its_key() {
        for u in Utility::MENU.iter().flatten() {
            assert_eq!(hint_of(*u), u.key().to_string(), "{:?}", u);
            assert!(!u.label().is_empty());
        }
    }

    #[test]
    fn the_widget_sees_the_same_rows() {
        let items = items(&[]);
        assert_eq!(items.len(), Utility::MENU.len());
        for (i, slot) in Utility::MENU.iter().enumerate() {
            assert_eq!(items[i].separator, slot.is_none(), "row {i}");
            assert_eq!(Utility::at(i), *slot, "row {i}");
        }
    }

    fn elsewhere() -> Vec<Elsewhere> {
        vec![
            Elsewhere {
                index: 0,
                name: "MAIN".into(),
            },
            Elsewhere {
                index: 3,
                name: "TIMEPULSE".into(),
            },
        ]
    }

    /// The sessions sit under the utilities, behind one separator, and the row
    /// the widget highlights has to mean the session it shows.
    #[test]
    fn the_sessions_are_the_rows_below() {
        let e = elsewhere();
        let items = items(&e);
        assert_eq!(items.len(), Utility::MENU.len() + 1 + e.len());
        assert!(
            items[Utility::MENU.len()].separator,
            "a separator between them"
        );
        assert_eq!(items[Utility::MENU.len() + 1].label, "MAIN");
        assert_eq!(at(Utility::MENU.len() + 1, &e), Some(Choice::GoTo(0)));
        assert_eq!(at(Utility::MENU.len() + 2, &e), Some(Choice::GoTo(3)));
        assert_eq!(at(items.len(), &e), None, "past the end is nothing");
    }

    /// The digit shown is the digit `Alt` already answers to: the session's own
    /// position, not the row it happens to be on. Two numbering schemes for the
    /// same list is how people learn to distrust both.
    #[test]
    fn a_session_answers_to_the_number_the_rail_gives_it() {
        let e = elsewhere();
        assert_eq!(items(&e)[Utility::MENU.len() + 2].hint, "4");
        assert_eq!(from_key('4', &e), Some(Choice::GoTo(3)));
        assert_eq!(from_key('1', &e), Some(Choice::GoTo(0)));
        // Not a session anyone is offering: the row is not there.
        assert_eq!(from_key('2', &e), None);
        // And the letters still reach the utilities.
        assert_eq!(from_key('u', &e), Some(Choice::Do(Utility::Uuid)));
    }

    /// Alone, there is nothing to jump to and no separator dangling under the
    /// last utility.
    #[test]
    fn one_session_adds_nothing_to_the_menu() {
        let items = items(&[]);
        assert!(!items.last().expect("rows").separator);
        assert_eq!(at(items.len(), &[]), None);
    }

    /// A path with a space in it is the normal case, not the exotic one.
    #[test]
    fn paths_and_names_come_out_ready_to_paste() {
        assert_eq!(inserted(Utility::ThisPath, ""), "'/tmp/here'");
        assert_eq!(inserted(Utility::OtherPath, ""), "'/tmp/there'");
        assert_eq!(
            inserted(Utility::SelectedNames, ""),
            "'one.txt' 'two files.txt'"
        );
    }

    #[test]
    fn generators_produce_something_different_each_time() {
        for u in [Utility::Uuid, Utility::RandomToken, Utility::Password] {
            let a = inserted(u, "");
            let b = inserted(u, "");
            assert_ne!(a, b, "{u:?} produced the same thing twice");
            assert!(!a.is_empty());
        }
    }

    #[test]
    fn base64_round_trips_through_the_menu() {
        let encoded = inserted(Utility::Base64Encode, "echo ciao");
        assert_eq!(encoded, "ZWNobyBjaWFv");
        let decoded = inserted(Utility::Base64Decode, &encoded);
        assert_eq!(decoded, "echo ciao");
    }

    /// Decoding to control characters would put invisible bytes on a line that
    /// still gets sent to a shell. Refusing is the only safe answer.
    #[test]
    fn base64_that_is_not_text_is_refused_rather_than_pasted() {
        for line in ["//79", "not base64!", ""] {
            assert!(
                matches!(run(Utility::Base64Decode, &cx(line)), Outcome::Nothing(_)),
                "{line:?} should have been refused"
            );
        }
    }

    #[test]
    fn transforms_say_so_rather_than_doing_nothing_silently() {
        for u in [Utility::Base64Encode, Utility::QuoteLine] {
            assert!(matches!(run(u, &cx("")), Outcome::Nothing(_)), "{u:?}");
        }
    }

    #[test]
    fn an_empty_selection_is_reported_not_pasted_as_nothing() {
        let empty = Context {
            line: "",
            selection: None,
            this_path: "/tmp".into(),
            other_path: "/tmp".into(),
            selected: vec![],
            selected_paths: vec![],
        };
        assert!(matches!(
            run(Utility::SelectedNames, &empty),
            Outcome::Nothing(_)
        ));
    }

    fn with_selection<'a>(line: &'a str, sel: &'a str) -> Context<'a> {
        Context {
            line,
            selection: Some(sel),
            this_path: "/tmp/here".into(),
            other_path: "/tmp/there".into(),
            selected: vec![],
            selected_paths: vec![],
        }
    }

    /// Selecting something and having the menu act on something else is the
    /// surprise that costs trust in the whole menu.
    #[test]
    fn a_selection_wins_over_the_command_line() {
        let sel = with_selection("this is the line", "ciao");
        match run(Utility::Base64Encode, &sel) {
            Outcome::Insert(s) => assert_eq!(s, "Y2lhbw=="),
            other => panic!("expected the selection to be inserted, got {other:?}"),
        }
    }

    /// A selection can be anywhere — the shell's screen most of all — so its
    /// result has nothing to replace and is added to the command line instead.
    #[test]
    fn a_transform_on_a_selection_adds_rather_than_replaces() {
        let sel = with_selection("keep me", "one two");
        assert!(matches!(
            run(Utility::QuoteLine, &sel),
            Outcome::Insert(ref s) if s == "'one two'"
        ));
        // With no selection the same utility rewrites the line in place.
        assert!(matches!(
            run(Utility::QuoteLine, &cx("one two")),
            Outcome::Replace(ref s) if s == "'one two'"
        ));
    }

    /// A selection of nothing but spaces is not a selection.
    #[test]
    fn a_blank_selection_falls_back_to_the_line() {
        let sel = with_selection("echo hi", "   \n ");
        assert!(matches!(
            run(Utility::Base64Encode, &sel),
            Outcome::Replace(ref s) if s == "ZWNobyBoaQ=="
        ));
    }

    /// Text selected off a terminal screen arrives with the trailing spaces of
    /// the row it came from; base64 does not survive them.
    #[test]
    fn a_selection_is_trimmed_before_being_decoded() {
        let sel = with_selection("", "  Y2lhbw==  \n");
        assert!(matches!(run(Utility::Base64Decode, &sel), Outcome::Insert(ref s) if s == "ciao"));
    }
}

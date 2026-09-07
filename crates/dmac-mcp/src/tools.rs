//! The tool catalogue.
//!
//! Data, not code: the schema a model reads and the dispatch table are the same
//! list, so a tool cannot be advertised and then not exist, or exist and never
//! be offered.
//!
//! What is here is what only DMACommander knows — where the panels are, what is
//! selected, where you have been, which sessions are open. Reading and writing
//! files is deliberately absent: every agent already has that, and a second,
//! subtly different implementation of it is a liability, not a feature.

use serde_json::{Value, json};

/// Told to the model once, at connect. It explains the thing a schema cannot:
/// that these tools are about a *live* window someone is looking at.
pub const INSTRUCTIONS: &str = "\
DMACommander is an orthodox file manager the user is looking at right now. \
These tools read and drive that live window: its two panels, the directory \
history, the sessions, and the shell it hosts — which is very likely the shell \
you are running in. Prefer `state` before acting, so you are working from where \
the user actually is rather than from where they were when they last told you.";

/// What a tool acts on, which decides what happens when the agent calling it
/// has no session any more.
///
/// Data rather than a rule in somebody's head, because it is what the guard is
/// derived from *and* what the test that enforces the guard enumerates. A tool
/// added without thinking about this fails that test rather than shipping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Reads or writes one session's own state. An agent whose session has been
    /// closed must be refused: falling back to whichever session is on screen
    /// hands it somebody else's workspace, and its next call moves panels that
    /// have nothing to do with it.
    Session,
    /// Reads only, and answering about the session on screen is a reasonable
    /// answer for a bridge that named no session — a person running the tools
    /// by hand means "here".
    Reading,
    /// Not about a session at all: the whole window, or the process.
    Window,
}

/// One entry: the name, what it is for, what it acts on, and its arguments.
struct Tool {
    name: &'static str,
    description: &'static str,
    scope: Scope,
    /// `(name, type, required, description)`.
    args: &'static [(&'static str, &'static str, bool, &'static str)],
}

const TOOLS: &[Tool] = &[
    Tool {
        name: "state",
        description: "Where the commander is right now: the current session, both panels with \
                      their directory, cursor and selection, which panel is active, and whether \
                      the panels or the hosted shell are on screen. Cheap; call it first.",
        scope: Scope::Reading,
        args: &[],
    },
    Tool {
        name: "list",
        description: "The entries of a directory, with kind and size. Defaults to the active \
                      panel's directory, which is usually what is meant by \"here\".",
        scope: Scope::Reading,
        args: &[
            (
                "path",
                "string",
                false,
                "Directory to list. Defaults to the active panel's.",
            ),
            (
                "limit",
                "integer",
                false,
                "Maximum entries to return. Defaults to 200.",
            ),
        ],
    },
    Tool {
        name: "navigate",
        description: "Take a panel to a directory. The user sees it move.",
        scope: Scope::Session,
        args: &[
            (
                "path",
                "string",
                true,
                "Absolute path, or one relative to the panel's directory.",
            ),
            (
                "panel",
                "string",
                false,
                "\"left\", \"right\", or \"active\" (the default).",
            ),
        ],
    },
    Tool {
        name: "select",
        description: "Set, add to, or clear a panel's selection by name. This is the selection \
                      the user's own F5/F6/F8 keys would act on, so say what you selected.",
        scope: Scope::Session,
        args: &[
            (
                "names",
                "array",
                false,
                "Entry names in the panel's directory.",
            ),
            (
                "mode",
                "string",
                false,
                "\"set\" (default), \"add\", or \"clear\".",
            ),
            (
                "panel",
                "string",
                false,
                "\"left\", \"right\", or \"active\" (the default).",
            ),
        ],
    },
    Tool {
        name: "command",
        description: "Put a line on the command line, and optionally run it in the session's \
                      shell. Running it is visible and irreversible — it is the user's shell, in \
                      the user's directory. Prefer run=false and let them press Enter.",
        scope: Scope::Session,
        args: &[
            ("line", "string", true, "The command line."),
            (
                "run",
                "boolean",
                false,
                "Run it. Defaults to false: typing it is not running it.",
            ),
        ],
    },
    Tool {
        name: "history",
        description: "Directories the panels have visited, across every session and across \
                      restarts. Good for \"where was that project again\".",
        scope: Scope::Reading,
        args: &[
            (
                "order",
                "string",
                false,
                "\"recent\" (default), \"frequent\", or \"session\".",
            ),
            (
                "filter",
                "string",
                false,
                "Fuzzy filter, as the user's own Ctrl-H box does it.",
            ),
            ("limit", "integer", false, "Maximum rows. Defaults to 30."),
        ],
    },
    Tool {
        name: "sessions",
        description: "Every open session: name, directories, whether it is hosting an agent, and \
                      which one is on screen.",
        scope: Scope::Window,
        args: &[],
    },
    Tool {
        name: "switch_session",
        description: "Bring a session to the screen, by name or by position.",
        scope: Scope::Window,
        args: &[
            ("name", "string", false, "Session name."),
            (
                "index",
                "integer",
                false,
                "Zero-based position, if you have no name.",
            ),
        ],
    },
    Tool {
        name: "notify",
        description: "Put a line in the commander's status bar. The way to tell the user \
                      something without interrupting what they are typing.",
        scope: Scope::Window,
        args: &[("message", "string", true, "One short line.")],
    },
    Tool {
        name: "recycle",
        description: "Rebuild the commander and restart it in place, keeping the same terminal,                       the same sessions and the same conversation. Only rebuilds when it is                       running from its own source tree; a released binary just restarts.                       Nothing is torn down unless the build succeeds — a failed build leaves                       everything exactly as it was, with the compiler's complaint in the status                       bar. Note that this restarts whatever is hosted inside it, including you:                       say what you are doing before you call it.",
        scope: Scope::Window,
        args: &[(
            "build",
            "boolean",
            false,
            "Rebuild first. Defaults to true when running from a source tree, false otherwise.",
        )],
    },
    Tool {
        name: "screen",
        description: "The commander's screen as text, exactly as rendered. Use it to see what \
                      the user is seeing — including the output of whatever is running in the \
                      hosted shell.",
        scope: Scope::Window,
        args: &[],
    },
];

/// Is this a tool we offer? Checked before dispatch, so an unknown name never
/// reaches the commander.
pub fn exists(name: &str) -> bool {
    TOOLS.iter().any(|t| t.name == name)
}

/// The catalogue in the shape `tools/list` returns.
pub fn catalogue() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            let mut properties = serde_json::Map::new();
            let mut required: Vec<Value> = Vec::new();
            for (name, kind, is_required, description) in t.args {
                let mut schema = json!({ "type": kind, "description": description });
                // An array of what? A schema without it is one a strict client
                // refuses and a lenient one guesses at.
                if *kind == "array"
                    && let Some(obj) = schema.as_object_mut()
                {
                    obj.insert("items".to_string(), json!({ "type": "string" }));
                }
                properties.insert((*name).to_string(), schema);
                if *is_required {
                    required.push(json!(name));
                }
            }
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": {
                    "type": "object",
                    "properties": Value::Object(properties),
                    "required": required,
                },
            })
        })
        .collect()
}

/// The tool names, for anyone that needs to check its dispatch is complete.
/// The tools that act on one session, by name.
///
/// The frontend derives its guard from this and its test enumerates it, so a
/// tool that is added as [`Scope::Session`] and then forgets to check gets
/// caught rather than shipped.
pub fn session_scoped() -> Vec<&'static str> {
    TOOLS
        .iter()
        .filter(|t| t.scope == Scope::Session)
        .map(|t| t.name)
        .collect()
}

pub fn names() -> Vec<&'static str> {
    TOOLS.iter().map(|t| t.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_two_tools_share_a_name() {
        let mut seen = names();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count);
    }

    #[test]
    fn every_tool_says_what_it_is_for() {
        for t in TOOLS {
            assert!(t.description.len() > 40, "{} is undocumented", t.name);
            for (name, kind, _, description) in t.args {
                assert!(!description.is_empty(), "{}.{name}", t.name);
                assert!(
                    matches!(*kind, "string" | "integer" | "boolean" | "array" | "object"),
                    "{}.{name}: {kind} is not a JSON Schema type",
                    t.name
                );
            }
        }
    }

    #[test]
    fn required_arguments_are_listed_in_the_schema() {
        let listed = catalogue();
        let navigate = listed
            .iter()
            .find(|t| t["name"] == "navigate")
            .expect("navigate");
        assert_eq!(navigate["inputSchema"]["required"], json!(["path"]));
        assert_eq!(
            navigate["inputSchema"]["properties"]["panel"]["type"],
            "string"
        );
    }

    #[test]
    fn an_array_argument_says_what_it_holds() {
        let listed = catalogue();
        let select = listed
            .iter()
            .find(|t| t["name"] == "select")
            .expect("select");
        assert_eq!(
            select["inputSchema"]["properties"]["names"]["items"]["type"],
            "string"
        );
    }

    #[test]
    fn exists_agrees_with_the_catalogue() {
        for name in names() {
            assert!(exists(name));
        }
        assert!(!exists("delete_everything"));
    }
}

//! DMACommander as a Model Context Protocol server.
//!
//! An agent running *inside* the commander can see what the commander sees: the
//! two panels, where they are, what is selected, the directory history, the
//! sessions, the hosted shell. That is the point — a coding agent that has to be
//! told where you are, in prose, every time you move, is an agent working from a
//! blurred photograph of your screen.
//!
//! # Shape
//!
//! The protocol is JSON-RPC 2.0, one message per line, exactly as MCP's stdio
//! transport specifies. This module owns the protocol and the tool catalogue;
//! it owns no state and knows nothing about panels. What a tool *does* is
//! [`Commander`], implemented by whoever is running the UI.
//!
//! The split matters: the UI is single-threaded and owns everything, while
//! connections arrive on tasks. Parsing on the connection and dispatching on the
//! UI thread means no state is ever behind a lock, and a badly-behaved client
//! cannot stall a frame.

// Tests assert; `unwrap`/`expect` there are how a failure is reported.
// In non-test code the workspace lints still forbid them.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub mod bridge;
pub mod tools;

/// Where a running commander listens: one socket per process, named by pid so
/// two commanders on one machine never fight over it.
pub fn socket_path(root: &std::path::Path, pid: u32) -> std::path::PathBuf {
    let preferred = root.join("mcp").join(format!("{pid}.sock"));
    // A Unix socket path lives in a fixed-size field — 104 bytes on macOS, 108
    // on Linux — and binding past it fails. The config directory is usually
    // well inside that, but a long user name or a deep XDG root is not, and the
    // failure is silent: the socket simply never appears and the agent that
    // was told about it finds nothing. Falling back to the temporary directory
    // keeps the path short enough to bind.
    const LIMIT: usize = 100;
    if preferred.as_os_str().len() <= LIMIT {
        return preferred;
    }
    std::env::temp_dir().join(format!("dmac-{pid}.sock"))
}

/// Remove sockets left by runs that are no longer here.
///
/// One file per run accumulates otherwise, and a stale one is worse than
/// clutter: it is a path an agent can be pointed at and get nothing from.
/// Returns how many were removed.
#[cfg(unix)]
pub fn clear_stale_sockets(root: &std::path::Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root.join("mcp")) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let path = e.path();
        let Some(pid) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() as i32 || alive(pid) {
            continue;
        }
        if std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(not(unix))]
pub fn clear_stale_sockets(_root: &std::path::Path) -> usize {
    0
}

/// Whether a process exists. Signal 0 asks without disturbing it.
#[cfg(unix)]
#[allow(unsafe_code)]
fn alive(pid: i32) -> bool {
    // SAFETY: two integers in, one out; signal 0 delivers nothing.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The handshake a bridge sends first, naming the session it belongs to.
/// `None` when the line is not a handshake at all, which is how a client that
/// speaks straight JSON-RPC still works.
pub fn attach_session(line: &str) -> Option<Option<String>> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    (v.get("dmac")?.as_str()? == "attach")
        .then(|| v.get("session").and_then(Value::as_str).map(str::to_string))
}

/// The protocol version this speaks. MCP dates its revisions.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// What the commander can be asked to do.
///
/// One method rather than one per tool: the catalogue is data, and a trait with
/// eleven methods would have to be edited in lockstep with it.
pub trait Commander {
    /// Run a tool. `Err` is a *tool* error — reported to the model as a failed
    /// result it can react to, not as a broken connection.
    fn call(&mut self, tool: &str, args: &Value) -> Result<Value, String>;
}

/// One JSON-RPC request.
#[derive(Debug, Clone, Deserialize)]
struct Request {
    /// Absent for a notification, which is a message that must not be answered.
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

// The JSON-RPC codes this can produce. Others exist; these are the ones a
// well-formed client can actually provoke.
const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const METHOD_NOT_FOUND: i32 = -32601;

/// Handle one incoming line.
///
/// Returns the line to write back, or `None` for a notification — answering one
/// is a protocol violation, and some clients drop the connection over it.
pub fn dispatch(line: &str, commander: &mut impl Commander) -> Option<String> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let request: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            // No id could be read, so the error is reported against null. That
            // is what the specification asks for and what clients expect.
            return Some(encode(&Response {
                jsonrpc: "2.0",
                id: Value::Null,
                result: None,
                error: Some(RpcError {
                    code: PARSE_ERROR,
                    message: format!("not valid JSON-RPC: {e}"),
                }),
            }));
        }
    };

    let id = request.id.clone()?;

    let outcome = match request.method.as_str() {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "dmac", "version": env!("CARGO_PKG_VERSION") },
            "instructions": tools::INSTRUCTIONS,
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools::catalogue() })),
        "tools/call" => {
            let name = request.params.get("name").and_then(Value::as_str);
            let arguments = request
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match name {
                None => Err((INVALID_REQUEST, "tools/call needs a name".to_string())),
                Some(name) if !tools::exists(name) => {
                    Err((METHOD_NOT_FOUND, format!("no such tool: {name}")))
                }
                Some(name) => {
                    // A tool that fails is a *result*, not a transport error:
                    // the model is meant to read the message and try something
                    // else, which it cannot do if the call never comes back.
                    Ok(match commander.call(name, &arguments) {
                        Ok(value) => content(&value, false),
                        Err(message) => content(&Value::String(message), true),
                    })
                }
            }
        }
        other => Err((METHOD_NOT_FOUND, format!("no such method: {other}"))),
    };

    Some(match outcome {
        Ok(result) => encode(&Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }),
        Err((code, message)) => encode(&Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message }),
        }),
    })
}

/// An MCP tool result.
///
/// Structured content is sent *as well as* the text, not instead of it: clients
/// that understand it get the real shape, and the rest get something readable
/// rather than nothing.
fn content(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    if !is_error && !value.is_string() && let Some(obj) = result.as_object_mut() {
        obj.insert("structuredContent".to_string(), value.clone());
    }
    result
}

fn encode(response: &Response) -> String {
    serde_json::to_string(response).unwrap_or_else(|_| {
        // Serialising our own types cannot fail, but a panic here would take
        // down a UI thread over a log line.
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32603,"message":"could not encode"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A commander that records what it was asked and answers predictably.
    #[derive(Default)]
    struct Spy {
        calls: Vec<(String, Value)>,
        fail: bool,
    }

    impl Commander for Spy {
        fn call(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
            self.calls.push((tool.to_string(), args.clone()));
            if self.fail {
                return Err("nope".to_string());
            }
            Ok(json!({ "ok": true }))
        }
    }

    fn ask(line: &str, spy: &mut Spy) -> Value {
        let out = dispatch(line, spy).expect("a request must be answered");
        serde_json::from_str(&out).expect("the answer must be JSON")
    }

    #[test]
    fn initialize_reports_the_protocol_and_the_name() {
        let mut spy = Spy::default();
        let r = ask(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#, &mut spy);
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(r["result"]["serverInfo"]["name"], "dmac");
        assert_eq!(r["id"], 1);
    }

    /// A notification has no id, and answering one is a protocol violation that
    /// some clients drop the connection over.
    #[test]
    fn a_notification_is_not_answered() {
        let mut spy = Spy::default();
        assert!(dispatch(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, &mut spy).is_none());
        assert!(dispatch("", &mut spy).is_none());
    }

    #[test]
    fn the_catalogue_comes_back_whole() {
        let mut spy = Spy::default();
        let r = ask(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#, &mut spy);
        let listed = r["result"]["tools"].as_array().expect("an array");
        assert_eq!(listed.len(), tools::catalogue().len());
        for t in listed {
            assert!(t["name"].is_string());
            assert!(t["description"].is_string(), "{t}");
            assert_eq!(
                t["inputSchema"]["type"], "object",
                "every tool needs a schema: {t}"
            );
        }
    }

    #[test]
    fn calling_a_tool_reaches_the_commander() {
        let mut spy = Spy::default();
        let r = ask(
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"state","arguments":{"x":1}}}"#,
            &mut spy,
        );
        assert_eq!(spy.calls, vec![("state".to_string(), json!({"x": 1}))]);
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["structuredContent"]["ok"], true);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .is_some_and(|t| t.contains("ok")),
            "the text form is there for clients that ignore structure"
        );
    }

    /// A tool that fails must come back as a result the model can read, not as
    /// an error that ends the exchange.
    #[test]
    fn a_failing_tool_is_a_result_not_a_broken_call() {
        let mut spy = Spy {
            fail: true,
            ..Spy::default()
        };
        let r = ask(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"state"}}"#,
            &mut spy,
        );
        assert!(r["error"].is_null(), "not a transport error");
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["content"][0]["text"], "nope");
    }

    #[test]
    fn an_unknown_tool_is_refused_before_it_reaches_the_commander() {
        let mut spy = Spy::default();
        let r = ask(
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"rm_rf"}}"#,
            &mut spy,
        );
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
        assert!(spy.calls.is_empty(), "and it never ran");
    }

    #[test]
    fn rubbish_is_answered_rather_than_ignored() {
        let mut spy = Spy::default();
        let r = ask("{not json", &mut spy);
        assert_eq!(r["error"]["code"], PARSE_ERROR);
        assert_eq!(r["id"], Value::Null);
    }

    #[test]
    fn an_unknown_method_says_so() {
        let mut spy = Spy::default();
        let r = ask(r#"{"jsonrpc":"2.0","id":6,"method":"resources/list"}"#, &mut spy);
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    }

    /// Every response has to fit on one line: the transport is line-delimited,
    /// and a newline inside one would be read as two messages.
    #[test]
    fn responses_are_single_lines() {
        let mut spy = Spy::default();
        for line in [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"state"}}"#,
        ] {
            let out = dispatch(line, &mut spy).unwrap();
            assert!(!out.contains('\n'), "{out}");
        }
    }
}

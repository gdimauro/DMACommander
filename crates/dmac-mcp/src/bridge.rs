//! The stdio side: a pipe between an MCP client and a running commander.
//!
//! An MCP client starts its servers itself, as child processes, and talks to
//! them over stdin and stdout. But the thing worth talking to here is the
//! commander already on screen, not a fresh one. So `dmac --mcp <socket>` is a
//! few dozen lines of pipe: whatever the client writes goes to the socket, and
//! whatever comes back goes to the client.
//!
//! Unix only. Windows wants a named pipe, and half an implementation would be
//! worse than an honest "not here yet".

use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

/// Connect, announce which session this client belongs to, and pump until one
/// side closes.
#[cfg(unix)]
pub fn run(socket: &Path, session: Option<&str>) -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;

    let stream = UnixStream::connect(socket)?;
    let mut to_commander = stream.try_clone()?;

    // The first line is the handshake, not a request: it says which session's
    // panels this client should be looking at. Without it an agent hosted in
    // one session would act on whichever session the user happened to be
    // looking at when the call landed.
    let hello = json!({ "dmac": "attach", "session": session });
    writeln!(to_commander, "{hello}")?;
    to_commander.flush()?;

    // Reading the socket runs on its own thread: both directions are blocking,
    // and a single thread doing both would deadlock the first time the client
    // is quiet while the commander has something to say.
    let from_commander = std::thread::spawn(move || {
        let mut out = std::io::stdout();
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if writeln!(out, "{line}").is_err() || out.flush().is_err() {
                break;
            }
        }
    });

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if to_commander.write_all(line.as_bytes()).is_err() || to_commander.flush().is_err() {
            break;
        }
    }

    // Close the *writing* half only. Shutting down both would tear the socket
    // down while answers to the last requests are still in flight — a client
    // that pipes its requests in and closes stdin would get nothing back at
    // all. Half-closing says "no more from me", the commander finishes what it
    // was asked, and the reader thread ends when the commander closes its side.
    let _ = to_commander.shutdown(std::net::Shutdown::Write);
    let _ = from_commander.join();
    Ok(())
}

#[cfg(not(unix))]
pub fn run(_socket: &Path, _session: Option<&str>) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "the MCP bridge needs a Unix socket; Windows support is not written yet",
    ))
}

/// Drain a reader into a string, for callers that want the whole exchange.
/// Used by the tests, and by anything that wants to script the bridge.
pub fn drain(mut r: impl Read) -> std::io::Result<String> {
    let mut s = String::new();
    r.read_to_string(&mut s)?;
    Ok(s)
}

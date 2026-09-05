//! The system clipboard.
//!
//! In the core rather than in a UI crate because it is a system service, not a
//! rendering concern: the GPU backend and a headless mode need the same one.
//!
//! The handle is kept alive for the life of the process on purpose. On X11 the
//! clipboard has no server — the *owning process* answers requests for its
//! contents — so a handle created per call would put text on the clipboard that
//! vanishes the moment the call returns. macOS and Windows have a real
//! pasteboard and do not care either way.

use std::sync::{Mutex, OnceLock};

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("no clipboard is available here: {0}")]
    Unavailable(String),
    #[error("the clipboard is empty, or holds something that is not text")]
    Empty,
    #[error("the clipboard is in use by something else")]
    Busy,
}

pub type Result<T> = std::result::Result<T, ClipboardError>;

fn handle() -> &'static Mutex<Option<arboard::Clipboard>> {
    static HANDLE: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
    HANDLE.get_or_init(|| Mutex::new(arboard::Clipboard::new().ok()))
}

/// Put text on the system clipboard.
pub fn set_text(text: &str) -> Result<()> {
    let mut guard = handle().lock().map_err(|_| ClipboardError::Busy)?;
    let clip = guard
        .as_mut()
        .ok_or_else(|| ClipboardError::Unavailable("none was found at startup".into()))?;
    clip.set_text(text.to_string())
        .map_err(|e| ClipboardError::Unavailable(e.to_string()))
}

/// Read text from the system clipboard.
pub fn text() -> Result<String> {
    let mut guard = handle().lock().map_err(|_| ClipboardError::Busy)?;
    let clip = guard
        .as_mut()
        .ok_or_else(|| ClipboardError::Unavailable("none was found at startup".into()))?;
    match clip.get_text() {
        Ok(s) if s.is_empty() => Err(ClipboardError::Empty),
        Ok(s) => Ok(s),
        Err(arboard::Error::ContentNotAvailable) => Err(ClipboardError::Empty),
        Err(e) => Err(ClipboardError::Unavailable(e.to_string())),
    }
}

/// Encode text as an OSC 52 sequence, which asks the *terminal* to set its
/// clipboard.
///
/// The fallback for when there is no local clipboard to talk to — over SSH the
/// system clipboard belongs to the wrong machine, and the terminal at the other
/// end of the connection is the one the user is actually looking at.
///
/// Returns `None` for anything too large: terminals cap the sequence they will
/// accept, and one truncated halfway is worse than none, because the user
/// pastes something that looks complete and is not.
pub fn osc52(text: &str) -> Option<String> {
    const LIMIT: usize = 64 * 1024;
    if text.len() > LIMIT {
        return None;
    }
    Some(format!("\x1b]52;c;{}\x07", base64(text.as_bytes())))
}

/// Standard base64. Written out rather than pulled in: it is fourteen lines and
/// this is the only place in the workspace that needs it.
fn base64(bytes: &[u8]) -> String {
    const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(SET[(n >> (18 - i * 6)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_bytes_that_are_not_ascii() {
        assert_eq!(base64("é".as_bytes()), "w6k=");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn osc52_wraps_the_payload_the_way_terminals_expect() {
        let s = osc52("hi").expect("small enough");
        assert!(s.starts_with("\x1b]52;c;"), "{s:?}");
        assert!(s.ends_with('\x07'), "{s:?}");
        assert!(s.contains("aGk="), "{s:?}");
    }

    /// Truncated-but-plausible is the failure mode worth avoiding: the user
    /// pastes what looks like their selection and gets most of it.
    #[test]
    fn osc52_refuses_more_than_a_terminal_will_take() {
        assert!(osc52(&"x".repeat(64 * 1024 + 1)).is_none());
        assert!(osc52(&"x".repeat(1024)).is_some());
    }
}

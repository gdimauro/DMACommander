//! Small text utilities: the things you find yourself wanting halfway through
//! typing a command, and currently leave the file manager to get.
//!
//! Pure functions with no I/O, so the menu that offers them cannot block a
//! frame and every one of them is testable without a terminal.

use std::fmt::Write as _;

/// A random UUID, version 4.
pub fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Now, as RFC 3339 / ISO 8601 with the local offset.
pub fn timestamp_iso() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%:z")
        .to_string()
}

/// Now, as whole seconds since the Unix epoch.
pub fn timestamp_unix() -> String {
    jiff::Timestamp::now().as_second().to_string()
}

/// Today, as `2026-09-05` — sorts correctly as a filename prefix, which is the
/// only reason to date-stamp a file by hand.
pub fn date_stamp() -> String {
    jiff::Zoned::now().strftime("%Y-%m-%d").to_string()
}

/// `n` random bytes as lowercase hex.
pub fn random_hex(n: usize) -> String {
    let mut out = String::with_capacity(n * 2);
    for b in random_bytes(n) {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// A random password of `len` characters.
///
/// The alphabet leaves out the characters people misread — `0`/`O`, `1`/`l`/`I`
/// — because a password is usually read aloud or retyped at least once, and the
/// entropy those few characters buy is not worth the support call.
pub fn password(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#%^&*-_=+";
    // Rejection sampling: taking a byte modulo the alphabet size would make the
    // first few characters likelier than the rest, which is a real bias even if
    // a small one, and there is no reason to accept it.
    let limit = (256 / ALPHABET.len()) * ALPHABET.len();
    let mut out = String::with_capacity(len);
    while out.len() < len {
        for b in random_bytes(len.saturating_sub(out.len()).max(8) * 2) {
            if (b as usize) < limit {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    out
}

/// Bytes from the operating system's random source.
///
/// Falls back to nothing rather than to a weak generator: a password that looks
/// random and is not is worse than an error, because nobody checks it again.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    match getrandom::fill(&mut buf) {
        Ok(()) => buf,
        Err(_) => Vec::new(),
    }
}

/// Standard base64.
pub fn base64_encode(bytes: &[u8]) -> String {
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

/// Decode standard base64, tolerating missing padding and embedded whitespace —
/// base64 arrives wrapped in emails and YAML far more often than it arrives
/// clean, and refusing those would make the utility useless where it is needed.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for c in s.chars() {
        if c.is_whitespace() || c == '=' {
            continue;
        }
        let v = match c {
            'A'..='Z' => c as u32 - 'A' as u32,
            'a'..='z' => c as u32 - 'a' as u32 + 26,
            '0'..='9' => c as u32 - '0' as u32 + 52,
            '+' => 62,
            '/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// Wrap a string so a shell reads it as one literal word.
///
/// A path is data. It arrives from the filesystem, from an archive, or from a
/// remote listing, and a directory really can be called `; rm -rf ~` — nothing
/// stops anyone creating one. Single quotes suspend every kind of expansion a
/// shell does, and the only character they cannot contain is the single quote
/// itself, which is closed, escaped and reopened.
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

    #[test]
    fn a_uuid_has_the_right_shape_and_version() {
        let u = uuid_v4();
        assert_eq!(u.len(), 36, "{u}");
        let parts: Vec<&str> = u.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'), "version nibble: {u}");
        assert!("89ab".contains(&parts[3][..1]), "variant nibble: {u}");
    }

    #[test]
    fn uuids_do_not_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(uuid_v4()), "a uuid came round twice");
        }
    }

    #[test]
    fn a_password_is_the_length_asked_for_and_avoids_lookalikes() {
        for len in [1usize, 8, 20, 64] {
            let p = password(len);
            assert_eq!(p.chars().count(), len, "{p}");
            for c in ['0', 'O', '1', 'l', 'I'] {
                assert!(!p.contains(c), "{p} contains the lookalike {c}");
            }
        }
    }

    /// The whole point of a generated password: two of them are not the same.
    #[test]
    fn passwords_differ() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(password(20)), "a password came round twice");
        }
    }

    #[test]
    fn random_hex_is_hex_of_the_right_length() {
        let h = random_hex(16);
        assert_eq!(h.len(), 32);
        assert!(
            h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "{h}"
        );
    }

    #[test]
    fn base64_matches_the_standard_vectors() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(
                base64_encode(plain.as_bytes()),
                encoded,
                "encoding {plain:?}"
            );
            assert_eq!(
                base64_decode(encoded).as_deref(),
                Some(plain.as_bytes()),
                "decoding {encoded:?}"
            );
        }
    }

    /// Base64 arrives wrapped in emails and YAML far more often than it arrives
    /// clean; refusing those is refusing the cases it is wanted for.
    #[test]
    fn base64_decoding_survives_wrapping_and_missing_padding() {
        assert_eq!(base64_decode("Zm9v\nYmFy").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(base64_decode("Zm9vYg").as_deref(), Some(&b"foob"[..]));
        assert_eq!(base64_decode("  Zm9v  ").as_deref(), Some(&b"foo"[..]));
        assert!(base64_decode("not base64!").is_none());
    }

    #[test]
    fn base64_round_trips_bytes_that_are_not_text() {
        let bytes: Vec<u8> = (0u8..=255).collect();
        assert_eq!(
            base64_decode(&base64_encode(&bytes)).as_deref(),
            Some(&bytes[..])
        );
    }

    /// A directory can be called almost anything, including things that look
    /// like shell syntax. If quoting is wrong, using a filename runs it.
    #[test]
    fn quoting_makes_a_path_one_literal_word() {
        assert_eq!(shell_quote("/tmp/plain"), "'/tmp/plain'");
        assert_eq!(shell_quote("/tmp/with space"), "'/tmp/with space'");
        assert_eq!(shell_quote("/tmp/; rm -rf ~"), "'/tmp/; rm -rf ~'");
        assert_eq!(shell_quote("/tmp/$(whoami)"), "'/tmp/$(whoami)'");
        assert_eq!(shell_quote("/tmp/`id`"), "'/tmp/`id`'");
        assert_eq!(shell_quote("/tmp/a'b"), r#"'/tmp/a'\''b'"#);
    }

    #[test]
    fn a_timestamp_is_sortable_and_a_date_stamp_is_a_prefix() {
        let t = timestamp_iso();
        assert!(t.len() >= 25, "{t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], "T");
        let d = date_stamp();
        assert_eq!(d.len(), 10, "{d}");
        assert_eq!(&t[..10], &d[..], "the two must agree about today");
        assert!(timestamp_unix().parse::<i64>().unwrap() > 1_700_000_000);
    }
}

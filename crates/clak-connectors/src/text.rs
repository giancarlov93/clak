//! Small tools for address texts: the `%XX` sequences (RFC 3986) and lengths counted
//! in UTF-16 units. Cleaning up a text for a window (`texts::clean`) and the
//! command-line names (`cli_names`) belong to the hub.

/// The length in UTF-16 units.
pub(crate) fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Everything as `%XX` except the RFC 3986 unreserved characters.
pub(crate) fn escape_data(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The bytes of a text with the `%XX` sequences decoded (malformed ones stay as they are).
fn unescape_bytes(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A query part (`+` is a space), decoded; invalid UTF-8 is replaced.
pub(crate) fn unescape_lossy(text: &str) -> String {
    String::from_utf8_lossy(&unescape_bytes(&text.replace('+', " "))).into_owned()
}

/// The same, but `None` if the decoded bytes aren't UTF-8: such a link can't be read.
pub(crate) fn unescape_strict(text: &str) -> Option<String> {
    String::from_utf8(unescape_bytes(&text.replace('+', " "))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_sequenze_percentuali() {
        assert_eq!(unescape_lossy("a%2Bb+c%"), "a+b c%");
        assert_eq!(unescape_strict("%E2%80%AE").as_deref(), Some("\u{202E}"));
        assert_eq!(unescape_strict("%FF"), None);
        assert_eq!(escape_data("a b/ç~"), "a%20b%2F%C3%A7~");
    }
}

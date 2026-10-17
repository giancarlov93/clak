//! Texts and URLs: cleaned-up names and descriptions (with the hub's rule), the `%XX`
//! encoding of a piece of a URL, the origin of a URL.

use url::{Host, Url};

/// A single short line, with no control characters or text-direction tricks: the hub's
/// rule (`clak_hub::texts::clean`). Titles, names and descriptions are written by whoever
/// publishes the OpenAPI, and they end up in front of a model and in a permission prompt.
pub fn clean(text: &str, limit: usize) -> String {
    clak_hub::texts::clean(Some(text), limit)
}

/// Cleaned up, or nothing if nothing is left.
pub fn clean_opt(text: Option<&str>, limit: usize) -> Option<String> {
    text.map(|t| clean(t, limit)).filter(|t| !t.is_empty())
}

/// The length in UTF-16 units.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The leading part, at most `units` UTF-16 units long, without splitting a character.
pub fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut used = 0;
    for (at, c) in text.char_indices() {
        if used + c.len_utf16() > units {
            return &text[..at];
        }
        used += c.len_utf16();
    }
    text
}

// =========================================================================
// URLs
// =========================================================================

/// Only letters, digits and `- _ . ~` stay; the rest becomes `%XX`
/// of its UTF-8 bytes. A `/` inside a value stays inside its path segment.
pub fn escape_data(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A field of an `application/x-www-form-urlencoded` form: as above, but space is `+`.
pub fn escape_form(text: &str) -> String {
    escape_data(text).replace("%20", "+")
}

/// `%XX` turns back into bytes; whatever isn't UTF-8 becomes �.
pub fn unescape_data(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

/// Every line break becomes a space: a header value fits on one line.
pub fn one_line(text: &str) -> String {
    text.replace("\r\n", " ").replace(
        ['\r', '\n', '\u{0C}', '\u{85}', '\u{2028}', '\u{2029}'],
        " ",
    )
}

/// This computer: `localhost`, 127.x.x.x, ::1.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Host, and port if it isn't the default one.
pub fn authority(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    }
}

/// The URL without query or fragment.
pub fn left_part_path(url: &Url) -> String {
    format!("{}://{}{}", url.scheme(), authority(url), url.path())
}

/// Same origin: scheme, host and port (the default one, if not written).
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme().eq_ignore_ascii_case(b.scheme())
        && a.host_str()
            .unwrap_or("")
            .eq_ignore_ascii_case(b.host_str().unwrap_or(""))
        && a.port_or_known_default() == b.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ripulisce_e_accorcia() {
        assert_eq!(clean("  due\n\tparole\u{202E} ", 80), "due parole");
        assert_eq!(clean("abcdef", 4), "abc…");
        assert_eq!(clean("", 4), "");
    }

    #[test]
    fn la_codifica_degli_indirizzi() {
        assert_eq!(escape_data("a/b c~è"), "a%2Fb%20c~%C3%A8");
        assert_eq!(escape_form("a&b c"), "a%26b+c");
        assert_eq!(unescape_data("a%2Fb%20c%zz%"), "a/b c%zz%");
    }
}

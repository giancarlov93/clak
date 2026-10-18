//! Small helpers to read the wire's JSON trees leniently: a field that is missing or of
//! another type counts as "nothing", not an error.

use serde_json::{Map, Value};

pub(crate) type Object = Map<String, Value>;

/// The text of a field, if it is a string.
pub(crate) fn str_of<'a>(object: &'a Object, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

/// True only if the field is `true`: a "yes" written as text or a number doesn't count.
pub(crate) fn flag(object: &Object, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The objects of a field that is a list; the rest is skipped.
pub(crate) fn objects(value: Option<&Value>) -> Vec<&Object> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_object).collect())
        .unwrap_or_default()
}

/// An object inside an object.
pub(crate) fn object_of<'a>(object: &'a Object, key: &str) -> Option<&'a Object> {
    object.get(key).and_then(Value::as_object)
}

/// Compact, on one line.
pub(crate) fn compact(value: &Value) -> String {
    clak_wire::json::text(value)
}

/// Indented by two spaces.
pub(crate) fn pretty(value: &Value) -> String {
    String::from_utf8(clak_wire::json::encode_pretty(value)).unwrap_or_else(|_| "null".into())
}

/// The length of a text in UTF-16 units: that is how the limits and the printed numbers
/// are counted ("25,000 characters").
pub(crate) fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The prefix of at most `units` UTF-16 units, without splitting a character.
pub(crate) fn utf16_prefix(text: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, c) in text.char_indices() {
        count += c.len_utf16();
        if count > units {
            return &text[..index];
        }
    }
    text
}

/// An absolute path. On the Mac also `~`, and without `.` and `..`.
pub(crate) fn full_path(path: &str) -> std::path::PathBuf {
    use std::path::{Path, PathBuf};
    #[cfg(unix)]
    let expanded: PathBuf = if path == "~" {
        clak_wire::location::user_home()
    } else if let Some(rest) = path.strip_prefix("~/") {
        clak_wire::location::user_home().join(rest)
    } else {
        PathBuf::from(path)
    };
    #[cfg(windows)]
    let expanded = PathBuf::from(path);
    let absolute = std::path::absolute(&expanded).unwrap_or_else(|_| Path::new(path).to_path_buf());
    #[cfg(unix)]
    {
        // `absolute` on Unix leaves the `..`: they are removed lexically, without touching the disk.
        use std::path::Component;
        let mut clean = PathBuf::new();
        for part in absolute.components() {
            match part {
                Component::ParentDir => {
                    clean.pop();
                }
                Component::CurDir => {}
                other => clean.push(other.as_os_str()),
            }
        }
        clean
    }
    #[cfg(windows)]
    absolute
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_are_counted_in_utf16_units() {
        assert_eq!(utf16_len("résumé"), 6);
        assert_eq!(utf16_len("😀"), 2);
        assert_eq!(utf16_prefix("abc😀d", 4), "abc");
        assert_eq!(utf16_prefix("abc😀d", 5), "abc😀");
        assert_eq!(utf16_prefix("ab", 10), "ab");
    }
}

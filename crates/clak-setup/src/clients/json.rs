//! The JSON of the clients' configuration files: it preserves key order and numbers as
//! written (`1.50` stays `1.50`), and rejects comments, extra commas and duplicate keys.
//! A file that isn't plain JSON is not rewritten: half of it would be lost.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    /// The number as written in the file.
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(Object),
}

/// An object, with keys in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Object(Vec<(String, Json)>);

impl Object {
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Puts the value in place of the existing one, or at the end.
    pub fn insert(&mut self, key: &str, value: Json) {
        match self.0.iter_mut().find(|(k, _)| k == key) {
            Some((_, slot)) => *slot = value,
            None => self.0.push((key.to_owned(), value)),
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Json> {
        let index = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(index).1)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub fn keys(&self) -> Vec<&str> {
        self.0.iter().map(|(k, _)| k.as_str()).collect()
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Json> {
        self.0.iter_mut().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl Json {
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Json::Object(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Json::Array(a) => Some(a),
            _ => None,
        }
    }

    /// A field, if it is an object and has it.
    pub fn get(&self, key: &str) -> Option<&Json> {
        self.as_object()?.get(key)
    }

    /// Compact, on one line.
    pub fn text(&self) -> String {
        match self {
            Json::Null => "null".into(),
            Json::Bool(b) => b.to_string(),
            Json::Number(n) => n.clone(),
            Json::String(s) => quote(s),
            Json::Array(items) => format!(
                "[{}]",
                items.iter().map(Json::text).collect::<Vec<_>>().join(",")
            ),
            Json::Object(o) => {
                format!(
                    "{{{}}}",
                    o.0.iter()
                        .map(|(k, v)| format!("{}:{}", quote(k), v.text()))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            }
        }
    }

    /// Indented the way editors write it: one key per line, `[]` and `{}` when empty.
    pub fn pretty(&self, indent: &str, newline: &str) -> String {
        self.pretty_at(indent, newline, 0)
    }

    fn pretty_at(&self, indent: &str, newline: &str, level: usize) -> String {
        let inner = indent.repeat(level + 1);
        let outer = indent.repeat(level);
        match self {
            Json::Array(items) if !items.is_empty() => {
                let lines: Vec<String> = items
                    .iter()
                    .map(|v| inner.clone() + &v.pretty_at(indent, newline, level + 1))
                    .collect();
                format!(
                    "[{newline}{}{newline}{outer}]",
                    lines.join(&format!(",{newline}"))
                )
            }
            Json::Object(o) if !o.is_empty() => {
                let lines: Vec<String> =
                    o.0.iter()
                        .map(|(k, v)| {
                            format!(
                                "{inner}{}: {}",
                                quote(k),
                                v.pretty_at(indent, newline, level + 1)
                            )
                        })
                        .collect();
                format!(
                    "{{{newline}{}{newline}{outer}}}",
                    lines.join(&format!(",{newline}"))
                )
            }
            other => other.text(),
        }
    }
}

/// A JSON string, without escaping non-ASCII characters.
fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JsonError {
    /// Not plain JSON, at the given line (from 1).
    Syntax { line: usize },
    /// A key repeated in the same object, at the given line.
    Repeated { line: usize },
}

pub(crate) fn parse(text: &str) -> Result<Json, JsonError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        at: 0,
    };
    parser.blank();
    let value = parser.value(0)?;
    parser.blank();
    if parser.at != parser.bytes.len() {
        return Err(parser.syntax());
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Parser<'_> {
    fn line(&self) -> usize {
        self.bytes[..self.at.min(self.bytes.len())]
            .iter()
            .filter(|&&b| b == b'\n')
            .count()
            + 1
    }

    fn syntax(&self) -> JsonError {
        JsonError::Syntax { line: self.line() }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn blank(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(self.syntax())
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, JsonError> {
        if depth > 512 {
            return Err(self.syntax());
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::String),
            Some(b't') => self.word("true", Json::Bool(true)),
            Some(b'f') => self.word("false", Json::Bool(false)),
            Some(b'n') => self.word("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.syntax()),
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, JsonError> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.syntax())
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.at += 1;
        let mut object = Object::default();
        self.blank();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Object(object));
        }
        loop {
            self.blank();
            if self.peek() != Some(b'"') {
                return Err(self.syntax());
            }
            let start = self.at;
            let key = self.string()?;
            if object.get(&key).is_some() {
                self.at = start;
                return Err(JsonError::Repeated { line: self.line() });
            }
            self.blank();
            self.expect(b':')?;
            self.blank();
            let value = self.value(depth + 1)?;
            object.0.push((key, value));
            self.blank();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Json::Object(object));
                }
                _ => return Err(self.syntax()),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, JsonError> {
        self.at += 1;
        let mut items = Vec::new();
        self.blank();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Array(items));
        }
        loop {
            self.blank();
            items.push(self.value(depth + 1)?);
            self.blank();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Json::Array(items));
                }
                _ => return Err(self.syntax()),
            }
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Json, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.syntax()),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(self.syntax());
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(self.syntax());
            }
        }
        Ok(Json::Number(self.text[start..self.at].to_owned()))
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self
            .text
            .get(self.at..self.at + 4)
            .ok_or_else(|| self.syntax())?;
        let code = u32::from_str_radix(digits, 16).map_err(|_| self.syntax())?;
        if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(self.syntax());
        }
        self.at += 4;
        Ok(code)
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.syntax());
            };
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let Some(escape) = self.peek() else {
                        return Err(self.syntax());
                    };
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code)
                                && self.bytes[self.at..].starts_with(b"\\u")
                            {
                                let back = self.at;
                                self.at += 2;
                                let low = self.hex4()?;
                                if (0xDC00..0xE000).contains(&low) {
                                    code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                                } else {
                                    self.at = back;
                                }
                            }
                            // A lone surrogate half becomes the replacement character.
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        _ => {
                            self.at -= 1;
                            return Err(self.syntax());
                        }
                    }
                }
                0x00..=0x1F => return Err(self.syntax()),
                _ => {
                    // A whole character, even a multi-byte one.
                    let rest = &self.text[self.at..];
                    let c = rest.chars().next().ok_or_else(|| self.syntax())?;
                    out.push(c);
                    self.at += c.len_utf8();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conserva_ordine_e_numeri() {
        let value = parse("{\"b\": 1.50, \"a\": [1e5, -0, true, null, \"Città\\n\"]}").unwrap();
        assert_eq!(value.as_object().unwrap().keys(), ["b", "a"]);
        assert_eq!(
            value.text(),
            "{\"b\":1.50,\"a\":[1e5,-0,true,null,\"Città\\n\"]}"
        );
        assert_eq!(
            value.pretty("  ", "\n"),
            "{\n  \"b\": 1.50,\n  \"a\": [\n    1e5,\n    -0,\n    true,\n    null,\n    \"Città\\n\"\n  ]\n}"
        );
    }

    #[test]
    fn rifiuta_commenti_virgole_e_chiavi_doppie() {
        assert_eq!(parse("{\n  // no\n}"), Err(JsonError::Syntax { line: 2 }));
        assert_eq!(parse("{\"a\": 1,}"), Err(JsonError::Syntax { line: 1 }));
        assert_eq!(
            parse("{\"a\": 1,\n\"a\": 2}"),
            Err(JsonError::Repeated { line: 2 })
        );
        assert!(parse("01").is_err());
        assert!(parse("\"\u{1}\"").is_err());
        assert_eq!(
            parse("\"\\ud83d\\ude00\"").unwrap(),
            Json::String("😀".into())
        );
    }
}

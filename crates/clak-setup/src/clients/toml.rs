//! A hand-written TOML 1.0 reader, with no dependencies: it only serves to **verify**
//! Codex's `config.toml` before and after touching it. If something doesn't add up, it
//! stops: better not to write a file we haven't fully understood.
//!
//! It honors the rules that matter for not getting the entry wrong: dotted keys,
//! tables defined only once, closed inline tables, arrays of tables.
//! Strings, arrays and tables are real; everything else (numbers, dates, booleans) stays
//! the text it is written with, after checking its shape.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TomlValue {
    String(String),
    Scalar(String),
    Array(Vec<TomlValue>),
    Table(TomlTable),
}

pub(crate) type TomlTable = BTreeMap<String, TomlValue>;

impl TomlValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            TomlValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&TomlTable> {
        match self {
            TomlValue::Table(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[TomlValue]> {
        match self {
            TomlValue::Array(a) => Some(a),
            _ => None,
        }
    }

    fn is_table(&self) -> bool {
        matches!(self, TomlValue::Table(_))
    }

    fn is_array(&self) -> bool {
        matches!(self, TomlValue::Array(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TomlError(pub String);

impl fmt::Display for TomlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

type Path = Vec<String>;

/// The whole document, as a table.
pub(crate) fn parse(text: &str) -> Result<TomlTable, TomlError> {
    // "\r\n" counts as "\n": enough to verify the file, and lines are counted the same.
    let mut parser = Parser::new(&text.replace("\r\n", "\n"));
    parser.document()?;
    Ok(match parser.root {
        TomlValue::Table(t) => t,
        _ => TomlTable::new(),
    })
}

struct Parser {
    chars: Vec<char>,
    at: usize,
    line: usize,
    root: TomlValue,
    /// The tables opened with `[name]`: a second time is an error.
    defined: HashSet<Path>,
    /// Inline tables and static arrays: they can't be extended later.
    closed: HashSet<Path>,
    /// The arrays of tables (`[[name]]`), with how many each has: a path goes through them.
    arrays: HashMap<Path, usize>,
}

impl Parser {
    fn new(text: &str) -> Parser {
        Parser {
            chars: text.chars().collect(),
            at: 0,
            line: 1,
            root: TomlValue::Table(TomlTable::new()),
            defined: HashSet::new(),
            closed: HashSet::new(),
            arrays: HashMap::new(),
        }
    }

    fn fail(&self, what: impl fmt::Display) -> TomlError {
        TomlError(format!("line {}: {what}", self.line))
    }

    fn current(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn advance(&mut self) {
        if self.chars[self.at] == '\n' {
            self.line += 1;
        }
        self.at += 1;
    }

    fn skip_blank(&mut self) {
        while matches!(self.current(), Some(' ' | '\t')) {
            self.advance();
        }
    }

    fn skip_comment(&mut self) {
        if self.current() != Some('#') {
            return;
        }
        while self.current().is_some_and(|c| c != '\n') {
            self.advance();
        }
    }

    /// End of line: spaces, a comment, a newline or the end of the file.
    fn end_of_line(&mut self) -> Result<(), TomlError> {
        self.skip_blank();
        self.skip_comment();
        match self.current() {
            None => Ok(()),
            Some('\n') => {
                self.advance();
                Ok(())
            }
            Some(_) => Err(self.fail("unexpected text after a value")),
        }
    }

    fn document(&mut self) -> Result<(), TomlError> {
        let mut table: Path = Vec::new();
        loop {
            while matches!(self.current(), Some(' ' | '\t' | '\n')) {
                self.advance();
            }
            self.skip_comment();
            let Some(c) = self.current() else {
                return Ok(());
            };
            if c == '\n' {
                continue;
            }
            if c == '[' {
                self.advance();
                let many = self.current() == Some('[');
                if many {
                    self.advance();
                }
                self.skip_blank();
                let key = self.key_path()?;
                self.skip_blank();
                if self.current() != Some(']') {
                    return Err(self.fail("unclosed header"));
                }
                self.advance();
                if many {
                    if self.current() != Some(']') {
                        return Err(self.fail("unclosed header"));
                    }
                    self.advance();
                }
                self.end_of_line()?;
                table = self.open(key, many)?;
            } else {
                let key = self.key_path()?;
                self.skip_blank();
                if self.current() != Some('=') {
                    return Err(self.fail("expected ="));
                }
                self.advance();
                self.skip_blank();
                let value = self.value()?;
                let mut path = table.clone();
                path.extend(key);
                self.assign(path, value)?;
                self.end_of_line()?;
            }
        }
    }

    // keys

    fn key_path(&mut self) -> Result<Path, TomlError> {
        let mut parts = vec![self.simple_key()?];
        loop {
            self.skip_blank();
            if self.current() != Some('.') {
                return Ok(parts);
            }
            self.advance();
            self.skip_blank();
            parts.push(self.simple_key()?);
        }
    }

    fn simple_key(&mut self) -> Result<String, TomlError> {
        match self.current() {
            Some('"') => self.basic_string(),
            Some('\'') => self.literal_string(),
            _ => {
                let mut key = String::new();
                while let Some(c) = self
                    .current()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                {
                    key.push(c);
                    self.advance();
                }
                if key.is_empty() {
                    return Err(self.fail("expected a key"));
                }
                Ok(key)
            }
        }
    }

    // structure

    /// The "real" path of a key: inside an array of tables it enters the last one.
    fn resolved(&self, path: &[String]) -> Path {
        let mut out: Path = Vec::new();
        let mut walked: Path = Vec::new();
        for part in path {
            out.push(part.clone());
            walked.push(part.clone());
            if let Some(count) = self.arrays.get(&walked) {
                out.push(format!("#{}", count - 1));
            }
        }
        out
    }

    fn get(&self, path: &[String]) -> Option<&TomlValue> {
        let mut node = &self.root;
        for part in path {
            let index = part.strip_prefix('#').and_then(|n| n.parse::<usize>().ok());
            match (node, index) {
                (TomlValue::Array(items), Some(i)) if i < items.len() => node = &items[i],
                (TomlValue::Table(t), _) => node = t.get(part)?,
                _ => return None,
            }
        }
        Some(node)
    }

    /// Writes a value at the end of an already real path, creating the missing tables.
    fn put(&mut self, path: &[String], value: TomlValue) {
        fn insert(node: &mut TomlValue, rest: &[String], value: TomlValue) {
            let Some(head) = rest.first() else {
                *node = value;
                return;
            };
            if let (TomlValue::Array(items), Some(i)) = (
                &mut *node,
                head.strip_prefix('#').and_then(|n| n.parse::<usize>().ok()),
            ) {
                if i == items.len() {
                    items.push(TomlValue::Table(TomlTable::new()));
                }
                if i < items.len() {
                    insert(&mut items[i], &rest[1..], value);
                }
                return;
            }
            if !node.is_table() {
                *node = TomlValue::Table(TomlTable::new());
            }
            if let TomlValue::Table(table) = node {
                let child = table
                    .entry(head.clone())
                    .or_insert_with(|| TomlValue::Table(TomlTable::new()));
                insert(child, &rest[1..], value);
            }
        }
        insert(&mut self.root, path, value);
    }

    /// Every prefix must be a table that can still be extended.
    fn check_prefixes(&self, path: &[String]) -> Result<(), TomlError> {
        for n in 1..path.len().max(1) {
            let prefix = &path[..n];
            if self.closed.contains(prefix) {
                return Err(self.fail(format!("“{}” is already closed", prefix.join("."))));
            }
            if let Some(existing) = self.get(&self.resolved(prefix))
                && !existing.is_table()
                && !self.arrays.contains_key(prefix)
            {
                return Err(self.fail(format!("“{}” is not a table", prefix.join("."))));
            }
        }
        Ok(())
    }

    fn open(&mut self, path: Path, many: bool) -> Result<Path, TomlError> {
        self.check_prefixes(&path)?;
        let name = path.join(".");
        if self.closed.contains(&path) {
            return Err(self.fail(format!("“{name}” is already closed")));
        }
        if many {
            let (last, init) = path.split_last().expect("a key has at least one part");
            let mut slot = self.resolved(init);
            slot.push(last.clone());
            if self.get(&slot).is_some() && !self.arrays.contains_key(&path) {
                return Err(self.fail(format!("“{name}” is already defined")));
            }
            let count = self.arrays.get(&path).copied().unwrap_or(0);
            if count == 0 {
                self.put(&slot, TomlValue::Array(Vec::new()));
            }
            self.arrays.insert(path.clone(), count + 1);
            let real = self.resolved(&path);
            self.put(&real, TomlValue::Table(TomlTable::new()));
            // The previous array element's subtables don't apply to the new one.
            self.defined
                .retain(|d| !(d.len() > path.len() && d.starts_with(&path)));
        } else {
            if self.defined.contains(&path) || self.arrays.contains_key(&path) {
                return Err(self.fail(format!("“{name}” is already defined")));
            }
            let real = self.resolved(&path);
            match self.get(&real) {
                Some(existing) if !existing.is_table() => {
                    return Err(self.fail(format!("“{name}” is not a table")));
                }
                Some(_) => {}
                None => self.put(&real, TomlValue::Table(TomlTable::new())),
            }
            self.defined.insert(path.clone());
        }
        Ok(path)
    }

    fn assign(&mut self, path: Path, value: TomlValue) -> Result<(), TomlError> {
        self.check_prefixes(&path)?;
        let real = self.resolved(&path);
        if self.get(&real).is_some() {
            return Err(self.fail(format!("“{}” is already defined", path.join("."))));
        }
        let closes = value.is_table() || value.is_array();
        self.put(&real, value);
        if closes {
            self.closed.insert(path);
        }
        Ok(())
    }

    // values

    fn value(&mut self) -> Result<TomlValue, TomlError> {
        match self.current() {
            None | Some('\n' | '#') => Err(self.fail("missing value")),
            Some('"') => Ok(TomlValue::String(self.basic_string()?)),
            Some('\'') => Ok(TomlValue::String(self.literal_string()?)),
            Some('[') => self.array(),
            Some('{') => self.inline_table(),
            Some(_) => Ok(TomlValue::Scalar(self.scalar()?)),
        }
    }

    fn skip_all(&mut self) {
        loop {
            while matches!(self.current(), Some(' ' | '\t' | '\n')) {
                self.advance();
            }
            if self.current() != Some('#') {
                return;
            }
            self.skip_comment();
        }
    }

    fn array(&mut self) -> Result<TomlValue, TomlError> {
        self.advance();
        let mut items = Vec::new();
        loop {
            self.skip_all();
            if self.current() == Some(']') {
                self.advance();
                return Ok(TomlValue::Array(items));
            }
            items.push(self.value()?);
            self.skip_all();
            if self.current() == Some(',') {
                self.advance();
                continue;
            }
            if self.current() != Some(']') {
                return Err(self.fail("unclosed list"));
            }
            self.advance();
            return Ok(TomlValue::Array(items));
        }
    }

    fn inline_table(&mut self) -> Result<TomlValue, TomlError> {
        self.advance();
        // An inline table is a world of its own: it is read with a new parser on its keys.
        let mut table = Parser::new("");
        self.skip_blank();
        if self.current() == Some('}') {
            self.advance();
            return Ok(table.root);
        }
        loop {
            self.skip_blank();
            let key = self.key_path()?;
            self.skip_blank();
            if self.current() != Some('=') {
                return Err(self.fail("expected ="));
            }
            self.advance();
            self.skip_blank();
            let value = self.value()?;
            let name = key.join(".");
            if table.assign(key, value).is_err() {
                return Err(self.fail(format!("“{name}” repeated in an inline table")));
            }
            self.skip_blank();
            if self.current() == Some(',') {
                self.advance();
                continue;
            }
            if self.current() != Some('}') {
                return Err(self.fail("unclosed inline table"));
            }
            self.advance();
            return Ok(table.root);
        }
    }

    fn scalar(&mut self) -> Result<String, TomlError> {
        let mut text = String::new();
        while let Some(c) = self
            .current()
            .filter(|c| !matches!(c, ',' | ']' | '}' | '#' | '\n'))
        {
            text.push(c);
            self.advance();
        }
        let text = text.trim_matches([' ', '\t']).to_owned();
        if !valid_scalar(&text) {
            return Err(self.fail(format!("invalid value: {text}")));
        }
        Ok(text)
    }

    fn starts(&self, s: &str) -> bool {
        let needle: Vec<char> = s.chars().collect();
        self.chars
            .get(self.at..self.at + needle.len())
            .is_some_and(|slice| slice == needle.as_slice())
    }

    /// After a multi-line string closes, an extra quote (at most two) is part of the text.
    fn extra_quotes(&self, quote: char) -> bool {
        let n = self.chars[self.at..]
            .iter()
            .take_while(|&&c| c == quote)
            .count();
        (1..=2).contains(&n)
    }

    fn trim_first_newline(&mut self) {
        if self.current() == Some('\n') {
            self.advance();
        }
    }

    fn literal_string(&mut self) -> Result<String, TomlError> {
        if self.starts("'''") {
            self.at += 3;
            self.trim_first_newline();
            let mut text = String::new();
            while !self.starts("'''") {
                let Some(c) = self.current() else {
                    return Err(self.fail("unclosed string"));
                };
                text.push(c);
                self.advance();
            }
            self.at += 3;
            // Up to two extra quotes belong to the text: '''a''''' is "a''".
            while self.current() == Some('\'') && self.extra_quotes('\'') {
                text.push('\'');
                self.advance();
            }
            return Ok(text);
        }
        self.advance();
        let mut text = String::new();
        while let Some(c) = self.current().filter(|c| *c != '\'') {
            if c == '\n' {
                return Err(self.fail("unclosed string"));
            }
            text.push(c);
            self.advance();
        }
        if self.current() != Some('\'') {
            return Err(self.fail("unclosed string"));
        }
        self.advance();
        Ok(text)
    }

    fn basic_string(&mut self) -> Result<String, TomlError> {
        let multiline = self.starts("\"\"\"");
        self.at += if multiline { 3 } else { 1 };
        if multiline {
            self.trim_first_newline();
        }
        let mut text = String::new();
        loop {
            let Some(c) = self.current() else {
                return Err(self.fail("unclosed string"));
            };
            if multiline && self.starts("\"\"\"") {
                self.at += 3;
                while self.current() == Some('"') && self.extra_quotes('"') {
                    text.push('"');
                    self.advance();
                }
                return Ok(text);
            }
            if !multiline && c == '"' {
                self.advance();
                return Ok(text);
            }
            if !multiline && c == '\n' {
                return Err(self.fail("unclosed string"));
            }
            if c == '\\' {
                self.advance();
                let Some(e) = self.current() else {
                    return Err(self.fail("unclosed string"));
                };
                if multiline && matches!(e, ' ' | '\t' | '\n') {
                    // "\" at end of line: newlines and spaces are eaten up to the next character.
                    while matches!(self.current(), Some(' ' | '\t' | '\n')) {
                        self.advance();
                    }
                    continue;
                }
                self.advance();
                match e {
                    'b' => text.push('\u{08}'),
                    't' => text.push('\t'),
                    'n' => text.push('\n'),
                    'f' => text.push('\u{0C}'),
                    'r' => text.push('\r'),
                    'e' => text.push('\u{1B}'),
                    '"' => text.push('"'),
                    '\\' => text.push('\\'),
                    'u' | 'U' => {
                        let count = if e == 'u' { 4 } else { 8 };
                        let digits: String = self
                            .chars
                            .get(self.at..self.at + count)
                            .map(|s| s.iter().collect())
                            .unwrap_or_default();
                        let scalar = (digits.chars().count() == count
                            && digits.chars().all(|d| d.is_ascii_hexdigit()))
                        .then(|| u32::from_str_radix(&digits, 16).ok())
                        .flatten()
                        .and_then(char::from_u32);
                        let Some(scalar) = scalar else {
                            return Err(self.fail("invalid escape"));
                        };
                        self.at += count;
                        text.push(scalar);
                    }
                    _ => return Err(self.fail("invalid escape")),
                }
                continue;
            }
            text.push(c);
            self.advance();
        }
    }
}

/// Numbers, booleans, dates and times as TOML allows them.
pub(crate) fn valid_scalar(text: &str) -> bool {
    let s = text.as_bytes();
    let whole = |f: &dyn Fn(&[u8], &mut usize) -> bool| {
        let mut i = 0;
        f(s, &mut i) && i == s.len()
    };
    matches!(text, "true" | "false")
        || matches!(text.trim_start_matches(['+', '-']), "inf" | "nan")
            && text.len() - text.trim_start_matches(['+', '-']).len() <= 1
        || whole(&|s, i| {
            sign(s, i);
            decimal(s, i)
        })
        || whole(&|s, i| prefixed(s, i, b"0x", u8::is_ascii_hexdigit))
        || whole(&|s, i| prefixed(s, i, b"0o", |b| (b'0'..=b'7').contains(b)))
        || whole(&|s, i| prefixed(s, i, b"0b", |b| *b == b'0' || *b == b'1'))
        || whole(&float)
        || whole(&datetime)
        || whole(&time)
}

fn sign(s: &[u8], i: &mut usize) {
    if matches!(s.get(*i), Some(b'+' | b'-')) {
        *i += 1;
    }
}

/// `d(_?d)*`.
fn underscored(s: &[u8], i: &mut usize, digit: impl Fn(&u8) -> bool) -> bool {
    if !s.get(*i).is_some_and(&digit) {
        return false;
    }
    *i += 1;
    loop {
        if s.get(*i).is_some_and(&digit) {
            *i += 1;
        } else if s.get(*i) == Some(&b'_') && s.get(*i + 1).is_some_and(&digit) {
            *i += 2;
        } else {
            return true;
        }
    }
}

/// `0|[1-9](_?[0-9])*`.
fn decimal(s: &[u8], i: &mut usize) -> bool {
    match s.get(*i) {
        Some(b'0') => {
            *i += 1;
            true
        }
        Some(b'1'..=b'9') => underscored(s, i, u8::is_ascii_digit),
        _ => false,
    }
}

fn prefixed(s: &[u8], i: &mut usize, prefix: &[u8], digit: impl Fn(&u8) -> bool) -> bool {
    if !s[*i..].starts_with(prefix) {
        return false;
    }
    *i += prefix.len();
    underscored(s, i, digit)
}

fn float(s: &[u8], i: &mut usize) -> bool {
    sign(s, i);
    if !decimal(s, i) {
        return false;
    }
    if s.get(*i) == Some(&b'.') {
        *i += 1;
        if !underscored(s, i, u8::is_ascii_digit) {
            return false;
        }
    }
    if matches!(s.get(*i), Some(b'e' | b'E')) {
        *i += 1;
        sign(s, i);
        if !underscored(s, i, u8::is_ascii_digit) {
            return false;
        }
    }
    true
}

fn fixed_digits(s: &[u8], i: &mut usize, n: usize) -> bool {
    if s.len() >= *i + n && s[*i..*i + n].iter().all(u8::is_ascii_digit) {
        *i += n;
        true
    } else {
        false
    }
}

fn literal(s: &[u8], i: &mut usize, byte: u8) -> bool {
    if s.get(*i) == Some(&byte) {
        *i += 1;
        true
    } else {
        false
    }
}

/// `hh:mm(:ss(.f+)?)?`.
fn time(s: &[u8], i: &mut usize) -> bool {
    if !(fixed_digits(s, i, 2) && literal(s, i, b':') && fixed_digits(s, i, 2)) {
        return false;
    }
    let back = *i;
    if literal(s, i, b':') {
        if !fixed_digits(s, i, 2) {
            *i = back;
            return true;
        }
        let dot = *i;
        if literal(s, i, b'.') && !underscore_free_digits(s, i) {
            *i = dot;
        }
    }
    true
}

fn underscore_free_digits(s: &[u8], i: &mut usize) -> bool {
    let start = *i;
    while s.get(*i).is_some_and(u8::is_ascii_digit) {
        *i += 1;
    }
    *i > start
}

/// `yyyy-mm-dd([Tt ]time([Zz]|±hh:mm)?)?`.
fn datetime(s: &[u8], i: &mut usize) -> bool {
    if !(fixed_digits(s, i, 4)
        && literal(s, i, b'-')
        && fixed_digits(s, i, 2)
        && literal(s, i, b'-')
        && fixed_digits(s, i, 2))
    {
        return false;
    }
    let back = *i;
    if matches!(s.get(*i), Some(b'T' | b't' | b' ')) {
        *i += 1;
        if !time(s, i) {
            *i = back;
            return true;
        }
        let zone = *i;
        if matches!(s.get(*i), Some(b'Z' | b'z')) {
            *i += 1;
        } else if matches!(s.get(*i), Some(b'+' | b'-')) {
            *i += 1;
            if !(fixed_digits(s, i, 2) && literal(s, i, b':') && fixed_digits(s, i, 2)) {
                *i = zone;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tables_dotted_keys_and_arrays_of_tables() {
        let table = parse(
            "# comment\nmodel = \"o3\"\nn = 1_000\nf = -1.5e3\nd = 1979-05-27T07:32:00Z\nok = true\n\n[mcp_servers.clak]\ncommand = 'C:\\x\\shim.exe'\nargs = [\"-v\", # c\n  'b',\n]\n\n[[p]]\na = 1\n[[p]]\na = 2\n[p.sub]\nb = \"\"\"\nx\\\n   y\"\"\"\n",
        )
        .unwrap();
        let clak = table["mcp_servers"].as_table().unwrap()["clak"]
            .as_table()
            .unwrap();
        assert_eq!(clak["command"].as_str(), Some("C:\\x\\shim.exe"));
        assert_eq!(clak["args"].as_array().unwrap().len(), 2);
        let p = table["p"].as_array().unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(
            p[1].as_table().unwrap()["sub"].as_table().unwrap()["b"].as_str(),
            Some("xy")
        );
    }

    #[test]
    fn rejects_what_is_not_toml() {
        for text in [
            "model = \n",
            "[broken\n",
            "a = 1\na = 2\n",
            "[t]\n[t]\n",
            "x = { a = 1 }\n[x.b]\n",
            "x = { a = 1 }\nx.b = 2\n",
            "a = 01\n",
            "a = \"\\q\"\n",
            "a = 'unclosed\n",
            "a = 1 b\n",
            "mcp_servers = { other = { command = \"x\" } }\n[mcp_servers.clak]\ncommand = 'y'\n",
        ] {
            assert!(parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn scalar_values() {
        for ok in [
            "true",
            "1",
            "-0",
            "+inf",
            "nan",
            "0xDEAD_beef",
            "0o17",
            "0b101",
            "3.14",
            "1e10",
            "6.626e-34",
            "1979-05-27",
            "07:32:00.999",
            "1979-05-27 07:32:00+01:00",
        ] {
            assert!(valid_scalar(ok), "{ok}");
        }
        for bad in [
            "01",
            "1__0",
            "_1",
            "1.",
            ".5",
            "True",
            "0x",
            "++1",
            "1979-5-27",
            "abc",
        ] {
            assert!(!valid_scalar(bad), "{bad}");
        }
    }
}

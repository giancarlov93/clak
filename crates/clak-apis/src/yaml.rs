//! A hand-written YAML reader, just enough for OpenAPI and Swagger documents: more often
//! than not they're written in YAML. The same on both systems, so a document reads the
//! same way. The result is the JSON tree, because that's what an OpenAPI document becomes
//! anyway.
//!
//! Only the first document is read. The rules are those of YAML 1.2 for what real
//! documents use; the rest (complex keys, tabs as indentation) stops with an error instead
//! of guessing. Duplicate keys are an error, as in the wire JSON: there's no choosing
//! between "first wins" and "last wins".
//!
//! Numbers become JSON numbers: 64-bit integers when they fit, floating point otherwise
//! (`3.10` becomes `3.1`).

use serde_json::{Map, Number, Value};
use std::collections::HashMap;
use std::fmt;

/// The text isn't YAML, or it's YAML this reader doesn't accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YamlError {
    /// What's wrong, without the where.
    pub reason: String,
    /// The line (from 1) where the reader stopped.
    pub line: Option<usize>,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "{} at line {line}", self.reason),
            None => f.write_str(&self.reason),
        }
    }
}

impl std::error::Error for YamlError {}

/// Reads the first document of a YAML text.
pub fn parse(text: &str) -> Result<Value, YamlError> {
    // The recursive descent uses the stack, and 256 levels (in a debug build, where every
    // call is heavy) don't fit in an arbitrary thread's stack: reading happens on a thread
    // with its own stack, so the depth cap really protects, whoever the caller is.
    let owned = text.to_owned();
    let reader = std::thread::Builder::new()
        .name("clak-yaml".into())
        .stack_size(16 << 20)
        .spawn(move || Parser::new(&owned).document());
    match reader {
        Ok(handle) => handle.join().unwrap_or_else(|_| {
            Err(YamlError {
                reason: "the YAML reader failed".into(),
                line: None,
            })
        }),
        // Without a new thread, read here: the depth cap still holds.
        Err(_) => Parser::new(text).document(),
    }
}

const MAX_DEPTH: usize = 256;
/// An alias repeats the whole node it points to: without a cap, ten lines of nested
/// aliases make a billion nodes.
const MAX_NODES: usize = 2_000_000;

#[derive(Clone, Copy)]
struct Mark {
    pos: usize,
    line: usize,
    line_start: usize,
}

struct Anchor {
    value: Value,
    /// How many nodes it's worth: that many are counted on every alias.
    size: usize,
    /// How many levels it has below itself: an alias must not break the depth limit.
    depth: usize,
}

/// A recursive descent by lines and indentation. The text starts with a fake line break:
/// that way every line, even the first, is reached the same way, after a `\n`.
struct Parser {
    s: Vec<char>,
    pos: usize,
    line: usize,
    line_start: usize,
    /// Saw `---` or `...` after the first document: the rest isn't read.
    ended: bool,
    anchors: HashMap<String, Anchor>,
    produced: usize,
    deepest: usize,
}

type Res<T> = Result<T, YamlError>;

fn is_space(c: Option<char>) -> bool {
    matches!(c, Some(' ' | '\t'))
}

fn is_separator(c: Option<char>) -> bool {
    matches!(c, None | Some(' ' | '\t' | '\n'))
}

fn is_flow_indicator(c: Option<char>) -> bool {
    matches!(c, Some(',' | '[' | ']' | '{' | '}'))
}

fn is_string_tag(tag: Option<&str>) -> bool {
    matches!(tag, Some("!!str" | "!<tag:yaml.org,2002:str>"))
}

fn object_with(key: String, value: Value) -> Value {
    let mut o = Map::new();
    o.insert(key, value);
    Value::Object(o)
}

impl Parser {
    fn new(text: &str) -> Parser {
        let mut s = vec!['\n'];
        let mut after_cr = false;
        for c in text.chars() {
            if c == '\r' {
                s.push('\n');
            } else if !(c == '\n' && after_cr) {
                s.push(c);
            }
            after_cr = c == '\r';
        }
        if s.len() > 1 && s[1] == '\u{FEFF}' {
            s.remove(1);
        }
        Parser {
            s,
            pos: 0,
            line: 0,
            line_start: 0,
            ended: false,
            anchors: HashMap::new(),
            produced: 0,
            deepest: 0,
        }
    }

    // ---- cursor ----

    fn peek_at(&self, k: usize) -> Option<char> {
        self.s.get(self.pos + k).copied()
    }

    fn peek(&self) -> Option<char> {
        self.peek_at(0)
    }

    fn column(&self) -> isize {
        (self.pos - self.line_start) as isize
    }

    fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            line: self.line,
            line_start: self.line_start,
        }
    }

    fn reset(&mut self, m: Mark) {
        self.pos = m.pos;
        self.line = m.line;
        self.line_start = m.line_start;
    }

    fn advance(&mut self) {
        if self.s[self.pos] == '\n' {
            self.line += 1;
            self.line_start = self.pos + 1;
        }
        self.pos += 1;
    }

    fn fail(&self, reason: impl Into<String>) -> YamlError {
        YamlError {
            reason: reason.into(),
            line: Some(self.line),
        }
    }

    fn text(&self, from: usize, to: usize) -> String {
        self.s[from..to].iter().collect()
    }

    fn at_line_end(&self) -> bool {
        matches!(self.peek(), None | Some('\n'))
    }

    fn skip_spaces(&mut self) {
        while is_space(self.peek()) {
            self.advance();
        }
    }

    fn skip_line(&mut self) {
        while self.peek().is_some_and(|c| c != '\n') {
            self.advance();
        }
    }

    /// `#` opens a comment only at the start of a line or after a space: `a#b` is text.
    fn skip_comment(&mut self) {
        if self.peek() == Some('#')
            && (self.pos == self.line_start || is_space(Some(self.s[self.pos - 1])))
        {
            self.skip_line();
        }
    }

    fn is_dash_entry(&self) -> bool {
        self.peek() == Some('-') && is_separator(self.peek_at(1))
    }

    fn is_marker(&self) -> bool {
        if self.column() != 0 || self.pos + 2 >= self.s.len() || !is_separator(self.peek_at(3)) {
            return false;
        }
        let c = self.s[self.pos];
        (c == '-' || c == '.') && self.s[self.pos + 1] == c && self.s[self.pos + 2] == c
    }

    /// Moves the cursor to the first character of the next line with content and returns
    /// its column; nothing at the end of the document. If already there, it doesn't move:
    /// whoever read a block stopped right there to see the indentation.
    fn next_content_line(&mut self, prelude: bool) -> Res<Option<isize>> {
        if self.ended {
            return Ok(None);
        }
        if self.pos < self.s.len()
            && !matches!(self.s[self.pos], '\n' | ' ' | '\t' | '#')
            && self.s[self.line_start..self.pos].iter().all(|&c| c == ' ')
        {
            return Ok(Some(self.column()));
        }
        self.skip_spaces();
        self.skip_comment();
        loop {
            let Some(c) = self.peek() else {
                return Ok(None);
            };
            if c != '\n' {
                return Err(self.fail("unexpected text"));
            }
            self.advance();
            let mut tab = false;
            while let Some(d) = self.peek().filter(|&d| is_space(Some(d))) {
                if d == '\t' {
                    tab = true;
                }
                self.advance();
            }
            let Some(d) = self.peek() else {
                return Ok(None);
            };
            if d == '\n' {
                continue;
            }
            if d == '#' {
                self.skip_comment();
                continue;
            }
            if tab {
                return Err(self.fail("tabs are not allowed for indentation"));
            }
            if prelude && self.column() == 0 && d == '%' {
                // %YAML, %TAG: they change nothing here
                self.skip_line();
                continue;
            }
            if self.is_marker() {
                if prelude {
                    if d == '-' {
                        return Ok(Some(0));
                    }
                    self.skip_line();
                    continue;
                }
                self.ended = true;
                return Ok(None);
            }
            return Ok(Some(self.column()));
        }
    }

    fn end_of_line(&mut self) -> Res<()> {
        self.skip_spaces();
        self.skip_comment();
        match self.peek() {
            None | Some('\n') => Ok(()),
            Some(':') => Err(self.fail("mapping values are not allowed here")),
            Some(_) => Err(self.fail("unexpected text after a value")),
        }
    }

    /// Counts a produced node: both alias avalanches and nesting are stopped here.
    fn made(&mut self, depth: usize) -> Res<()> {
        if depth >= MAX_DEPTH {
            return Err(self.fail("nested too deeply"));
        }
        self.produced += 1;
        if self.produced > MAX_NODES {
            return Err(self.fail(format!("too many nodes (more than {MAX_NODES})")));
        }
        self.deepest = self.deepest.max(depth);
        Ok(())
    }

    // ---- document and blocks ----

    fn document(&mut self) -> Res<Value> {
        if self.next_content_line(true)?.is_none() {
            return Ok(Value::Null);
        }
        let value = if self.is_marker() {
            self.pos += 3;
            self.block_node(-1, 0, false, false)?
        } else {
            self.block_node(-1, 0, true, false)?
        };
        if self.next_content_line(false)?.is_some() {
            return Err(self.fail("unexpected text"));
        }
        Ok(value)
    }

    /// A node that starts where we are: after `key:`, after `- `, at the document start.
    /// `parent` is the parent's column: what's on the following lines belongs to this node
    /// only if it's further right (or, for a list under a key, at the same column:
    /// `key:\n- a` is the common way to write it). `compact` says whether a mapping or a
    /// list can start on the same line, as after `- `.
    fn block_node(
        &mut self,
        parent: isize,
        depth: usize,
        compact: bool,
        same_indent_sequence: bool,
    ) -> Res<Value> {
        self.skip_spaces();
        let start = self.column();
        let (anchor, tag) = self.properties()?;
        self.skip_comment();
        let before = (self.produced, self.deepest);
        self.deepest = depth;
        let value = if !self.at_line_end() {
            if compact {
                self.node_here(start, parent, depth, tag.as_deref())?
            } else {
                self.inline_node(parent, depth, tag.as_deref())?
            }
        } else {
            match self.next_content_line(false)? {
                Some(col)
                    if col > parent
                        || (same_indent_sequence && col == parent && self.is_dash_entry()) =>
                {
                    self.node_here(col, parent, depth, tag.as_deref())?
                }
                _ => {
                    self.made(depth)?;
                    if is_string_tag(tag.as_deref()) {
                        Value::String(String::new())
                    } else {
                        Value::Null
                    }
                }
            }
        };
        self.remember(anchor, &value, before, depth);
        Ok(value)
    }

    fn remember(
        &mut self,
        anchor: Option<String>,
        value: &Value,
        before: (usize, usize),
        depth: usize,
    ) {
        if let Some(anchor) = anchor {
            let entry = Anchor {
                value: value.clone(),
                size: self.produced - before.0,
                depth: self.deepest - depth,
            };
            self.anchors.insert(anchor, entry);
        }
        self.deepest = self.deepest.max(before.1);
    }

    fn node_here(
        &mut self,
        col: isize,
        parent: isize,
        depth: usize,
        tag: Option<&str>,
    ) -> Res<Value> {
        if self.is_dash_entry() {
            let column = self.column();
            return self.block_sequence(column, depth);
        }
        if self.peek() == Some('?') && is_separator(self.peek_at(1)) {
            return Err(self.fail("complex keys are not supported"));
        }
        if self.is_implicit_key() {
            return self.block_mapping(col, depth);
        }
        self.inline_node(parent, depth, tag)
    }

    /// A value that sits on the line (and maybe continues below): never a block mapping or
    /// block list, which can't start here.
    fn inline_node(&mut self, parent: isize, depth: usize, tag: Option<&str>) -> Res<Value> {
        let Some(c) = self.peek() else {
            return Err(self.fail("unexpected end of text"));
        };
        let value = match c {
            '|' | '>' => {
                self.made(depth)?;
                return Ok(Value::String(self.block_scalar(parent)?));
            }
            '[' | '{' => {
                let value = if c == '[' {
                    self.flow_sequence(depth)?
                } else {
                    self.flow_mapping(depth)?
                };
                self.skip_spaces();
                if self.peek() == Some(':') {
                    return Err(self.fail("a mapping key must be a scalar"));
                }
                value
            }
            '"' | '\'' => {
                self.made(depth)?;
                Value::String(self.quoted()?)
            }
            '*' => self.alias(depth)?,
            _ => {
                self.made(depth)?;
                let text = self.plain_scalar(parent, false)?;
                resolve(&text, tag)
            }
        };
        self.end_of_line()?;
        Ok(value)
    }

    fn block_mapping(&mut self, c: isize, depth: usize) -> Res<Value> {
        self.made(depth)?;
        let mut object = Map::new();
        loop {
            let key_line = self.line;
            let key = self.mapping_key()?;
            if object.contains_key(&key) {
                return Err(YamlError {
                    reason: format!("duplicate key “{key}”"),
                    line: Some(key_line),
                });
            }
            let value = self.block_node(c, depth + 1, false, true)?;
            object.insert(key, value);
            match self.next_content_line(false)? {
                Some(col) if col >= c => {
                    if col != c || self.is_dash_entry() {
                        return Err(self.fail("bad indentation"));
                    }
                }
                _ => break,
            }
        }
        Ok(Value::Object(object))
    }

    fn block_sequence(&mut self, c: isize, depth: usize) -> Res<Value> {
        self.made(depth)?;
        let mut items = Vec::new();
        loop {
            self.advance(); // the dash
            items.push(self.block_node(c, depth + 1, true, false)?);
            match self.next_content_line(false)? {
                Some(col) if col >= c => {
                    if col != c {
                        return Err(self.fail("bad indentation"));
                    }
                    // A key at the same column: the list was the value of the previous one.
                    if !self.is_dash_entry() {
                        break;
                    }
                }
                _ => break,
            }
        }
        Ok(Value::Array(items))
    }

    /// Looks ahead without moving: is the line `key: …`?
    fn is_implicit_key(&mut self) -> bool {
        let back = self.mark();
        let answer = self.looks_like_key();
        self.reset(back);
        answer
    }

    fn looks_like_key(&mut self) -> bool {
        if self.properties().is_err() {
            return false;
        }
        let Some(c) = self.peek() else { return false };
        if c == '"' || c == '\'' {
            let start = self.line;
            if self.quoted().is_err() || self.line != start {
                return false;
            }
        } else {
            if !self.can_start_plain(false) {
                return false;
            }
            self.plain_line(false);
        }
        self.skip_spaces();
        self.peek() == Some(':') && is_separator(self.peek_at(1))
    }

    /// A key is the scalar's text: `200:` gives "200", `true:` gives "true".
    fn mapping_key(&mut self) -> Res<String> {
        self.properties()?;
        let Some(c) = self.peek() else {
            return Err(self.fail("unexpected end of text"));
        };
        let key = match c {
            '"' | '\'' => self.quoted()?,
            '?' if is_separator(self.peek_at(1)) => {
                return Err(self.fail("complex keys are not supported"));
            }
            '[' | '{' | '*' => return Err(self.fail("a mapping key must be a scalar")),
            _ => {
                if !self.can_start_plain(false) {
                    return Err(self.fail("could not find expected ':'"));
                }
                self.plain_line(false)
            }
        };
        self.skip_spaces();
        if !(self.peek() == Some(':') && is_separator(self.peek_at(1))) {
            return Err(self.fail("could not find expected ':'"));
        }
        self.advance();
        Ok(key)
    }

    // ---- properties and aliases ----

    /// `&anchor` and `!tag` in front of a node. Tags are ignored, except `!!str`, which asks
    /// for text specifically: `!!str 123` is "123", not 123.
    fn properties(&mut self) -> Res<(Option<String>, Option<String>)> {
        let mut anchor = None;
        let mut tag = None;
        while let Some(c) = self.peek().filter(|&c| c == '&' || c == '!') {
            self.advance();
            let mut name = String::new();
            if c == '!' && self.peek() == Some('<') {
                while let Some(d) = self.peek().filter(|&d| d != '>' && d != '\n') {
                    name.push(d);
                    self.advance();
                }
                if self.peek() != Some('>') {
                    return Err(self.fail("unterminated tag"));
                }
                name.push('>');
                self.advance();
            }
            while let Some(d) = self
                .peek()
                .filter(|&d| !is_separator(Some(d)) && !is_flow_indicator(Some(d)))
            {
                name.push(d);
                self.advance();
            }
            if c == '&' {
                if name.is_empty() {
                    return Err(self.fail("an anchor needs a name"));
                }
                anchor = Some(name);
            } else {
                tag = Some(format!("!{name}"));
            }
            self.skip_spaces();
        }
        Ok((anchor, tag))
    }

    fn alias(&mut self, depth: usize) -> Res<Value> {
        self.advance();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|d| !is_separator(Some(d)) && !is_flow_indicator(Some(d)))
        {
            self.advance();
        }
        let name = self.text(start, self.pos);
        let Some(anchor) = self.anchors.get(&name) else {
            return Err(self.fail(format!("unknown alias “*{name}”")));
        };
        let (size, below, value) = (anchor.size, anchor.depth, anchor.value.clone());
        if depth + below >= MAX_DEPTH {
            return Err(self.fail("nested too deeply"));
        }
        self.produced += size;
        if self.produced > MAX_NODES {
            return Err(self.fail(format!("too many nodes (more than {MAX_NODES})")));
        }
        self.deepest = self.deepest.max(depth + below);
        Ok(value)
    }

    // ---- flow collections ----

    /// Spaces, line breaks and comments: inside `[…]` and `{…}` indentation doesn't count.
    fn flow_skip(&mut self) {
        while let Some(c) = self.peek() {
            if c == ' ' || c == '\t' || c == '\n' {
                self.advance();
            } else if c == '#'
                && (self.pos == self.line_start || is_space(Some(self.s[self.pos - 1])))
            {
                self.skip_line();
            } else {
                return;
            }
        }
    }

    /// A node inside a flow collection, with its text if it's a scalar: needed when it
    /// turns out afterwards to be a key.
    fn flow_node(&mut self, depth: usize) -> Res<(Value, Option<String>)> {
        self.flow_skip();
        let (anchor, tag) = self.properties()?;
        self.flow_skip();
        let Some(c) = self.peek() else {
            return Err(self.fail("unterminated flow collection"));
        };
        let before = (self.produced, self.deepest);
        self.deepest = depth;
        let result = if is_flow_indicator(Some(c)) && c != '[' && c != '{' {
            if anchor.is_none() && tag.is_none() {
                return Err(self.fail(format!("unexpected character “{c}”")));
            }
            self.made(depth)?;
            let empty = if is_string_tag(tag.as_deref()) {
                Value::String(String::new())
            } else {
                Value::Null
            };
            (empty, Some(String::new()))
        } else if c == '?' && is_separator(self.peek_at(1)) {
            return Err(self.fail("complex keys are not supported"));
        } else {
            match c {
                '[' => (self.flow_sequence(depth)?, None),
                '{' => (self.flow_mapping(depth)?, None),
                '*' => (self.alias(depth)?, None),
                '"' | '\'' => {
                    self.made(depth)?;
                    let text = self.quoted()?;
                    (Value::String(text.clone()), Some(text))
                }
                _ => {
                    self.made(depth)?;
                    let text = self.plain_scalar(-1, true)?;
                    (resolve(&text, tag.as_deref()), Some(text))
                }
            }
        };
        self.remember(anchor, &result.0, before, depth);
        Ok(result)
    }

    fn flow_sequence(&mut self, depth: usize) -> Res<Value> {
        let start_line = self.line;
        self.made(depth)?;
        self.advance();
        let mut items = Vec::new();
        loop {
            self.flow_skip();
            let Some(c) = self.peek() else {
                return Err(YamlError {
                    reason: "unterminated flow sequence".into(),
                    line: Some(start_line),
                });
            };
            if c == ']' {
                self.advance();
                return Ok(Value::Array(items));
            }
            let (value, key) = self.flow_node(depth + 1)?;
            self.flow_skip();
            if self.peek() == Some(':') {
                // `[a: 1]`: a pair is a single-key mapping.
                let Some(key) = key else {
                    return Err(self.fail("a mapping key must be a scalar"));
                };
                self.advance();
                self.flow_skip();
                let inner = if matches!(self.peek(), Some(',' | ']')) {
                    Value::Null
                } else {
                    self.flow_node(depth + 2)?.0
                };
                items.push(object_with(key, inner));
                self.flow_skip();
            } else {
                items.push(value);
            }
            match self.peek() {
                Some(',') => self.advance(),
                Some(']') | None => {}
                _ => return Err(self.fail("expected ',' or ']'")),
            }
        }
    }

    fn flow_mapping(&mut self, depth: usize) -> Res<Value> {
        let start_line = self.line;
        self.made(depth)?;
        self.advance();
        let mut object = Map::new();
        loop {
            self.flow_skip();
            let Some(c) = self.peek() else {
                return Err(YamlError {
                    reason: "unterminated flow mapping".into(),
                    line: Some(start_line),
                });
            };
            if c == '}' {
                self.advance();
                return Ok(Value::Object(object));
            }
            let key_line = self.line;
            let Some(key) = self.flow_node(depth + 1)?.1 else {
                return Err(self.fail("a mapping key must be a scalar"));
            };
            self.flow_skip();
            let mut value = Value::Null;
            if self.peek() == Some(':') {
                self.advance();
                self.flow_skip();
                if !matches!(self.peek(), Some(',' | '}')) {
                    value = self.flow_node(depth + 1)?.0;
                }
            }
            if object.contains_key(&key) {
                return Err(YamlError {
                    reason: format!("duplicate key “{key}”"),
                    line: Some(key_line),
                });
            }
            object.insert(key, value);
            self.flow_skip();
            match self.peek() {
                Some(',') => self.advance(),
                Some('}') | None => {}
                _ => return Err(self.fail("expected ',' or '}'")),
            }
        }
    }

    // ---- scalars ----

    fn can_start_plain(&self, flow: bool) -> bool {
        let Some(c) = self.peek().filter(|&c| !is_separator(Some(c))) else {
            return false;
        };
        if c == '-' || c == '?' || c == ':' {
            let next = self.peek_at(1);
            return !(is_separator(next) || (flow && is_flow_indicator(next)));
        }
        !"[]{},#&*!|>'\"%@`".contains(c)
    }

    /// A piece of a plain scalar up to the end of the line or to what closes it (`: `,
    /// ` #`, and in flow `,[]{}`), without trailing spaces.
    fn plain_line(&mut self, flow: bool) -> String {
        let start = self.pos;
        let mut end = self.pos;
        while let Some(c) = self.peek().filter(|&c| c != '\n') {
            if c == ':'
                && (is_separator(self.peek_at(1)) || (flow && is_flow_indicator(self.peek_at(1))))
            {
                break;
            }
            if c == '#' && is_space(Some(self.s[self.pos - 1])) {
                break;
            }
            if flow && is_flow_indicator(Some(c)) {
                break;
            }
            self.advance();
            if !is_space(Some(c)) {
                end = self.pos;
            }
        }
        self.pos = end;
        self.text(start, end)
    }

    /// A plain scalar, possibly over several lines: the following lines, indented more than
    /// the parent, are joined with a space; an empty line between two is a line break.
    fn plain_scalar(&mut self, parent: isize, flow: bool) -> Res<String> {
        if !self.can_start_plain(flow) {
            return Err(match self.peek() {
                Some(c) => self.fail(format!("unexpected character “{c}”")),
                None => self.fail("unexpected end of text"),
            });
        }
        let mut result = self.plain_line(flow);
        loop {
            let back = self.mark();
            self.skip_spaces();
            if self.peek() != Some('\n') {
                self.reset(back);
                break;
            }
            let mut breaks = 0;
            while self.peek() == Some('\n') {
                self.advance();
                breaks += 1;
                while self.peek() == Some(' ') || (flow && self.peek() == Some('\t')) {
                    self.advance();
                }
            }
            if matches!(self.peek(), None | Some('#' | '\t'))
                || (!flow && self.column() <= parent)
                || self.is_marker()
            {
                self.reset(back);
                break;
            }
            let more = self.plain_line(flow);
            if more.is_empty() {
                self.reset(back);
                break;
            }
            if breaks == 1 {
                result.push(' ');
            } else {
                result.push_str(&"\n".repeat(breaks - 1));
            }
            result.push_str(&more);
        }
        Ok(result)
    }

    /// In single or double quotes. A line break becomes a space, an empty line a line
    /// break; spaces around the break are dropped. Only double quotes honor `\`.
    fn quoted(&mut self) -> Res<String> {
        let quote = self.s[self.pos];
        let double = quote == '"';
        let start_line = self.line;
        self.advance();
        let mut out = String::new();
        let mut spaces = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(YamlError {
                    reason: "unterminated quoted scalar".into(),
                    line: Some(start_line),
                });
            };
            if c == quote {
                out.push_str(&spaces);
                self.advance();
                if double || self.peek() != Some('\'') {
                    return Ok(out);
                }
                out.push('\''); // '' inside single quotes
                self.advance();
                spaces.clear();
            } else if is_space(Some(c)) {
                spaces.push(c);
                self.advance();
            } else if c == '\n' {
                spaces.clear();
                let mut breaks = 0;
                while self.peek() == Some('\n') {
                    self.advance();
                    breaks += 1;
                    self.skip_spaces();
                }
                if breaks == 1 {
                    out.push(' ');
                } else {
                    out.push_str(&"\n".repeat(breaks - 1));
                }
            } else {
                out.push_str(&spaces);
                spaces.clear();
                self.advance();
                if double && c == '\\' {
                    self.escape(&mut out)?;
                } else {
                    out.push(c);
                }
            }
        }
    }

    fn escape(&mut self, out: &mut String) -> Res<()> {
        let Some(e) = self.peek() else {
            return Err(self.fail("unterminated quoted scalar"));
        };
        self.advance();
        let simple = match e {
            '0' => Some('\0'),
            'a' => Some('\u{07}'),
            'b' => Some('\u{08}'),
            't' | '\t' => Some('\t'),
            'n' => Some('\n'),
            'v' => Some('\u{0B}'),
            'f' => Some('\u{0C}'),
            'r' => Some('\r'),
            'e' => Some('\u{1B}'),
            ' ' => Some(' '),
            '"' => Some('"'),
            '/' => Some('/'),
            '\\' => Some('\\'),
            'N' => Some('\u{85}'),
            '_' => Some('\u{A0}'),
            'L' => Some('\u{2028}'),
            'P' => Some('\u{2029}'),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            return Ok(());
        }
        let value = match e {
            '\n' => {
                // escaped line break: the lines are joined without a space
                self.skip_spaces();
                return Ok(());
            }
            'x' => self.hex(2)?,
            'U' => self.hex(8)?,
            'u' => {
                let mut value = self.hex(4)?;
                if (0xD800..=0xDBFF).contains(&value)
                    && self.peek() == Some('\\')
                    && self.peek_at(1) == Some('u')
                {
                    self.advance();
                    self.advance();
                    let low = self.hex(4)?;
                    if !(0xDC00..=0xDFFF).contains(&low) {
                        return Err(self.fail("unpaired surrogate"));
                    }
                    value = 0x10000 + ((value - 0xD800) << 10) + (low - 0xDC00);
                }
                value
            }
            _ => return Err(self.fail(format!("invalid escape “\\{e}”"))),
        };
        let Some(c) = char::from_u32(value) else {
            return Err(self.fail("invalid escape"));
        };
        out.push(c);
        Ok(())
    }

    fn hex(&mut self, count: usize) -> Res<u32> {
        let mut value = 0u32;
        for _ in 0..count {
            let Some(digit) = self.peek().and_then(|c| c.to_digit(16)) else {
                return Err(self.fail("invalid escape"));
            };
            value = value << 4 | digit;
            self.advance();
        }
        Ok(value)
    }

    /// `|` keeps line breaks, `>` folds them into spaces. `-` drops the final line break,
    /// `+` also keeps trailing empty lines; a digit gives the indentation instead of
    /// inferring it.
    fn block_scalar(&mut self, parent: isize) -> Res<String> {
        let literal = self.s[self.pos] == '|';
        self.advance();
        let mut chomp = ' ';
        let mut indent: Option<isize> = None;
        for _ in 0..2 {
            match self.peek() {
                Some(c @ ('-' | '+')) if chomp == ' ' => {
                    chomp = c;
                    self.advance();
                }
                Some(c @ '1'..='9') if indent.is_none() => {
                    indent = Some(parent + c.to_digit(10).unwrap_or(0) as isize);
                    self.advance();
                }
                _ => {}
            }
        }
        self.skip_spaces();
        self.skip_comment();
        if !self.at_line_end() {
            return Err(self.fail("invalid block scalar header"));
        }

        let mut lines: Vec<String> = Vec::new();
        let mut final_break = false;
        while self.peek() == Some('\n') {
            let back = self.mark();
            self.advance();
            let mut n: isize = 0;
            while self.peek() == Some(' ') {
                self.advance();
                n += 1;
            }
            let Some(c) = self.peek() else {
                self.reset(back);
                break;
            };
            if c == '\n' {
                lines.push(
                    indent
                        .map(|i| " ".repeat((n - i).max(0) as usize))
                        .unwrap_or_default(),
                );
                continue;
            }
            if indent.is_none() {
                if n <= parent {
                    self.reset(back);
                    break;
                }
                indent = Some(n);
            }
            let ind = indent.unwrap_or(n);
            if n < ind || (n == 0 && self.is_marker()) {
                self.reset(back);
                break;
            }
            let start = self.pos;
            self.skip_line();
            lines.push(" ".repeat((n - ind) as usize) + &self.text(start, self.pos));
            final_break = self.peek() == Some('\n');
        }

        let last = lines.iter().rposition(|l| !l.is_empty());
        let body: &[String] = match last {
            Some(i) => &lines[..=i],
            None => &[],
        };
        let trailing = lines.len() - last.map_or(0, |i| i + 1);
        let mut out = if literal { body.join("\n") } else { fold(body) };
        if chomp != '-' && !body.is_empty() && final_break {
            out.push('\n');
        }
        if chomp == '+' {
            out.push_str(&"\n".repeat(trailing));
        }
        Ok(out)
    }
}

/// The folding of `>`: between two lines of text a line break is a space; empty lines stay
/// line breaks; more-indented lines keep their line breaks.
fn fold(lines: &[String]) -> String {
    let mut out = String::new();
    let mut empties = 0;
    let mut previous_indented: Option<bool> = None;
    for line in lines {
        if line.is_empty() {
            empties += 1;
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        match previous_indented {
            None => out.push_str(&"\n".repeat(empties)),
            Some(false) if !indented => {
                if empties == 0 {
                    out.push(' ');
                } else {
                    out.push_str(&"\n".repeat(empties));
                }
            }
            _ => out.push_str(&"\n".repeat(empties + 1)),
        }
        out.push_str(line);
        previous_indented = Some(indented);
        empties = 0;
    }
    out
}

// ---- types ----

/// The type of a plain scalar: null, booleans, integers and
/// floating-point numbers in the common forms; everything else is text. Quoted and block
/// scalars are always text.
fn resolve(text: &str, tag: Option<&str>) -> Value {
    if is_string_tag(tag) {
        return Value::String(text.to_owned());
    }
    match text {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => number(text).unwrap_or_else(|| Value::String(text.to_owned())),
    }
}

/// The number, or nothing if it isn't one: a sign, digits, maybe a point with more digits
/// (at least one digit in total), maybe an exponent. No `0x`, `.inf`, `1_000`.
fn number(text: &str) -> Option<Value> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut negative = false;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }
    let digits = |i: &mut usize| {
        let from = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        &text[from..*i]
    };
    let whole = digits(&mut i);
    let sign = if negative { "-" } else { "" };
    if i == b.len()
        && !whole.is_empty()
        && let Ok(value) = format!("{sign}{whole}").parse::<i64>()
    {
        return Some(Value::from(value));
    }
    let mut fraction: Option<&str> = None;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        fraction = Some(digits(&mut i));
    }
    if whole.is_empty() && fraction.is_none_or(str::is_empty) {
        return None;
    }
    let mut exponent = String::new();
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        exponent.push('e');
        i += 1;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            exponent.push(b[i] as char);
            i += 1;
        }
        let power = digits(&mut i);
        if power.is_empty() {
            return None;
        }
        exponent.push_str(power);
    }
    if i != b.len() {
        return None;
    }
    let mut literal = format!("{sign}{}", if whole.is_empty() { "0" } else { whole });
    if let Some(fraction) = fraction {
        literal.push('.');
        literal.push_str(if fraction.is_empty() { "0" } else { fraction });
    }
    literal.push_str(&exponent);
    let value: f64 = literal.parse().ok()?;
    Number::from_f64(value).map(Value::Number)
}

use super::toml::{self, TomlValue};
use super::{ClientState, ClientStatus, McpClient, SERVER, line, safe_file, same};
use crate::places::{is_dir, parent};
use crate::{Places, Result, SetupError};
use std::path::PathBuf;

/// Codex keeps servers in TOML: `[mcp_servers.name]`.
///
/// The file is read with a real parser and written as text, removing or adding only
/// the entry's block: so comments and order of the rest stay the user's. The result
/// is read back before saving, and if it doesn't say what it should, it isn't
/// saved.
#[derive(Debug, Clone)]
pub struct CodexClient {
    command: Vec<String>,
    path: PathBuf,
}

/// The entry found: executable and arguments.
struct Entry {
    command: Option<String>,
    args: Vec<String>,
}

impl CodexClient {
    pub fn new(places: &Places, command: &[String]) -> CodexClient {
        CodexClient {
            command: command.to_vec(),
            path: places.home.join(".codex").join("config.toml"),
        }
    }

    /// Our entry, if any.
    fn entry(text: &str) -> Result<Option<Entry>> {
        let model = toml::parse(text).map_err(|e| {
            SetupError(format!(
                "config.toml can't be read ({e}): not rewriting it."
            ))
        })?;
        let Some(servers) = model.get("mcp_servers") else {
            return Ok(None);
        };
        let Some(table) = servers.as_table() else {
            return Err(SetupError(
                "config.toml doesn't have the expected shape: not touching it.".into(),
            ));
        };
        let Some(entry) = table.get(SERVER) else {
            return Ok(None);
        };
        let Some(ours) = entry.as_table() else {
            return Ok(Some(Entry {
                command: None,
                args: Vec::new(),
            }));
        };
        let args = ours
            .get("args")
            .and_then(TomlValue::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|x| match x {
                        TomlValue::String(s) | TomlValue::Scalar(s) => s.clone(),
                        _ => String::new(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(Entry {
            command: ours
                .get("command")
                .and_then(TomlValue::as_str)
                .map(str::to_owned),
            args,
        }))
    }

    fn same(&self, entry: &Entry) -> bool {
        same(&self.command, entry.command.as_deref(), &entry.args)
    }

    fn block(&self, newline: &str) -> String {
        let command = self
            .command
            .first()
            .map(|c| quote(c))
            .unwrap_or_else(|| "''".into());
        let args: Vec<String> = self.command.iter().skip(1).map(|a| quote(a)).collect();
        format!(
            "[mcp_servers.{SERVER}]{newline}command = {command}{newline}args = [{}]{newline}",
            args.join(", ")
        )
    }
}

/// The text without the `[mcp_servers.clak]` block and its subtables.
fn without(text: &str) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.split('\n') {
        let bare = line.strip_suffix('\r').unwrap_or(line);
        if any_header(bare) {
            skipping = our_header(bare);
        }
        if !skipping {
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn any_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// The header of our entry or of one of its subtables, with the name written in any legal way.
fn our_header(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('[') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix("mcp_servers") else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix('.') else {
        return false;
    };
    let rest = rest.trim_start();
    let names = ["\"clak\"", "'clak'", "clak"];
    let Some(rest) = names.iter().find_map(|name| rest.strip_prefix(name)) else {
        return false;
    };
    let mut rest = rest.trim_start();
    if let Some(sub) = rest.strip_prefix('.') {
        let Some(end) = sub.find(']') else {
            return false;
        };
        rest = &sub[end..];
    }
    let Some(rest) = rest.strip_prefix(']') else {
        return false;
    };
    let rest = rest.trim_start();
    rest.is_empty() || rest.starts_with('#')
}

/// In single quotes the text stays as it is; if it contains a single quote, in double quotes
/// with escapes.
fn quote(value: &str) -> String {
    if !value.contains('\'') && !value.chars().any(char::is_control) {
        return format!("'{value}'");
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl McpClient for CodexClient {
    fn name(&self) -> &str {
        "Codex"
    }

    fn note(&self) -> &str {
        "Applies to new Codex sessions"
    }

    fn command(&self) -> &[String] {
        &self.command
    }

    fn status(&self) -> ClientStatus {
        if !is_dir(parent(&self.path)) {
            return ClientStatus::new(ClientState::NotInstalled, &self.path);
        }
        let found = safe_file::read(&self.path).and_then(|(text, _)| CodexClient::entry(&text));
        match found {
            Ok(None) => ClientStatus::new(ClientState::Absent, &self.path),
            Ok(Some(entry)) => {
                let state = if self.same(&entry) {
                    ClientState::Ok
                } else {
                    ClientState::Stale
                };
                ClientStatus::with(
                    state,
                    &self.path,
                    line(entry.command.as_deref(), &entry.args),
                    entry.command,
                )
            }
            Err(e) => ClientStatus::with(ClientState::Unreadable, &self.path, e.0, None),
        }
    }

    fn add(&self) -> Result<()> {
        let (raw, bom) = safe_file::read(&self.path)?;
        CodexClient::entry(&raw)?; // if it can't be read, stop here
        let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        let rest = without(&raw);
        let rest = rest.trim_end();
        let text = if rest.is_empty() {
            String::new()
        } else {
            format!("{rest}{newline}{newline}")
        } + &self.block(newline);
        let written = CodexClient::entry(&text).ok().flatten();
        if !written.is_some_and(|w| self.same(&w)) {
            return Err(SetupError(
                "config.toml already defines the servers in a form that can't be changed safely: add the entry by hand.".into(),
            ));
        }
        safe_file::write(&self.path, &text, bom)
    }

    fn remove(&self) -> Result<()> {
        let (raw, bom) = safe_file::read(&self.path)?;
        if CodexClient::entry(&raw)?.is_none() {
            return Ok(());
        }
        let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        let text = format!("{}{newline}", without(&raw).trim_end());
        // "Can't be read" is not "no longer there": we need the latter.
        if !matches!(CodexClient::entry(&text), Ok(None)) {
            return Err(SetupError(
                "config.toml defines the entry in a form that can't be removed safely: remove it by hand.".into(),
            ));
        }
        safe_file::write(&self.path, &text, bom)
    }

    fn snippet(&self) -> String {
        self.block("\n").trim_end().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_the_entry_headers() {
        for ours in [
            "[mcp_servers.clak]",
            " [ mcp_servers . 'clak' ] # c",
            "[mcp_servers.\"clak\".env]",
            "[mcp_servers.clak.env.x]",
        ] {
            assert!(our_header(ours), "{ours}");
        }
        for other in [
            "[mcp_servers.clak2]",
            "[mcp_servers.other]",
            "[mcp_servers]",
            "[[mcp_servers.clak]]",
            "[mcp_servers.clak] x",
        ] {
            assert!(!our_header(other), "{other}");
        }
    }

    #[test]
    fn quotes_only_when_needed() {
        assert_eq!(
            quote(r"C:\Program Files\x.exe"),
            r"'C:\Program Files\x.exe'"
        );
        assert_eq!(quote("it's"), "\"it's\"");
        assert_eq!(quote("a\\'\n"), "\"a\\\\'\\n\"");
    }
}

use super::json::{self, Json, JsonError, Object};
use super::{ClientState, ClientStatus, McpClient, SERVER, line, safe_file, same};
use crate::places::{is_dir, parent};
use crate::{Result, SetupError};
use std::path::{Path, PathBuf};

/// A client that keeps servers in a JSON object: `{section: {name: {...}}}`.
///
/// The file is read with a JSON that preserves key order and numbers as written, and
/// rejects comments and duplicate keys: a file that isn't plain JSON is not rewritten
/// (half of it would be lost).
#[derive(Debug, Clone)]
pub struct JsonFileClient {
    name: String,
    note: String,
    command: Vec<String>,
    /// The candidate files, most likely first: the first that exists is used.
    files: Vec<PathBuf>,
    /// `mcpServers` almost everywhere, `servers` for VS Code.
    section: String,
    /// VS Code also wants `"type": "stdio"`.
    typed: bool,
}

impl JsonFileClient {
    pub fn new(name: impl Into<String>, files: Vec<PathBuf>, command: &[String], note: impl Into<String>) -> JsonFileClient {
        JsonFileClient {
            name: name.into(),
            note: note.into(),
            command: command.to_vec(),
            files,
            section: "mcpServers".into(),
            typed: false,
        }
    }

    /// Another section instead of `mcpServers`.
    pub fn section(mut self, section: impl Into<String>) -> JsonFileClient {
        self.section = section.into();
        self
    }

    /// With `"type": "stdio"` in the entry.
    pub fn typed(mut self) -> JsonFileClient {
        self.typed = true;
        self
    }

    /// The first candidate that exists; if none, the first whose folder exists.
    pub fn file(&self) -> PathBuf {
        self.files
            .iter()
            .find(|f| f.exists())
            .or_else(|| self.files.iter().find(|f| is_dir(parent(f))))
            .or(self.files.first())
            .cloned()
            .unwrap_or_default()
    }

    fn installed(&self) -> bool {
        self.files.iter().any(|f| is_dir(parent(f)))
    }

    fn entry(&self) -> Json {
        let mut entry = Object::default();
        if self.typed {
            entry.insert("type", Json::String("stdio".into()));
        }
        entry.insert("command", Json::String(self.command.first().cloned().unwrap_or_default()));
        entry.insert("args", Json::Array(self.command.iter().skip(1).cloned().map(Json::String).collect()));
        Json::Object(entry)
    }

    fn load(&self, path: &Path) -> Result<(Object, String, bool)> {
        let (raw, bom) = safe_file::read(path)?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if raw.trim().is_empty() {
            return Ok((Object::default(), raw, bom));
        }
        let data = match json::parse(&raw) {
            Ok(data) => data,
            Err(JsonError::Repeated { .. }) => {
                return Err(SetupError(format!("{name} has a repeated key: not touching it.")));
            }
            Err(JsonError::Syntax { line }) => {
                return Err(SetupError(format!(
                    "{name} isn't plain JSON (line {line}). It may contain comments: not rewriting it, add the entry by hand."
                )));
            }
        };
        let shape = |value: Option<&Json>| value.is_none_or(|v| v.as_object().is_some());
        if let Json::Object(object) = data {
            let section = object.get(&self.section);
            if shape(section) && shape(section.and_then(|s| s.get(SERVER))) {
                return Ok((object, raw, bom));
            }
        }
        Err(SetupError(format!("{name} doesn't have the expected shape: not touching it.")))
    }

    fn save(path: &Path, data: Object, raw: &str, bom: bool) -> Result<()> {
        let newline = if raw.contains("\r\n") { "\r\n" } else { "\n" };
        // The indentation is that of the first indented line that was there.
        let mut indent = "  ".to_owned();
        for line in raw.split('\n').skip(1) {
            let lead: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            if lead.is_empty() {
                continue;
            }
            indent = if lead.starts_with('\t') { "\t".into() } else { " ".repeat(lead.chars().count().min(8)) };
            break;
        }
        let mut text = Json::Object(data).pretty(&indent, newline);
        if raw.is_empty() || raw.ends_with('\n') {
            text.push_str(newline);
        }
        safe_file::write(path, &text, bom)
    }

    /// The executable and arguments of an entry.
    pub(crate) fn read(entry: &Object) -> (Option<String>, Vec<String>) {
        let command = entry.get("command").and_then(Json::as_str).map(str::to_owned);
        let args = entry
            .get("args")
            .and_then(Json::as_array)
            .map(|items| items.iter().map(|a| a.as_str().map(str::to_owned).unwrap_or_else(|| a.text())).collect())
            .unwrap_or_default();
        (command, args)
    }
}

impl McpClient for JsonFileClient {
    fn name(&self) -> &str {
        &self.name
    }

    fn note(&self) -> &str {
        &self.note
    }

    fn command(&self) -> &[String] {
        &self.command
    }

    fn status(&self) -> ClientStatus {
        let path = self.file();
        if !self.installed() {
            return ClientStatus::new(ClientState::NotInstalled, path);
        }
        match self.load(&path) {
            Ok((data, _, _)) => {
                let Some(entry) = data.get(&self.section).and_then(|s| s.get(SERVER)).and_then(Json::as_object) else {
                    return ClientStatus::new(ClientState::Absent, path);
                };
                let (command, args) = JsonFileClient::read(entry);
                let state = if same(&self.command, command.as_deref(), &args) { ClientState::Ok } else { ClientState::Stale };
                ClientStatus::with(state, path, line(command.as_deref(), &args), command)
            }
            Err(e) => ClientStatus::with(ClientState::Unreadable, path, e.0, None),
        }
    }

    fn add(&self) -> Result<()> {
        let path = self.file();
        let (mut data, raw, bom) = self.load(&path)?;
        if data.get(&self.section).and_then(Json::as_object).is_none() {
            data.insert(&self.section, Json::Object(Object::default()));
        }
        if let Some(Json::Object(servers)) = data.get_mut(&self.section) {
            servers.insert(SERVER, self.entry());
        }
        JsonFileClient::save(&path, data, &raw, bom)
    }

    fn remove(&self) -> Result<()> {
        let path = self.file();
        let (mut data, raw, bom) = self.load(&path)?;
        let Some(Json::Object(servers)) = data.get_mut(&self.section) else { return Ok(()) };
        if servers.remove(SERVER).is_some() {
            JsonFileClient::save(&path, data, &raw, bom)?;
        }
        Ok(())
    }

    fn snippet(&self) -> String {
        let mut servers = Object::default();
        servers.insert(SERVER, self.entry());
        let mut root = Object::default();
        root.insert(&self.section, Json::Object(servers));
        Json::Object(root).pretty("  ", "\n")
    }
}

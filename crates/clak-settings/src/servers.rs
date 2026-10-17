//! MCP servers: listing, adding, changing, removing them. A server is at an address, or is
//! a program on this computer (stdio) that Clak starts: then the confirmation shows the
//! file that will run, and environment variables (encrypted like a token) appear only by name.

use crate::args::Args;
use crate::{
    Answer, HubSettings, Stop, clean, command, find, json_text, row_of, rows, say, secret, some,
    str_of, unencrypted,
};
use clak_connectors::launch::{self, Launch};
use clak_connectors::{ConnectorCredential, ConnectorEntry, ConnectorHub, phase, same_origin};
use clak_hub::{ConsentLine, SettingsChange, cli_names};
use clak_wire::json::truthy;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// The warning on every confirmation that starts a new program.
const PROGRAM_WARNING: &str = "Clak starts this program on this computer, with your permissions, every time it starts: add it only if you trust it.";

impl HubSettings {
    pub(crate) async fn add_server(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let name = args.required("name")?;
        let command = command(args.text("command")?);
        if let Some(line) = args.text("stdio")? {
            if args.has("url") || args.has("token") {
                return Err(say("--stdio runs a program: it takes no --url or --token."));
            }
            let env = env_of(args)?.unwrap_or_default();
            return self
                .add_program(call_id, &name, &line, env, command, cancel)
                .await;
        }
        if args.has("env") {
            return Err(say("--env is for a program started with --stdio."));
        }
        let Some(url) = args
            .text("url")?
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty())
        else {
            return Err(say(
                "--url or --stdio is required: the server's address, or the command line of the program that runs it.",
            ));
        };
        let token = secret(args.text("token")?);
        if let Some(problem) = ConnectorHub::problem(Some(&name), Some(&url))
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(say(problem));
        }
        if let Some(twin) = self.connectors.find_by_url(&url) {
            return Err(say(format!(
                "“{}” already reaches {url}: change it with edit-server --server {}.",
                twin.name,
                self.server_cli(&twin)
            )));
        }

        let change = SettingsChange {
            title: format!("Add the MCP server “{}”", clean(&name)),
            action: "add this MCP server".into(),
            lines: vec![
                ConsentLine::new("Name", name.as_str()),
                ConsentLine::new("Address", url.as_str()),
                ConsentLine::new(
                    "Command",
                    format!(
                        "clak {}",
                        command.clone().unwrap_or_else(|| cli_names::from(&name))
                    ),
                ),
                ConsentLine::new(
                    "Access",
                    if token.is_none() {
                        "none (if the server asks for a sign-in, you do it from the panel)"
                    } else {
                        "a token, given by the agent"
                    },
                ),
            ],
            tools: vec![],
            warning: unencrypted(&url, token.is_some()),
        };
        self.confirm(call_id, change, cancel).await?;

        let entry = self
            .connectors
            .add(&name, &url, token.as_deref(), command.as_deref())
            .map_err(Stop::Failed)?;
        let row = self.settled(&entry.id, cancel).await?;
        Ok(json_text(&next(server_row(&row))))
    }

    /// A server that is a program: the command line is checked, and the program is looked
    /// up, before bothering the user.
    async fn add_program(
        &self,
        call_id: &str,
        name: &str,
        line: &str,
        env: BTreeMap<String, String>,
        command: Option<String>,
        cancel: &CancellationToken,
    ) -> Answer {
        let launch = Launch::parse(line).map_err(say)?;
        if let Some(problem) = ConnectorHub::problem_stdio(Some(name), &env)
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(say(problem));
        }
        if let Some(twin) = self.connectors.find_by_launch(&launch) {
            return Err(say(format!(
                "“{}” already runs {}: change it with edit-server --server {}.",
                twin.name,
                launch.display(),
                self.server_cli(&twin)
            )));
        }
        let file = located(&launch).await?;
        let change = SettingsChange {
            title: format!("Add the MCP server “{}”", clean(name)),
            action: "add this MCP server".into(),
            lines: vec![
                ConsentLine::new("Name", name),
                ConsentLine::new("Program", launch.display()),
                ConsentLine::new("File", file.display().to_string()),
                ConsentLine::new("Environment", env_names(&env)),
                ConsentLine::new(
                    "Command",
                    format!(
                        "clak {}",
                        command.clone().unwrap_or_else(|| cli_names::from(name))
                    ),
                ),
            ],
            tools: vec![],
            warning: Some(PROGRAM_WARNING.into()),
        };
        self.confirm(call_id, change, cancel).await?;

        let entry = self
            .connectors
            .add_stdio(name, &launch, &env, command.as_deref())
            .map_err(Stop::Failed)?;
        let row = self.settled(&entry.id, cancel).await?;
        Ok(json_text(&next(server_row(&row))))
    }

    pub(crate) async fn edit_server(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let old = self.find_server(&args.required("server")?)?;
        if old.is_stdio() {
            return self.edit_program(call_id, args, old, cancel).await;
        }
        if args.has("stdio") || args.has("env") || args.has("remove_env") {
            return Err(say(format!(
                "“{}” is a server at an address: --stdio and --env are for one that runs a program. To switch, remove it and add it again.",
                old.name
            )));
        }
        let name = args.text("name")?.unwrap_or_else(|| old.name.clone());
        let url = args
            .text("url")?
            .map(|u| u.trim().to_owned())
            .unwrap_or_else(|| old.url.clone());
        let command = if args.has("command") {
            command(args.text("command")?)
        } else {
            old.command.clone()
        };
        let token = secret(args.text("token")?);
        let drop = args.bool("remove_token")? == Some(true);
        let enabled = args.bool("enabled")?;
        if token.is_some() && drop {
            return Err(say("--token and --remove-token together: choose one."));
        }
        if let Some(problem) = ConnectorHub::problem(Some(&name), Some(&url))
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(say(problem));
        }
        if let Some(twin) = self.connectors.find_by_url(&url)
            && twin.id != old.id
        {
            return Err(say(format!("“{}” already reaches {url}.", twin.name)));
        }

        let saved = self.connectors.has_credential(&old.id);
        let cli = self.server_cli(&old);
        let mut lines = vec![ConsentLine::new(
            "Server",
            format!("{} (clak {cli})", old.name),
        )];
        if name != old.name {
            lines.push(ConsentLine::new("Name", format!("{} → {name}", old.name)));
        }
        if url != old.url {
            lines.push(ConsentLine::new("Address", format!("{} → {url}", old.url)));
        }
        if command != old.command {
            lines.push(ConsentLine::new(
                "Command",
                format!(
                    "clak {cli} → clak {}",
                    command.clone().unwrap_or_else(|| cli_names::from(&name))
                ),
            ));
        }
        if token.is_some() {
            lines.push(ConsentLine::new(
                "Access",
                if saved {
                    "the saved token or sign-in is replaced by a token given by the agent"
                } else {
                    "a token, given by the agent"
                },
            ));
        } else if drop && saved {
            lines.push(ConsentLine::new(
                "Access",
                "the saved token or sign-in is removed",
            ));
        } else if saved && !same_origin(&old.url, &url) {
            lines.push(ConsentLine::new(
                "Access",
                "the saved token or sign-in stays behind: it isn't sent to the new host",
            ));
        }
        let edits = lines.len() > 1;
        if let Some(on) = enabled
            && on != old.enabled
        {
            lines.push(ConsentLine::new(
                "State",
                if on { "turned on" } else { "turned off" },
            ));
        }
        if lines.len() == 1 {
            return Err(say("Nothing to change: pass what should change (see -h)."));
        }

        let warning = unencrypted(
            &url,
            token.is_some() || (saved && !drop && same_origin(&old.url, &url)),
        );
        let change = SettingsChange {
            title: format!("Change the MCP server “{}”", clean(&old.name)),
            action: "make this change".into(),
            lines,
            tools: vec![],
            warning,
        };
        self.confirm(call_id, change, cancel).await?;

        // Meanwhile someone (the panel, another agent) may have changed it: apply only to
        // what the user saw.
        if self.connectors.get(&old.id).as_ref() != Some(&old) {
            return Err(say(format!(
                "“{}” changed while the user was deciding: read it again (servers) and retry.",
                old.name
            )));
        }
        if enabled == Some(false) {
            self.connectors
                .set_enabled(&old.id, false)
                .await
                .map_err(Stop::Failed)?;
        }
        if edits {
            let credential = match (&token, drop) {
                (Some(token), _) => Some(ConnectorCredential {
                    token: Some(token.clone()),
                    grant: None,
                }),
                (None, true) => Some(ConnectorCredential::default()),
                (None, false) => None,
            };
            self.connectors
                .edit(&old.id, &name, command.as_deref(), &url, credential)
                .await
                .map_err(Stop::Failed)?;
        }
        if enabled == Some(true) {
            self.connectors
                .set_enabled(&old.id, true)
                .await
                .map_err(Stop::Failed)?;
        }
        let row = self.settled(&old.id, cancel).await?;
        Ok(json_text(&next(server_row(&row))))
    }

    async fn edit_program(
        &self,
        call_id: &str,
        args: &Args<'_>,
        old: ConnectorEntry,
        cancel: &CancellationToken,
    ) -> Answer {
        if args.has("url") || args.has("token") || args.has("remove_token") {
            return Err(say(format!(
                "“{}” runs a program on this computer: it has no address or token (change --stdio or --env).",
                old.name
            )));
        }
        let old_launch = old.launch.clone().expect("a program");
        let name = args.text("name")?.unwrap_or_else(|| old.name.clone());
        let command = if args.has("command") {
            command(args.text("command")?)
        } else {
            old.command.clone()
        };
        let launch = match args.text("stdio")? {
            Some(line) => Launch::parse(&line).map_err(say)?,
            None => old_launch.clone(),
        };
        let new_env = env_of(args)?;
        let drop = args.bool("remove_env")? == Some(true);
        if new_env.is_some() && drop {
            return Err(say("--env and --remove-env together: choose one."));
        }
        let old_env = self.connectors.env(&old.id);
        let env = if drop { Some(BTreeMap::new()) } else { new_env }.filter(|e| *e != old_env);
        let enabled = args.bool("enabled")?;
        if let Some(problem) =
            ConnectorHub::problem_stdio(Some(&name), env.as_ref().unwrap_or(&old_env))
                .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(say(problem));
        }
        if let Some(twin) = self.connectors.find_by_launch(&launch)
            && twin.id != old.id
        {
            return Err(say(format!(
                "“{}” already runs {}.",
                twin.name,
                launch.display()
            )));
        }

        let cli = self.server_cli(&old);
        let mut lines = vec![ConsentLine::new(
            "Server",
            format!("{} (clak {cli})", old.name),
        )];
        if name != old.name {
            lines.push(ConsentLine::new("Name", format!("{} → {name}", old.name)));
        }
        let moved = launch != old_launch;
        if moved {
            let file = located(&launch).await?;
            lines.push(ConsentLine::new(
                "Program",
                format!("{} → {}", old_launch.display(), launch.display()),
            ));
            lines.push(ConsentLine::new("File", file.display().to_string()));
        }
        if command != old.command {
            lines.push(ConsentLine::new(
                "Command",
                format!(
                    "clak {cli} → clak {}",
                    command.clone().unwrap_or_else(|| cli_names::from(&name))
                ),
            ));
        }
        if let Some(env) = &env {
            lines.push(ConsentLine::new(
                "Environment",
                if env.is_empty() {
                    "the saved variables are removed".to_owned()
                } else if old_env.is_empty() {
                    env_names(env)
                } else {
                    format!("{} (replacing the saved ones)", env_names(env))
                },
            ));
        }
        let edits = lines.len() > 1;
        if let Some(on) = enabled
            && on != old.enabled
        {
            lines.push(ConsentLine::new(
                "State",
                if on { "turned on" } else { "turned off" },
            ));
        }
        if lines.len() == 1 {
            return Err(say("Nothing to change: pass what should change (see -h)."));
        }

        let change = SettingsChange {
            title: format!("Change the MCP server “{}”", clean(&old.name)),
            action: "make this change".into(),
            lines,
            tools: vec![],
            warning: moved.then(|| PROGRAM_WARNING.to_owned()),
        };
        self.confirm(call_id, change, cancel).await?;

        if self.connectors.get(&old.id).as_ref() != Some(&old) {
            return Err(say(format!(
                "“{}” changed while the user was deciding: read it again (servers) and retry.",
                old.name
            )));
        }
        if enabled == Some(false) {
            self.connectors
                .set_enabled(&old.id, false)
                .await
                .map_err(Stop::Failed)?;
        }
        if edits {
            self.connectors
                .edit_stdio(&old.id, &name, command.as_deref(), &launch, env)
                .await
                .map_err(Stop::Failed)?;
        }
        if enabled == Some(true) {
            self.connectors
                .set_enabled(&old.id, true)
                .await
                .map_err(Stop::Failed)?;
        }
        let row = self.settled(&old.id, cancel).await?;
        Ok(json_text(&next(server_row(&row))))
    }

    pub(crate) async fn remove_server(
        &self,
        call_id: &str,
        args: &Args<'_>,
        cancel: &CancellationToken,
    ) -> Answer {
        let old = self.find_server(&args.required("server")?)?;
        let mut lines = vec![
            ConsentLine::new(
                "Server",
                format!("{} (clak {})", old.name, self.server_cli(&old)),
            ),
            if old.is_stdio() {
                ConsentLine::new("Program", old.target())
            } else {
                ConsentLine::new("Address", old.url.as_str())
            },
        ];
        if self.connectors.has_credential(&old.id) {
            lines.push(ConsentLine::new(
                "Access",
                "its token or sign-in is deleted",
            ));
        }
        if old.is_stdio() && !self.connectors.env(&old.id).is_empty() {
            lines.push(ConsentLine::new(
                "Environment",
                "its saved variables are deleted",
            ));
        }
        let change = SettingsChange {
            title: format!("Remove the MCP server “{}”", clean(&old.name)),
            action: "remove this MCP server".into(),
            lines,
            tools: vec![],
            warning: None,
        };
        self.confirm(call_id, change, cancel).await?;
        self.connectors
            .remove(&old.id)
            .await
            .map_err(Stop::Failed)?;
        Ok(json_text(&json!({ "removed": old.name })))
    }

    fn find_server(&self, key: &str) -> Result<ConnectorEntry, Stop> {
        let rows = rows(&self.connectors.view());
        find(
            &self.connectors.entries(),
            key,
            |e| e.id.as_str(),
            |e| cli_of(&rows, e),
            |e| e.name.as_str(),
            "MCP server",
            "servers",
        )
    }

    fn server_cli(&self, entry: &ConnectorEntry) -> String {
        cli_of(&rows(&self.connectors.view()), entry)
    }

    /// A server just added or changed: wait a little to learn whether it answers.
    async fn settled(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Map<String, Value>, Stop> {
        let until = Instant::now() + self.settle();
        loop {
            let rows = rows(&self.connectors.view());
            let Some(row) = row_of(&rows, id) else {
                return Err(say("The server was removed in the meantime."));
            };
            if str_of(row, "state") != Some(phase::CONNECTING) || Instant::now() >= until {
                return Ok(row.clone());
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                _ = cancel.cancelled() => return Err(Stop::Canceled),
            }
        }
    }
}

/// The environment variables of `--env`: a JSON object of texts (a number or a yes/no
/// becomes text).
fn env_of(args: &Args<'_>) -> Result<Option<BTreeMap<String, String>>, Stop> {
    let Some(node) = args.node("env") else {
        return Ok(None);
    };
    let object = match node {
        Value::Object(object) => object.clone(),
        // From the command line the JSON can arrive as text.
        Value::String(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(object)) => object,
            _ => return Err(say(ENV_FORMAT)),
        },
        _ => return Err(say(ENV_FORMAT)),
    };
    let mut env = BTreeMap::new();
    for (key, value) in object {
        let value = match value {
            Value::String(text) => text,
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => return Err(say(ENV_FORMAT)),
        };
        env.insert(key, value);
    }
    Ok(Some(env))
}

const ENV_FORMAT: &str = r#"--env is a JSON object of texts, e.g. {"API_KEY": "…", "DEBUG": "1"}."#;

/// The variable names, for the confirmation: not the values, they're often keys.
fn env_names(env: &BTreeMap<String, String>) -> String {
    if env.is_empty() {
        "none".to_owned()
    } else {
        let names: Vec<&str> = env.keys().map(String::as_str).collect();
        format!(
            "{} (values given by the agent, saved encrypted)",
            some(&names)
        )
    }
}

/// The file that will run, looked up the way the hub will look it up; or why it's missing.
async fn located(launch: &Launch) -> Result<PathBuf, Stop> {
    launch::find(&launch.program).await.ok_or_else(|| {
        say(format!(
            "“{}” was not found on this computer: neither a file nor a program in the PATH. Pass its full path, or install it first.",
            launch.program
        ))
    })
}

/// A server's command: the one it has in the core, or the one it will have.
fn cli_of(rows: &[Map<String, Value>], entry: &ConnectorEntry) -> String {
    row_of(rows, &entry.id)
        .and_then(|r| str_of(r, "cli"))
        .map(str::to_owned)
        .or_else(|| entry.command.clone())
        .unwrap_or_else(|| cli_names::from(&entry.name))
}

/// A server for the agent: no secrets, only whether there are any.
pub(crate) fn server_row(row: &Map<String, Value>) -> Value {
    let has_token = truthy(row.get("has_token"));
    let program =
        (str_of(row, "transport") == Some("stdio")).then(|| str_of(row, "launch").unwrap_or(""));
    let env: Vec<&str> = match row.get("env") {
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    let access = if program.is_some() {
        if env.is_empty() {
            "none".to_owned()
        } else {
            format!("environment variables {}", env.join(", "))
        }
    } else if str_of(row, "auth") == Some(ConnectorEntry::OAUTH_AUTH) {
        if has_token {
            format!(
                "OAuth sign-in at {}",
                str_of(row, "signin_host").unwrap_or("")
            )
        } else {
            "OAuth, not signed in".to_owned()
        }
    } else if has_token {
        "token".to_owned()
    } else {
        "none".to_owned()
    };
    let mut server = json!({
        "name": str_of(row, "name"),
        "command": str_of(row, "cli")
            .or_else(|| str_of(row, "command"))
            .map(str::to_owned)
            .unwrap_or_else(|| cli_names::from(str_of(row, "name").unwrap_or(""))),
        "url": str_of(row, "url"),
        "enabled": truthy(row.get("enabled")),
        "state": str_of(row, "state"),
        "error": str_of(row, "error"),
        "access": access,
        "tools": row.get("tools").cloned().unwrap_or(Value::Null),
        "id": str_of(row, "id"),
    });
    // A program has no address: it has its command line.
    if let (Some(program), Some(fields)) = (program, server.as_object_mut()) {
        fields.remove("url");
        fields.insert("program".into(), program.into());
    }
    server
}

/// What to do now, as told to the agent: a server ready, or what's missing.
fn next(mut server: Value) -> Value {
    let name = server["name"].as_str().unwrap_or("").to_owned();
    let text = match server["state"].as_str() {
        Some(phase::ACTIVE) => format!(
            "Ready: `clak {} list` shows its commands.",
            server["command"].as_str().unwrap_or("")
        ),
        Some(phase::SIGN_IN_REQUIRED) => format!(
            "The server asks for a sign-in: ask the user to open Clak, MCP tab, and choose Sign in on “{name}”."
        ),
        Some(phase::REJECTED) => {
            "The server rejected the token: pass the right one with edit-server --token, or ask the user.".to_owned()
        }
        Some(phase::UNREACHABLE) if server.get("program").is_some() => {
            "The program isn't working yet (see error): Clak restarts it by itself.".to_owned()
        }
        Some(phase::UNREACHABLE) => "The server doesn't answer yet: Clak keeps trying by itself.".to_owned(),
        Some(phase::OFF) => "Turned off: agents don't see it until it's turned on again.".to_owned(),
        _ => "Still connecting: check again with `clak settings servers`.".to_owned(),
    };
    server["next"] = text.into();
    server
}

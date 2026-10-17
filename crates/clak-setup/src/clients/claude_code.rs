use super::json::{self, Json};
use super::json_file::JsonFileClient;
use super::{ClientState, ClientStatus, McpClient, SERVER, line, safe_file, same};
use crate::{Places, Result, SetupError};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Runs a command (executable, arguments, environment) and returns its exit code and text.
/// Fake in the tests.
pub type CommandRunner = Arc<dyn Fn(&Path, &[String], &BTreeMap<String, String>) -> std::io::Result<(i32, String)> + Send + Sync>;

/// Claude Code: its file is read, and written only through `claude mcp`.
///
/// It uses the `user` scope, the one that applies in every folder: the offer comes
/// from an application, not from a project.
#[derive(Clone)]
pub struct ClaudeCodeClient {
    places: Places,
    command: Vec<String>,
    run: CommandRunner,
    path: PathBuf,
}

impl ClaudeCodeClient {
    pub fn new(places: &Places, command: &[String]) -> ClaudeCodeClient {
        ClaudeCodeClient::with_runner(places, command, Arc::new(run_process))
    }

    /// With another way of launching `claude`: for the tests.
    pub fn with_runner(places: &Places, command: &[String], run: CommandRunner) -> ClaudeCodeClient {
        let config = match places.var("CLAUDE_CONFIG_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => places.home.clone(),
        };
        ClaudeCodeClient { places: places.clone(), command: command.to_vec(), run, path: config.join(".claude.json") }
    }

    /// `claude` in the PATH. On the Mac also where its installer puts it, even if the PATH
    /// of whoever launched us doesn't say so.
    fn cli(&self) -> Option<PathBuf> {
        let path = self.places.var("PATH").unwrap_or("");
        if cfg!(windows) {
            for folder in path.split(';').filter(|f| !f.is_empty()) {
                for ext in [".exe", ".cmd", ".bat"] {
                    let candidate = Path::new(folder.trim_matches('"')).join(format!("claude{ext}"));
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
            None
        } else {
            let home = &self.places.home;
            path.split(':')
                .filter(|f| !f.is_empty())
                .map(PathBuf::from)
                .chain([home.join(".local").join("bin"), home.join(".claude").join("local")])
                .map(|folder| folder.join("claude"))
                .find(|candidate| executable(candidate))
        }
    }

    fn claude(&self, args: &[&str]) -> Result<()> {
        let Some(cli) = self.cli() else {
            return Err(SetupError(if cfg!(windows) {
                "The `claude` command isn't in PATH: add the entry by hand.".into()
            } else {
                "The `claude` command can't be found (neither in PATH nor in ~/.local/bin): add the entry by hand.".into()
            }));
        };
        let mut environment = self.places.environment.clone();
        if !cfg!(windows) {
            // `claude` installed with npm is a script for `node`: it needs a PATH to find it in.
            let folder = cli.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
            let path = match environment.get("PATH") {
                Some(rest) if !rest.is_empty() => format!("{folder}:{rest}"),
                _ => folder,
            };
            environment.insert("PATH".into(), path);
        }
        let mut full = vec!["mcp".to_owned()];
        full.extend(args.iter().map(|a| (*a).to_owned()));
        let (code, output) =
            (self.run)(&cli, &full, &environment).map_err(|e| SetupError(format!("`claude mcp` didn't respond: {e}")))?;
        if code != 0 {
            let text = output.trim();
            return Err(SetupError(if text.is_empty() {
                "`claude mcp` failed.".into()
            } else {
                text.chars().take(400).collect()
            }));
        }
        Ok(())
    }

    /// The entry, if any.
    fn remove_entry(&self) -> Result<()> {
        if !matches!(self.status().state, ClientState::Ok | ClientState::Stale) {
            return Ok(());
        }
        self.claude(&["remove", "--scope", "user", SERVER])
    }
}

impl McpClient for ClaudeCodeClient {
    fn name(&self) -> &str {
        "Claude Code"
    }

    fn note(&self) -> &str {
        "Applies to new sessions"
    }

    fn command(&self) -> &[String] {
        &self.command
    }

    fn status(&self) -> ClientStatus {
        let exists = self.path.exists();
        if self.cli().is_none() && !exists {
            return ClientStatus::new(ClientState::NotInstalled, &self.path);
        }
        if !exists {
            return ClientStatus::new(ClientState::Absent, &self.path);
        }
        let unreadable = |e: &dyn std::fmt::Display| {
            ClientStatus::with(ClientState::Unreadable, &self.path, format!(".claude.json can't be read: {e}"), None)
        };
        let text = match safe_file::read(&self.path) {
            Ok((text, _)) => text,
            Err(e) => return unreadable(&e),
        };
        let data = match json::parse(&text) {
            Ok(data) => data,
            Err(json::JsonError::Syntax { line }) => return unreadable(&format!("not JSON (line {line})")),
            Err(json::JsonError::Repeated { line }) => return unreadable(&format!("repeated key (line {line})")),
        };
        let Some(entry) = data.get("mcpServers").and_then(|s| s.get(SERVER)).and_then(Json::as_object) else {
            return ClientStatus::new(ClientState::Absent, &self.path);
        };
        let (command, args) = JsonFileClient::read(entry);
        let state = if same(&self.command, command.as_deref(), &args) { ClientState::Ok } else { ClientState::Stale };
        ClientStatus::with(state, &self.path, line(command.as_deref(), &args), command)
    }

    fn add(&self) -> Result<()> {
        self.remove_entry()?;
        let mut args = vec!["add", "--scope", "user", SERVER, "--"];
        args.extend(self.command.iter().map(String::as_str));
        self.claude(&args)
    }

    fn remove(&self) -> Result<()> {
        self.remove_entry()
    }

    fn snippet(&self) -> String {
        let parts: Vec<String> =
            self.command.iter().map(|part| if part.contains(' ') { format!("\"{part}\"") } else { part.clone() }).collect();
        format!("claude mcp add --scope user {SERVER} -- {}", parts.join(" "))
    }
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// A process with its environment, output collected, one minute at most.
fn run_process(exe: &Path, args: &[String], environment: &BTreeMap<String, String>) -> std::io::Result<(i32, String)> {
    let mut command = Command::new(exe);
    command.args(args).env_clear().envs(environment).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn()?;
    // The output is read while the process runs: with the pipe full it would stall.
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = Vec::new();
            if let Some(mut stream) = stream {
                let _ = stream.read_to_end(&mut text);
            }
            String::from_utf8_lossy(&text).into_owned()
        })
    };
    let stdout = drain(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no response within 60 seconds"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let stderr = stderr.join().unwrap_or_default();
    let stdout = stdout.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), stderr + &stdout))
}

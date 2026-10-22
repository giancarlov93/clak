//! Which version of each agent is installed, and how a newer one comes: the command-line
//! agents are on npm (the latest is read there by the app), and update with their own
//! command (`claude update`, `opencode upgrade`) or with the package manager that installed
//! them. The apps update themselves: only their version is shown.

use crate::launcher::Installed;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `--version` may take: Node starts slowly, the first time.
const ASK_LIMIT: Duration = Duration::from_secs(8);

/// How an agent updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// The agent itself, with these arguments, in a terminal: `claude update`.
    Itself(Vec<String>),
    /// A command to type in a terminal: `npm install -g @openai/codex@latest`.
    Type(String),
}

/// The npm package of a command-line agent, where its latest version is.
pub fn npm_package(agent: &str) -> Option<&'static str> {
    match agent {
        "claude-code" => Some("@anthropic-ai/claude-code"),
        "codex" => Some("@openai/codex"),
        "gemini" => Some("@google/gemini-cli"),
        "opencode" => Some("opencode-ai"),
        _ => None,
    }
}

/// How this agent updates, if Clak can say. Homebrew's installs update with Homebrew.
pub fn update(agent: &Installed) -> Option<Update> {
    let path = agent.path.to_string_lossy().to_lowercase();
    let brew = path.contains("homebrew") || path.contains("/cellar/") || path.contains("linuxbrew");
    let package = npm_package(&agent.id)?;
    Some(match agent.id.as_str() {
        "claude-code" => Update::Itself(vec!["update".into()]),
        "opencode" => Update::Itself(vec!["upgrade".into()]),
        "codex" if brew => Update::Type("brew upgrade codex".into()),
        "gemini" if brew => Update::Type("brew upgrade gemini-cli".into()),
        _ => Update::Type(format!("npm install -g {package}@latest")),
    })
}

/// The installed version, or `None` if it can't be told.
pub fn installed(agent: &Installed) -> Option<String> {
    if agent.app {
        return app_version(&agent.path);
    }
    let mut command = Command::new(&agent.path);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < ASK_LIMIT => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    parse(&out)
}

/// An app's version: from its bundle on the Mac; on Windows from Electron's `package.json`
/// next to it, or from the `app-<version>` folders of an app that updates with Squirrel.
fn app_version(path: &Path) -> Option<String> {
    if cfg!(target_os = "macos") {
        let out = Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
            .arg(path.join("Contents").join("Info.plist"))
            .stderr(Stdio::null())
            .output()
            .ok()?;
        return parse(&String::from_utf8_lossy(&out.stdout));
    }
    let folder = path.parent()?;
    let package = folder.join("resources").join("app").join("package.json");
    if let Some(version) = std::fs::read(&package)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|p| p.get("version")?.as_str().map(str::to_owned))
    {
        return Some(version);
    }
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix("app-").and_then(parse)
        })
        .max_by(|a, b| compare(a, b))
}

/// The first version in a text: "2.1.304 (Claude Code)" → "2.1.304", "codex-cli 0.12.0" →
/// "0.12.0".
pub fn parse(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',')
        .map(|word| word.trim_start_matches('v'))
        .find(|word| {
            let mut parts = word.split('.');
            parts
                .next()
                .is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
                && parts
                    .next()
                    .is_some_and(|p| p.chars().next().is_some_and(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// Whether `latest` is newer than `installed`, by their numbers: "2.1.10" after "2.1.9". A
/// version with a suffix ("-beta") comes before the same one without.
pub fn newer(installed: &str, latest: &str) -> bool {
    compare(latest, installed) == std::cmp::Ordering::Greater
}

fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    let split = |v: &str| {
        let (numbers, suffix) = v.split_once(['-', '+']).unwrap_or((v, ""));
        let numbers: Vec<u64> = numbers.split('.').map(|n| n.parse().unwrap_or(0)).collect();
        (numbers, suffix.is_empty())
    };
    let (a, a_final) = split(a);
    let (b, b_final) = split(b);
    let width = a.len().max(b.len());
    let pad = |v: Vec<u64>| {
        let mut v = v;
        v.resize(width, 0);
        v
    };
    pad(a).cmp(&pad(b)).then(a_final.cmp(&b_final))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn versions_are_read_from_what_the_agents_print() {
        assert_eq!(parse("2.1.304 (Claude Code)\n").as_deref(), Some("2.1.304"));
        assert_eq!(parse("codex-cli 0.163.0").as_deref(), Some("0.163.0"));
        assert_eq!(parse("v2.1.0").as_deref(), Some("2.1.0"));
        assert_eq!(
            parse("opencode 2.1.0-beta.3").as_deref(),
            Some("2.1.0-beta.3")
        );
        assert_eq!(parse("no version here"), None);
    }

    #[test]
    fn newer_by_the_numbers() {
        assert!(newer("2.1.9", "2.1.10"));
        assert!(newer("2.1", "2.1.1"));
        assert!(newer("2.1.0-beta.3", "2.1.0"));
        assert!(!newer("2.1.10", "2.1.9"));
        assert!(!newer("2.1.0", "2.1.0"));
    }

    #[test]
    fn each_agent_updates_its_own_way() {
        let agent = |id: &str, path: &str| Installed {
            id: id.into(),
            name: id.into(),
            app: false,
            path: PathBuf::from(path),
            folder: true,
        };
        assert_eq!(
            update(&agent("claude-code", "/Users/me/.local/bin/claude")),
            Some(Update::Itself(vec!["update".into()]))
        );
        assert_eq!(
            update(&agent("codex", "/opt/homebrew/bin/codex")),
            Some(Update::Type("brew upgrade codex".into()))
        );
        assert_eq!(
            update(&agent("gemini", "/usr/local/lib/node_modules/.bin/gemini")),
            Some(Update::Type(
                "npm install -g @google/gemini-cli@latest".into()
            ))
        );
        assert_eq!(
            update(&agent("vscode", "/Applications/Visual Studio Code.app")),
            None
        );
    }
}

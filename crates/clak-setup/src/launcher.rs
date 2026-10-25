//! The agents installed on this computer, and how Clak launches them: a command-line agent
//! in a terminal, in the folder the user picks, with the secret of its launch in
//! `CLAK_LAUNCH`; an app through the system, on the folder if it takes one.
//!
//! The secret never appears on a command line or in a shell's history: it goes in a small
//! script, readable only by the user, that deletes itself as soon as it runs. On the Mac
//! the script is a `.command`, opened by the terminal the user chose for them (Terminal,
//! or iTerm); on Windows a `.cmd`, in Windows Terminal or, without it, in a console. When
//! the agent ends, the terminal stays open in the folder.

use crate::{Places, Result, SetupError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// An agent Clak knows how to find and launch.
struct Known {
    id: &'static str,
    /// The name the hub gives the agent when it shows up (`provenance::friendly_name_*`):
    /// the launch waits for that name.
    name: &'static str,
    /// The command of a command-line agent.
    command: Option<&'static str>,
    /// The bundle of an app on the Mac.
    mac_app: Option<&'static str>,
    /// The executable of an app on Windows, under `%LOCALAPPDATA%`.
    windows_app: Option<&'static str>,
    /// An app that opens on a folder.
    folder: bool,
}

const KNOWN: [Known; 8] = [
    cli("claude-code", "Claude Code", "claude"),
    cli("codex", "Codex", "codex"),
    cli("gemini", "Gemini CLI", "gemini"),
    cli("opencode", "OpenCode", "opencode"),
    Known {
        id: "claude-desktop",
        name: "Claude Desktop",
        command: None,
        mac_app: Some("Claude.app"),
        windows_app: Some(r"AnthropicClaude\claude.exe"),
        folder: false,
    },
    app(
        "vscode",
        "Visual Studio Code",
        "Visual Studio Code.app",
        r"Programs\Microsoft VS Code\Code.exe",
    ),
    app(
        "cursor",
        "Cursor",
        "Cursor.app",
        r"Programs\cursor\Cursor.exe",
    ),
    app(
        "windsurf",
        "Windsurf",
        "Windsurf.app",
        r"Programs\Windsurf\Windsurf.exe",
    ),
];

const fn cli(id: &'static str, name: &'static str, command: &'static str) -> Known {
    Known {
        id,
        name,
        command: Some(command),
        mac_app: None,
        windows_app: None,
        folder: true,
    }
}

const fn app(
    id: &'static str,
    name: &'static str,
    mac: &'static str,
    windows: &'static str,
) -> Known {
    Known {
        id,
        name,
        command: None,
        mac_app: Some(mac),
        windows_app: Some(windows),
        folder: true,
    }
}

/// An agent found on this computer.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub id: String,
    /// The name the hub will give it: the launch waits for it.
    pub name: String,
    /// An app (launched by the system) rather than a command (launched in a terminal).
    pub app: bool,
    pub path: PathBuf,
    /// It opens on a folder: the command-line agents always, some apps.
    pub folder: bool,
}

impl Installed {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "app": self.app,
            "path": self.path.to_string_lossy(),
            "folder": self.folder,
            "system": system_modes(&self.id),
        })
    }
}

/// How a preset changes an agent's system prompt, written in a file next to the preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// After the agent's own.
    Append,
    /// In place of the agent's own.
    Replace,
}

impl Mode {
    pub fn parse(text: &str) -> Option<Mode> {
        match text {
            "append" => Some(Mode::Append),
            "replace" => Some(Mode::Replace),
            _ => None,
        }
    }
}

/// The ways an agent takes a system prompt at launch: Codex and Gemini CLI only replace it,
/// OpenCode only adds to it (through the `instructions` of a config file).
pub fn system_modes(agent: &str) -> &'static [&'static str] {
    match agent {
        "claude-code" => &["append", "replace"],
        "codex" | "gemini" => &["replace"],
        "opencode" => &["append"],
        _ => &[],
    }
}

/// What a preset adds to the launch of a command-line agent.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    /// The system prompt, in a file.
    pub system: Option<(Mode, PathBuf)>,
    /// For OpenCode, the config file that points to the system prompt: written by the caller,
    /// with [`opencode_config`].
    pub opencode_config: Option<PathBuf>,
    pub model: Option<String>,
}

/// What the agent gets on its command line and in its environment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extra {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// A model name: a word, with the dots, colons, slashes and brackets of the providers' names.
pub fn valid_model(model: &str) -> bool {
    (1..=100).contains(&model.len())
        && model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._:/@-[]".contains(c))
}

/// The arguments and the variables that carry the options to an agent, as each one takes
/// them; what it can't take is an error, not left out.
pub fn extra(agent: &str, options: &Options) -> Result<Extra> {
    let mut extra = Extra::default();
    if let Some(model) = &options.model {
        if !valid_model(model) {
            return Err(SetupError(format!("\"{model}\" isn't a model name.")));
        }
        extra.args.extend([
            if agent == "claude-code" {
                "--model"
            } else {
                "-m"
            }
            .to_owned(),
            model.clone(),
        ]);
    }
    if let Some((mode, file)) = &options.system {
        let path = file.to_string_lossy().into_owned();
        match (agent, mode) {
            ("claude-code", Mode::Append) => {
                extra
                    .args
                    .extend(["--append-system-prompt-file".into(), path]);
            }
            ("claude-code", Mode::Replace) => {
                extra.args.extend(["--system-prompt-file".into(), path]);
            }
            // A TOML literal string: no escapes, so the path can't hold a quote.
            ("codex", Mode::Replace) if !path.contains('\'') => {
                extra
                    .args
                    .extend(["-c".into(), format!("model_instructions_file='{path}'")]);
            }
            ("gemini", Mode::Replace) => extra.env.push(("GEMINI_SYSTEM_MD".into(), path)),
            ("opencode", Mode::Append) => {
                let config = options
                    .opencode_config
                    .as_ref()
                    .ok_or_else(|| SetupError::new("OpenCode needs its config file."))?;
                extra.env.push((
                    "OPENCODE_CONFIG".into(),
                    config.to_string_lossy().into_owned(),
                ));
            }
            _ => {
                return Err(SetupError::new(
                    "This agent can't take this system prompt at launch.",
                ));
            }
        }
    }
    Ok(extra)
}

/// The config file that hands OpenCode the system prompt: its `instructions`, merged with the
/// user's own config.
pub fn opencode_config(prompt: &Path) -> String {
    serde_json::to_string_pretty(&json!({ "instructions": [prompt.to_string_lossy()] }))
        .unwrap_or_default()
}

/// The known agents installed here, in the catalog's order.
pub fn installed(places: &Places) -> Vec<Installed> {
    KNOWN
        .iter()
        .filter_map(|known| {
            let path = match known.command {
                Some(command) => find_command(places, command)?,
                None => find_app(places, known)?,
            };
            Some(Installed {
                id: known.id.into(),
                name: known.name.into(),
                app: known.command.is_none(),
                path,
                folder: known.folder,
            })
        })
        .collect()
}

/// A command in the PATH, or where the installers put it even if the PATH doesn't say so.
fn find_command(places: &Places, command: &str) -> Option<PathBuf> {
    let home = &places.home;
    let separator = if cfg!(windows) { ';' } else { ':' };
    let mut folders: Vec<PathBuf> = places
        .var("PATH")
        .unwrap_or("")
        .split(separator)
        .filter(|f| !f.is_empty())
        .map(|f| PathBuf::from(f.trim_matches('"')))
        .collect();
    folders.extend([
        home.join(".local").join("bin"),
        home.join(".claude").join("local"),
    ]);
    if cfg!(windows) {
        folders.push(places.app_data.join("npm"));
    } else {
        folders.extend([
            home.join(".npm-global").join("bin"),
            home.join(".bun").join("bin"),
            home.join(".opencode").join("bin"),
            home.join(".volta").join("bin"),
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
        ]);
    }
    let names: Vec<String> = if cfg!(windows) {
        [".exe", ".cmd", ".bat"]
            .iter()
            .map(|ext| format!("{command}{ext}"))
            .collect()
    } else {
        vec![command.to_owned()]
    };
    folders
        .iter()
        .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
        .find(|candidate| executable(candidate))
}

fn find_app(places: &Places, known: &Known) -> Option<PathBuf> {
    if cfg!(windows) {
        let path = places.local_app_data.join(known.windows_app?);
        path.is_file().then_some(path)
    } else {
        let bundle = known.mac_app?;
        [
            PathBuf::from("/Applications").join(bundle),
            places.home.join("Applications").join(bundle),
        ]
        .into_iter()
        .find(|path| path.is_dir())
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

/// Launches `agent`. A command-line agent opens in a terminal in `folder` (the home if
/// none), with `token` in `CLAK_LAUNCH` and what a preset adds (`extra`); the script goes in
/// `scripts`. An app opens through the system, on `folder` if it takes one.
pub fn launch(
    places: &Places,
    agent: &Installed,
    folder: Option<&Path>,
    token: Option<&str>,
    extra: &Extra,
    scripts: &Path,
) -> Result<()> {
    let folder = folder.filter(|f| f.is_dir());
    if agent.app {
        return launch_app(agent, folder.filter(|_| agent.folder));
    }
    let folder = folder.unwrap_or(&places.home);
    clean(scripts);
    let script = write_script(scripts, &agent.path, folder, token, extra)?;
    let opened = open_terminal(places, &script, folder);
    if opened.is_err() {
        let _ = std::fs::remove_file(&script);
    }
    opened
}

/// The scripts nobody ran (the terminal didn't start): gone after an hour.
fn clean(scripts: &Path) {
    let Ok(entries) = std::fs::read_dir(scripts) else {
        return;
    };
    let hour_ago = SystemTime::now() - Duration::from_secs(3600);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|at| at < hour_ago);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The script that starts the agent: readable only by the user, deleted as it runs.
pub fn write_script(
    scripts: &Path,
    agent: &Path,
    folder: &Path,
    token: Option<&str>,
    extra: &Extra,
) -> Result<PathBuf> {
    std::fs::create_dir_all(scripts)?;
    private(scripts)?;
    let name = format!("launch-{}", clak_wire::json::random_hex(8));
    let (path, text) = if cfg!(windows) {
        (
            scripts.join(format!("{name}.cmd")),
            cmd_script(agent, folder, token, extra)?,
        )
    } else {
        (
            scripts.join(format!("{name}.command")),
            sh_script(agent, folder, token, extra),
        )
    };
    std::fs::write(&path, text)?;
    private(&path)?;
    Ok(path)
}

/// `#!/bin/sh`: the agent with its secret and its variables, then the user's shell, in the
/// folder. The variables are the agent's alone: not the shell's that follows.
pub fn sh_script(agent: &Path, folder: &Path, token: Option<&str>, extra: &Extra) -> String {
    let mut line: String = token
        .map(|t| ("CLAK_LAUNCH", t))
        .into_iter()
        .chain(extra.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .map(|(k, v)| format!("{k}={} ", sh_quote(v)))
        .collect();
    line.push_str(&sh_quote(&agent.to_string_lossy()));
    for arg in &extra.args {
        line.push(' ');
        line.push_str(&sh_quote(arg));
    }
    format!(
        "#!/bin/sh\n\
         rm -f -- \"$0\"\n\
         cd -- {} || exit 1\n\
         {line}\n\
         exec \"${{SHELL:-/bin/zsh}}\" -l\n",
        sh_quote(&folder.to_string_lossy()),
    )
}

/// A single block: `cmd` reads a batch file line by line, and this one deletes itself. The
/// variables are cleared after the agent: the console stays open, and a `gemini` typed
/// there later starts as usual. Every value goes between double quotes, where only `%`
/// still means something; a double quote can't be written there, and is refused.
pub fn cmd_script(
    agent: &Path,
    folder: &Path,
    token: Option<&str>,
    extra: &Extra,
) -> Result<String> {
    let quoted = |text: &str| -> Result<String> {
        if text.contains('"') {
            return Err(SetupError(format!("Can't pass {text} to the agent.")));
        }
        Ok(format!("\"{}\"", cmd_escape(text)))
    };
    let env = token
        .map(|t| ("CLAK_LAUNCH", t))
        .into_iter()
        .chain(extra.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let (mut set, mut unset) = (String::new(), String::new());
    for (k, v) in env {
        set.push_str(&format!(" & set {}", quoted(&format!("{k}={v}"))?));
        unset.push_str(&format!(" & set \"{k}=\""));
    }
    let mut call = format!("call {}", quoted(&agent.to_string_lossy())?);
    for arg in &extra.args {
        call.push(' ');
        call.push_str(&quoted(arg)?);
    }
    Ok(format!(
        "@echo off\r\n(del \"%~f0\" & cd /d {}{set} & {call}{unset})\r\n",
        quoted(&folder.to_string_lossy())?,
    ))
}

fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Inside double quotes only `%` still means something to `cmd`.
fn cmd_escape(text: &str) -> String {
    text.replace('%', "%%")
}

/// The folder and the script: the user's alone (the script runs, so it is executable).
#[cfg(unix)]
fn private(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// On Windows the user's profile is already the user's own.
#[cfg(not(unix))]
fn private(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_terminal(_places: &Places, script: &Path, _folder: &Path) -> Result<()> {
    run(std::process::Command::new("/usr/bin/open").arg(script))
}

#[cfg(windows)]
fn open_terminal(places: &Places, script: &Path, folder: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let terminal = places.local_app_data.join(r"Microsoft\WindowsApps\wt.exe");
    if terminal.is_file() {
        return run(std::process::Command::new(terminal)
            .arg("-d")
            .arg(folder)
            .args(["cmd", "/k"])
            .arg(script));
    }
    run(std::process::Command::new("cmd")
        .args(["/c", "start", "", "cmd", "/k"])
        .arg(script)
        .current_dir(folder)
        .creation_flags(CREATE_NO_WINDOW))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_terminal(_places: &Places, _script: &Path, _folder: &Path) -> Result<()> {
    Err(SetupError::new(
        "Launching a terminal works on the Mac and on Windows.",
    ))
}

#[cfg(target_os = "macos")]
fn launch_app(agent: &Installed, folder: Option<&Path>) -> Result<()> {
    let mut command = std::process::Command::new("/usr/bin/open");
    command.arg("-a").arg(&agent.path);
    if let Some(folder) = folder {
        command.arg(folder);
    }
    run(&mut command)
}

#[cfg(not(target_os = "macos"))]
fn launch_app(agent: &Installed, folder: Option<&Path>) -> Result<()> {
    let mut command = std::process::Command::new(&agent.path);
    if let Some(folder) = folder {
        command.arg(folder);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|e| SetupError(format!("Couldn't start {}: {e}", agent.name)))
}

#[cfg(any(target_os = "macos", windows))]
fn run(command: &mut std::process::Command) -> Result<()> {
    let status = command
        .status()
        .map_err(|e| SetupError(format!("Couldn't open the terminal: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(SetupError(format!(
            "Couldn't open the terminal ({status})."
        )))
    }
}

/// The catalog entry of an agent, by id: what a preset names.
pub fn find<'a>(installed: &'a [Installed], id: &str) -> Option<&'a Installed> {
    installed.iter().find(|a| a.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn temp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("clak-launch-{}", clak_wire::json::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn a_command_is_found_in_the_path_or_where_installers_put_it() {
        use std::collections::BTreeMap;
        use std::os::unix::fs::PermissionsExt;
        let home = temp();
        let bin = home.join("bin");
        let local = home.join(".local").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for file in [bin.join("codex"), local.join("claude")] {
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Not executable: not an agent.
        std::fs::write(bin.join("gemini"), "").unwrap();
        let environment = BTreeMap::from([("PATH".to_owned(), bin.to_string_lossy().into_owned())]);
        let places = Places::new(&home, home.join("data"), home.join("data"), environment);
        // Only what is in this home: the system's own folders (/usr/local/bin) are searched
        // too, and the computer running the tests may have an agent there.
        let found: Vec<(String, PathBuf)> = installed(&places)
            .into_iter()
            .filter(|a| !a.app && a.path.starts_with(&home))
            .map(|a| (a.id, a.path))
            .collect();
        assert_eq!(
            found,
            vec![
                ("claude-code".to_owned(), local.join("claude")),
                ("codex".to_owned(), bin.join("codex")),
            ]
        );
        let _ = std::fs::remove_dir_all(home);
    }

    /// What this computer has: by hand, `cargo test -p clak-setup installed_here -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn installed_here() {
        for agent in installed(&Places::real()) {
            println!("{}", agent.to_json());
        }
    }

    #[test]
    fn the_secret_goes_to_the_agent_only_and_quoted() {
        let script = sh_script(
            Path::new("/Users/me/.local/bin/claude"),
            Path::new("/Users/me/it's here"),
            Some("abc123"),
            &Extra::default(),
        );
        assert_eq!(
            script,
            "#!/bin/sh\nrm -f -- \"$0\"\ncd -- '/Users/me/it'\\''s here' || exit 1\n\
             CLAK_LAUNCH='abc123' '/Users/me/.local/bin/claude'\nexec \"${SHELL:-/bin/zsh}\" -l\n"
        );
        let plain = sh_script(
            Path::new("/bin/claude"),
            Path::new("/tmp"),
            None,
            &Extra::default(),
        );
        assert!(!plain.contains("CLAK_LAUNCH"));
    }

    #[test]
    fn the_variables_and_arguments_of_a_preset_are_the_agents_own() {
        let extra = Extra {
            args: vec!["-m".into(), "gemini-2.5-pro".into()],
            env: vec![(
                "GEMINI_SYSTEM_MD".into(),
                "/Users/me/.clak/presets/p/it's.md".into(),
            )],
        };
        let script = sh_script(
            Path::new("/bin/gemini"),
            Path::new("/tmp"),
            Some("s"),
            &extra,
        );
        assert!(script.contains(
            "CLAK_LAUNCH='s' GEMINI_SYSTEM_MD='/Users/me/.clak/presets/p/it'\\''s.md' '/bin/gemini' '-m' 'gemini-2.5-pro'\n"
        ));
    }

    #[test]
    fn the_windows_script_is_one_block_that_deletes_itself() {
        let script = cmd_script(
            Path::new(r"C:\Users\me\AppData\Roaming\npm\claude.cmd"),
            Path::new(r"C:\Work\100% done"),
            Some("abc123"),
            &Extra::default(),
        )
        .unwrap();
        assert_eq!(
            script,
            "@echo off\r\n(del \"%~f0\" & cd /d \"C:\\Work\\100%% done\" & set \"CLAK_LAUNCH=abc123\" \
             & call \"C:\\Users\\me\\AppData\\Roaming\\npm\\claude.cmd\" & set \"CLAK_LAUNCH=\")\r\n"
        );
        let extra = Extra {
            args: vec!["--model".into(), "opus".into()],
            env: vec![("GEMINI_SYSTEM_MD".into(), r"C:\Users\me (work)\s.md".into())],
        };
        let script = cmd_script(Path::new(r"C:\g.cmd"), Path::new(r"C:\w"), None, &extra).unwrap();
        assert_eq!(
            script,
            "@echo off\r\n(del \"%~f0\" & cd /d \"C:\\w\" & set \"GEMINI_SYSTEM_MD=C:\\Users\\me (work)\\s.md\" \
             & call \"C:\\g.cmd\" \"--model\" \"opus\" & set \"GEMINI_SYSTEM_MD=\")\r\n"
        );
        let quote = Extra {
            args: vec!["a\"b".into()],
            env: vec![],
        };
        assert!(cmd_script(Path::new(r"C:\g.cmd"), Path::new(r"C:\w"), None, &quote).is_err());
    }

    #[test]
    fn each_agent_takes_the_system_prompt_its_own_way() {
        let file = PathBuf::from("/h/.clak/presets/p/system.md");
        let options = |mode| Options {
            system: Some((mode, file.clone())),
            opencode_config: Some(PathBuf::from("/h/.clak/presets/p/opencode.json")),
            model: None,
        };
        let args = |agent, mode| extra(agent, &options(mode)).map(|e| (e.args, e.env));
        let path = file.to_string_lossy().into_owned();
        assert_eq!(
            args("claude-code", Mode::Append).unwrap().0,
            vec!["--append-system-prompt-file".to_owned(), path.clone()]
        );
        assert_eq!(
            args("claude-code", Mode::Replace).unwrap().0,
            vec!["--system-prompt-file".to_owned(), path.clone()]
        );
        assert_eq!(
            args("codex", Mode::Replace).unwrap().0,
            vec!["-c".to_owned(), format!("model_instructions_file='{path}'")]
        );
        assert_eq!(
            args("gemini", Mode::Replace).unwrap().1,
            vec![("GEMINI_SYSTEM_MD".to_owned(), path.clone())]
        );
        assert_eq!(
            args("opencode", Mode::Append).unwrap().1,
            vec![(
                "OPENCODE_CONFIG".to_owned(),
                "/h/.clak/presets/p/opencode.json".to_owned()
            )]
        );
        // What an agent can't take is refused, not left out.
        assert!(args("codex", Mode::Append).is_err());
        assert!(args("gemini", Mode::Append).is_err());
        assert!(args("opencode", Mode::Replace).is_err());
        for agent in ["claude-code", "codex", "gemini", "opencode"] {
            for mode in system_modes(agent) {
                assert!(
                    args(agent, Mode::parse(mode).unwrap()).is_ok(),
                    "{agent} {mode}"
                );
            }
        }
        let model = |agent, model: &str| {
            extra(
                agent,
                &Options {
                    model: Some(model.into()),
                    ..Options::default()
                },
            )
            .map(|e| e.args)
        };
        assert_eq!(
            model("claude-code", "opus").unwrap(),
            vec!["--model", "opus"]
        );
        assert_eq!(
            model("opencode", "anthropic/claude-sonnet-4").unwrap(),
            vec!["-m", "anthropic/claude-sonnet-4"]
        );
        assert!(model("codex", "gpt-5 & calc").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn the_script_is_the_users_own() {
        use std::os::unix::fs::PermissionsExt;
        let dir = temp().join("launch");
        let path = write_script(
            &dir,
            Path::new("/bin/claude"),
            Path::new("/tmp"),
            Some("s"),
            &Extra::default(),
        )
        .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o700);
        assert!(path.extension().is_some_and(|e| e == "command"));
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}

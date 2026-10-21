//! Settings: skill, PATH, MCP clients (the Integration part), start with the system.
//! The rules live in `clak-setup`; here only the commands for the pages.
//!
//! Each row of Settings › Integration has an id (`path`, `skill:<agent>`, `client:<name>`), a
//! state in words, a color (`ok`, `warn`, `err`, `off`), the file that would be touched and
//! the buttons it needs. Everything that reads or writes the disk or the registry, or runs
//! `claude mcp`, runs off the main thread.

use crate::bridge;
use clak_setup::clients::{self, ClientState, McpClient};
use clak_setup::skill::{Skill, SkillState};
use clak_setup::{Places, installation, shim};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// A row of Settings › Integration.
#[derive(Serialize)]
struct Row {
    id: String,
    name: String,
    state: String,
    /// ok | warn | err | off: the color of the dot (and of the pill).
    tone: &'static str,
    /// The file (or the link) that would be touched.
    path: String,
    /// The button that adds or updates, if needed.
    primary: Option<&'static str>,
    /// The button that removes, if there is something of ours to remove.
    remove: Option<&'static str>,
    /// The how, for the tooltip: the row only says the state.
    help: Option<String>,
    /// The entry to copy and paste by hand, if there is one.
    copy: bool,
}

/// What Settings › Integration shows and does, rebuilt on every request: the user and other
/// programs change those files too.
struct Setup {
    places: Places,
    /// `clak` next to the app, if it is there.
    cli: Option<String>,
    /// The shim command, if we know where it is: without it, there is nothing useful to write.
    shim: Option<Vec<String>>,
    skills: Vec<Skill>,
    clients: Vec<Box<dyn McpClient>>,
}

/// The app folder: `clak` and the shim sit next to it, as the installer puts them.
fn app_directory() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

impl Setup {
    fn new(home: &Path) -> Setup {
        let places = Places::real();
        let folder = app_directory();
        let cli = Some(folder.join(installation::CLI))
            .filter(|p| p.is_file())
            .map(|p| p.to_string_lossy().into_owned());
        let shim = shim::locate(&folder, home);
        let skills = cli
            .iter()
            .flat_map(|cli| Skill::agents(&places, cli.clone()))
            .collect();
        let command = shim
            .clone()
            .unwrap_or_else(|| vec![shim::FILE_NAME.to_owned()]);
        let clients = clients::all(&places, &command);
        Setup {
            places,
            cli,
            shim,
            skills,
            clients,
        }
    }

    /// A path inside the home, written as the user would write it (on the Mac).
    fn show(&self, path: &Path) -> String {
        let text = path.to_string_lossy().into_owned();
        if cfg!(windows) {
            return text;
        }
        let home = self.places.home.to_string_lossy();
        if text == home {
            "~".into()
        } else if let Some(rest) = text.strip_prefix(&format!("{home}/")) {
            format!("~/{rest}")
        } else {
            text
        }
    }

    fn view(&self) -> Value {
        json!({
            "cli": self.cli,
            "shim": self.shim.as_ref().map(|s| s.join(" ")),
            "shim_name": shim::FILE_NAME,
            "path": self.path_row(),
            "skills": self.skill_rows(),
            "clients": self.clients.iter().map(|c| self.client_row(c.as_ref())).collect::<Vec<_>>(),
        })
    }

    /// The command line on the PATH: the app folder on the user's PATH.
    #[cfg(windows)]
    fn path_row(&self) -> Option<Row> {
        let cli = self.cli.as_ref()?;
        let inside = clak_setup::user_path::UserPath::new(parent_of(cli)).contains();
        Some(Row {
            id: "path".into(),
            name: "clak".into(),
            state: if inside {
                "In PATH"
            } else {
                "Not in PATH · optional, the skill uses the full path"
            }
            .into(),
            tone: if inside { "ok" } else { "off" },
            path: cli.clone(),
            primary: (!inside).then_some("Add to PATH"),
            remove: inside.then_some("Remove from PATH"),
            help: (!inside).then(|| "User PATH, not the system one".to_owned()),
            copy: true,
        })
    }

    /// The command line on the PATH: a link in `~/.local/bin`.
    #[cfg(unix)]
    fn path_row(&self) -> Option<Row> {
        use clak_setup::user_path::PathLinkState as S;
        let link = self.link()?;
        let status = link.status();
        let folder = self.show(Path::new(link.folder()));
        let (state, tone) = match status.state {
            S::NoFolder => (
                format!("No {folder} · optional, the skill uses the full path"),
                "off",
            ),
            S::Absent => (
                "Not in PATH · optional, the skill uses the full path".to_owned(),
                "off",
            ),
            S::Ok => (format!("In PATH, linked from {folder}"), "ok"),
            S::Stale => (
                format!(
                    "Links to another copy: {}",
                    status.target.as_deref().unwrap_or("?")
                ),
                "warn",
            ),
            S::Foreign => (
                format!("Another “clak” in {folder}, not ours: left alone"),
                "err",
            ),
        };
        let help = (status.state == S::NoFolder).then(|| {
            format!(
                "To type just “clak”, create {folder} and put it on the PATH (for zsh, in ~/.zprofile: export PATH=\"$HOME/.local/bin:$PATH\"). The Claude Code installer does it by itself."
            )
        });
        Some(Row {
            id: "path".into(),
            name: "clak".into(),
            state,
            tone,
            path: self.show(Path::new(&status.path)),
            primary: match status.state {
                S::Absent => Some("Add to PATH"),
                S::Stale => Some("Update"),
                _ => None,
            },
            remove: matches!(status.state, S::Ok | S::Stale).then_some("Remove from PATH"),
            help,
            copy: true,
        })
    }

    #[cfg(unix)]
    fn link(&self) -> Option<clak_setup::user_path::PathLink> {
        self.cli
            .as_ref()
            .map(|cli| clak_setup::user_path::PathLink::user_bin(&self.places, cli.clone()))
    }

    /// The agents on this computer; with none of them, Claude Code, to say it's missing.
    fn skill_rows(&self) -> Vec<Row> {
        let present: Vec<Row> = self
            .skills
            .iter()
            .filter(|s| s.status().state != SkillState::NotInstalled)
            .map(|s| self.skill_row(s))
            .collect();
        if present.is_empty() {
            self.skills
                .first()
                .map(|s| self.skill_row(s))
                .into_iter()
                .collect()
        } else {
            present
        }
    }

    fn skill_row(&self, skill: &Skill) -> Row {
        let status = skill.status();
        let (state, tone) = match status.state {
            SkillState::NotInstalled => (format!("{} not found", skill.name()), "off"),
            SkillState::Absent => ("not installed".to_owned(), "off"),
            SkillState::Ok => ("installed".to_owned(), "ok"),
            SkillState::Stale => ("outdated".to_owned(), "warn"),
            SkillState::Foreign => (
                "another “clak” skill, not ours: left alone".to_owned(),
                "err",
            ),
        };
        Row {
            id: format!("skill:{}", skill.name()),
            name: skill.name().to_owned(),
            state,
            tone,
            path: self.show(&status.path),
            primary: match status.state {
                SkillState::Stale => Some("Update"),
                SkillState::Absent | SkillState::NotInstalled => Some("Install"),
                _ => None,
            },
            remove: matches!(status.state, SkillState::Ok | SkillState::Stale).then_some("Remove"),
            help: None,
            copy: false,
        }
    }

    fn client_row(&self, client: &dyn McpClient) -> Row {
        let status = client.status();
        let (state, tone) = match status.state {
            ClientState::NotInstalled => ("not found".to_owned(), "off"),
            ClientState::Absent => ("not configured".to_owned(), "off"),
            ClientState::Ok => ("configured".to_owned(), "ok"),
            ClientState::Stale => (format!("another shim: {}", status.detail), "warn"),
            ClientState::Unreadable => (status.detail.clone(), "err"),
        };
        let writable = self.shim.is_some();
        Row {
            id: format!("client:{}", client.name()),
            name: client.name().to_owned(),
            state,
            tone,
            path: self.show(&status.path),
            primary: match status.state {
                ClientState::Stale if writable => Some("Update"),
                ClientState::Absent if writable => Some("Add"),
                _ => None,
            },
            remove: matches!(status.state, ClientState::Ok | ClientState::Stale)
                .then_some("Remove"),
            help: None,
            copy: writable,
        }
    }

    fn skill(&self, id: &str) -> Option<&Skill> {
        self.skills
            .iter()
            .find(|s| format!("skill:{}", s.name()) == id)
    }

    fn client(&self, id: &str) -> Option<&dyn McpClient> {
        self.clients
            .iter()
            .find(|c| format!("client:{}", c.name()) == id)
            .map(|c| c.as_ref())
    }

    /// Adds (or updates) what the row says, and returns what the user must do for it to
    /// show, if anything: the row's state already says it is done.
    fn add(&self, id: &str) -> Result<Option<String>, String> {
        if id == "path" {
            return self.add_path();
        }
        if let Some(skill) = self.skill(id) {
            skill.install().map_err(|e| e.0)?;
            return Ok(None);
        }
        if let Some(client) = self.client(id) {
            if self.shim.is_none() {
                return Err(format!("Can't find {} next to this app.", shim::FILE_NAME));
            }
            client.add().map_err(|e| e.0)?;
            return Ok(note(client));
        }
        Err(format!("Don't know what “{id}” is."))
    }

    fn remove(&self, id: &str) -> Result<Option<String>, String> {
        if id == "path" {
            return self.remove_path();
        }
        if let Some(skill) = self.skill(id) {
            skill.remove().map_err(|e| e.0)?;
            return Ok(None);
        }
        if let Some(client) = self.client(id) {
            client.remove().map_err(|e| e.0)?;
            return Ok(note(client));
        }
        Err(format!("Don't know what “{id}” is."))
    }

    #[cfg(windows)]
    fn add_path(&self) -> Result<Option<String>, String> {
        let cli = self
            .cli
            .as_ref()
            .ok_or_else(|| format!("Can't find {} next to this app.", installation::CLI))?;
        clak_setup::user_path::UserPath::new(parent_of(cli))
            .add()
            .map_err(|e| e.0)?;
        Ok(None)
    }

    #[cfg(windows)]
    fn remove_path(&self) -> Result<Option<String>, String> {
        let cli = self
            .cli
            .as_ref()
            .ok_or_else(|| format!("Can't find {} next to this app.", installation::CLI))?;
        clak_setup::user_path::UserPath::new(parent_of(cli))
            .remove()
            .map_err(|e| e.0)?;
        Ok(None)
    }

    #[cfg(unix)]
    fn add_path(&self) -> Result<Option<String>, String> {
        let link = self
            .link()
            .ok_or_else(|| format!("Can't find {} next to this app.", installation::CLI))?;
        link.add().map_err(|e| e.0)?;
        Ok(None)
    }

    #[cfg(unix)]
    fn remove_path(&self) -> Result<Option<String>, String> {
        let link = self
            .link()
            .ok_or_else(|| format!("Can't find {} next to this app.", installation::CLI))?;
        link.remove().map_err(|e| e.0)?;
        Ok(None)
    }

    /// What the user would paste by hand: the path of `clak`, the text of the skill, a
    /// client's entry.
    fn snippet(&self, id: &str) -> Option<String> {
        if id == "path" {
            return self.cli.clone();
        }
        if id.starts_with("skill:") {
            return self.cli.as_deref().map(Skill::text);
        }
        self.shim.as_ref()?;
        self.client(id).map(|c| c.snippet())
    }
}

/// What the client asks of the user for the change to show (restarting it), if anything.
fn note(client: &dyn McpClient) -> Option<String> {
    Some(client.note())
        .filter(|n| !n.is_empty())
        .map(str::to_owned)
}

#[cfg(windows)]
fn parent_of(cli: &str) -> PathBuf {
    Path::new(cli)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// The work on disk, registry and `claude mcp`, off the main thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| e.to_string())
}

fn home(app: &AppHandle) -> PathBuf {
    bridge::of(app).home().clone()
}

/// How the computer is set up for agents: skill, PATH, MCP clients.
#[tauri::command]
pub async fn agents_status(app: AppHandle) -> Result<Value, String> {
    let home = home(&app);
    blocking(move || Setup::new(&home).view()).await
}

/// Adds or updates a row (`path`, `skill:…`, `client:…`): returns what to say below it, if
/// anything.
#[tauri::command]
pub async fn agents_add(app: AppHandle, id: String) -> Result<Option<String>, String> {
    let home = home(&app);
    blocking(move || Setup::new(&home).add(&id)).await?
}

/// Removes whatever of ours is in a row.
#[tauri::command]
pub async fn agents_remove(app: AppHandle, id: String) -> Result<Option<String>, String> {
    let home = home(&app);
    blocking(move || Setup::new(&home).remove(&id)).await?
}

/// The text to copy for a row: the path, the skill, the client's entry.
#[tauri::command]
pub async fn agents_snippet(app: AppHandle, id: String) -> Result<Option<String>, String> {
    let home = home(&app);
    blocking(move || Setup::new(&home).snippet(&id)).await
}

/// "Choose the shim…": the shim picked by hand, remembered in the preferences. False if the
/// user closed the window without choosing.
#[tauri::command]
pub async fn agents_choose_shim(app: AppHandle) -> Result<bool, String> {
    use tauri_plugin_dialog::DialogExt;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_title("Where is the shim?");
    dialog = if cfg!(windows) {
        dialog.add_filter("Clak shim", &["exe"])
    } else {
        dialog
    };
    dialog.pick_file(move |file| {
        let _ = sender.send(file);
    });
    let Some(file) = receiver.await.ok().flatten() else {
        return Ok(false);
    };
    let path = file.into_path().map_err(|e| e.to_string())?;
    let home = home(&app);
    blocking(move || {
        shim::remember(&home, &[path.to_string_lossy().into_owned()]).map_err(|e| e.0)
    })
    .await??;
    Ok(true)
}

/// Whatever pointed at an installation that is gone moves to this one (on the Mac, like
/// `--setup` on Windows): per row, what to say below it. On Windows it does nothing:
/// the installer does it.
#[tauri::command]
pub async fn agents_repair(app: AppHandle) -> Result<Value, String> {
    let home = home(&app);
    blocking(move || repair(&home)).await
}

#[cfg(unix)]
pub fn repair(home: &Path) -> Value {
    let setup = Setup::new(home);
    let (Some(cli), Some(shim)) = (
        setup.cli.as_ref(),
        setup.shim.as_ref().and_then(|s| s.first()),
    ) else {
        return json!({});
    };
    json!(installation::repair_gone(
        &setup.places,
        Path::new(cli),
        Path::new(shim)
    ))
}

#[cfg(windows)]
pub fn repair(_home: &Path) -> Value {
    json!({})
}

// --- Settings ---------------------------------------------------------------

/// The Settings tab: start with the system as it really is (it also changes from Task
/// Manager or Login Items), the notifications, the folder, the version.
#[tauri::command]
pub async fn settings_status(app: AppHandle) -> Result<Value, String> {
    use clak_setup::autostart::{Autostart, AutostartState};
    let bridge = bridge::of(&app);
    let notifications = bridge.prefs.notifications();
    let consent_sound = bridge.prefs.consent_sound();
    let connection_sound = bridge.prefs.connection_sound();
    let update_check = bridge.prefs.update_check();
    let auto_update = bridge.prefs.auto_update();
    let server_icons = bridge.prefs.server_icons();
    let home = bridge.home().to_string_lossy().into_owned();
    let version = app.package_info().version.to_string();
    let autostart = blocking(|| {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from(installation::TRAY_APP));
        let autostart = Autostart::new(&exe);
        let state = match autostart.state() {
            AutostartState::On => "on",
            AutostartState::Off => "off",
            AutostartState::DisabledBySystem => "disabled",
            AutostartState::Elsewhere => "elsewhere",
        };
        json!({ "state": state, "registered": autostart.registered(), "installed": installed(&exe) })
    })
    .await?;
    Ok(json!({
        "autostart": autostart,
        "notifications": notifications,
        "consent_sound": consent_sound,
        "connection_sound": connection_sound,
        "update_check": update_check,
        "auto_update": auto_update,
        "server_icons": server_icons,
        "update": crate::updates::update_status(),
        "home": home,
        "version": version,
    }))
}

/// Where the installer puts it (on the Mac, in /Applications): a copy elsewhere may vanish or
/// be a different one.
fn installed(exe: &Path) -> bool {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .is_some_and(|local| exe.starts_with(local.join("Programs")))
    } else {
        exe.starts_with("/Applications/")
    }
}

/// Opens a page of the system settings, or the hub folder.
/// `startup`: Startup apps (Login Items on the Mac); `notifications`; `home`.
#[tauri::command]
pub async fn settings_open(app: AppHandle, target: String) -> Result<(), String> {
    let url = match (target.as_str(), cfg!(windows)) {
        ("startup", true) => "ms-settings:startupapps",
        ("startup", false) => "x-apple.systempreferences:com.apple.LoginItems-Settings.extension",
        ("notifications", true) => "ms-settings:notifications",
        ("notifications", false) => {
            "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
        }
        ("home", _) => {
            let home = home(&app);
            let folder = home.clone();
            blocking(move || std::fs::create_dir_all(&folder))
                .await?
                .map_err(|e| e.to_string())?;
            return app
                .opener()
                .open_path(home.to_string_lossy(), None::<&str>)
                .map_err(|e| e.to_string());
        }
        _ => return Err(format!("unknown target: {target}")),
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

//! The backup from Settings › General: written by itself a few seconds after something
//! changes (a server, an API, a token, a preset, a permission), into the folder the user
//! chose, locked with their password; and restored from a file, after a check of each item on
//! this computer (see `clak_backup::plan`). Off until the user turns it on.
//!
//! What a backup holds in clear never reaches a page: the restore keeps it here, between
//! the check and the user's ticks, and the pages get only names, places and how each item
//! stands.

use crate::bridge;
use clak_apis::ApiStore;
use clak_backup::{Backup, Done, Lock, Plan, Settings, Targets};
use clak_connectors::ConnectorStore;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

/// After a change, so that a burst of them makes one backup.
const SETTLE: Duration = Duration::from_secs(5);
/// Between two looks at most: what changes without the hub saying so (an OAuth sign-in
/// refreshed with a new refresh token).
const EVERY: Duration = Duration::from_secs(10 * 60);
/// Between two backups at least, however busy the hub is.
const SPACING: Duration = Duration::from_secs(20);

#[derive(Default)]
pub struct Backups {
    /// One at a time: a backup, a change to the settings.
    gate: tokio::sync::Mutex<()>,
    wake: tokio::sync::Notify,
    /// The backup being restored, opened and checked: until the user restores or cancels.
    pending: Mutex<Option<Plan>>,
}

fn of(app: &AppHandle) -> Arc<Backups> {
    app.state::<Arc<Backups>>().inner().clone()
}

/// Something changed: a backup soon, if it is on.
pub fn nudge(app: &AppHandle) {
    if let Some(backups) = app.try_state::<Arc<Backups>>() {
        backups.wake.notify_one();
    }
}

/// The automatic backup, for as long as the app is open.
pub fn start(app: AppHandle) {
    app.manage(Arc::new(Backups::default()));
    tauri::async_runtime::spawn(async move {
        let backups = of(&app);
        // Whatever changed while the app was closed, once the hub has started.
        tokio::time::sleep(Duration::from_secs(30)).await;
        loop {
            // How it went is in backup.json, and the panel hears it.
            let _ = run(&app, false).await;
            tokio::time::sleep(SPACING).await;
            tokio::select! {
                _ = backups.wake.notified() => {}
                _ = tokio::time::sleep(EVERY) => {}
            }
            tokio::time::sleep(SETTLE).await;
        }
    });
}

/// A backup if it is on and something changed (`always`: in any case, even if off). The
/// pages hear how it went.
async fn run(app: &AppHandle, always: bool) -> Result<Done, String> {
    let backups = of(app);
    let _gate = backups.gate.lock().await;
    let home = bridge::of(app).home().clone();
    let settings = Settings::load(&home);
    if !always && !(settings.enabled && settings.ready()) {
        return Ok(Done::Unchanged);
    }
    let done = tauri::async_runtime::spawn_blocking(move || {
        let connectors = ConnectorStore::new(
            &home,
            Arc::from(clak_platform::default_box(&home, "connectors.secrets")),
        );
        clak_backup::back_up(&home, &connectors, &ApiStore::open(&home), always)
    })
    .await
    .map_err(|e| e.to_string())?;
    if !matches!(done, Ok(Done::Unchanged)) {
        let _ = app.emit("backup", status_of(app));
    }
    done
}

fn status_of(app: &AppHandle) -> Value {
    Settings::load(bridge::of(app).home()).view()
}

fn panel_only(window: &WebviewWindow) -> Result<(), String> {
    if window.label() == "panel" {
        Ok(())
    } else {
        Err("Only from the panel.".into())
    }
}

/// `{ enabled, folder, keep, password, last }`: whether a password was chosen, never the lock.
#[tauri::command]
pub fn backup_status(app: AppHandle) -> Value {
    status_of(&app)
}

/// Changes what is given: the folder, the password (a new one: the backups already written
/// keep theirs), how many are kept, on or off. Turned on, a backup is written at once.
#[tauri::command]
pub async fn backup_configure(
    app: AppHandle,
    window: WebviewWindow,
    folder: Option<String>,
    password: Option<String>,
    keep: Option<usize>,
    enabled: Option<bool>,
) -> Result<Value, String> {
    panel_only(&window)?;
    let lock = match password {
        Some(password) => Some(
            tauri::async_runtime::spawn_blocking(move || {
                Lock::new(&password, clak_backup::Cost::DEFAULT)
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    let backups = of(&app);
    let home = bridge::of(&app).home().clone();
    let on = {
        let _gate = backups.gate.lock().await;
        let mut settings = Settings::load(&home);
        if let Some(folder) = folder {
            settings.folder = Some(folder_for(&folder, &home)?);
            settings.fingerprint = None;
        }
        if let Some(lock) = lock {
            settings.set_lock(lock);
        }
        if let Some(keep) = keep {
            settings.keep = keep.clamp(1, clak_backup::settings::MAX_KEEP);
        }
        if let Some(enabled) = enabled {
            settings.enabled = enabled;
        }
        if settings.enabled && !settings.ready() {
            return Err("Choose a folder and a password first.".into());
        }
        settings
            .save(&home)
            .map_err(|e| format!("Couldn't save the backup's settings: {e}"))?;
        settings.enabled
    };
    // At once, not after the pause of the automatic ones: the panel shows how it went.
    if on {
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = run(&app, false).await;
        });
    }
    Ok(status_of(&app))
}

/// A folder for the backups: an absolute path, not the hub's own folder (a backup is for
/// when that one is lost).
fn folder_for(folder: &str, home: &Path) -> Result<PathBuf, String> {
    let path = PathBuf::from(folder.trim());
    if !path.is_absolute() {
        return Err("Choose a folder.".into());
    }
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    if same(&path, home) {
        return Err(
            "Not the hub's own folder: a backup is for when that one is lost. Choose another one."
                .into(),
        );
    }
    Ok(path)
}

/// A backup now, even if nothing changed.
#[tauri::command]
pub async fn backup_now(app: AppHandle, window: WebviewWindow) -> Result<Value, String> {
    panel_only(&window)?;
    run(&app, true).await?;
    Ok(status_of(&app))
}

/// Where the backups go, from the system picker.
#[tauri::command]
pub async fn backup_choose_folder(window: WebviewWindow) -> Option<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_title("Where Clak's backups go")
        .set_parent(&window)
        .pick_folder(move |path| {
            let _ = sender.send(path);
        });
    let path = receiver.await.ok().flatten()?;
    path.into_path()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// A backup to restore, from the system picker, starting in the backups' folder: its path
/// and what it says of itself (when, from which computer). `None` if the user closed it.
#[tauri::command]
pub async fn backup_restore_pick(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<Option<Value>, String> {
    panel_only(&window)?;
    let mut picker = window
        .dialog()
        .file()
        .set_title("Restore a backup of Clak")
        .add_filter("Clak backups", &[clak_backup::file::EXTENSION]);
    if let Some(folder) = Settings::load(bridge::of(&app).home())
        .folder
        .filter(|f| f.is_dir())
    {
        picker = picker.set_directory(folder);
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    picker.set_parent(&window).pick_file(move |path| {
        let _ = sender.send(path);
    });
    let Some(path) = receiver
        .await
        .ok()
        .flatten()
        .and_then(|p| p.into_path().ok())
    else {
        return Ok(None);
    };
    let backup = read(&path)?;
    Ok(Some(json!({
        "path": path.to_string_lossy(),
        "header": backup.header.to_json(),
    })))
}

fn read(path: &Path) -> Result<Backup, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Can't read {}: {e}", path.display()))?;
    Backup::decode(&bytes).map_err(|e| e.to_string())
}

/// The hub and its servers, only when the hub lives in the app: a restore changes it.
fn targets_of(app: &AppHandle) -> Result<Owned, String> {
    let bridge = bridge::of(app);
    let read_only = || crate::editors::READ_ONLY.to_owned();
    Ok(Owned {
        home: bridge.home().clone(),
        broker: bridge.broker().ok_or_else(read_only)?,
        connectors: bridge.connectors().ok_or_else(read_only)?,
        apis: bridge.apis(),
    })
}

/// What [`Targets`] borrows, owned: it crosses threads.
struct Owned {
    home: PathBuf,
    broker: clak_hub::Broker,
    connectors: Arc<clak_connectors::ConnectorHub>,
    apis: Option<Arc<clak_apis::ApiHub>>,
}

impl Owned {
    fn targets(&self) -> Targets<'_> {
        Targets {
            home: &self.home,
            broker: &self.broker,
            connectors: &self.connectors,
            apis: self.apis.as_deref(),
        }
    }

    /// What is here now. Slow (the PATH, on the Mac from the login shell).
    fn here(&self) -> clak_backup::Here {
        self.targets().here(clak_connectors::launch::user_path())
    }
}

/// Opens the backup at `path` with the password, and checks each item on this computer:
/// what can be restored, what is already here, what must be changed or pointed to.
#[tauri::command]
pub async fn backup_restore_open(
    app: AppHandle,
    window: WebviewWindow,
    path: String,
    password: String,
) -> Result<Value, String> {
    panel_only(&window)?;
    let owned = Arc::new(targets_of(&app)?);
    let plan = tauri::async_runtime::spawn_blocking(move || {
        let backup = read(Path::new(&path))?;
        let contents = backup.open(&password).map_err(|e| e.to_string())?;
        if contents.is_empty() {
            return Err("This backup is empty: there is nothing to restore.".to_owned());
        }
        let platform = owned.broker.options().platform.clone();
        Ok(Plan::check(
            backup.header.clone(),
            contents,
            &owned.here(),
            platform.as_ref(),
        ))
    })
    .await
    .map_err(|e| e.to_string())??;
    let view = plan.view();
    *of(&app).pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(plan);
    Ok(view)
}

/// The user points an application of the backup to where it is now on this computer: the
/// restore as it stands then, or `None` if they closed the picker.
#[tauri::command]
pub async fn backup_restore_locate(
    app: AppHandle,
    window: WebviewWindow,
    id: String,
) -> Result<Option<Value>, String> {
    panel_only(&window)?;
    let backups = of(&app);
    let was = {
        let pending = backups.pending.lock().unwrap_or_else(|p| p.into_inner());
        let plan = pending.as_ref().ok_or(GONE)?;
        let item = plan.item(&id).ok_or(GONE)?;
        item.detail.clone()
    };
    let Some(exe) = crate::commands::app_choose(window, was).await? else {
        return Ok(None);
    };
    let owned = Arc::new(targets_of(&app)?);
    let mut plan = backups
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .ok_or(GONE)?;
    let (plan, result) = tauri::async_runtime::spawn_blocking(move || {
        let platform = owned.broker.options().platform.clone();
        let result = plan
            .locate(&id, &exe, &owned.here(), platform.as_ref())
            .map(|_| ());
        (plan, result)
    })
    .await
    .map_err(|e| e.to_string())?;
    let view = plan.view();
    *backups.pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(plan);
    result?;
    Ok(Some(view))
}

const GONE: &str = "Open the backup again: the restore was closed.";

/// Restores the ticked items: for each one, what happened. The backup is then forgotten.
#[tauri::command]
pub async fn backup_restore_apply(
    app: AppHandle,
    window: WebviewWindow,
    ids: Vec<String>,
) -> Result<Vec<Value>, String> {
    panel_only(&window)?;
    let owned = targets_of(&app)?;
    let plan = of(&app)
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take()
        .ok_or(GONE)?;
    let chosen: HashSet<String> = ids.into_iter().collect();
    let outcomes = clak_backup::restore(&plan, &chosen, &owned.targets()).await;
    nudge(&app);
    Ok(outcomes.iter().map(clak_backup::Outcome::to_json).collect())
}

/// The user closed the restore: the opened backup is forgotten.
#[tauri::command]
pub fn backup_restore_cancel(app: AppHandle) {
    of(&app)
        .pending
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
}

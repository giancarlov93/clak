//! Whether a newer Clak is out: the latest GitHub release, read every hour and when the user
//! asks from Settings. The user is told (a notification, the Settings tab, the tray box), and
//! the update installs through Tauri's updater: on request, or by itself when the user opted
//! in and no agent is connected. The updater only accepts files signed with Clak's key (the
//! public half is in tauri.conf.json).

use crate::{bridge, notices};
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

const LATEST: &str = "https://api.github.com/repos/giancarlov93/clak/releases/latest";
const RELEASES: &str = "https://github.com/giancarlov93/clak/releases/";
/// Turns the checks off, whatever the preference says.
const OPT_OUT: &str = "CLAK_NO_UPDATE_CHECK";
const FIRST: Duration = Duration::from_secs(30);
/// Between two looks at GitHub. Measured on the clock: a computer that slept looks again as
/// soon as it wakes up.
const EVERY: Duration = Duration::from_secs(60 * 60);
/// How often the loop wakes up: to see whether a look is due, and whether an automatic install
/// waiting for the agents to go can start.
const TICK: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
}

#[derive(Default)]
struct Seen {
    /// The newer release, if there is one.
    newer: Option<Release>,
    /// The version already notified: once per version.
    notified: Option<String>,
    etag: Option<String>,
    /// When GitHub last answered.
    checked: Option<SystemTime>,
    /// When the last look started, answered or not.
    tried: Option<SystemTime>,
    /// Why the last look failed.
    error: Option<String>,
    checking: bool,
    installing: bool,
    /// The version whose automatic install failed: it is not tried again by itself.
    failed: Option<String>,
}

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Seen) -> T) -> T {
    let mut guard = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Seen::default))
}

/// The checks run only in a release build, with the preference on and no opt-out variable,
/// and not inside another app's package (see `setup::foreign_package`).
fn enabled(app: &AppHandle) -> bool {
    !cfg!(debug_assertions)
        && std::env::var_os(OPT_OUT).is_none()
        && crate::setup::foreign_package().is_none()
        && bridge::of(app).prefs.update_check()
}

/// Whether an automatic install goes ahead now: opted in, something newer, nobody connected.
pub fn install_now(auto: bool, newer: bool, busy: usize) -> bool {
    auto && newer && busy == 0
}

/// Whether a look at GitHub is due: never tried, tried an hour ago or more, or the clock
/// went back.
pub fn due(tried: Option<SystemTime>, now: SystemTime) -> bool {
    tried.is_none_or(|t| now.duration_since(t).map_or(true, |age| age >= EVERY))
}

/// Looks a little after the start, then every hour; an automatic install starts as soon as
/// no agent is connected.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST).await;
        loop {
            if enabled(&app) {
                if with(|seen| due(seen.tried, SystemTime::now())) {
                    check(&app, false).await;
                }
                let bridge = bridge::of(&app);
                let auto = bridge.prefs.auto_update();
                let newer = with(|seen| {
                    !seen.installing
                        && seen
                            .newer
                            .as_ref()
                            .is_some_and(|r| seen.failed.as_deref() != Some(r.version.as_str()))
                });
                if install_now(auto, newer, notices::connected(&bridge.state()))
                    && install(&app).await.is_err()
                {
                    with(|seen| seen.failed = seen.newer.as_ref().map(|r| r.version.clone()));
                }
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

/// Looks at GitHub now. A newer version is notified once, unless the user asked from
/// Settings: they are looking already.
async fn check(app: &AppHandle, asked: bool) {
    if with(|seen| std::mem::replace(&mut seen.checking, true)) {
        return;
    }
    with(|seen| seen.tried = Some(SystemTime::now()));
    emit(app);
    let current = app.package_info().version.to_string();
    let result = fetch(&current).await;
    let notify = with(|seen| {
        seen.checking = false;
        match result {
            Err(e) => {
                seen.error = Some(e.to_string());
                return None;
            }
            // Nothing new since the last time: what was seen then still holds.
            Ok(None) => {}
            Ok(Some(latest)) => {
                seen.newer = is_newer(&latest.version, &current).then_some(latest);
            }
        }
        seen.error = None;
        seen.checked = Some(SystemTime::now());
        let release = seen.newer.clone()?;
        if seen.notified.as_deref() == Some(release.version.as_str()) {
            return None;
        }
        seen.notified = Some(release.version.clone());
        Some(release)
    });
    emit(app);
    if let Some(release) = notify.filter(|_| !asked) {
        let _ = app
            .notification()
            .builder()
            .title(format!("Clak {} is available", release.version))
            .body(if bridge::of(app).prefs.auto_update() {
                "It installs by itself when no agent is connected."
            } else {
                "Install it from Settings › About."
            })
            .show();
    }
}

/// The latest release; `None` when nothing changed since the last time, or when it is not
/// one to offer.
async fn fetch(current: &str) -> Result<Option<Release>, reqwest::Error> {
    let client = reqwest::Client::builder()
        .user_agent(format!("Clak/{current}"))
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut request = client
        .get(LATEST)
        .header("Accept", "application/vnd.github+json");
    if let Some(etag) = with(|seen| seen.etag.clone()) {
        request = request.header("If-None-Match", etag);
    }
    let response = request.send().await?;
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let response = response.error_for_status()?;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body: Value = response.json().await?;
    let release = release_of(&body);
    if release.is_some() {
        with(|seen| seen.etag = etag);
    }
    Ok(release)
}

/// The version and page of a GitHub release. The page must be one of Clak's releases: it is
/// what the Download button opens.
pub fn release_of(body: &Value) -> Option<Release> {
    let tag = body.get("tag_name")?.as_str()?;
    let url = body.get("html_url")?.as_str()?;
    if body.get("draft").and_then(Value::as_bool) == Some(true)
        || body.get("prerelease").and_then(Value::as_bool) == Some(true)
        || !url.starts_with(RELEASES)
    {
        return None;
    }
    let version = tag.trim_start_matches('v');
    parse(version)?;
    Some(Release {
        version: version.to_owned(),
        url: url.to_owned(),
    })
}

fn parse(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    let triple = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(triple)
}

/// Whether `latest` comes after `current`. An unreadable version is never newer.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// What the Settings tab and the tray box show: the newer release, if any, when GitHub last
/// answered (seconds since 1970), why the last look failed, whether one is under way.
fn status() -> Value {
    let seconds = |t: SystemTime| t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    with(|seen| {
        json!({
            "newer": seen.newer.as_ref().map(|r| json!({ "version": r.version, "url": r.url })),
            "checked": seen.checked.map(seconds),
            "error": seen.error,
            "checking": seen.checking,
        })
    })
}

fn emit(app: &AppHandle) {
    let _ = app.emit("update", status());
}

#[tauri::command]
pub fn update_status() -> Value {
    status()
}

/// Looks at GitHub now, from Settings: also with the automatic checks turned off.
#[tauri::command]
pub async fn update_check_now(app: AppHandle) -> Result<Value, String> {
    if let Some(package) = crate::setup::foreign_package() {
        return Err(format!(
            "Clak is running inside the app package {package}. Quit Clak and open it again from the Start menu."
        ));
    }
    check(&app, true).await;
    Ok(status())
}

/// Opens the newer release's page in the browser.
#[tauri::command]
pub fn update_open(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let url = with(|seen| seen.newer.as_ref().map(|r| r.url.clone())).ok_or("no newer release")?;
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| e.to_string())
}

/// Downloads, checks the signature and installs: agents and programs are disconnected,
/// then Clak starts again on the new copy (on Windows the installer reopens it).
#[tauri::command]
pub async fn update_install(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
    if notices::connected(&bridge::of(&app).state()) > 0 {
        let (answer, asked) = tokio::sync::oneshot::channel();
        app.dialog()
            .message(
                "Agents and programs are disconnected while Clak updates, then it starts again.",
            )
            .title("Install the update now?")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Install".into(),
                "Cancel".into(),
            ))
            .show(move |yes| {
                let _ = answer.send(yes);
            });
        if !asked.await.unwrap_or(false) {
            return Ok(());
        }
    }
    install(&app).await
}

async fn install(app: &AppHandle) -> Result<(), String> {
    if let Some(package) = crate::setup::foreign_package() {
        return Err(format!(
            "Clak is running inside the app package {package}: the update would be installed there. Quit Clak and open it again from the Start menu."
        ));
    }
    if with(|seen| std::mem::replace(&mut seen.installing, true)) {
        return Err("An update is already being installed.".into());
    }
    let result = download_and_install(app).await;
    with(|seen| seen.installing = false);
    if let Err(e) = &result {
        let _ = app.emit("update-progress", json!({ "error": e }));
    }
    result
}

async fn download_and_install(app: &AppHandle) -> Result<(), String> {
    use tauri_plugin_updater::UpdaterExt;
    let text = |e: tauri_plugin_updater::Error| e.to_string();
    // On Windows the installer's own hook (--setup) reopens Clak: the updater mustn't as well.
    let updater = app
        .updater_builder()
        .restart_after_install(false)
        .build()
        .map_err(text)?;
    let update = updater
        .check()
        .await
        .map_err(text)?
        .ok_or("Clak is already up to date.")?;
    let version = update.version.clone();
    let mut downloaded = 0u64;
    let bytes = update
        .download(
            |chunk, total| {
                downloaded += chunk as u64;
                let _ = app.emit(
                    "update-progress",
                    json!({ "version": version, "downloaded": downloaded, "total": total }),
                );
            },
            || {},
        )
        .await
        .map_err(text)?;
    let _ = app.emit(
        "update-progress",
        json!({ "version": version, "installing": true }),
    );
    bridge::of(app).stop().await;
    // On Windows this starts the installer and exits. On the Mac it replaces the app; if it
    // fails, the old copy is still there: either way Clak starts again.
    let _ = update.install(&bytes);
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Over the network: `cargo test -p clak-app updates -- --ignored`.
    #[test]
    #[ignore]
    fn the_latest_release_is_read_from_github_then_not_modified() {
        tauri::async_runtime::block_on(async {
            let latest = fetch("0.0.1").await.unwrap().expect("a release");
            assert!(is_newer(&latest.version, "0.0.1"), "{latest:?}");
            assert!(latest.url.starts_with(RELEASES));
            assert_eq!(fetch("0.0.1").await.unwrap(), None);
        });
    }

    #[test]
    fn an_automatic_install_needs_the_opt_in_something_newer_and_no_agent() {
        assert!(install_now(true, true, 0));
        assert!(!install_now(false, true, 0));
        assert!(!install_now(true, false, 0));
        assert!(!install_now(true, true, 2));
    }

    #[test]
    fn a_look_is_due_every_hour_and_when_the_clock_went_back() {
        let now = SystemTime::now();
        assert!(due(None, now));
        assert!(!due(Some(now - Duration::from_secs(59 * 60)), now));
        assert!(due(Some(now - EVERY), now));
        assert!(due(Some(now - Duration::from_secs(9 * 60 * 60)), now));
        assert!(due(Some(now + Duration::from_secs(60)), now));
    }

    #[test]
    fn a_higher_version_is_newer_and_numbers_compare_as_numbers() {
        assert!(is_newer("0.3.12", "0.3.11"));
        assert!(is_newer("0.3.100", "0.3.99"));
        assert!(is_newer("0.4.0", "0.3.99"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.3.11", "0.3.11"));
        assert!(!is_newer("0.3.10", "0.3.11"));
        assert!(!is_newer("0.3", "0.2.0"));
        assert!(!is_newer("0.3.x", "0.2.0"));
        assert!(!is_newer("0.4.0", "dev"));
    }

    #[test]
    fn a_release_needs_a_version_and_a_page_of_clak() {
        let release = json!({
            "tag_name": "v0.3.12",
            "html_url": "https://github.com/giancarlov93/clak/releases/tag/v0.3.12",
            "draft": false,
            "prerelease": false,
        });
        assert_eq!(
            release_of(&release),
            Some(Release {
                version: "0.3.12".into(),
                url: "https://github.com/giancarlov93/clak/releases/tag/v0.3.12".into(),
            })
        );
        let mut elsewhere = release.clone();
        elsewhere["html_url"] = json!("https://example.com/clak");
        assert_eq!(release_of(&elsewhere), None);
        let mut pre = release.clone();
        pre["prerelease"] = json!(true);
        assert_eq!(release_of(&pre), None);
        let mut odd = release;
        odd["tag_name"] = json!("nightly");
        assert_eq!(release_of(&odd), None);
    }
}

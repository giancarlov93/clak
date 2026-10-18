//! Whether a newer Clak is out: the latest GitHub release, read now and then. It only tells
//! the user (a notification, and the Settings tab with a link to the release page); nothing
//! is downloaded or installed.

use crate::bridge;
use serde_json::{Value, json};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_notification::NotificationExt;

const LATEST: &str = "https://api.github.com/repos/giancarlov93/clak/releases/latest";
const RELEASES: &str = "https://github.com/giancarlov93/clak/releases/";
/// Turns the checks off, whatever the preference says.
const OPT_OUT: &str = "CLAK_NO_UPDATE_CHECK";
const FIRST: Duration = Duration::from_secs(30);
const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

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
}

static SEEN: Mutex<Option<Seen>> = Mutex::new(None);

fn with<T>(f: impl FnOnce(&mut Seen) -> T) -> T {
    let mut guard = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(Seen::default))
}

/// The checks run only in a release build, with the preference on and no opt-out variable.
fn enabled(app: &AppHandle) -> bool {
    !cfg!(debug_assertions)
        && std::env::var_os(OPT_OUT).is_none()
        && bridge::of(app).prefs.update_check()
}

/// Checks a little after the start, then every few hours.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST).await;
        loop {
            if enabled(&app) {
                check(&app).await;
            }
            tokio::time::sleep(EVERY).await;
        }
    });
}

async fn check(app: &AppHandle) {
    let current = app.package_info().version.to_string();
    let Ok(Some(latest)) = fetch(&current).await else {
        return;
    };
    let newer = is_newer(&latest.version, &current).then_some(latest);
    let notify = with(|seen| {
        seen.newer = newer.clone();
        match &newer {
            Some(r) if seen.notified.as_deref() != Some(r.version.as_str()) => {
                seen.notified = Some(r.version.clone());
                true
            }
            _ => false,
        }
    });
    let _ = app.emit("update", status_of(newer.as_ref()));
    if let Some(release) = newer.filter(|_| notify)
        && bridge::of(app).prefs.notifications()
    {
        let _ = app
            .notification()
            .builder()
            .title(format!("Clak {} is available", release.version))
            .body("Download it from Settings › About.")
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

fn status_of(newer: Option<&Release>) -> Value {
    match newer {
        Some(r) => json!({ "version": r.version, "url": r.url }),
        None => Value::Null,
    }
}

/// The newer release, if one was found: for the Settings tab.
#[tauri::command]
pub fn update_status() -> Value {
    with(|seen| status_of(seen.newer.as_ref()))
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

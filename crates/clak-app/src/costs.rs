//! The Costs tab: the agents' tokens (clak-setup's `costs`) at the API's prices. The prices
//! are LiteLLM's list, read at most once a day and kept in Clak's folder (`prices.json`):
//! without the network the one kept, and before the first the one that comes with Clak.

use crate::bridge;
use chrono::{Local, TimeZone};
use clak_setup::Places;
use clak_setup::costs::{self, Ledger, Prices};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::AppHandle;

const LIST: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
const FILE: &str = "prices.json";
/// How old the list can be before it is read again.
const FRESH: Duration = Duration::from_secs(24 * 3600);
/// After a failed attempt, the next one.
const RETRY: Duration = Duration::from_secs(3600);

/// The prices in use: where from, and when LiteLLM's list was read (`None`: Clak's own).
struct Known {
    prices: Arc<Prices>,
    updated: Option<u64>,
    asked: Option<Instant>,
}

/// One at a time: a second report waits for the list the first is reading.
static PRICES: LazyLock<tokio::sync::Mutex<Option<Known>>> = LazyLock::new(Default::default);

/// The files already read: only those that changed are read again.
static LEDGER: LazyLock<Mutex<Ledger>> = LazyLock::new(Default::default);

/// The rows of the Costs tab; with `refresh` the prices are read again from LiteLLM.
#[tauri::command]
pub async fn costs_report(app: AppHandle, refresh: Option<bool>) -> Result<Value, String> {
    let home = bridge::of(&app).home().clone();
    let (prices, updated) = prices(&home, refresh.unwrap_or(false)).await;
    let places = Places::real();
    let count = prices.len();
    let mut report = tauri::async_runtime::spawn_blocking(move || {
        let requests = LEDGER
            .lock()
            .map(|mut ledger| ledger.read(&places))
            .unwrap_or_default();
        costs::report(&requests, &prices, now(), local_day)
    })
    .await
    .map_err(|e| e.to_string())?;
    report["prices"] = json!({ "updated": updated, "models": count });
    Ok(report)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn local_day(at: i64) -> String {
    Local
        .timestamp_millis_opt(at)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

async fn prices(home: &Path, refresh: bool) -> (Arc<Prices>, Option<u64>) {
    let mut known = PRICES.lock().await;
    let known = known.get_or_insert_with(|| kept(&home.join(FILE)));
    let stale = known
        .updated
        .is_none_or(|at| (now() as u64).saturating_sub(at) > FRESH.as_millis() as u64);
    let due = refresh || (stale && known.asked.is_none_or(|at| at.elapsed() > RETRY));
    if due {
        known.asked = Some(Instant::now());
        if let Some(prices) = download(&home.join(FILE)).await {
            known.prices = Arc::new(prices);
            known.updated = Some(now() as u64);
        }
    }
    (known.prices.clone(), known.updated)
}

/// The list kept in Clak's folder, or else Clak's own.
fn kept(file: &Path) -> Known {
    let saved = std::fs::read_to_string(file)
        .ok()
        .and_then(|text| Prices::parse(&text));
    let updated = std::fs::metadata(file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    match saved {
        Some(prices) => Known {
            prices: Arc::new(prices),
            updated,
            asked: None,
        },
        None => Known {
            prices: Arc::new(Prices::builtin()),
            updated: None,
            asked: None,
        },
    }
}

/// LiteLLM's list, kept in Clak's folder once read: `None` if it can't be had.
async fn download(file: &Path) -> Option<Prices> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .ok()?;
    let response = client
        .get(LIST)
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let text = response.text().await.ok()?;
    let file = file.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        let prices = Prices::parse(&text)?;
        let draft = PathBuf::from(format!("{}.part", file.display()));
        if std::fs::write(&draft, &text).is_ok() {
            let _ = std::fs::rename(&draft, &file);
        }
        Some(prices)
    })
    .await
    .ok()
    .flatten()
}

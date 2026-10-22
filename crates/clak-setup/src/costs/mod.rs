//! What the agents spent, from the Usage tab: the tokens of every request, read from the
//! agents' own files, and their price at the API's rates (`prices`). With a subscription
//! the user pays a flat fee: these are what the same tokens would cost through the API.
//!
//! Each agent writes its tokens its own way (`read`): Claude Code on each answer in its
//! conversation files (Claude Desktop's Code tab writes the same files), Codex as running
//! totals in its session files, Gemini CLI on each answer in its session files, OpenCode on
//! each answer in its database, with the cost it worked out itself. The files are only read,
//! and read again only when they changed (`Ledger`).

pub mod prices;
mod read;

pub use prices::Prices;

use crate::Places;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::SystemTime;

const CLAUDE_CODE: &str = "claude-code";
const CODEX: &str = "codex";
const GEMINI: &str = "gemini";
const OPENCODE: &str = "opencode";

/// The agents whose tokens are read, in the order the panel draws them.
pub const AGENTS: [(&str, &str); 4] = [
    (CLAUDE_CODE, "Claude Code"),
    (CODEX, "Codex"),
    (GEMINI, "Gemini CLI"),
    (OPENCODE, "OpenCode"),
];

/// Claude's usage limits count in windows of five hours, from the hour of the first request.
const WINDOW: i64 = 5 * 3600 * 1000;

/// The tokens of a request, each kind at its own rate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    /// Read without the cache.
    pub input: u64,
    /// Written, the reasoning included.
    pub output: u64,
    /// Put in the cache for five minutes (or for as long as the provider keeps it).
    pub cache_write: u64,
    /// Put in Claude's cache for an hour: dearer.
    pub cache_write_hour: u64,
    /// Read from the cache: cheaper.
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_write_hour + self.cache_read
    }

    /// What the model read: the prices above 200k tokens go by this.
    pub fn prompt(&self) -> u64 {
        self.input + self.cache_write + self.cache_write_hour + self.cache_read
    }

    fn add(&mut self, other: &Tokens) {
        self.input += other.input;
        self.output += other.output;
        self.cache_write += other.cache_write;
        self.cache_write_hour += other.cache_write_hour;
        self.cache_read += other.cache_read;
    }
}

/// A request to a model, as an agent wrote it down.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub agent: &'static str,
    /// When, in milliseconds since 1970.
    pub at: i64,
    pub model: String,
    /// Who served it, when the agent says: OpenCode's `anthropic`, `openrouter`.
    pub provider: Option<String>,
    /// The folder the agent worked in.
    pub folder: Option<String>,
    /// The conversation it belongs to.
    pub session: String,
    pub tokens: Tokens,
    /// The cost the agent worked out itself, in dollars.
    pub recorded: Option<f64>,
    /// The same answer is written more than once (Claude Code writes each part of it, and
    /// again in a resumed conversation's file): counted once by this.
    pub(crate) key: Option<String>,
}

impl Request {
    /// At the API's rates; with the agent's own cost when the model has no price.
    pub fn cost(&self, prices: &Prices) -> Option<f64> {
        prices
            .cost(&self.model, self.provider.as_deref(), &self.tokens)
            .or(self.recorded)
    }
}

/// When a file was last read: its size and when it was written.
type Stamp = (u64, Option<SystemTime>);

/// The requests read from each file, read again only when the file changed.
#[derive(Default)]
pub struct Ledger {
    files: HashMap<PathBuf, (Stamp, Vec<Request>)>,
}

impl Ledger {
    pub fn new() -> Ledger {
        Ledger::default()
    }

    /// Every agent's requests, each once.
    pub fn read(&mut self, places: &Places) -> Vec<Request> {
        let mut found = HashSet::new();
        let mut all = Vec::new();
        let mut seen = HashSet::new();
        for source in read::sources(places) {
            let stamp = source.stamp();
            found.insert(source.path.clone());
            if !matches!(self.files.get(&source.path), Some((known, _)) if *known == stamp) {
                let requests = source.read();
                self.files.insert(source.path.clone(), (stamp, requests));
            }
            for request in &self.files[&source.path].1 {
                if request.key.as_ref().is_none_or(|k| seen.insert(k.clone())) {
                    all.push(request.clone());
                }
            }
        }
        self.files.retain(|path, _| found.contains(path));
        all
    }
}

/// The requests as the panel adds them up: a row per day, agent, model, folder and
/// conversation; the Claude window still open; the models with no price. `day` is the
/// local date of a time, as `2026-10-22`.
pub fn report(
    requests: &[Request],
    prices: &Prices,
    now: i64,
    day: impl Fn(i64) -> String,
) -> Value {
    #[derive(Default)]
    struct Row {
        tokens: Tokens,
        cost: f64,
        requests: u64,
        unpriced: u64,
    }
    let mut rows: BTreeMap<(String, &str, &str, Option<&str>, &str), Row> = BTreeMap::new();
    let mut unpriced = BTreeMap::<&str, u64>::new();
    for request in requests {
        let row = rows
            .entry((
                day(request.at),
                request.agent,
                &request.model,
                request.folder.as_deref(),
                &request.session,
            ))
            .or_default();
        row.tokens.add(&request.tokens);
        row.requests += 1;
        match request.cost(prices) {
            Some(cost) => row.cost += cost,
            None => {
                row.unpriced += 1;
                *unpriced.entry(&request.model).or_default() += request.tokens.total();
            }
        }
    }
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|((day, agent, model, folder, session), row)| {
            json!({
                "day": day,
                "agent": agent,
                "model": model,
                "folder": folder,
                "session": session,
                "input": row.tokens.input,
                "output": row.tokens.output,
                "cache_write": row.tokens.cache_write + row.tokens.cache_write_hour,
                "cache_read": row.tokens.cache_read,
                "cost": row.cost,
                "requests": row.requests,
                "unpriced": row.unpriced,
            })
        })
        .collect();
    json!({
        "agents": AGENTS.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
        "rows": rows,
        "window": window(requests, prices, now),
        "unpriced": unpriced.into_iter().map(|(model, tokens)| json!({ "model": model, "tokens": tokens })).collect::<Vec<_>>(),
    })
}

/// Claude Code's five-hour window, if one is open now: it starts at the hour of the first
/// request after five hours without one, or after the previous window ended.
fn window(requests: &[Request], prices: &Prices, now: i64) -> Value {
    let mut claude: Vec<&Request> = requests.iter().filter(|r| r.agent == CLAUDE_CODE).collect();
    claude.sort_by_key(|r| r.at);
    let mut start = None::<i64>;
    let mut last = i64::MIN;
    let mut open: Vec<&Request> = Vec::new();
    for request in claude {
        let ended = start.is_none_or(|s| request.at >= s + WINDOW || request.at - last >= WINDOW);
        if ended {
            start = Some(request.at - request.at.rem_euclid(3600 * 1000));
            open.clear();
        }
        last = request.at;
        open.push(request);
    }
    let Some(start) = start else {
        return Value::Null;
    };
    if now >= start + WINDOW || now - last >= WINDOW {
        return Value::Null;
    }
    let mut tokens = Tokens::default();
    let mut cost = 0.0;
    for request in &open {
        tokens.add(&request.tokens);
        cost += request.cost(prices).unwrap_or(0.0);
    }
    json!({
        "start": start,
        "end": start + WINDOW,
        "first": open.first().map(|r| r.at),
        "last": last,
        "requests": open.len(),
        "tokens": tokens.total(),
        "cost": cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3600 * 1000;

    fn prices() -> Prices {
        Prices::parse(r#"{ "m": { "input_cost_per_token": 1e-6, "output_cost_per_token": 1e-5 } }"#)
            .unwrap()
    }

    fn request(agent: &'static str, at: i64, model: &str, session: &str) -> Request {
        Request {
            agent,
            at,
            model: model.into(),
            provider: None,
            folder: Some("/work".into()),
            session: session.into(),
            tokens: Tokens {
                input: 1000,
                output: 100,
                ..Tokens::default()
            },
            recorded: None,
            key: None,
        }
    }

    #[test]
    fn rows_add_up_by_day_agent_model_folder_and_conversation() {
        let day = |at: i64| format!("d{}", at / (24 * HOUR));
        let requests = [
            request(CLAUDE_CODE, HOUR, "m", "a"),
            request(CLAUDE_CODE, 2 * HOUR, "m", "a"),
            request(CLAUDE_CODE, 3 * HOUR, "m", "b"),
            request(CODEX, 25 * HOUR, "unknown", "c"),
        ];
        let report = report(&requests, &prices(), 100 * HOUR, day);
        let rows = report["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["session"], "a");
        assert_eq!(rows[0]["requests"], 2);
        assert_eq!(rows[0]["input"], 2000);
        assert!(
            (rows[0]["cost"].as_f64().unwrap() - 2.0 * (1000.0 * 1e-6 + 100.0 * 1e-5)).abs()
                < 1e-12
        );
        assert_eq!(rows[2]["day"], "d1");
        assert_eq!(rows[2]["unpriced"], 1);
        assert_eq!(report["unpriced"][0]["model"], "unknown");
        // Long gone: no window open.
        assert!(report["window"].is_null());
    }

    #[test]
    fn the_window_starts_at_the_hour_of_the_first_request_after_a_pause() {
        let at = |h: f64| (h * HOUR as f64) as i64;
        let requests = [
            request(CLAUDE_CODE, at(1.5), "m", "a"),
            // After five hours: a new window, from 7:00.
            request(CLAUDE_CODE, at(7.25), "m", "a"),
            request(CLAUDE_CODE, at(9.0), "m", "a"),
            // Not Claude's.
            request(CODEX, at(9.5), "m", "c"),
        ];
        let open = window(&requests, &prices(), at(10.0));
        assert_eq!(open["start"], at(7.0));
        assert_eq!(open["end"], at(12.0));
        assert_eq!(open["requests"], 2);
        assert_eq!(open["first"], at(7.25));
        // Past its end, no window.
        assert!(window(&requests, &prices(), at(12.0)).is_null());
        // One past the end of the window opens the next.
        let later = [
            request(CLAUDE_CODE, at(7.25), "m", "a"),
            request(CLAUDE_CODE, at(12.5), "m", "a"),
        ];
        assert_eq!(window(&later, &prices(), at(13.0))["start"], at(12.0));
    }
}

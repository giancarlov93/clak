//! The agents' tokens, read from their own files: each answer once, each kind of token at
//! its own rate, the folder where the conversation started; what changed read again, what
//! is gone forgotten.

mod common;

use clak_setup::costs::{Ledger, Prices, Request};
use common::Sandbox;

const PROJECT: &str = "home/.claude/projects/-work-app";

fn prices() -> Prices {
    Prices::parse(
        r#"{
            "claude-sonnet-9": {
                "input_cost_per_token": 3e-6, "output_cost_per_token": 15e-6,
                "cache_creation_input_token_cost": 3.75e-6,
                "cache_creation_input_token_cost_above_1hr": 6e-6,
                "cache_read_input_token_cost": 3e-7
            },
            "gpt-9": { "input_cost_per_token": 1e-6, "output_cost_per_token": 8e-6, "cache_read_input_token_cost": 1e-7 },
            "gemini/gemini-9-pro": { "input_cost_per_token": 2e-6, "output_cost_per_token": 1e-5, "cache_read_input_token_cost": 2e-7 }
        }"#,
    )
    .unwrap()
}

fn of<'a>(all: &'a [Request], agent: &str) -> Vec<&'a Request> {
    let mut found: Vec<&Request> = all.iter().filter(|r| r.agent == agent).collect();
    found.sort_by_key(|r| r.at);
    found
}

fn claude_answer(message: &str, request: &str, at: &str, cwd: &str, usage: &str) -> String {
    format!(
        r#"{{"type":"assistant","timestamp":"{at}","cwd":"{cwd}","sessionId":"s1","requestId":"{request}","message":{{"id":"{message}","model":"claude-sonnet-9","usage":{usage}}}}}"#
    )
}

#[test]
fn claude_code_counts_each_answer_once_with_the_cache_of_an_hour() {
    let sandbox = Sandbox::new();
    let usage = r#"{"input_tokens":10,"output_tokens":200,"cache_creation_input_tokens":1500,"cache_read_input_tokens":4000,"cache_creation":{"ephemeral_5m_input_tokens":500,"ephemeral_1h_input_tokens":1000}}"#;
    let first = claude_answer(
        "msg_1",
        "req_1",
        "2026-10-21T10:00:00.000Z",
        "/work/app",
        usage,
    );
    let lines = [
        r#"{"type":"user","timestamp":"2026-10-21T09:59:00.000Z","cwd":"/work/app","sessionId":"s1","message":{"role":"user","content":"hi"}}"#.to_owned(),
        // An answer's first part, written while its output was still growing; then its last.
        first.replace(r#""output_tokens":200"#, r#""output_tokens":150"#),
        first.clone(),
        claude_answer("msg_2", "req_2", "2026-10-21T10:01:00.000Z", "/work/app/src", r#"{"input_tokens":5,"output_tokens":50}"#),
        r#"{"type":"assistant","timestamp":"2026-10-21T10:02:00.000Z","sessionId":"s1","message":{"id":"msg_3","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#.to_owned(),
    ];
    sandbox.write(&format!("{PROJECT}/s1.jsonl"), &lines.join("\n"));
    // A resumed conversation copies the earlier answers into its own file.
    sandbox.write(&format!("{PROJECT}/s2.jsonl"), &first);
    sandbox.write(
        &format!("{PROJECT}/s1/subagents/agent-a1.jsonl"),
        &claude_answer(
            "msg_4",
            "req_4",
            "2026-10-21T10:03:00.000Z",
            "/work/app",
            r#"{"input_tokens":1,"output_tokens":1}"#,
        ),
    );

    let all = Ledger::new().read(&sandbox.places);
    let claude = of(&all, "claude-code");
    assert_eq!(claude.len(), 3);
    let tokens = claude[0].tokens;
    assert_eq!(
        (
            tokens.input,
            tokens.output,
            tokens.cache_write,
            tokens.cache_write_hour,
            tokens.cache_read
        ),
        (10, 200, 500, 1000, 4000)
    );
    // Where it started, not where the agent moved to.
    assert!(
        claude
            .iter()
            .all(|r| r.folder.as_deref() == Some("/work/app"))
    );
    assert!(claude.iter().all(|r| r.session == "s1"));
    let cost = claude[0].cost(&prices()).unwrap();
    let expected = 10.0 * 3e-6 + 200.0 * 15e-6 + 500.0 * 3.75e-6 + 1000.0 * 6e-6 + 4000.0 * 3e-7;
    assert!((cost - expected).abs() < 1e-12);
}

#[test]
fn codex_counts_what_its_running_total_grew_by() {
    let sandbox = Sandbox::new();
    let total = |at: &str, input: u64, cached: u64, output: u64| {
        format!(
            r#"{{"timestamp":"{at}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":0,"total_tokens":0}}}}}}}}"#
        )
    };
    let lines = [
        r#"{"timestamp":"2026-10-21T10:00:00.000Z","type":"session_meta","payload":{"id":"cx-1","cwd":"/work/api"}}"#.to_owned(),
        r#"{"timestamp":"2026-10-21T10:00:01.000Z","type":"turn_context","payload":{"cwd":"/work/api","model":"gpt-9"}}"#.to_owned(),
        r#"{"timestamp":"2026-10-21T10:00:02.000Z","type":"event_msg","payload":{"type":"token_count","info":null}}"#.to_owned(),
        total("2026-10-21T10:00:03.000Z", 1000, 400, 100),
        // Written again with the limits: nothing new.
        total("2026-10-21T10:00:04.000Z", 1000, 400, 100),
        total("2026-10-21T10:01:00.000Z", 3000, 2400, 150),
    ];
    sandbox.write(
        "home/.codex/sessions/2026/10/21/rollout-2026-10-21T10-00-00-cx-1.jsonl",
        &lines.join("\n"),
    );
    let all = Ledger::new().read(&sandbox.places);
    let codex = of(&all, "codex");
    assert_eq!(codex.len(), 2);
    let used: Vec<(u64, u64, u64)> = codex
        .iter()
        .map(|r| (r.tokens.input, r.tokens.cache_read, r.tokens.output))
        .collect();
    assert_eq!(used, [(600, 400, 100), (0, 2000, 50)]);
    assert!(
        codex
            .iter()
            .all(|r| r.model == "gpt-9" && r.session == "cx-1")
    );
    assert_eq!(codex[0].folder.as_deref(), Some("/work/api"));
    let cost = codex[1].cost(&prices()).unwrap();
    assert!((cost - (2000.0 * 1e-7 + 50.0 * 8e-6)).abs() < 1e-12);
}

#[test]
fn gemini_counts_its_answers_with_the_thoughts_as_output() {
    let sandbox = Sandbox::new();
    sandbox.write("home/.gemini/tmp/abc/.project_root", "/work/site\n");
    let answer = |tokens: &str| {
        format!(
            r#"{{"id":"m2","type":"gemini","timestamp":"2026-10-21T10:00:05.000Z","model":"gemini-9-pro","content":"ok","tokens":{tokens}}}"#
        )
    };
    let lines = [
        r#"{"sessionId":"g-1","projectHash":"abc","startTime":"2026-10-21T10:00:00.000Z"}"#
            .to_owned(),
        r#"{"id":"m1","type":"user","timestamp":"2026-10-21T10:00:00.000Z","content":"hi"}"#
            .to_owned(),
        answer(r#"{"input":10,"output":1,"cached":0,"thoughts":0,"tool":0,"total":11}"#),
        // The same message, finished: it takes the place of the first.
        answer(r#"{"input":1200,"output":80,"cached":1000,"thoughts":20,"tool":5,"total":1305}"#),
    ];
    sandbox.write(
        "home/.gemini/tmp/abc/chats/session-2026-10-21T10-00-g1.jsonl",
        &lines.join("\n"),
    );
    let all = Ledger::new().read(&sandbox.places);
    let gemini = of(&all, "gemini");
    assert_eq!(gemini.len(), 1);
    let t = gemini[0].tokens;
    assert_eq!((t.input, t.output, t.cache_read), (205, 100, 1000));
    assert_eq!(gemini[0].folder.as_deref(), Some("/work/site"));
    assert_eq!(gemini[0].session, "g-1");
    // `gemini-9-pro` is priced as `gemini/gemini-9-pro`.
    assert!(gemini[0].cost(&prices()).is_some());
}

#[test]
fn opencode_has_its_own_cost_for_the_models_without_a_price() {
    let sandbox = Sandbox::new();
    let db_path = sandbox
        .dir(&["home", ".local", "share", "opencode"])
        .join("opencode.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, directory TEXT NOT NULL);
         CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
            type TEXT NOT NULL, data TEXT NOT NULL);
         INSERT INTO session_v2 VALUES ('ses_1', '/work/tool');",
    )
    .unwrap();
    let add = |id: &str, kind: &str, data: &str| {
        db.execute(
            "INSERT INTO session_message VALUES (?1, 'ses_1', ?2, ?3)",
            rusqlite::params![id, kind, data],
        )
        .unwrap();
    };
    add("m1", "user", r#"{"time":{"created":1792591200000}}"#);
    add(
        "m2",
        "assistant",
        r#"{"time":{"created":1792591201000},"model":{"id":"house-model-free","providerID":"opencode"},"cost":0,"tokens":{"input":100,"output":10,"reasoning":5,"cache":{"read":900,"write":0}}}"#,
    );
    add(
        "m3",
        "assistant",
        r#"{"time":{"created":1792591202000},"model":{"id":"claude-sonnet-9","providerID":"anthropic"},"cost":0,"tokens":{"input":100,"output":10,"reasoning":0,"cache":{"read":0,"write":0}}}"#,
    );
    // Stopped before it answered: no tokens.
    add(
        "m4",
        "assistant",
        r#"{"time":{"created":1792591203000},"model":{"id":"claude-sonnet-9","providerID":"anthropic"},"finish":"error"}"#,
    );
    drop(db);
    let all = Ledger::new().read(&sandbox.places);
    let opencode = of(&all, "opencode");
    assert_eq!(opencode.len(), 2);
    assert_eq!(opencode[0].tokens.output, 15);
    assert_eq!(opencode[0].folder.as_deref(), Some("/work/tool"));
    let p = prices();
    // No price: what OpenCode said it cost.
    assert_eq!(opencode[0].cost(&p), Some(0.0));
    // A price: the API's, a subscription's $0 notwithstanding.
    assert!((opencode[1].cost(&p).unwrap() - (100.0 * 3e-6 + 10.0 * 15e-6)).abs() < 1e-12);
}

#[test]
fn only_what_changed_is_read_again_and_what_is_gone_is_forgotten() {
    let sandbox = Sandbox::new();
    let usage = r#"{"input_tokens":1,"output_tokens":1}"#;
    let one = claude_answer(
        "msg_1",
        "req_1",
        "2026-10-21T10:00:00.000Z",
        "/work/app",
        usage,
    );
    let file = sandbox.write(&format!("{PROJECT}/s1.jsonl"), &one);
    let mut ledger = Ledger::new();
    assert_eq!(ledger.read(&sandbox.places).len(), 1);
    let two = claude_answer(
        "msg_2",
        "req_2",
        "2026-10-21T10:05:00.000Z",
        "/work/app",
        usage,
    );
    sandbox.write(&format!("{PROJECT}/s1.jsonl"), &format!("{one}\n{two}\n"));
    assert_eq!(ledger.read(&sandbox.places).len(), 2);
    std::fs::remove_file(file).unwrap();
    assert!(ledger.read(&sandbox.places).is_empty());
}

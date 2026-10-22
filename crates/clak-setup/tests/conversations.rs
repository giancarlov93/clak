//! Claude Code's past conversations: listed with their title and folder, renamed by adding a
//! line, resumed in the folder where they took place, deleted with what is kept beside them.
//! Claude Desktop's Code sessions: listed as the app shows them, opened in it, never changed.

mod common;

use clak_setup::conversations;
use common::{Sandbox, read};
use serde_json::json;

const PROJECT: &str = "home/.claude/projects/-work-app";

fn line(value: serde_json::Value) -> String {
    format!("{value}\n")
}

/// A conversation in `folder`: a reminder, then the user's prompt, then an answer.
fn conversation(sandbox: &Sandbox, id: &str, folder: &str, prompt: &str) {
    let text = [
        line(json!({ "type": "user", "isMeta": true, "cwd": folder, "sessionId": id,
            "message": { "role": "user", "content": "<local-command-caveat>Caveat</local-command-caveat>" } })),
        line(json!({ "type": "user", "cwd": folder, "sessionId": id,
            "message": { "role": "user", "content": [{ "type": "text", "text": prompt }] } })),
        line(json!({ "type": "assistant", "cwd": folder, "sessionId": id,
            "message": { "role": "assistant", "content": [{ "type": "text", "text": "Sure." }] } })),
    ]
    .concat();
    sandbox.write(&format!("{PROJECT}/{id}.jsonl"), &text);
}

#[test]
fn without_claude_code_there_is_none() {
    let sandbox = Sandbox::new();
    assert!(
        conversations::list(&sandbox.places, "claude-code")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_agent_whose_conversations_clak_doesnt_read() {
    let sandbox = Sandbox::new();
    assert!(!conversations::supported("vscode"));
    assert!(conversations::list(&sandbox.places, "vscode").is_err());
    assert!(
        conversations::list(&sandbox.places, "codex")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_title_is_the_first_prompt_and_the_folder_where_it_took_place() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "aaa-1", "/work/app", "Fix   the\nlogin page");
    let all = conversations::list(&sandbox.places, "claude-code").unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "aaa-1");
    assert_eq!(all[0].title, "Fix the login page");
    assert!(!all[0].named);
    assert_eq!(all[0].folder.as_deref(), Some("/work/app"));
}

#[test]
fn a_long_prompt_is_cut() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "aaa-1", "/work/app", &"word ".repeat(100));
    let title = &conversations::list(&sandbox.places, "claude-code").unwrap()[0].title;
    assert_eq!(title.chars().count(), 120);
    assert!(title.ends_with('…'));
}

#[test]
fn without_a_prompt_or_as_a_subagent_it_is_not_listed() {
    let sandbox = Sandbox::new();
    sandbox.write(
        &format!("{PROJECT}/empty.jsonl"),
        &line(json!({ "type": "summary", "summary": "Nothing" })),
    );
    sandbox.write(
        &format!("{PROJECT}/side.jsonl"),
        &line(
            json!({ "type": "user", "isSidechain": true, "cwd": "/work/app",
            "message": { "role": "user", "content": "Look around" } }),
        ),
    );
    assert!(
        conversations::list(&sandbox.places, "claude-code")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_rename_adds_a_line_and_the_last_one_wins() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "aaa-1", "/work/app", "Fix the login page");
    let before = read(&sandbox.path(&format!("{PROJECT}/aaa-1.jsonl")));

    conversations::rename(&sandbox.places, "claude-code", "aaa-1", "  Login\tfix ").unwrap();
    conversations::rename(&sandbox.places, "claude-code", "aaa-1", "Login, for good").unwrap();

    let after = read(&sandbox.path(&format!("{PROJECT}/aaa-1.jsonl")));
    assert!(
        after.starts_with(&before),
        "the lines already there stay as they are"
    );
    let added: Vec<serde_json::Value> = after[before.len()..]
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        added,
        [
            json!({ "type": "custom-title", "customTitle": "Login fix", "sessionId": "aaa-1" }),
            json!({ "type": "custom-title", "customTitle": "Login, for good", "sessionId": "aaa-1" }),
        ]
    );
    let all = conversations::list(&sandbox.places, "claude-code").unwrap();
    assert_eq!(all[0].title, "Login, for good");
    assert!(all[0].named);
}

#[test]
fn a_rename_after_a_last_line_without_newline_starts_its_own() {
    let sandbox = Sandbox::new();
    let open = line(json!({ "type": "user", "cwd": "/work/app",
        "message": { "role": "user", "content": "Hello" } }));
    sandbox.write(&format!("{PROJECT}/aaa-1.jsonl"), open.trim_end());
    conversations::rename(&sandbox.places, "claude-code", "aaa-1", "Greeting").unwrap();
    let all = conversations::list(&sandbox.places, "claude-code").unwrap();
    assert_eq!(all[0].title, "Greeting");
}

#[test]
fn an_empty_name_or_an_unknown_id_is_refused() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "aaa-1", "/work/app", "Fix the login page");
    assert!(conversations::rename(&sandbox.places, "claude-code", "aaa-1", "  ").is_err());
    assert!(conversations::rename(&sandbox.places, "claude-code", "nope", "Name").is_err());
    assert!(conversations::rename(&sandbox.places, "claude-code", "../x", "Name").is_err());
}

#[test]
fn it_resumes_in_its_folder() {
    let sandbox = Sandbox::new();
    let folder = sandbox.dir(&["work", "app"]);
    let folder_text = folder.to_string_lossy().into_owned();
    conversation(&sandbox, "aaa-1", &folder_text, "Fix the login page");
    let (args, at) = conversations::resume(&sandbox.places, "claude-code", "aaa-1").unwrap();
    assert_eq!(args, ["--resume", "aaa-1"]);
    assert_eq!(at, Some(folder));
}

#[test]
fn it_does_not_resume_in_a_folder_that_is_gone() {
    let sandbox = Sandbox::new();
    let gone = sandbox.path("work/gone").to_string_lossy().into_owned();
    conversation(&sandbox, "aaa-1", &gone, "Fix the login page");
    let error = conversations::resume(&sandbox.places, "claude-code", "aaa-1").unwrap_err();
    assert!(error.0.contains("is gone"), "{}", error.0);
}

#[test]
fn its_own_title_wins_over_the_one_of_the_conversation_it_continues() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "bbb-2", "/work/app", "Go on");
    let path = sandbox.path(&format!("{PROJECT}/bbb-2.jsonl"));
    let mut text = read(&path);
    text.push_str(&line(
        json!({ "type": "custom-title", "customTitle": "Mine", "sessionId": "bbb-2" }),
    ));
    text.push_str(&line(
        json!({ "type": "custom-title", "customTitle": "Earlier", "sessionId": "aaa-1" }),
    ));
    std::fs::write(&path, text).unwrap();
    let all = conversations::list(&sandbox.places, "claude-code").unwrap();
    assert_eq!(all[0].title, "Mine");
}

#[test]
fn a_deleted_conversation_takes_its_folders_and_leaves_the_others() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "aaa-1", "/work/app", "Fix the login page");
    conversation(&sandbox, "bbb-2", "/work/app", "Write the tests");
    sandbox.write(&format!("{PROJECT}/aaa-1/tool-results/one.txt"), "out");
    sandbox.write("home/.claude/file-history/aaa-1/one", "before");
    sandbox.write("home/.claude/session-env/aaa-1/env", "x");
    sandbox.write("home/.claude/file-history/bbb-2/one", "before");
    conversations::delete(&sandbox.places, "claude-code", "aaa-1").unwrap();
    let all = conversations::list(&sandbox.places, "claude-code").unwrap();
    assert_eq!(
        all.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
        ["bbb-2"]
    );
    assert!(!sandbox.path(&format!("{PROJECT}/aaa-1")).exists());
    assert!(!sandbox.path("home/.claude/file-history/aaa-1").exists());
    assert!(!sandbox.path("home/.claude/session-env/aaa-1").exists());
    assert!(sandbox.path("home/.claude/file-history/bbb-2/one").exists());
}

#[test]
fn deleting_an_unknown_or_invalid_id_is_refused() {
    let sandbox = Sandbox::new();
    conversation(&sandbox, "aaa-1", "/work/app", "Fix the login page");
    assert!(conversations::delete(&sandbox.places, "claude-code", "zzz-9").is_err());
    assert!(conversations::delete(&sandbox.places, "claude-code", "../aaa-1").is_err());
    assert!(conversations::delete(&sandbox.places, "codex", "aaa-1").is_err());
    assert_eq!(
        conversations::list(&sandbox.places, "claude-code")
            .unwrap()
            .len(),
        1
    );
}

/// A Code session of Claude Desktop: its file where the app keeps it, on the Mac and on
/// Windows (the sandbox has both, each system reads its own).
fn desktop_session(sandbox: &Sandbox, id: &str, fields: serde_json::Value) {
    let mut entry = json!({ "sessionId": id, "createdAt": 1000, "isArchived": false });
    for (key, value) in fields.as_object().unwrap() {
        entry[key] = value.clone();
    }
    for base in ["home/Library/Application Support", "appdata"] {
        sandbox.write(
            &format!("{base}/Claude/claude-code-sessions/acc/org/{id}.json"),
            &entry.to_string(),
        );
    }
}

#[test]
fn claude_desktop_sessions_are_listed_with_the_title_the_app_shows() {
    let sandbox = Sandbox::new();
    let work = sandbox.dir(&["work", "app"]).to_string_lossy().into_owned();
    desktop_session(
        &sandbox,
        "local_a1",
        json!({ "title": "Mine", "titleSource": "user",
        "originCwd": work, "cwd": format!("{work}/.worktrees/x"), "cliSessionId": "aaa-1",
        "lastActivityAt": 3000 }),
    );
    desktop_session(
        &sandbox,
        "local_b2",
        json!({ "cwd": "/work/site", "cliSessionId": "gone-2", "lastActivityAt": 5000 }),
    );
    desktop_session(
        &sandbox,
        "local_c3",
        json!({ "title": "Old", "isArchived": true }),
    );
    sandbox.write(
        "home/Library/Application Support/Claude/claude-code-sessions/acc/org/scheduled-tasks.json",
        "{}",
    );
    // Claude Code still has the conversation of the first, not of the second.
    conversation(&sandbox, "aaa-1", &work, "Fix the login page");
    let all = conversations::list(&sandbox.places, "claude-desktop").unwrap();
    let ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["local_b2", "local_a1"]);
    assert_eq!(all[0].title, "site");
    assert!(!all[0].named);
    assert_eq!(all[0].terminal, None);
    assert_eq!(all[1].title, "Mine");
    assert!(all[1].named);
    assert_eq!(all[1].folder.as_deref(), Some(work.as_str()));
    assert_eq!(all[1].terminal.as_deref(), Some("aaa-1"));
    assert_eq!(all[1].updated, 3000);
}

#[test]
fn a_claude_desktop_session_opens_in_the_app_and_is_not_changed_by_clak() {
    let sandbox = Sandbox::new();
    let work = sandbox.dir(&["work", "app"]);
    let work_text = work.to_string_lossy().into_owned();
    desktop_session(
        &sandbox,
        "local_a1",
        json!({ "title": "Mine", "originCwd": work_text }),
    );
    assert_eq!(
        conversations::link(&sandbox.places, "claude-desktop", "local_a1").unwrap(),
        "claude://code/continue?session=local_a1"
    );
    assert_eq!(
        conversations::folder(&sandbox.places, "claude-desktop", "local_a1").unwrap(),
        Some(work)
    );
    assert!(conversations::link(&sandbox.places, "claude-desktop", "local_zz").is_err());
    assert!(conversations::link(&sandbox.places, "claude-desktop", "local_a1&x=1").is_err());
    assert!(conversations::link(&sandbox.places, "claude-code", "local_a1").is_err());
    assert!(conversations::resume(&sandbox.places, "claude-desktop", "local_a1").is_err());
    assert!(!conversations::can("claude-desktop").unwrap().rename);
    assert!(conversations::rename(&sandbox.places, "claude-desktop", "local_a1", "New").is_err());
    assert!(conversations::delete(&sandbox.places, "claude-desktop", "local_a1").is_err());
    assert_eq!(
        conversations::list(&sandbox.places, "claude-desktop").unwrap()[0].title,
        "Mine"
    );
}

/// A conversation with an answer and a command used: for the search and the export.
fn talk(sandbox: &Sandbox, id: &str, prompt: &str, answer: &str) {
    let text = [
        line(json!({ "type": "user", "cwd": "/work/app", "sessionId": id,
            "message": { "role": "user", "content": prompt } })),
        line(json!({ "type": "assistant", "cwd": "/work/app", "sessionId": id,
            "message": { "role": "assistant", "content": [
                { "type": "text", "text": answer },
                { "type": "tool_use", "name": "Bash", "input": { "command": "ls" } } ] } })),
        line(json!({ "type": "user", "isMeta": true, "cwd": "/work/app", "sessionId": id,
            "message": { "role": "user", "content": "<system-reminder>secret plans</system-reminder>" } })),
    ]
    .concat();
    sandbox.write(&format!("{PROJECT}/{id}.jsonl"), &text);
}

#[test]
fn the_search_finds_words_said_by_either_side_not_in_reminders() {
    let sandbox = Sandbox::new();
    talk(
        &sandbox,
        "aaa-1",
        "Fix the Login page",
        "The session cookie expired.",
    );
    talk(&sandbox, "bbb-2", "Write the tests", "Done, all green.");
    let search = |words: &[&str]| {
        let words: Vec<String> = words.iter().map(|w| w.to_string()).collect();
        conversations::search(&sandbox.places, "claude-code", &words).unwrap()
    };
    assert_eq!(search(&["login", "COOKIE"]), ["aaa-1"]);
    assert_eq!(search(&["green"]), ["bbb-2"]);
    assert!(search(&["login", "green"]).is_empty());
    assert!(search(&["plans"]).is_empty());
    assert!(search(&[" "]).is_empty());
}

#[test]
fn the_export_is_markdown_with_the_commands_named() {
    let sandbox = Sandbox::new();
    talk(
        &sandbox,
        "aaa-1",
        "Fix the login page",
        "The session cookie expired.",
    );
    let (title, text) = conversations::export(&sandbox.places, "claude-code", "aaa-1").unwrap();
    assert_eq!(title, "Fix the login page");
    assert_eq!(
        text,
        "# Fix the login page\n\n`/work/app`\n\n## You\n\nFix the login page\n\n## Agent\n\nThe session cookie expired.\n\n*Used Bash*\n"
    );
}

#[test]
fn several_are_deleted_at_once_and_each_says_what_it_takes() {
    let sandbox = Sandbox::new();
    talk(&sandbox, "aaa-1", "One", "1");
    talk(&sandbox, "bbb-2", "Two", "2");
    talk(&sandbox, "ccc-3", "Three", "3");
    sandbox.write("home/.claude/file-history/aaa-1/one", "0123456789");
    let all = conversations::list(&sandbox.places, "claude-code").unwrap();
    let one = all.iter().find(|c| c.id == "aaa-1").unwrap();
    let two = all.iter().find(|c| c.id == "bbb-2").unwrap();
    assert_eq!(one.size, two.size + 10);
    let failed = conversations::delete_many(
        &sandbox.places,
        "claude-code",
        &["aaa-1".into(), "zzz-9".into(), "bbb-2".into()],
    );
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].0, "zzz-9");
    let left: Vec<String> = conversations::list(&sandbox.places, "claude-code")
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(left, ["ccc-3"]);
}

#[test]
fn opencode_sessions_come_from_its_database_without_the_ones_its_agents_started() {
    let sandbox = Sandbox::new();
    let work = sandbox.dir(&["work", "app"]);
    let work_text = work.to_string_lossy().into_owned();
    let db_path = sandbox
        .dir(&["home", ".local", "share", "opencode"])
        .join("opencode.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch(
        "CREATE TABLE session_v2 (id TEXT PRIMARY KEY, parent_id TEXT, directory TEXT NOT NULL,
            title TEXT, time_updated INTEGER NOT NULL, time_archived INTEGER);",
    )
    .unwrap();
    let add = |id: &str, parent: Option<&str>, title: &str, updated: i64, archived: Option<i64>| {
        db.execute(
            "INSERT INTO session_v2 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, parent, work_text, title, updated, archived],
        )
        .unwrap();
    };
    add("ses_one", None, "Fix the parser", 2000, None);
    add("ses_two", None, "Newer", 3000, None);
    add("ses_child", Some("ses_one"), "Subtask", 4000, None);
    add("ses_old", None, "Archived", 5000, Some(5000));
    drop(db);
    let all = conversations::list(&sandbox.places, "opencode").unwrap();
    let ids: Vec<&str> = all.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["ses_two", "ses_one"]);
    assert_eq!(all[1].title, "Fix the parser");
    let (args, folder) = conversations::resume(&sandbox.places, "opencode", "ses_one").unwrap();
    assert_eq!(args, ["--session", "ses_one"]);
    assert_eq!(folder, Some(work));
    assert!(conversations::resume(&sandbox.places, "opencode", "ses_one; rm -rf x").is_err());
    assert!(conversations::delete(&sandbox.places, "opencode", "ses_one").is_err());
}

#[test]
fn codex_sessions_with_their_names_without_injected_context_or_programs() {
    let sandbox = Sandbox::new();
    let work = sandbox.dir(&["work", "app"]);
    let work_text = work.to_string_lossy().into_owned();
    let session = |file: &str, id: &str, source: &str, prompt: &str| {
        let text = [
            line(json!({ "timestamp": "t", "type": "session_meta",
                "payload": { "id": id, "cwd": work_text, "source": source } })),
            line(json!({ "type": "response_item", "payload": { "type": "message", "role": "user",
                "content": [{ "type": "input_text", "text": "# AGENTS.md instructions for /work\n\n<INSTRUCTIONS>" }] } })),
            line(json!({ "type": "event_msg", "payload": { "type": "user_message", "message": prompt } })),
            line(json!({ "type": "response_item", "payload": { "type": "message", "role": "assistant",
                "content": [{ "type": "output_text", "text": "Patched the parser." }] } })),
            line(json!({ "type": "response_item", "payload": { "type": "function_call", "name": "shell" } })),
        ]
        .concat();
        sandbox.write(&format!("home/.codex/sessions/2026/10/22/{file}"), &text);
    };
    session(
        "rollout-2026-10-22T10-00-00-c1.jsonl",
        "c1",
        "cli",
        "Fix the parser",
    );
    session(
        "rollout-2026-10-22T11-00-00-c2.jsonl",
        "c2",
        "cli",
        "Second",
    );
    session(
        "rollout-2026-10-22T12-00-00-c3.jsonl",
        "c3",
        "exec",
        "From a script",
    );
    sandbox.write(
        "home/.codex/session_index.jsonl",
        &[
            line(json!({ "id": "c2", "thread_name": "Old name", "updated_at": "t" })),
            line(json!({ "id": "c2", "thread_name": "Release", "updated_at": "t" })),
        ]
        .concat(),
    );
    let all = conversations::list(&sandbox.places, "codex").unwrap();
    let mut titles: Vec<(&str, &str, bool)> = all
        .iter()
        .map(|c| (c.id.as_str(), c.title.as_str(), c.named))
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        [("c1", "Fix the parser", false), ("c2", "Release", true)]
    );
    let (args, folder) = conversations::resume(&sandbox.places, "codex", "c1").unwrap();
    assert_eq!(args, ["resume", "c1"]);
    assert_eq!(folder, Some(work));
    let words = vec!["parser".to_owned(), "patched".to_owned()];
    let mut found = conversations::search(&sandbox.places, "codex", &words).unwrap();
    found.sort();
    assert_eq!(found, ["c1", "c2"]);
    let (_, text) = conversations::export(&sandbox.places, "codex", "c1").unwrap();
    assert!(text.contains("## You\n\nFix the parser"), "{text}");
    assert!(text.contains("*Used shell*"), "{text}");
    assert!(!text.contains("AGENTS.md"), "{text}");
}

#[test]
fn gemini_sessions_with_their_project_folder_and_summary() {
    let sandbox = Sandbox::new();
    let work = sandbox.dir(&["work", "app"]);
    let work_text = work.to_string_lossy().into_owned();
    sandbox.write(
        "home/.gemini/tmp/app/.project_root",
        &format!("{work_text}\n"),
    );
    let text = [
        line(json!({ "sessionId": "g-1", "projectHash": "h", "startTime": "t", "lastUpdated": "t", "kind": "main" })),
        line(json!({ "id": "m1", "timestamp": "t", "type": "user", "content": "/stats" })),
        line(json!({ "id": "m2", "timestamp": "t", "type": "user", "content": [{ "text": "Explain the cache" }] })),
        line(json!({ "id": "m3", "timestamp": "t", "type": "gemini", "content": "draft",
            "toolCalls": [{ "name": "read_file" }] })),
        line(json!({ "id": "m3", "timestamp": "t", "type": "gemini", "content": "It is an LRU." })),
        line(json!({ "$set": { "summary": "Cache explained" } })),
    ]
    .concat();
    sandbox.write(
        "home/.gemini/tmp/app/chats/session-2026-10-22T10-00-g-1.jsonl",
        &text,
    );
    sandbox.write(
        "home/.gemini/tmp/app/chats/session-2026-10-22T11-00-sub.jsonl",
        &line(json!({ "sessionId": "sub", "kind": "subagent" })),
    );
    let all = conversations::list(&sandbox.places, "gemini").unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].title, "Cache explained");
    assert_eq!(all[0].folder.as_deref(), Some(work_text.as_str()));
    let (args, folder) = conversations::resume(&sandbox.places, "gemini", "g-1").unwrap();
    assert_eq!(args, ["--resume", "g-1"]);
    assert_eq!(folder, Some(work));
    let words = vec!["lru".to_owned()];
    assert_eq!(
        conversations::search(&sandbox.places, "gemini", &words).unwrap(),
        ["g-1"]
    );
    let draft = vec!["draft".to_owned()];
    assert!(
        conversations::search(&sandbox.places, "gemini", &draft)
            .unwrap()
            .is_empty()
    );
    let (_, text) = conversations::export(&sandbox.places, "gemini", "g-1").unwrap();
    assert!(text.contains("## You\n\nExplain the cache"), "{text}");
    assert!(!text.contains("/stats"), "{text}");
}

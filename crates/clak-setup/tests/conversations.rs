//! Claude Code's past conversations: listed with their title and folder, renamed by adding a
//! line, resumed in the folder where they took place, deleted with what is kept beside them.

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
fn only_claude_code_for_now() {
    let sandbox = Sandbox::new();
    assert!(!conversations::supported("codex"));
    assert!(conversations::list(&sandbox.places, "codex").is_err());
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

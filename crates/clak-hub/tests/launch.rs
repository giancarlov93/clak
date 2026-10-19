//! Agents launched from Clak: authorized without a question, with the programs of their
//! preset. Everything else still asks.

mod support;

use clak_hub::ConsentKind;
use clak_hub::launch::{Preset, PresetProgram, Target};
use serde_json::{Value, json};
use support::*;

fn prepare(hub: &TestHub, agent: &str, secret: bool, preset: Option<Preset>) -> Option<String> {
    hub.hub()
        .broker()
        .run(|c| c.prepare_launch(agent, secret, preset))
}

/// The key the hub remembers the test notepad by (see `apps` in the snapshot).
async fn notepad_key(hub: &TestHub) -> String {
    let state = hub.snapshot().await;
    state["apps"][0]["key"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn a_launched_agent_is_authorized_once_without_a_question() {
    let hub = TestHub::start().await;
    let token = prepare(&hub, "Claude Code", true, None).unwrap();

    let response = hub
        .ask("Claude Code", "list", Some(json!({ "launch": token })))
        .await;
    assert!(ok(&response), "{response:?}");
    assert_eq!(hub.prompt.count(), 0);
    let state = hub.snapshot().await;
    assert_eq!(state["sessions"][0]["launched"], json!(true));
    assert_eq!(state["launches"], json!([]));

    // Used up: a new Claude Code process with the same secret is asked as usual.
    hub.platform.start_now("Claude Code", &[]);
    let again = hub.open(
        Some("Claude Code"),
        "list",
        Some(json!({ "launch": token })),
    );
    let (_reader, question) = tokio::join!(again, hub.prompt.next(ConsentKind::Session));
    question.deny();
    hub.stop().await;
}

#[tokio::test]
async fn a_wrong_secret_or_another_agent_still_asks() {
    let hub = TestHub::start().await;
    let token = prepare(&hub, "Claude Code", true, None).unwrap();

    let wrong = hub.open(
        Some("Claude Code"),
        "list",
        Some(json!({ "launch": "nope" })),
    );
    let (_reader, question) = tokio::join!(wrong, hub.prompt.next(ConsentKind::Session));
    question.deny();

    // Codex can't take Claude Code's launch, even with its secret.
    let other = hub.open(Some("Codex"), "list", Some(json!({ "launch": token })));
    let (_reader, question) = tokio::join!(other, hub.prompt.next(ConsentKind::Session));
    question.deny();
    assert_eq!(
        hub.snapshot().await["launches"].as_array().unwrap().len(),
        1
    );
    hub.stop().await;
}

#[tokio::test]
async fn a_preset_gives_all_the_documents_of_its_application_and_only_its_commands() {
    let hub = TestHub::start().await;
    let first = hub.register_as("Test Notepad", "shopping.txt").await;
    let second = hub.register_as("Test Notepad", "todo.txt").await;
    let other = hub.register_as("Other Editor", "notes.txt").await;
    let key = notepad_key(&hub).await;
    let questions = hub.prompt.count();

    let preset = Preset {
        name: "Shopping".into(),
        programs: vec![PresetProgram {
            target: Target::App(key),
            tools: Some(vec!["get_text".into()]),
        }],
    };
    let token = prepare(&hub, "Claude Code", true, Some(preset)).unwrap();
    let response = hub
        .ask("Claude Code", "list", Some(json!({ "launch": token })))
        .await;
    assert!(ok(&response), "{response:?}");

    // Both documents of the notepad, right away; not the other program.
    let state = hub.snapshot().await;
    assert_eq!(state["sessions"][0]["preset"], json!("Shopping"));
    let grants = state["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 2);
    assert!(grants.iter().all(|g| g["tools"] == json!(["get_text"])));

    for program in ["test-notepad", "test-notepad-2"] {
        let response = hub
            .call("Claude Code", program, "get_text", None, None)
            .await;
        assert!(ok(&response), "{response:?}");
    }
    assert_eq!(hub.prompt.count(), questions);

    // A command outside the preset asks, as a new command does.
    let call = |resource: &str, tool: &str| {
        Some(json!({ "resource": resource, "tool": tool, "arguments": { "text": "x" } }))
    };
    let _outside = hub
        .open(
            Some("Claude Code"),
            "call",
            call("test-notepad", "append_text"),
        )
        .await;
    hub.prompt.next(ConsentKind::Tool).await.deny();

    // A program outside the preset asks too.
    let _elsewhere = hub
        .open(
            Some("Claude Code"),
            "call",
            call("other-editor", "get_text"),
        )
        .await;
    hub.prompt.next(ConsentKind::Use).await.deny();

    drop((first, second, other));
    hub.stop().await;
}

#[tokio::test]
async fn a_document_opened_later_is_covered_by_the_preset() {
    let hub = TestHub::start().await;
    let _first = hub.register_as("Test Notepad", "shopping.txt").await;
    let key = notepad_key(&hub).await;
    let preset = Preset {
        name: "Notes".into(),
        programs: vec![PresetProgram {
            target: Target::App(key),
            tools: None,
        }],
    };
    let token = prepare(&hub, "Claude Code", true, Some(preset)).unwrap();
    assert!(ok(&hub
        .ask("Claude Code", "list", Some(json!({ "launch": token })))
        .await));

    let _later = hub.register_as("Test Notepad", "later.txt").await;
    let questions = hub.prompt.count();
    let response = hub
        .call(
            "Claude Code",
            "test-notepad-2",
            "append_text",
            Some(json!({ "text": "x" })),
            None,
        )
        .await;
    assert!(ok(&response), "{response:?}");
    assert_eq!(hub.prompt.count(), questions);
    hub.stop().await;
}

#[tokio::test]
async fn an_app_launch_takes_only_a_new_session() {
    let hub = TestHub::start().await;
    prepare(&hub, "Claude Desktop", false, None);

    // Claude Desktop was already open: its session is asked as usual.
    let old = hub.open(Some("Claude Desktop"), "list", None);
    let (_reader, question) = tokio::join!(old, hub.prompt.next(ConsentKind::Session));
    question.deny();
    hub.stop().await;
}

#[tokio::test]
async fn an_agent_started_inside_the_launched_app_is_authorized() {
    let hub = TestHub::start().await;
    prepare(&hub, "Visual Studio Code", false, None);
    hub.platform
        .start_now("Claude Code", &["zsh", "Visual Studio Code"]);

    let response = hub.ask("Claude Code", "list", None).await;
    assert!(ok(&response), "{response:?}");
    assert_eq!(hub.prompt.count(), 0);
    let state = hub.snapshot().await;
    assert_eq!(state["sessions"][0]["launched"], json!(true));
    assert_eq!(state["launches"], Value::Array(vec![]));
    hub.stop().await;
}

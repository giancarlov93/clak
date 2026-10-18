//! The stdio connectors: the MCP server is a program on this computer that the hub starts
//! (here the fake server `clak-fake-stdio`). To agents it looks like an HTTP server, with
//! the same permissions; if it stops it restarts, and its environment variables stay encrypted.

mod fake;
mod support;

use clak_connectors::launch::Launch;
use clak_connectors::{ConnectorHub, phase};
use clak_hub::ConsentKind;
use serde_json::json;
use std::collections::BTreeMap;
use support::*;

fn fake_server(args: &[&str]) -> Launch {
    Launch {
        program: env!("CARGO_BIN_EXE_clak-fake-stdio").to_owned(),
        args: args.iter().map(|a| a.to_string()).collect(),
    }
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[tokio::test]
async fn a_program_is_added_and_brings_its_commands() {
    let hub = TestHub::start().await;
    let launch = fake_server(&[]);
    let entry = hub
        .connectors
        .add_stdio(
            "Browser",
            &launch,
            &env(&[("FAKE_INSTRUCTIONS", "First «echo».")]),
            None,
        )
        .unwrap();
    assert!(entry.is_stdio());
    assert_eq!(entry.url, "");
    let seen = hub.connector(&entry.id, phase::ACTIVE).await;
    assert_eq!(seen["tools"], 6);
    assert_eq!(seen["cli"], "browser");
    assert_eq!(seen["transport"], "stdio");
    assert_eq!(seen["launch"], launch.display());
    assert_eq!(seen["env"], json!(["FAKE_INSTRUCTIONS"]));
    assert_eq!(hub.connectors.find_by_launch(&launch), Some(entry.clone()));

    hub.prompt.set_allow_all(true);
    let listed = hub.ask("claude", "list", None).await;
    let origin = listed["resources"][0]["origin"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(origin.contains("local program clak-fake-stdio"), "{origin}");
    let described = hub
        .ask("claude", "tools", Some(json!({ "resource": "browser" })))
        .await;
    assert_eq!(described["instructions"], "First «echo».");
    assert_eq!(
        content(
            &hub.call("claude", "browser", "echo", Some(json!({"text": "hello"})))
                .await
        ),
        "echo: hello"
    );
    // The variables reach the program.
    assert_eq!(
        content(
            &hub.call(
                "claude",
                "browser",
                "env",
                Some(json!({"name": "FAKE_INSTRUCTIONS"}))
            )
            .await
        ),
        "First «echo»."
    );
    assert!(hub.log_text().contains("environment FAKE_INSTRUCTIONS"));
    hub.stop().await;
}

#[tokio::test]
async fn the_variables_stay_encrypted_and_a_new_hub_rereads_the_program() {
    let secret = "very-secret-key-4711";
    let hub = TestHub::start().await;
    let launch = fake_server(&["--tools", "core,debug"]);
    let entry = hub
        .connectors
        .add_stdio("Browser", &launch, &env(&[("API_KEY", secret)]), None)
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    assert!(!hub.disk().contains(secret));
    assert!(!hub.log_text().contains(secret));
    assert!(!hub.snapshot().to_string().contains(secret));

    let reread = hub.store().all();
    assert_eq!(reread[0].launch.as_ref(), Some(&launch));
    assert_eq!(hub.store().env(&entry.id), env(&[("API_KEY", secret)]));

    hub.connectors.remove(&entry.id).await.unwrap();
    assert!(hub.store().env(&entry.id).is_empty());
    hub.stop().await;
}

#[tokio::test]
async fn a_program_that_stops_restarts_and_says_why() {
    let hub = TestHub::start().await;
    let entry = hub
        .connectors
        .add_stdio("Browser", &fake_server(&[]), &BTreeMap::new(), None)
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    hub.prompt.set_allow_all(true);
    let asked = {
        let _ = hub
            .call("claude", "browser", "echo", Some(json!({"text": "one"})))
            .await;
        hub.prompt.count()
    };

    let quit = hub.call("claude", "browser", "quit", None).await;
    assert!(is_error(&quit));
    let down = hub
        .connector_until(&entry.id, |c| {
            c["error"].as_str().is_some_and(|e| e.contains("bye"))
        })
        .await;
    assert!(down["error"].as_str().unwrap().contains("code 3"), "{down}");

    // It restarts by itself, and the permissions stay: it's the same program.
    hub.connector(&entry.id, phase::ACTIVE).await;
    assert_eq!(
        content(
            &hub.call("claude", "browser", "echo", Some(json!({"text": "two"})))
                .await
        ),
        "echo: two"
    );
    assert_eq!(hub.prompt.count(), asked);
    hub.stop().await;
}

#[tokio::test]
async fn a_program_that_fails_to_start_says_so_in_its_own_words() {
    let hub = TestHub::start().await;
    let entry = hub
        .connectors
        .add_stdio(
            "Broken",
            &fake_server(&["--fail-start"]),
            &BTreeMap::new(),
            None,
        )
        .unwrap();
    let seen = hub
        .connector_until(&entry.id, |c| {
            c["state"] == phase::UNREACHABLE && !c["error"].is_null()
        })
        .await;
    let error = seen["error"].as_str().unwrap();
    assert!(
        error.contains("code 2") && error.contains("missing API key"),
        "{error}"
    );

    let missing = Launch {
        program: "clak-program-that-does-not-exist".into(),
        args: vec![],
    };
    let entry = hub
        .connectors
        .add_stdio("Missing", &missing, &BTreeMap::new(), None)
        .unwrap();
    let seen = hub
        .connector_until(&entry.id, |c| {
            c["state"] == phase::UNREACHABLE && !c["error"].is_null()
        })
        .await;
    assert!(
        seen["error"].as_str().unwrap().contains("was not found"),
        "{seen}"
    );
    hub.stop().await;
}

#[tokio::test]
async fn a_changed_list_arrives_right_away() {
    let hub = TestHub::start().await;
    let entry = hub
        .connectors
        .add_stdio("Browser", &fake_server(&[]), &BTreeMap::new(), None)
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    hub.prompt.set_allow_all(true);
    assert_eq!(
        content(&hub.call("claude", "browser", "grow", None).await),
        "grown"
    );
    // The periodic refresh is every ten minutes: it's the server's notification that brings the new command.
    hub.connector_until(&entry.id, |c| c["tools"] == 7).await;
    hub.stop().await;
}

#[tokio::test]
async fn a_confirmation_asked_by_the_program_is_given_by_the_user() {
    let hub = TestHub::start().await;
    let entry = hub
        .connectors
        .add_stdio("Browser", &fake_server(&[]), &BTreeMap::new(), None)
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    hub.prompt.set_allow_all(true);
    let _ = hub
        .call("claude", "browser", "echo", Some(json!({"text": "x"})))
        .await;
    hub.prompt.set_allow_all(false);

    let call = hub.call("claude", "browser", "ask", None);
    let (called, ()) = tokio::join!(call, async {
        let question = hub.prompt.next(ConsentKind::Confirm).await;
        assert_eq!(question.request.question, "Proceed?");
        question.allow();
    });
    assert_eq!(content(&called), "action: accept");
    hub.stop().await;
}

#[tokio::test]
async fn changing_the_variables_restarts_and_revokes_permissions_renaming_does_not() {
    let hub = TestHub::start().await;
    let connectors = &hub.connectors;
    let launch = fake_server(&[]);
    let entry = connectors
        .add_stdio("Browser", &launch, &env(&[("COLOR", "red")]), None)
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    hub.prompt.set_allow_all(true);
    let _ = hub
        .call("claude", "browser", "echo", Some(json!({"text": "x"})))
        .await;
    hub.prompt.set_allow_all(false);

    // A new name, the same variables: the permissions stay.
    connectors
        .edit_stdio(
            &entry.id,
            "Navigator",
            Some("browser"),
            &launch,
            Some(env(&[("COLOR", "red")])),
        )
        .await
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    let asked = hub.prompt.count();
    assert_eq!(
        content(
            &hub.call("claude", "browser", "env", Some(json!({"name": "COLOR"})))
                .await
        ),
        "red"
    );
    assert_eq!(hub.prompt.count(), asked);

    // New variables: the program restarts with them, and the permission is asked again.
    connectors
        .edit_stdio(
            &entry.id,
            "Navigator",
            Some("browser"),
            &launch,
            Some(env(&[("COLOR", "blue")])),
        )
        .await
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    let call = hub.call("claude", "browser", "env", Some(json!({"name": "COLOR"})));
    let (called, ()) = tokio::join!(call, async {
        hub.prompt.next(ConsentKind::Use).await.allow()
    });
    assert_eq!(content(&called), "blue");
    assert!(hub.log_text().contains("environment COLOR"));
    assert!(!hub.log_text().contains("blue"));

    // An address isn't given to a program, and vice versa.
    assert!(
        connectors
            .edit(&entry.id, "X", None, "https://x.it/mcp", None)
            .await
            .is_err()
    );
    hub.stop().await;
}

#[tokio::test]
async fn name_and_variables_are_checked_before_saving() {
    let hub = TestHub::start().await;
    let launch = fake_server(&[]);
    assert_eq!(
        hub.connectors
            .add_stdio("", &launch, &BTreeMap::new(), None)
            .unwrap_err(),
        "A name is required."
    );
    assert!(
        hub.connectors
            .add_stdio("B", &launch, &env(&[("NO-PE", "x")]), None)
            .is_err()
    );
    assert!(
        hub.connectors
            .add_stdio("B", &launch, &BTreeMap::new(), Some("Not Valid"))
            .is_err()
    );
    assert!(hub.connectors.entries().is_empty());
    hub.stop().await;
}

#[tokio::test]
async fn the_probe_starts_the_program_and_tells_what_it_offers() {
    let probe = ConnectorHub::probe_stdio(&fake_server(&[]), &BTreeMap::new()).await;
    assert!(probe.ok, "{}", probe.message);
    assert_eq!(probe.tools, 6);
    assert_eq!(probe.server.as_deref(), Some("fake-stdio"));
    assert_eq!(probe.message, "Started (fake-stdio): 6 commands.");

    let probe = ConnectorHub::probe_stdio(&fake_server(&["--fail-start"]), &BTreeMap::new()).await;
    assert!(!probe.ok);
    assert!(
        probe.message.contains("missing API key"),
        "{}",
        probe.message
    );
}

/// A launcher like `npx`: a script that starts the real program, which is therefore a
/// grandchild of the hub (a \`.cmd\` on Windows, an \`sh\` script elsewhere).
fn wrapper(dir: &std::path::Path) -> Launch {
    let exe = env!("CARGO_BIN_EXE_clak-fake-stdio");
    std::fs::create_dir_all(dir).unwrap();
    #[cfg(windows)]
    let script = {
        let script = dir.join("fake server.cmd");
        std::fs::write(&script, format!("@echo off\r\n\"{exe}\" %*\r\n")).unwrap();
        script
    };
    #[cfg(unix)]
    let script = {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake server.sh");
        std::fs::write(&script, format!("#!/bin/sh\n\"{exe}\" \"$@\"\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    };
    Launch {
        program: script.display().to_string(),
        args: vec!["one two".into()],
    }
}

fn alive(pid: &str) -> bool {
    #[cfg(windows)]
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .unwrap();
    #[cfg(unix)]
    let out = std::process::Command::new("ps")
        .args(["-p", pid, "-o", "comm="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).contains("clak-fake-stdio")
}

#[tokio::test]
async fn a_program_launched_by_a_script_stops_with_everything_it_started() {
    let hub = TestHub::start().await;
    let launch = wrapper(&hub.home.join("bin"));
    let entry = hub
        .connectors
        .add_stdio("Browser", &launch, &BTreeMap::new(), None)
        .unwrap();
    hub.connector(&entry.id, phase::ACTIVE).await;
    hub.prompt.set_allow_all(true);
    let pid = content(&hub.call("claude", "browser", "pid", None).await);
    assert!(alive(&pid), "{pid}");

    hub.connectors.remove(&entry.id).await.unwrap();
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while alive(&pid) {
        assert!(std::time::Instant::now() < until, "{pid} still running");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    hub.stop().await;
}

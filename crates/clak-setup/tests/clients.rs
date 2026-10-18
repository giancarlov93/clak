//! The MCP clients: JSON files, Codex's TOML, Claude Code through its command.

mod common;

use clak_setup::clients::{ClaudeCodeClient, ClientState, CommandRunner, McpClient};
use common::{SHIM, Sandbox, json, keys, read};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[test]
fn a_missing_client_is_not_created_out_of_nothing() {
    let sandbox = Sandbox::new();
    let cursor = sandbox.client("Cursor");
    assert_eq!(cursor.status().state, ClientState::NotInstalled);
    assert!(!sandbox.places.home.join(".cursor").exists());
}

#[test]
fn adds_the_entry_and_leaves_everything_else_intact() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.gemini/settings.json",
        "{\n  \"ui\": {\"theme\": \"Café\"},\n  \"mcpServers\": {\"other\": {\"command\": \"x\"}},\n  \"model\": 1.50\n}\n",
    );
    let gemini = sandbox.client("Gemini CLI");
    assert_eq!(gemini.status().state, ClientState::Absent);

    gemini.add().unwrap();

    let status = gemini.status();
    assert_eq!(status.state, ClientState::Ok);
    assert_eq!(status.path, path);
    let text = read(&path);
    let data = json(&path);
    assert_eq!(data["mcpServers"]["clak"]["command"], SHIM);
    assert_eq!(data["mcpServers"]["other"]["command"], "x");
    assert_eq!(keys(&data), ["ui", "mcpServers", "model"]);
    assert!(text.contains("Café")); // no \u00e9
    assert!(text.contains("1.50")); // numbers stay written as they were
    assert!(text.ends_with("}\n"));
    assert!(!text.contains('\r'));
}

#[test]
fn keeps_tabs_line_endings_and_bom() {
    let sandbox = Sandbox::new();
    #[cfg(windows)]
    let relative = "appdata/Code/User/mcp.json";
    #[cfg(not(windows))]
    let relative = "home/Library/Application Support/Code/User/mcp.json";
    let path = sandbox.write_bom(
        relative,
        "{\r\n\t\"servers\": {\r\n\t\t\"markitdown\": {\"type\": \"stdio\", \"command\": \"uvx\"}\r\n\t},\r\n\t\"inputs\": []\r\n}",
    );
    let code = sandbox.client("VS Code");

    code.add().unwrap();

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..3], &[0xEF, 0xBB, 0xBF]);
    let text = String::from_utf8(bytes[3..].to_vec()).unwrap();
    assert!(text.contains("\r\n\t\"servers\": {\r\n\t\t\"markitdown\""));
    assert!(!text.ends_with('\n'));
    let entry = &serde_json::from_str::<serde_json::Value>(&text).unwrap()["servers"]["clak"];
    assert_eq!(entry["type"], "stdio"); // VS Code wants the type, and the section is called `servers`
    assert_eq!(code.status().state, ClientState::Ok);
}

#[test]
fn a_file_with_comments_is_not_rewritten() {
    let sandbox = Sandbox::new();
    let original = "{\n  // my servers\n  \"mcpServers\": {}\n}\n";
    let path = sandbox.write("home/.cursor/mcp.json", original);
    let cursor = sandbox.client("Cursor");

    assert_eq!(cursor.status().state, ClientState::Unreadable);
    let error = cursor.add().unwrap_err();
    assert!(error.0.contains("line 2"), "{error}");
    assert_eq!(read(&path), original);
    assert!(!path.with_extension("json.clak-bak").exists());
    assert!(cursor.snippet().contains("\"mcpServers\""));
}

#[test]
fn a_repeated_key_is_left_alone() {
    let sandbox = Sandbox::new();
    let original = "{\"mcpServers\": {}, \"mcpServers\": {}}";
    let path = sandbox.write("home/.cursor/mcp.json", original);
    let cursor = sandbox.client("Cursor");
    assert_eq!(
        cursor.add().unwrap_err().0,
        "mcp.json has a repeated key: not touching it."
    );
    assert_eq!(read(&path), original);
}

#[test]
fn an_entry_pointing_elsewhere_is_stale_and_gets_updated() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.cursor/mcp.json",
        "{\"mcpServers\": {\"clak\": {\"command\": \"C:/old/clak-shim.exe\", \"args\": [\"-v\"], \"env\": {\"A\": \"1\"}}}}",
    );
    let cursor = sandbox.client("Cursor");

    let before = cursor.status();
    assert_eq!(before.state, ClientState::Stale);
    assert_eq!(before.detail, "C:/old/clak-shim.exe -v");

    cursor.add().unwrap();
    assert_eq!(cursor.status().state, ClientState::Ok);
    assert_eq!(
        json(&path)["mcpServers"]["clak"]["args"],
        serde_json::json!([])
    );
}

#[cfg(windows)]
#[test]
fn the_same_path_written_with_other_slashes_is_the_same_entry() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "home/.cursor/mcp.json",
        "{\"mcpServers\": {\"clak\": {\"command\": \"c:/program files/clak/CLAK-SHIM.EXE\"}}}",
    );
    assert_eq!(sandbox.client("Cursor").status().state, ClientState::Ok);
}

#[cfg(not(windows))]
#[test]
fn the_same_path_written_another_way_is_the_same_entry() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "home/.cursor/mcp.json",
        "{\"mcpServers\": {\"clak\": {\"command\": \"/Applications//Clak.app/./Contents/MacOS/x/../clak-shim/\"}}}",
    );
    assert_eq!(sandbox.client("Cursor").status().state, ClientState::Ok);
}

#[test]
fn removes_only_its_own_entry_and_keeps_one_copy_of_the_original() {
    let sandbox = Sandbox::new();
    let original = "{\n    \"mcpServers\": {\n        \"other\": {\"command\": \"x\"}\n    }\n}\n";
    let path = sandbox.write("home/.lmstudio/mcp.json", original);
    let lm = sandbox.client("LM Studio");

    lm.add().unwrap();
    lm.remove().unwrap();
    lm.remove().unwrap(); // the second time there is nothing to do

    assert_eq!(lm.status().state, ClientState::Absent);
    assert_eq!(keys(&json(&path)["mcpServers"]), ["other"]);
    assert!(read(&path).contains("\n    \"mcpServers\"")); // four-space indentation
    assert_eq!(read(&path.with_extension("json.clak-bak")), original); // the copy is the one from before any change
    assert!(!path.with_extension("json.clak-tmp").exists());
}

#[test]
fn creates_the_file_if_the_client_exists_but_has_no_config_yet() {
    let sandbox = Sandbox::new();
    sandbox.dir(&["home", ".codeium", "windsurf"]);
    let windsurf = sandbox.client("Windsurf");
    assert_eq!(windsurf.status().state, ClientState::Absent);

    windsurf.add().unwrap();

    assert_eq!(windsurf.status().state, ClientState::Ok);
    let shim = serde_json::to_string(SHIM).unwrap();
    assert_eq!(
        read(&windsurf.status().path),
        format!(
            "{{\n  \"mcpServers\": {{\n    \"clak\": {{\n      \"command\": {shim},\n      \"args\": []\n    }}\n  }}\n}}\n"
        )
    );
}

#[test]
fn a_section_that_is_not_an_object_is_not_touched() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.cursor/mcp.json", "{\"mcpServers\": []}");
    let cursor = sandbox.client("Cursor");
    assert_eq!(cursor.status().state, ClientState::Unreadable);
    assert!(cursor.add().is_err());
    assert_eq!(read(&path), "{\"mcpServers\": []}");
}

#[cfg(windows)]
#[test]
fn store_claude_desktop_is_written_to_its_own_appdata_copy() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "appdata/Claude/claude_desktop_config.json",
        "{\"old\": true}",
    );
    let packaged = sandbox.write(
        "localappdata/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude/claude_desktop_config.json",
        "{\n  \"preferences\": {}\n}\n",
    );
    let desktop = sandbox.client("Claude Desktop");

    assert_eq!(desktop.status().path, packaged);
    desktop.add().unwrap();

    assert!(json(&packaged)["mcpServers"]["clak"].is_object());
    assert_eq!(
        read(
            &sandbox
                .places
                .app_data
                .join("Claude")
                .join("claude_desktop_config.json")
        ),
        "{\"old\": true}"
    );
}

const EXISTING: &str = "# my configuration\nmodel = \"gpt-5\"\n\n[mcp_servers.other]\ncommand = \"npx\"\nargs = [\"-y\", \"something\"]\n\n[projects.'C:\\work']\ntrust_level = \"trusted\"\n";

#[test]
fn codex_adds_and_removes_the_block_without_touching_comments_or_other_tables() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", EXISTING);
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Absent);

    codex.add().unwrap();

    assert_eq!(codex.status().state, ClientState::Ok);
    let text = read(&path);
    assert!(text.starts_with(EXISTING));
    assert!(
        text.ends_with(&format!(
            "\n[mcp_servers.clak]\ncommand = '{SHIM}'\nargs = []\n"
        )),
        "{text}"
    );

    codex.add().unwrap(); // twice doesn't duplicate
    assert_eq!(
        read(&path)
            .lines()
            .filter(|l| *l == "[mcp_servers.clak]")
            .count(),
        1
    );

    codex.remove().unwrap();
    assert_eq!(read(&path), EXISTING);
    assert_eq!(read(&path.with_extension("toml.clak-bak")), EXISTING);
}

#[test]
fn codex_also_removes_the_subtables_of_its_own_entry() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.codex/config.toml",
        "model = \"x\"\n\n[mcp_servers.clak]\ncommand = \"old\"\n\n[mcp_servers.clak.env]\nA = \"1\"\n\n[mcp_servers.clak2]\ncommand = \"stays\"\n",
    );
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Stale);

    codex.remove().unwrap();

    assert_eq!(
        read(&path),
        "model = \"x\"\n\n[mcp_servers.clak2]\ncommand = \"stays\"\n"
    );
}

#[test]
fn codex_an_entry_with_a_quoted_name_is_replaced() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.codex/config.toml",
        "model = \"x\"\n\n[mcp_servers.'clak']\ncommand = 'old'\nargs = []\n",
    );
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Stale);

    codex.add().unwrap();

    assert_eq!(codex.status().state, ClientState::Ok);
    let text = read(&path);
    assert!(!text.contains("old"));
    assert!(text.starts_with("model = \"x\"\n\n[mcp_servers.clak]\n"));

    codex.remove().unwrap();
    assert_eq!(read(&path), "model = \"x\"\n");
}

#[test]
fn codex_keeps_windows_line_endings() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", "model = \"x\"\r\n");
    let codex = sandbox.client("Codex");
    codex.add().unwrap();
    assert_eq!(
        read(&path),
        format!("model = \"x\"\r\n\r\n[mcp_servers.clak]\r\ncommand = '{SHIM}'\r\nargs = []\r\n")
    );
    codex.remove().unwrap();
    assert_eq!(read(&path), "model = \"x\"\r\n");
}

#[test]
fn codex_an_inline_entry_is_not_edited_blindly() {
    let sandbox = Sandbox::new();
    let inline = "mcp_servers = { clak = { command = \"old\" } }\n";
    let path = sandbox.write("home/.codex/config.toml", inline);
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Stale);

    assert!(codex.add().is_err());
    assert!(codex.remove().is_err());
    assert_eq!(read(&path), inline);
}

#[test]
fn codex_a_broken_toml_is_not_rewritten() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", "model = \n[broken\n");
    let codex = sandbox.client("Codex");
    let status = codex.status();
    assert_eq!(status.state, ClientState::Unreadable);
    assert!(
        status
            .detail
            .starts_with("config.toml can't be read (line 1:"),
        "{}",
        status.detail
    );
    assert!(codex.add().is_err());
    assert_eq!(read(&path), "model = \n[broken\n");
}

/// A fake `claude` in the fake machine's `bin` folder.
fn fake_claude(sandbox: &Sandbox, name: &str) -> PathBuf {
    let bin = sandbox.dir(&["bin"]);
    let path = bin.join(name);
    std::fs::write(&path, "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn recorder(calls: &Arc<Mutex<Vec<String>>>) -> CommandRunner {
    let calls = calls.clone();
    Arc::new(move |_exe, args, _env| {
        calls.lock().unwrap().push(args.join(" "));
        Ok((0, String::new()))
    })
}

#[test]
fn claude_code_writes_only_through_its_command() {
    let sandbox = Sandbox::new();
    let bin = fake_claude(
        &sandbox,
        if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        },
    );
    let places = sandbox.places.with_environment(BTreeMap::from([(
        "PATH".to_owned(),
        bin.to_string_lossy().into_owned(),
    )]));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let config = sandbox.write("home/.claude.json", "{\"numStartups\": 3}");
    let client = ClaudeCodeClient::with_runner(&places, &[SHIM.to_owned()], recorder(&calls));

    assert_eq!(client.status().state, ClientState::Absent);
    client.add().unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        [format!("mcp add --scope user clak -- {SHIM}")]
    );
    assert_eq!(read(&config), "{\"numStartups\": 3}"); // we don't touch the file ourselves

    // If the entry is already there and points elsewhere, it is removed first.
    std::fs::write(
        &config,
        "{\"mcpServers\": {\"clak\": {\"type\": \"stdio\", \"command\": \"old\", \"args\": []}}}",
    )
    .unwrap();
    assert_eq!(client.status().state, ClientState::Stale);
    calls.lock().unwrap().clear();
    client.add().unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "mcp remove --scope user clak".to_owned(),
            format!("mcp add --scope user clak -- {SHIM}")
        ]
    );

    calls.lock().unwrap().clear();
    client.remove().unwrap();
    assert_eq!(*calls.lock().unwrap(), ["mcp remove --scope user clak"]);
    assert!(
        client
            .snippet()
            .starts_with("claude mcp add --scope user clak -- ")
    );
}

#[test]
fn claude_code_a_command_error_reaches_the_user() {
    let sandbox = Sandbox::new();
    let bin = fake_claude(
        &sandbox,
        if cfg!(windows) {
            "claude.cmd"
        } else {
            "claude"
        },
    );
    let places = sandbox.places.with_environment(BTreeMap::from([(
        "PATH".to_owned(),
        bin.to_string_lossy().into_owned(),
    )]));
    let client = ClaudeCodeClient::with_runner(
        &places,
        &[SHIM.to_owned()],
        Arc::new(|_, _, _| Ok((1, "MCP server clak already exists\n".to_owned()))),
    );
    assert_eq!(
        client.add().unwrap_err().0,
        "MCP server clak already exists"
    );
}

#[test]
fn claude_code_without_the_command_on_path_says_so_and_writes_nothing() {
    let sandbox = Sandbox::new();
    let config = sandbox.write("home/.claude.json", "{}");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let client =
        ClaudeCodeClient::with_runner(&sandbox.places, &[SHIM.to_owned()], recorder(&calls));
    assert_eq!(client.status().state, ClientState::Absent);
    assert!(client.add().unwrap_err().0.contains("PATH"));
    assert_eq!(read(&config), "{}");
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn claude_config_dir_moves_the_claude_code_file() {
    let sandbox = Sandbox::new();
    let config = sandbox.dir(&["claude"]);
    let places = sandbox.places.with_environment(BTreeMap::from([(
        "CLAUDE_CONFIG_DIR".to_owned(),
        config.to_string_lossy().into_owned(),
    )]));
    let client = ClaudeCodeClient::new(&places, &[SHIM.to_owned()]);
    assert_eq!(client.status().path, config.join(".claude.json"));
}

#[test]
fn the_catalog_is_in_panel_order() {
    let sandbox = Sandbox::new();
    let names: Vec<String> = clak_setup::clients::all(&sandbox.places, &[SHIM.to_owned()])
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "Claude Code",
            "Claude Desktop",
            "VS Code",
            "Cursor",
            "Windsurf",
            "Gemini CLI",
            "Codex",
            "LM Studio"
        ]
    );
}

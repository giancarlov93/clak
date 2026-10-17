//! The MCP clients: JSON files, Codex's TOML, Claude Code through its command.

mod common;

use clak_setup::clients::{ClaudeCodeClient, ClientState, CommandRunner, McpClient};
use common::{SHIM, Sandbox, json, keys, read};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[test]
fn un_client_che_non_c_e_non_viene_creato_dal_nulla() {
    let sandbox = Sandbox::new();
    let cursor = sandbox.client("Cursor");
    assert_eq!(cursor.status().state, ClientState::NotInstalled);
    assert!(!sandbox.places.home.join(".cursor").exists());
}

#[test]
fn aggiunge_la_voce_e_lascia_intatto_tutto_il_resto() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.gemini/settings.json",
        "{\n  \"ui\": {\"theme\": \"Città\"},\n  \"mcpServers\": {\"altro\": {\"command\": \"x\"}},\n  \"model\": 1.50\n}\n",
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
    assert_eq!(data["mcpServers"]["altro"]["command"], "x");
    assert_eq!(keys(&data), ["ui", "mcpServers", "model"]);
    assert!(text.contains("Città")); // no \u00e0
    assert!(text.contains("1.50")); // numbers stay written as they were
    assert!(text.ends_with("}\n"));
    assert!(!text.contains('\r'));
}

#[test]
fn conserva_tabulazioni_fine_riga_e_bom() {
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
fn un_file_con_commenti_non_viene_riscritto() {
    let sandbox = Sandbox::new();
    let original = "{\n  // i miei server\n  \"mcpServers\": {}\n}\n";
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
fn una_chiave_ripetuta_non_si_tocca() {
    let sandbox = Sandbox::new();
    let original = "{\"mcpServers\": {}, \"mcpServers\": {}}";
    let path = sandbox.write("home/.cursor/mcp.json", original);
    let cursor = sandbox.client("Cursor");
    assert_eq!(cursor.add().unwrap_err().0, "mcp.json has a repeated key: not touching it.");
    assert_eq!(read(&path), original);
}

#[test]
fn una_voce_che_punta_altrove_e_da_aggiornare_e_si_aggiorna() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.cursor/mcp.json",
        "{\"mcpServers\": {\"clak\": {\"command\": \"C:/vecchio/clak-shim.exe\", \"args\": [\"-v\"], \"env\": {\"A\": \"1\"}}}}",
    );
    let cursor = sandbox.client("Cursor");

    let before = cursor.status();
    assert_eq!(before.state, ClientState::Stale);
    assert_eq!(before.detail, "C:/vecchio/clak-shim.exe -v");

    cursor.add().unwrap();
    assert_eq!(cursor.status().state, ClientState::Ok);
    assert_eq!(json(&path)["mcpServers"]["clak"]["args"], serde_json::json!([]));
}

#[cfg(windows)]
#[test]
fn lo_stesso_percorso_scritto_con_altre_barre_e_la_stessa_voce() {
    let sandbox = Sandbox::new();
    sandbox.write("home/.cursor/mcp.json", "{\"mcpServers\": {\"clak\": {\"command\": \"c:/program files/clak/CLAK-SHIM.EXE\"}}}");
    assert_eq!(sandbox.client("Cursor").status().state, ClientState::Ok);
}

#[cfg(not(windows))]
#[test]
fn lo_stesso_percorso_scritto_in_un_altro_modo_e_la_stessa_voce() {
    let sandbox = Sandbox::new();
    sandbox.write(
        "home/.cursor/mcp.json",
        "{\"mcpServers\": {\"clak\": {\"command\": \"/Applications//Clak.app/./Contents/MacOS/x/../clak-shim/\"}}}",
    );
    assert_eq!(sandbox.client("Cursor").status().state, ClientState::Ok);
}

#[test]
fn rimuove_solo_la_propria_voce_e_tiene_una_sola_copia_dell_originale() {
    let sandbox = Sandbox::new();
    let original = "{\n    \"mcpServers\": {\n        \"altro\": {\"command\": \"x\"}\n    }\n}\n";
    let path = sandbox.write("home/.lmstudio/mcp.json", original);
    let lm = sandbox.client("LM Studio");

    lm.add().unwrap();
    lm.remove().unwrap();
    lm.remove().unwrap(); // the second time there is nothing to do

    assert_eq!(lm.status().state, ClientState::Absent);
    assert_eq!(keys(&json(&path)["mcpServers"]), ["altro"]);
    assert!(read(&path).contains("\n    \"mcpServers\"")); // four-space indentation
    assert_eq!(read(&path.with_extension("json.clak-bak")), original); // the copy is the one from before any change
    assert!(!path.with_extension("json.clak-tmp").exists());
}

#[test]
fn crea_il_file_se_il_client_c_e_ma_non_ha_ancora_configurazione() {
    let sandbox = Sandbox::new();
    sandbox.dir(&["home", ".codeium", "windsurf"]);
    let windsurf = sandbox.client("Windsurf");
    assert_eq!(windsurf.status().state, ClientState::Absent);

    windsurf.add().unwrap();

    assert_eq!(windsurf.status().state, ClientState::Ok);
    let shim = serde_json::to_string(SHIM).unwrap();
    assert_eq!(
        read(&windsurf.status().path),
        format!("{{\n  \"mcpServers\": {{\n    \"clak\": {{\n      \"command\": {shim},\n      \"args\": []\n    }}\n  }}\n}}\n")
    );
}

#[test]
fn una_sezione_che_non_e_un_oggetto_non_viene_toccata() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.cursor/mcp.json", "{\"mcpServers\": []}");
    let cursor = sandbox.client("Cursor");
    assert_eq!(cursor.status().state, ClientState::Unreadable);
    assert!(cursor.add().is_err());
    assert_eq!(read(&path), "{\"mcpServers\": []}");
}

#[cfg(windows)]
#[test]
fn claude_desktop_dello_store_si_scrive_nella_sua_copia_di_appdata() {
    let sandbox = Sandbox::new();
    sandbox.write("appdata/Claude/claude_desktop_config.json", "{\"vecchio\": true}");
    let packaged = sandbox.write(
        "localappdata/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude/claude_desktop_config.json",
        "{\n  \"preferences\": {}\n}\n",
    );
    let desktop = sandbox.client("Claude Desktop");

    assert_eq!(desktop.status().path, packaged);
    desktop.add().unwrap();

    assert!(json(&packaged)["mcpServers"]["clak"].is_object());
    assert_eq!(read(&sandbox.places.app_data.join("Claude").join("claude_desktop_config.json")), "{\"vecchio\": true}");
}

const EXISTING: &str = "# la mia configurazione\nmodel = \"gpt-5\"\n\n[mcp_servers.altro]\ncommand = \"npx\"\nargs = [\"-y\", \"qualcosa\"]\n\n[projects.'C:\\lavoro']\ntrust_level = \"trusted\"\n";

#[test]
fn codex_aggiunge_e_toglie_il_blocco_senza_toccare_commenti_e_tabelle_altrui() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", EXISTING);
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Absent);

    codex.add().unwrap();

    assert_eq!(codex.status().state, ClientState::Ok);
    let text = read(&path);
    assert!(text.starts_with(EXISTING));
    assert!(text.ends_with(&format!("\n[mcp_servers.clak]\ncommand = '{SHIM}'\nargs = []\n")), "{text}");

    codex.add().unwrap(); // twice doesn't duplicate
    assert_eq!(read(&path).lines().filter(|l| *l == "[mcp_servers.clak]").count(), 1);

    codex.remove().unwrap();
    assert_eq!(read(&path), EXISTING);
    assert_eq!(read(&path.with_extension("toml.clak-bak")), EXISTING);
}

#[test]
fn codex_toglie_anche_le_sottotabelle_della_propria_voce() {
    let sandbox = Sandbox::new();
    let path = sandbox.write(
        "home/.codex/config.toml",
        "model = \"x\"\n\n[mcp_servers.clak]\ncommand = \"vecchio\"\n\n[mcp_servers.clak.env]\nA = \"1\"\n\n[mcp_servers.clak2]\ncommand = \"resta\"\n",
    );
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Stale);

    codex.remove().unwrap();

    assert_eq!(read(&path), "model = \"x\"\n\n[mcp_servers.clak2]\ncommand = \"resta\"\n");
}

#[test]
fn codex_una_voce_con_il_nome_fra_apici_si_sostituisce() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", "model = \"x\"\n\n[mcp_servers.'clak']\ncommand = 'vecchio'\nargs = []\n");
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Stale);

    codex.add().unwrap();

    assert_eq!(codex.status().state, ClientState::Ok);
    let text = read(&path);
    assert!(!text.contains("vecchio"));
    assert!(text.starts_with("model = \"x\"\n\n[mcp_servers.clak]\n"));

    codex.remove().unwrap();
    assert_eq!(read(&path), "model = \"x\"\n");
}

#[test]
fn codex_conserva_i_ritorni_a_capo_di_windows() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", "model = \"x\"\r\n");
    let codex = sandbox.client("Codex");
    codex.add().unwrap();
    assert_eq!(read(&path), format!("model = \"x\"\r\n\r\n[mcp_servers.clak]\r\ncommand = '{SHIM}'\r\nargs = []\r\n"));
    codex.remove().unwrap();
    assert_eq!(read(&path), "model = \"x\"\r\n");
}

#[test]
fn codex_una_voce_scritta_in_linea_non_si_modifica_alla_cieca() {
    let sandbox = Sandbox::new();
    let inline = "mcp_servers = { clak = { command = \"vecchio\" } }\n";
    let path = sandbox.write("home/.codex/config.toml", inline);
    let codex = sandbox.client("Codex");
    assert_eq!(codex.status().state, ClientState::Stale);

    assert!(codex.add().is_err());
    assert!(codex.remove().is_err());
    assert_eq!(read(&path), inline);
}

#[test]
fn codex_un_toml_rotto_non_viene_riscritto() {
    let sandbox = Sandbox::new();
    let path = sandbox.write("home/.codex/config.toml", "model = \n[rotto\n");
    let codex = sandbox.client("Codex");
    let status = codex.status();
    assert_eq!(status.state, ClientState::Unreadable);
    assert!(status.detail.starts_with("config.toml can't be read (line 1:"), "{}", status.detail);
    assert!(codex.add().is_err());
    assert_eq!(read(&path), "model = \n[rotto\n");
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
fn claude_code_scrive_solo_attraverso_il_suo_comando() {
    let sandbox = Sandbox::new();
    let bin = fake_claude(&sandbox, if cfg!(windows) { "claude.exe" } else { "claude" });
    let places = sandbox.places.with_environment(BTreeMap::from([("PATH".to_owned(), bin.to_string_lossy().into_owned())]));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let config = sandbox.write("home/.claude.json", "{\"numStartups\": 3}");
    let client = ClaudeCodeClient::with_runner(&places, &[SHIM.to_owned()], recorder(&calls));

    assert_eq!(client.status().state, ClientState::Absent);
    client.add().unwrap();
    assert_eq!(*calls.lock().unwrap(), [format!("mcp add --scope user clak -- {SHIM}")]);
    assert_eq!(read(&config), "{\"numStartups\": 3}"); // we don't touch the file ourselves

    // If the entry is already there and points elsewhere, it is removed first.
    std::fs::write(&config, "{\"mcpServers\": {\"clak\": {\"type\": \"stdio\", \"command\": \"vecchio\", \"args\": []}}}").unwrap();
    assert_eq!(client.status().state, ClientState::Stale);
    calls.lock().unwrap().clear();
    client.add().unwrap();
    assert_eq!(*calls.lock().unwrap(), ["mcp remove --scope user clak".to_owned(), format!("mcp add --scope user clak -- {SHIM}")]);

    calls.lock().unwrap().clear();
    client.remove().unwrap();
    assert_eq!(*calls.lock().unwrap(), ["mcp remove --scope user clak"]);
    assert!(client.snippet().starts_with("claude mcp add --scope user clak -- "));
}

#[test]
fn claude_code_un_errore_del_comando_arriva_all_utente() {
    let sandbox = Sandbox::new();
    let bin = fake_claude(&sandbox, if cfg!(windows) { "claude.cmd" } else { "claude" });
    let places = sandbox.places.with_environment(BTreeMap::from([("PATH".to_owned(), bin.to_string_lossy().into_owned())]));
    let client = ClaudeCodeClient::with_runner(
        &places,
        &[SHIM.to_owned()],
        Arc::new(|_, _, _| Ok((1, "MCP server clak already exists\n".to_owned()))),
    );
    assert_eq!(client.add().unwrap_err().0, "MCP server clak already exists");
}

#[test]
fn claude_code_senza_il_comando_nel_path_si_dice_e_non_si_scrive() {
    let sandbox = Sandbox::new();
    let config = sandbox.write("home/.claude.json", "{}");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let client = ClaudeCodeClient::with_runner(&sandbox.places, &[SHIM.to_owned()], recorder(&calls));
    assert_eq!(client.status().state, ClientState::Absent);
    assert!(client.add().unwrap_err().0.contains("PATH"));
    assert_eq!(read(&config), "{}");
    assert!(calls.lock().unwrap().is_empty());
}

#[test]
fn claude_config_dir_sposta_il_file_di_claude_code() {
    let sandbox = Sandbox::new();
    let config = sandbox.dir(&["claude"]);
    let places = sandbox.places.with_environment(BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), config.to_string_lossy().into_owned())]));
    let client = ClaudeCodeClient::new(&places, &[SHIM.to_owned()]);
    assert_eq!(client.status().path, config.join(".claude.json"));
}

#[test]
fn il_catalogo_e_nell_ordine_del_pannello() {
    let sandbox = Sandbox::new();
    let names: Vec<String> = clak_setup::clients::all(&sandbox.places, &[SHIM.to_owned()]).iter().map(|c| c.name().to_owned()).collect();
    assert_eq!(names, ["Claude Code", "Claude Desktop", "VS Code", "Cursor", "Windsurf", "Gemini CLI", "Codex", "LM Studio"]);
}

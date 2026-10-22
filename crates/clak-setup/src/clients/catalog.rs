use super::{ClaudeCodeClient, CodexClient, JsonFileClient, McpClient};
use crate::Places;
use std::path::PathBuf;

/// The clients the panel can configure, in the order it shows them. `command` is the shim
/// command: executable (full path) and arguments.
pub fn all(places: &Places, command: &[String]) -> Vec<Box<dyn McpClient>> {
    let home = |parts: &[&str]| {
        parts
            .iter()
            .fold(places.home.clone(), |path, part| path.join(part))
    };

    let desktop = desktop_files(places);
    let code = vscode_file(places);
    let desktop_note = if cfg!(windows) {
        "Restart Claude Desktop (quit from the tray too)"
    } else {
        "Restart Claude Desktop (⌘Q)"
    };

    vec![
        Box::new(ClaudeCodeClient::new(places, command)),
        Box::new(JsonFileClient::new(
            "Claude Desktop",
            desktop,
            command,
            desktop_note,
        )),
        Box::new(
            JsonFileClient::new("VS Code", vec![code], command, "Default VS Code profile")
                .section("servers")
                .typed(),
        ),
        Box::new(JsonFileClient::new(
            "Cursor",
            vec![cursor_file(places)],
            command,
            "Restart Cursor if missing",
        )),
        Box::new(JsonFileClient::new(
            "Windsurf",
            vec![home(&[".codeium", "windsurf", "mcp_config.json"])],
            command,
            "Restart Windsurf if missing",
        )),
        Box::new(JsonFileClient::new(
            "Gemini CLI",
            vec![home(&[".gemini", "settings.json"])],
            command,
            "",
        )),
        Box::new(CodexClient::new(places, command)),
        Box::new(JsonFileClient::new(
            "LM Studio",
            vec![home(&[".lmstudio", "mcp.json"])],
            command,
            "Restart LM Studio if missing",
        )),
    ]
}

/// Claude Desktop's configuration, the candidates most likely first.
pub(crate) fn desktop_files(places: &Places) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        // Claude Desktop installed from the Store sees its own copy of %APPDATA%: that is
        // the one it reads, so that is the one to write.
        let mut desktop: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(places.local_app_data.join("Packages")) {
            let mut packages: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path().is_dir() && e.file_name().to_string_lossy().starts_with("Claude_")
                })
                .map(|e| e.path())
                .collect();
            packages.sort();
            desktop.extend(packages.into_iter().map(|p| {
                p.join("LocalCache")
                    .join("Roaming")
                    .join("Claude")
                    .join("claude_desktop_config.json")
            }));
        }
        desktop.push(
            places
                .app_data
                .join("Claude")
                .join("claude_desktop_config.json"),
        );
        desktop
    }
    #[cfg(not(windows))]
    {
        vec![
            places
                .application_support()
                .join("Claude")
                .join("claude_desktop_config.json"),
        ]
    }
}

/// VS Code's MCP servers, in the default profile.
pub(crate) fn vscode_file(places: &Places) -> PathBuf {
    #[cfg(windows)]
    let base = places.app_data.clone();
    #[cfg(not(windows))]
    let base = places.application_support();
    base.join("Code").join("User").join("mcp.json")
}

pub(crate) fn cursor_file(places: &Places) -> PathBuf {
    places.home.join(".cursor").join("mcp.json")
}

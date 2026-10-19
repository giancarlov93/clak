//! The presets: an agent, a folder, the programs it may use without asking and, optionally,
//! how it starts (a system prompt, a model), in `presets.json` in the hub folder. The hub
//! doesn't read them: the app hands it the preset when the user launches it (see
//! `clak_hub::launch`).
//!
//! ```json
//! { "presets": [ {
//!     "id": "preset_1a2b3c4d5e6f", "name": "Shopping", "agent": "claude-code",
//!     "folder": "/Users/me/shopping",
//!     "programs": [
//!       { "app": "/applications/word.app/…|Word|firma:Microsoft Corporation", "label": "Word" },
//!       { "name": "notion", "label": "Notion", "tools": ["search", "fetch"] }
//!     ],
//!     "system": { "mode": "append", "text": "Answer in Italian." },
//!     "model": "opus"
//! } ] }
//! ```
//!
//! At launch the system prompt goes in `presets/<id>/system.md` in the hub folder, where the
//! agent reads it (see `launcher::extra`): a file, so that no text passes on a command line.
//!
//! A program is a known application by its key in `apps.json` (all its documents) or a
//! connector by its name; `tools` only if not all its commands.

use crate::launcher::{self, Extra, Mode, Options};
use crate::{Result, SetupError};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

fn file(home: &Path) -> PathBuf {
    home.join("presets.json")
}

/// The presets, in the order they were made; none if the file is missing or unreadable.
pub fn all(home: &Path) -> Vec<Map<String, Value>> {
    std::fs::read(file(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.get("presets").and_then(Value::as_array).cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p {
            Value::Object(map) => checked(map).ok(),
            _ => None,
        })
        .collect()
}

pub fn get(home: &Path, id: &str) -> Option<Map<String, Value>> {
    all(home)
        .into_iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some(id))
}

/// Saves a preset: a new one (without `id`, which it gets) or a changed one. Returns it.
pub fn save(home: &Path, preset: Map<String, Value>) -> Result<Map<String, Value>> {
    let mut preset = checked(preset)?;
    let mut list = all(home);
    match preset.get("id").and_then(Value::as_str).map(str::to_owned) {
        Some(id) => match list
            .iter_mut()
            .find(|p| p.get("id").and_then(Value::as_str) == Some(id.as_str()))
        {
            Some(slot) => *slot = preset.clone(),
            None => list.push(preset.clone()),
        },
        None => {
            preset.insert("id".into(), json!(clak_wire::json::new_id("preset")));
            list.push(preset.clone());
        }
    }
    write(home, &list)?;
    Ok(preset)
}

pub fn remove(home: &Path, id: &str) -> Result<()> {
    let list: Vec<_> = all(home)
        .into_iter()
        .filter(|p| p.get("id").and_then(Value::as_str) != Some(id))
        .collect();
    write(home, &list)?;
    if valid_id(id) {
        let _ = std::fs::remove_dir_all(folder(home, id));
    }
    Ok(())
}

/// Where a preset keeps the files of its launch: an id made by `save`, never a path.
fn folder(home: &Path, id: &str) -> PathBuf {
    home.join("presets").join(id)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What a preset adds to its agent's launch: the system prompt written to its file (and, for
/// OpenCode, the config that points to it), the model.
pub fn extra(home: &Path, preset: &Map<String, Value>, agent: &str) -> Result<Extra> {
    let text = |key: &str| preset.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut options = Options {
        model: text("model"),
        ..Options::default()
    };
    let system = preset.get("system");
    let prompt = system.and_then(|s| s.get("text")).and_then(Value::as_str);
    let mode = system
        .and_then(|s| s.get("mode"))
        .and_then(Value::as_str)
        .and_then(Mode::parse);
    if let (Some(prompt), Some(mode), Some(id)) =
        (prompt, mode, text("id").filter(|id| valid_id(id)))
    {
        let dir = folder(home, &id);
        std::fs::create_dir_all(&dir)?;
        let file = dir.join("system.md");
        std::fs::write(&file, prompt)?;
        if agent == "opencode" {
            let config = dir.join("opencode.json");
            std::fs::write(&config, launcher::opencode_config(&file))?;
            options.opencode_config = Some(config);
        }
        options.system = Some((mode, file));
    }
    launcher::extra(agent, &options)
}

fn write(home: &Path, list: &[Map<String, Value>]) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let text =
        serde_json::to_string_pretty(&json!({ "presets": list })).unwrap_or_else(|_| "{}".into());
    std::fs::write(file(home), text)?;
    Ok(())
}

/// Only the known fields, with their types: what a page sends is not trusted as it is.
fn checked(preset: Map<String, Value>) -> Result<Map<String, Value>> {
    let text = |key: &str| {
        preset
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    let name = text("name").ok_or_else(|| SetupError::new("The preset needs a name."))?;
    let agent = text("agent").ok_or_else(|| SetupError::new("The preset needs an agent."))?;
    let mut programs = Vec::new();
    for program in preset
        .get("programs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |key: &str| {
            program
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        };
        let mut clean = Map::new();
        match (field("app"), field("name")) {
            (Some(key), _) => clean.insert("app".into(), json!(key)),
            (None, Some(name)) => clean.insert("name".into(), json!(name)),
            (None, None) => continue,
        };
        if let Some(label) = field("label") {
            clean.insert("label".into(), json!(label));
        }
        if let Some(tools) = program.get("tools").and_then(Value::as_array) {
            let tools: Vec<&str> = tools.iter().filter_map(Value::as_str).collect();
            clean.insert("tools".into(), json!(tools));
        }
        programs.push(Value::Object(clean));
    }
    let model = text("model");
    if model.as_deref().is_some_and(|m| !launcher::valid_model(m)) {
        return Err(SetupError::new(
            "A model name is one word: letters, digits and . _ : / @ - [ ].",
        ));
    }
    let system = preset.get("system").and_then(|system| {
        let mode = system
            .get("mode")
            .and_then(Value::as_str)
            .and_then(Mode::parse)?;
        let prompt = system
            .get("text")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())?;
        let mode = if mode == Mode::Append {
            "append"
        } else {
            "replace"
        };
        Some((mode, prompt))
    });
    if let Some((mode, _)) = system
        && !launcher::system_modes(&agent).contains(&mode)
    {
        return Err(SetupError::new(if mode == "append" {
            "This agent can't add to its system prompt at launch."
        } else {
            "This agent can't replace its system prompt at launch."
        }));
    }
    let mut clean = Map::new();
    if let Some(id) = text("id").filter(|id| valid_id(id)) {
        clean.insert("id".into(), json!(id));
    }
    clean.insert("name".into(), json!(name));
    clean.insert("agent".into(), json!(agent));
    if let Some(folder) = text("folder") {
        clean.insert("folder".into(), json!(folder));
    }
    clean.insert("programs".into(), Value::Array(programs));
    if let Some((mode, prompt)) = system {
        clean.insert("system".into(), json!({ "mode": mode, "text": prompt }));
    }
    if let Some(model) = model {
        clean.insert("model".into(), json!(model));
    }
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("clak-presets-{}", clak_wire::json::random_hex(6)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn object(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn a_preset_is_saved_changed_and_removed() {
        let home = temp();
        let saved = save(
            &home,
            object(json!({
                "name": " Shopping ", "agent": "claude-code", "folder": "/tmp",
                "programs": [
                    { "app": "key|Word|firma:Microsoft", "label": "Word", "extra": true },
                    { "name": "notion", "tools": ["search"] },
                    { "label": "nothing to point to" },
                ],
                "unknown": "dropped",
            })),
        )
        .unwrap();
        let id = saved["id"].as_str().unwrap().to_owned();
        assert!(id.starts_with("preset_"));
        assert_eq!(
            Value::Object(get(&home, &id).unwrap()),
            json!({
                "id": id, "name": "Shopping", "agent": "claude-code", "folder": "/tmp",
                "programs": [
                    { "app": "key|Word|firma:Microsoft", "label": "Word" },
                    { "name": "notion", "tools": ["search"] },
                ],
            })
        );

        let mut changed = saved.clone();
        changed.insert("name".into(), json!("Groceries"));
        save(&home, changed).unwrap();
        assert_eq!(all(&home).len(), 1);
        assert_eq!(all(&home)[0]["name"], json!("Groceries"));

        remove(&home, &id).unwrap();
        assert!(all(&home).is_empty());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_preset_starts_its_agent_with_its_prompt_and_its_model() {
        let home = temp();
        let saved = save(
            &home,
            object(json!({
                "name": "Review", "agent": "claude-code", "model": "opus",
                "system": { "mode": "replace", "text": "Review only." },
            })),
        )
        .unwrap();
        assert_eq!(
            saved["system"],
            json!({ "mode": "replace", "text": "Review only." })
        );
        let id = saved["id"].as_str().unwrap();
        let file = home.join("presets").join(id).join("system.md");
        let launch = extra(&home, &saved, "claude-code").unwrap();
        assert_eq!(
            launch.args,
            vec![
                "--model".to_owned(),
                "opus".to_owned(),
                "--system-prompt-file".to_owned(),
                file.to_string_lossy().into_owned(),
            ]
        );
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "Review only.");

        let open = save(
            &home,
            object(json!({
                "name": "Open", "agent": "opencode",
                "system": { "mode": "append", "text": "Be brief." },
            })),
        )
        .unwrap();
        let config = home
            .join("presets")
            .join(open["id"].as_str().unwrap())
            .join("opencode.json");
        assert_eq!(
            extra(&home, &open, "opencode").unwrap().env,
            vec![(
                "OPENCODE_CONFIG".to_owned(),
                config.to_string_lossy().into_owned()
            )]
        );
        assert!(
            std::fs::read_to_string(&config)
                .unwrap()
                .contains("system.md")
        );

        remove(&home, id).unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn what_a_preset_cant_do_is_refused() {
        let home = temp();
        // Gemini CLI only replaces its system prompt.
        assert!(
            save(
                &home,
                object(json!({
                    "name": "x", "agent": "gemini", "system": { "mode": "append", "text": "a" },
                }))
            )
            .is_err()
        );
        assert!(
            save(
                &home,
                object(json!({ "name": "x", "agent": "codex", "model": "gpt 5" }))
            )
            .is_err()
        );
        // An empty prompt is no prompt; an id that is a path is no id.
        let saved = save(
            &home,
            object(json!({
                "id": "../../elsewhere", "name": "x", "agent": "gemini",
                "system": { "mode": "replace", "text": "  " },
            })),
        )
        .unwrap();
        assert!(saved.get("system").is_none());
        assert!(saved["id"].as_str().unwrap().starts_with("preset_"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_preset_needs_a_name_and_an_agent() {
        let home = temp();
        assert!(save(&home, object(json!({ "agent": "codex" }))).is_err());
        assert!(save(&home, object(json!({ "name": "x", "agent": "  " }))).is_err());
        let _ = std::fs::remove_dir_all(home);
    }
}

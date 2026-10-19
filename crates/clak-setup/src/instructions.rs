//! The agents' instructions files, edited from the Agents tab: the global one of each agent
//! (`~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`…), read in every session, and the one in a
//! project folder (`CLAUDE.md`, `AGENTS.md`, `GEMINI.md`), read in the sessions opened there,
//! edited from a preset.
//!
//! They are the user's files, not Clak's: each is changed only when the user saves it, and
//! the text it had is kept next to it as `.clak-bak`.

use crate::places::suffixed;
use crate::{Places, Result, SetupError};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The global instructions file of an agent (by its catalog id, see `launcher`), if it has one.
pub fn global(places: &Places, agent: &str) -> Option<PathBuf> {
    let home = |parts: &[&str]| {
        parts
            .iter()
            .fold(places.home.clone(), |path, part| path.join(part))
    };
    let config = |var: &str, default: &str| match places.var(var) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => places.home.join(default),
    };
    Some(match agent {
        "claude-code" => config("CLAUDE_CONFIG_DIR", ".claude").join("CLAUDE.md"),
        "codex" => config("CODEX_HOME", ".codex").join("AGENTS.md"),
        "gemini" => home(&[".gemini", "GEMINI.md"]),
        "opencode" => home(&[".config", "opencode", "AGENTS.md"]),
        _ => return None,
    })
}

/// The instructions file of an agent in a project folder, if it reads one.
pub fn project(agent: &str, folder: &Path) -> Option<PathBuf> {
    let name = match agent {
        "claude-code" => "CLAUDE.md",
        "codex" | "opencode" => "AGENTS.md",
        "gemini" => "GEMINI.md",
        _ => return None,
    };
    Some(folder.join(name))
}

/// The text of an instructions file: empty if there is none yet.
pub fn read(path: &Path) -> Result<String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| SetupError(format!("{} isn't UTF-8 text.", path.display()))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(SetupError(format!("Couldn't read {}: {e}", path.display()))),
    }
}

/// Writes an instructions file, keeping the text it had as `<file>.clak-bak`. Nothing to
/// write, where there was nothing, leaves no empty file behind.
pub fn write(path: &Path, text: &str) -> Result<()> {
    let before = read(path)?;
    if before == text {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !before.is_empty() {
        std::fs::write(suffixed(path, ".clak-bak"), &before)?;
    }
    let draft = suffixed(path, ".clak-tmp");
    std::fs::write(&draft, text)?;
    std::fs::rename(&draft, path).map_err(|e| {
        let _ = std::fs::remove_file(&draft);
        SetupError(format!("Couldn't write {}: {e}", path.display()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "clak-instructions-{}",
            clak_wire::json::random_hex(6)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn each_agent_has_its_own_file_and_its_variables_move_it() {
        let home = temp();
        let places = Places::new(&home, home.join("data"), home.join("data"), BTreeMap::new());
        assert_eq!(
            global(&places, "claude-code"),
            Some(home.join(".claude").join("CLAUDE.md"))
        );
        assert_eq!(
            global(&places, "opencode"),
            Some(home.join(".config").join("opencode").join("AGENTS.md"))
        );
        assert_eq!(global(&places, "vscode"), None);
        let moved = places.with_environment(BTreeMap::from([(
            "CODEX_HOME".to_owned(),
            home.join("codex").to_string_lossy().into_owned(),
        )]));
        assert_eq!(
            global(&moved, "codex"),
            Some(home.join("codex").join("AGENTS.md"))
        );
        assert_eq!(project("gemini", &home), Some(home.join("GEMINI.md")));
        assert_eq!(project("claude-desktop", &home), None);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_save_keeps_the_text_it_replaces() {
        let home = temp();
        let file = home.join(".claude").join("CLAUDE.md");
        assert_eq!(read(&file).unwrap(), "");
        write(&file, "").unwrap();
        assert!(!file.exists(), "nothing written where there was nothing");
        write(&file, "first").unwrap();
        assert_eq!(read(&file).unwrap(), "first");
        assert!(!suffixed(&file, ".clak-bak").exists());
        write(&file, "second").unwrap();
        assert_eq!(read(&file).unwrap(), "second");
        assert_eq!(read(&suffixed(&file, ".clak-bak")).unwrap(), "first");
        let _ = std::fs::remove_dir_all(home);
    }
}

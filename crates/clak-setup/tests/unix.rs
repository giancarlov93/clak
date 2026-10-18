//! `clak` in the PATH with a link in `~/.local/bin`, start at login and the repair of
//! what pointed to a vanished app, on a fake home.

#![cfg(unix)]

mod common;

use clak_setup::autostart::{Autostart, AutostartState};
use clak_setup::clients::ClientState;
use clak_setup::installation;
use clak_setup::skill::{self, Skill};
use clak_setup::user_path::{PathLink, PathLinkState, PathLinkStatus};
use common::{Sandbox, json, keys, read};
use std::os::unix::fs::symlink;
use std::path::Path;

const CLI: &str = "/Applications/Clak.app/Contents/MacOS/clak";

fn link(sandbox: &Sandbox) -> PathLink {
    PathLink::user_bin(&sandbox.places, CLI)
}

fn target(path: &Path) -> Option<String> {
    std::fs::read_link(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

#[test]
fn senza_la_cartella_non_si_crea_niente_e_si_dice_cosa_fare() {
    let sandbox = Sandbox::new();
    assert_eq!(link(&sandbox).status().state, PathLinkState::NoFolder);
    assert!(link(&sandbox).add().unwrap_err().0.contains(".zprofile"));
    assert!(!sandbox.places.home.join(".local").exists());
}

#[test]
fn si_aggiunge_si_riconosce_e_si_toglie_rimettendo_tutto_com_era() {
    let sandbox = Sandbox::new();
    let bin = sandbox.dir(&["home", ".local", "bin"]);
    let claude = sandbox.write("home/.local/bin/claude", "#!/bin/sh\n");
    let listing = || {
        let mut names: Vec<String> = std::fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let before = listing();
    assert_eq!(link(&sandbox).status().state, PathLinkState::Absent);

    link(&sandbox).add().unwrap();
    let path = format!("{}/clak", bin.display());
    assert_eq!(
        link(&sandbox).status(),
        PathLinkStatus {
            state: PathLinkState::Ok,
            path: path.clone(),
            target: Some(CLI.into())
        }
    );
    assert_eq!(target(Path::new(&path)).as_deref(), Some(CLI));
    link(&sandbox).add().unwrap(); // twice changes nothing

    link(&sandbox).remove().unwrap();
    link(&sandbox).remove().unwrap();
    assert_eq!(link(&sandbox).status().state, PathLinkState::Absent);
    assert_eq!(listing(), before);
    assert_eq!(read(&claude), "#!/bin/sh\n");
}

#[test]
fn un_collegamento_a_un_altra_copia_dell_app_si_aggiorna() {
    let sandbox = Sandbox::new();
    let bin = sandbox.dir(&["home", ".local", "bin"]);
    symlink(
        "/Volumes/Altro/Clak.app/Contents/MacOS/clak",
        bin.join("clak"),
    )
    .unwrap();
    assert_eq!(link(&sandbox).status().state, PathLinkState::Stale);
    link(&sandbox).add().unwrap();
    assert_eq!(target(&bin.join("clak")).as_deref(), Some(CLI));

    // On removal too: it is ours even if it points to an app that is gone.
    std::fs::remove_file(bin.join("clak")).unwrap();
    symlink(
        "/Volumes/Altro/Clak.app/Contents/MacOS/clak",
        bin.join("clak"),
    )
    .unwrap();
    link(&sandbox).remove().unwrap();
    assert_eq!(link(&sandbox).status().state, PathLinkState::Absent);
}

#[test]
fn un_clak_che_non_e_nostro_non_si_tocca() {
    for kind in ["file", "altro collegamento", "cartella"] {
        let sandbox = Sandbox::new();
        let bin = sandbox.dir(&["home", ".local", "bin"]);
        let path = bin.join("clak");
        match kind {
            "file" => {
                sandbox.write("home/.local/bin/clak", "#!/bin/sh\necho mio\n");
            }
            "cartella" => {
                sandbox.dir(&["home", ".local", "bin", "clak"]);
            }
            _ => symlink("/usr/local/lib/clak/clak", &path).unwrap(),
        }
        assert_eq!(
            link(&sandbox).status().state,
            PathLinkState::Foreign,
            "{kind}"
        );
        assert!(link(&sandbox).add().is_err());
        link(&sandbox).remove().unwrap();
        match kind {
            "file" => assert_eq!(read(&path), "#!/bin/sh\necho mio\n"),
            "cartella" => assert!(path.is_dir()),
            _ => assert_eq!(target(&path).as_deref(), Some("/usr/local/lib/clak/clak")),
        }
    }
}

#[test]
fn il_collegamento_relativo_alla_stessa_app_e_il_nostro() {
    let sandbox = Sandbox::new();
    let bin = sandbox.dir(&["home", ".local", "bin"]);
    let cli = format!(
        "{}/../Applications/Clak.app/Contents/MacOS/clak",
        bin.display()
    );
    symlink(
        "../Applications/Clak.app/Contents/MacOS/clak",
        bin.join("clak"),
    )
    .unwrap();
    assert_eq!(
        PathLink::new(bin.to_string_lossy(), cli).status().state,
        PathLinkState::Ok
    );
}

#[test]
fn l_avvio_al_login_si_accende_si_spegne_e_riconosce_le_altre_copie() {
    let sandbox = Sandbox::new();
    let folder = sandbox.path("home/Library/LaunchAgents");
    let exe = "/Applications/Clak & Co.app/Contents/MacOS/ClakApp";
    let autostart = Autostart::with_folder(exe, &folder);
    assert_eq!(autostart.state(), AutostartState::Off);
    autostart.enable().unwrap();
    assert_eq!(autostart.state(), AutostartState::On);
    assert_eq!(autostart.registered().as_deref(), Some(exe));
    assert_eq!(
        Autostart::with_folder("/Altrove/Clak.app/Contents/MacOS/ClakApp", &folder).state(),
        AutostartState::Elsewhere
    );
    autostart.disable().unwrap();
    autostart.disable().unwrap();
    assert_eq!(autostart.state(), AutostartState::Off);
}

/// A real `…/X.app/Contents/MacOS` folder with clak and clak-shim inside.
fn app(sandbox: &Sandbox, name: &str) -> String {
    for tool in ["clak", "clak-shim"] {
        sandbox.write(&format!("{name}/Contents/MacOS/{tool}"), "#!/bin/sh\n");
    }
    sandbox
        .path(&format!("{name}/Contents/MacOS"))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn cio_che_indicava_un_app_sparita_si_sposta_su_questa() {
    let sandbox = Sandbox::new();
    let here = app(&sandbox, "nuova/Clak.app");
    let other = app(&sandbox, "altra/Clak.app");
    let gone = format!("{}/sparita/Clak.app/Contents/MacOS", sandbox.root.display());
    let bin = sandbox.dir(&["home", ".local", "bin"]);
    symlink(format!("{gone}/clak"), bin.join("clak")).unwrap();
    sandbox.write(
        "home/.claude/skills/clak/SKILL.md",
        &skill::mac_text(&format!("{gone}/clak")),
    );
    let cursor = sandbox.write(
        "home/.cursor/mcp.json",
        &format!("{{\"mcpServers\": {{\"clak\": {{\"command\": \"{gone}/clak-shim\"}}}}}}\n"),
    );
    // Another copy that still exists: the user decides which one to keep.
    let codex = sandbox.write(
        "home/.codex/config.toml",
        &format!("[mcp_servers.clak]\ncommand = \"{other}/clak-shim\"\n"),
    );

    let cli = Path::new(&here).join("clak");
    let shim = Path::new(&here).join("clak-shim");
    let done = installation::repair_gone(&sandbox.places, &cli, &shim);
    assert_eq!(
        done.keys().cloned().collect::<Vec<_>>(),
        ["client:Cursor", "path", "skill:Claude Code"]
    );
    assert!(
        done.values()
            .all(|v| v.starts_with("Updated: it pointed to an app that is gone")),
        "{done:?}"
    );
    assert_eq!(
        PathLink::user_bin(&sandbox.places, cli.to_string_lossy())
            .status()
            .state,
        PathLinkState::Ok
    );
    assert_eq!(
        Skill::claude_code(&sandbox.places, cli.to_string_lossy())
            .status()
            .state,
        skill::SkillState::Ok
    );
    assert_eq!(keys(&json(&cursor)["mcpServers"]), ["clak"]);
    assert_eq!(sandbox.client("Codex").status().state, ClientState::Stale);
    assert!(read(&codex).contains(&other));
    // Repaired once, there is nothing else to do.
    assert!(installation::repair_gone(&sandbox.places, &cli, &shim).is_empty());
}

#[test]
fn uno_shim_sparito_ma_di_altri_non_si_tocca() {
    let sandbox = Sandbox::new();
    let cursor = sandbox.write(
        "home/.cursor/mcp.json",
        &format!(
            "{{\"mcpServers\": {{\"clak\": {{\"command\": \"{}/sparito/altro-shim\"}}}}}}\n",
            sandbox.root.display()
        ),
    );
    let before = read(&cursor);
    let here = app(&sandbox, "nuova/Clak.app");
    assert!(
        installation::repair_gone(
            &sandbox.places,
            &Path::new(&here).join("clak"),
            &Path::new(&here).join("clak-shim")
        )
        .is_empty()
    );
    assert_eq!(read(&cursor), before);
}

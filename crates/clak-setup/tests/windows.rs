//! PATH, start with Windows, clak:// scheme and installation, on test keys
//! (`HKCU\Software\ClakTests\…`): the real ones are not touched.

#![cfg(windows)]

mod common;

use clak_setup::autostart::{Autostart, AutostartState};
use clak_setup::clients::{self, ClientState, McpClient};
use clak_setup::installation::{self, CLI, TRAY_APP};
use clak_setup::preferences::Preferences;
use clak_setup::registry::{self, EXPAND_STRING, STRING};
use clak_setup::shim;
use clak_setup::skill::{Skill, SkillState};
use clak_setup::uri_scheme::{UriScheme, UriSchemeState};
use clak_setup::user_path::UserPath;
use common::{Sandbox, json, keys, read};
use std::path::{Path, PathBuf};

const PARENT: &str = r"Software\ClakTests";

/// A test key of its own, removed at the end.
struct TestKey(String);

impl TestKey {
    fn new(kind: &str) -> TestKey {
        TestKey(format!(r"{PARENT}\{kind}-{}", clak_wire::json::random_hex(8)))
    }
}

impl Drop for TestKey {
    fn drop(&mut self) {
        let _ = registry::delete_tree(&self.0);
        registry::delete_if_empty(PARENT);
    }
}

fn raw_path(key: &str) -> Option<String> {
    registry::open(key, false)?.string(Some("Path"))
}

fn set(key: &str, name: Option<&str>, value: &str) {
    registry::create(key).unwrap().set_string(name, value, STRING).unwrap();
}

#[test]
fn il_path_si_aggiunge_una_volta_e_si_toglie_lasciando_il_resto_com_era() {
    let key = TestKey::new("Env");
    registry::create(&key.0).unwrap().set_string(Some("Path"), r"%USERPROFILE%\bin;C:\Strumenti;", EXPAND_STRING).unwrap();
    let path = UserPath::with_key(r"C:\Programmi\Clak", &key.0, false);
    assert!(!path.contains());

    path.add().unwrap();
    path.add().unwrap();
    assert!(path.contains());
    assert_eq!(raw_path(&key.0).unwrap(), r"%USERPROFILE%\bin;C:\Strumenti;C:\Programmi\Clak");
    // Variables stay variables.
    assert_eq!(registry::open(&key.0, false).unwrap().kind(Some("Path")), Some(EXPAND_STRING));

    assert!(UserPath::with_key(r"c:\programmi\clak\", &key.0, false).contains());
    path.remove().unwrap();
    assert!(!path.contains());
    assert_eq!(raw_path(&key.0).unwrap(), r"%USERPROFILE%\bin;C:\Strumenti");
}

#[test]
fn un_path_che_non_c_e_si_crea() {
    let key = TestKey::new("Env");
    let path = UserPath::with_key(r"C:\Programmi\Clak", &key.0, false);
    path.remove().unwrap();
    path.add().unwrap();
    assert_eq!(raw_path(&key.0).unwrap(), r"C:\Programmi\Clak");
}

#[test]
fn una_variabile_nel_path_vale_per_la_cartella_che_indica() {
    let key = TestKey::new("Env");
    let profile = std::env::var("USERPROFILE").unwrap();
    registry::create(&key.0).unwrap().set_string(Some("Path"), r"%USERPROFILE%\bin", EXPAND_STRING).unwrap();
    assert!(UserPath::with_key(format!(r"{profile}\bin"), &key.0, false).contains());
}

const EXE: &str = r"C:\Programmi\Clak\ClakApp.exe";

#[test]
fn l_avvio_si_accende_si_spegne_e_riconosce_gestione_attivita_e_le_altre_copie() {
    let root = TestKey::new("Win");
    let autostart = Autostart::with_root(EXE, &root.0);
    assert_eq!(autostart.state(), AutostartState::Off);
    autostart.enable().unwrap();
    assert_eq!(autostart.state(), AutostartState::On);
    let run = format!(r"{}\Run", root.0);
    assert_eq!(registry::open(&run, false).unwrap().string(Some(Autostart::NAME)).unwrap(), format!("\"{EXE}\""));
    assert_eq!(Autostart::with_root(r"D:\Altrove\ClakApp.exe", &root.0).state(), AutostartState::Elsewhere);

    // Task Manager turns it off without removing it; turning it back on from here clears that.
    let approved = format!(r"{}\Explorer\StartupApproved\Run", root.0);
    registry::create(&approved).unwrap().set_binary(Some(Autostart::NAME), &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap();
    assert_eq!(autostart.state(), AutostartState::DisabledBySystem);
    autostart.enable().unwrap();
    assert_eq!(autostart.state(), AutostartState::On);

    autostart.disable().unwrap();
    assert_eq!(autostart.state(), AutostartState::Off);
}

#[test]
fn lo_schema_clak_si_registra_si_aggiorna_e_non_tocca_quello_di_altri() {
    let root = TestKey::new("Win");
    let classes = format!(r"{}\Classes", root.0);
    let scheme = UriScheme::with_root(EXE, &classes);
    assert_eq!(scheme.state(), UriSchemeState::Off);
    scheme.register().unwrap();
    assert_eq!(scheme.state(), UriSchemeState::On);
    let command = registry::open(&format!(r"{classes}\clak\shell\open\command"), false).unwrap();
    assert_eq!(command.string(None).unwrap(), format!("\"{EXE}\" \"%1\""));
    assert_eq!(registry::open(&format!(r"{classes}\clak"), false).unwrap().string(Some("URL Protocol")).unwrap(), "");
    assert_eq!(UriScheme::with_root(r"D:\Altrove\ClakApp.exe", &classes).state(), UriSchemeState::Elsewhere);
    scheme.unregister().unwrap();
    assert_eq!(scheme.state(), UriSchemeState::Off);

    // Another program that already opens clak://: it is recognized, and not removed.
    set(&format!(r"{classes}\clak\shell\open\command"), None, r#""C:\Altro\altro.exe" "%1""#);
    assert_eq!(scheme.state(), UriSchemeState::Foreign);
    scheme.unregister().unwrap();
    let other = registry::open(&format!(r"{classes}\clak\shell\open\command"), false).unwrap();
    assert_eq!(other.string(None).unwrap(), r#""C:\Altro\altro.exe" "%1""#);
}

#[test]
fn lo_schema_con_la_nostra_descrizione_e_nostro_da_aggiornare() {
    let root = TestKey::new("Win");
    let classes = format!(r"{}\Classes", root.0);
    set(&format!(r"{classes}\clak"), None, "URL:Clak");
    set(&format!(r"{classes}\clak\shell\open\command"), None, r#""C:\Programmi\Altro\avvio.exe" "%1""#);
    let scheme = UriScheme::with_root(EXE, &classes);
    assert_eq!(scheme.state(), UriSchemeState::Elsewhere);
    scheme.register().unwrap();
    assert_eq!(scheme.state(), UriSchemeState::On);
}

#[test]
fn le_preferenze_hanno_un_valore_predefinito_e_lasciano_stare_il_resto_di_win_json() {
    let sandbox = Sandbox::new();
    let home = sandbox.dir(&["hub"]);
    std::fs::write(home.join("win.json"), "{\"shim\": [\"C:\\\\shim.exe\"]}").unwrap();
    let preferences = Preferences::new(&home);
    assert!(preferences.notifications());
    assert!(preferences.consent_sound());
    assert_eq!(preferences.autostart(), None);

    preferences.set_consent_sound(false).unwrap();
    preferences.set_autostart(Some(false)).unwrap();
    assert!(!Preferences::new(&home).consent_sound());
    assert_eq!(Preferences::new(&home).autostart(), Some(false));
    assert!(read(&home.join("win.json")).contains("shim.exe"));
    preferences.set_autostart(None).unwrap();
    assert!(!read(&home.join("win.json")).contains("autostart"));
}

#[test]
fn lo_shim_accanto_all_app_o_quello_ricordato() {
    let sandbox = Sandbox::new();
    let home = sandbox.dir(&["hub"]);
    let app = sandbox.dir(&["app"]);
    assert_eq!(shim::locate(&app, &home), None);
    let chosen = sandbox.write("altrove/clak-shim.exe", "");
    shim::remember(&home, &[chosen.to_string_lossy().into_owned(), "-v".into()]).unwrap();
    assert_eq!(shim::locate(&app, &home).unwrap()[1], "-v");
    let bundled = sandbox.write("app/clak-shim.exe", "");
    assert_eq!(shim::locate(&app, &home).unwrap(), [bundled.to_string_lossy().into_owned()]);
}

/// A fake machine with the test keys of an installation.
struct Machine {
    sandbox: Sandbox,
    env: TestKey,
    windows: TestKey,
}

impl Machine {
    fn new() -> Machine {
        Machine { sandbox: Sandbox::new(), env: TestKey::new("Env"), windows: TestKey::new("Win") }
    }

    fn autostart(&self, folder: &Path) -> Autostart {
        Autostart::with_root(folder.join(TRAY_APP), &self.windows.0)
    }

    fn scheme(&self, folder: &Path) -> UriScheme {
        UriScheme::with_root(folder.join(TRAY_APP), format!(r"{}\Classes", self.windows.0))
    }

    fn path(&self, folder: &Path) -> UserPath {
        UserPath::with_key(folder, &self.env.0, false)
    }

    fn preferences(&self) -> Preferences {
        Preferences::new(self.sandbox.dir(&["hub"]))
    }

    fn copy(&self, name: &str) -> PathBuf {
        let folder = self.sandbox.dir(&name.split('/').collect::<Vec<_>>());
        std::fs::write(folder.join(CLI), "").unwrap();
        std::fs::write(folder.join(TRAY_APP), "").unwrap();
        folder
    }

    fn setup(&self, folder: &Path) -> Vec<String> {
        installation::setup(
            folder,
            &self.sandbox.places,
            &self.path(folder),
            &self.autostart(folder),
            &self.scheme(folder),
            &self.preferences(),
        )
        .unwrap()
    }

    fn cleanup(&self, folder: &Path) -> Vec<String> {
        installation::cleanup(folder, &self.sandbox.places, &self.path(folder), &self.autostart(folder), &self.scheme(folder)).unwrap()
    }

    fn raw(&self) -> Option<String> {
        raw_path(&self.env.0)
    }

    fn set_path(&self, value: &str) {
        registry::create(&self.env.0).unwrap().set_string(Some("Path"), value, EXPAND_STRING).unwrap();
    }

    fn skill(&self, folder: &Path) -> Skill {
        Skill::claude_code(&self.sandbox.places, folder.join(CLI).to_string_lossy().into_owned())
    }

    fn client(&self, folder: &Path, name: &str) -> Box<dyn McpClient> {
        let shim = folder.join(shim::FILE_NAME).to_string_lossy().into_owned();
        clients::all(&self.sandbox.places, &[shim]).into_iter().find(|c| c.name() == name).unwrap()
    }
}

#[test]
fn installata_prende_il_posto_di_un_altra_copia_e_disinstallata_toglie_solo_il_suo() {
    let machine = Machine::new();
    let old = machine.copy("publish");
    let tools = machine.sandbox.dir(&["strumenti"]);
    machine.set_path(&format!("{};{}", tools.display(), old.display()));
    machine.sandbox.dir(&["home", ".claude"]);
    machine.skill(&old).install().unwrap();
    let cursor = machine.sandbox.write("home/.cursor/mcp.json", "{}");

    let installed = machine.copy("Programs/Clak");
    let done = machine.setup(&installed);
    assert!(done.iter().any(|l| l.starts_with("removed another copy from PATH")), "{done:?}");

    assert_eq!(machine.raw().unwrap(), format!("{};{}", tools.display(), installed.display()));
    assert_eq!(machine.scheme(&installed).state(), UriSchemeState::On);
    assert_eq!(machine.autostart(&installed).state(), AutostartState::On);
    let skill = machine.skill(&installed);
    assert_eq!(skill.status().state, SkillState::Ok);

    // The shim configured from the panel points to the installed copy: it goes away with it.
    let client = machine.client(&installed, "Cursor");
    client.add().unwrap();
    assert_eq!(client.status().state, ClientState::Ok);

    let done = machine.cleanup(&installed);
    assert!(done.contains(&"shim removed from Cursor".to_owned()), "{done:?}");
    assert_eq!(machine.raw().unwrap(), tools.display().to_string());
    assert_eq!(machine.scheme(&installed).state(), UriSchemeState::Off);
    assert_eq!(machine.autostart(&installed).state(), AutostartState::Off);
    assert!(!skill.path().exists());
    assert_eq!(client.status().state, ClientState::Absent);
    assert!(cursor.exists());
}

#[test]
fn dopo_un_installazione_tolta_tutto_cio_che_indicava_la_sua_cartella_passa_a_questa() {
    // A "Clak" folder that is gone, and its entries that remain.
    let machine = Machine::new();
    let gone = machine.sandbox.root.join("Tolta").join("Clak");
    let dev = machine.copy("sviluppo");
    let tools = machine.sandbox.dir(&["strumenti"]);
    machine.set_path(&format!("{};{};{}", tools.display(), gone.display(), dev.display()));
    let w = &machine.windows.0;
    set(&format!(r"{w}\Run"), Some(Autostart::NAME), &format!("\"{}\"", gone.join(TRAY_APP).display()));
    set(&format!(r"{w}\Classes\clak"), None, "URL:Clak");
    set(&format!(r"{w}\Classes\clak\shell\open\command"), None, &format!("\"{}\" \"%1\"", gone.join(TRAY_APP).display()));
    machine.sandbox.dir(&["home", ".claude"]);
    machine.skill(&gone).install().unwrap();
    let old_shim = gone.join(shim::FILE_NAME).to_string_lossy().replace('\\', "/");
    let cursor =
        machine.sandbox.write("home/.cursor/mcp.json", &format!("{{\"mcpServers\": {{\"clak\": {{\"command\": \"{old_shim}\"}}}}}}"));
    // An entry pointing to a shim that still exists (another copy) is left to the user.
    let dev_shim = dev.join(shim::FILE_NAME);
    std::fs::write(&dev_shim, "").unwrap();
    let dev_shim = dev_shim.to_string_lossy().replace('\\', "/");
    let gemini =
        machine.sandbox.write("home/.gemini/settings.json", &format!("{{\"mcpServers\": {{\"clak\": {{\"command\": \"{dev_shim}\"}}}}}}"));

    let installed = machine.copy("Programs/Clak");
    let done = machine.setup(&installed);

    assert_eq!(machine.raw().unwrap(), format!("{};{}", tools.display(), installed.display()));
    assert_eq!(machine.autostart(&installed).state(), AutostartState::On);
    assert_eq!(registry::open(&format!(r"{w}\Run"), false).unwrap().value_names(), [Autostart::NAME]);
    assert_eq!(machine.scheme(&installed).state(), UriSchemeState::On);
    assert_eq!(machine.skill(&installed).status().state, SkillState::Ok);

    assert!(done.contains(&"shim updated in Cursor".to_owned()), "{done:?}");
    assert_eq!(machine.client(&installed, "Cursor").status().state, ClientState::Ok);
    assert_eq!(keys(&json(&cursor)["mcpServers"]), ["clak"]);
    assert_eq!(machine.client(&installed, "Gemini CLI").status().state, ClientState::Stale);
    assert!(read(&gemini).contains(&dev_shim));
}

#[test]
fn disinstallata_non_tocca_cio_che_indica_un_altra_copia() {
    let machine = Machine::new();
    let other = machine.copy("altrove");
    machine.sandbox.dir(&["home", ".claude"]);
    let skill = machine.skill(&other);
    skill.install().unwrap();
    machine.set_path(&other.display().to_string());

    let installed = machine.copy("Programs/Clak");
    machine.cleanup(&installed);

    assert_eq!(machine.raw().unwrap(), other.display().to_string());
    assert_eq!(skill.status().state, SkillState::Ok);
}

#[test]
fn l_avvio_spento_dal_pannello_resta_spento_negli_aggiornamenti() {
    let machine = Machine::new();
    let installed = machine.copy("Programs/Clak");
    machine.preferences().set_autostart(Some(false)).unwrap();
    let done = machine.setup(&installed);
    assert!(done.contains(&"start with Windows left off, as chosen in the panel".to_owned()));
    assert_eq!(machine.autostart(&installed).state(), AutostartState::Off);

    machine.preferences().set_autostart(Some(true)).unwrap();
    machine.setup(&installed);
    assert_eq!(machine.autostart(&installed).state(), AutostartState::On);
}

#[test]
fn una_skill_scritta_da_altri_resta_e_senza_claude_code_non_se_ne_crea_una() {
    let machine = Machine::new();
    let installed = machine.copy("Programs/Clak");
    machine.setup(&installed);
    assert!(!machine.sandbox.places.home.join(".claude").exists());
    assert!(machine.path(&installed).contains());

    let foreign = machine.sandbox.write("home/.claude/skills/clak/SKILL.md", "---\nname: clak\ndescription: mia\n---\n");
    machine.setup(&installed);
    assert_eq!(read(&foreign), "---\nname: clak\ndescription: mia\n---\n");
}

#[test]
fn il_registro_dell_installazione_e_il_segno_di_riapertura() {
    let sandbox = Sandbox::new();
    let home = sandbox.dir(&["hub"]);
    installation::append_log(&home, false, &["added to PATH: x".into(), "skill installed: y".into()]);
    installation::append_log(&home, true, &["removed from PATH: x".into()]);
    let log = read(&installation::log_file(&home));
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 3);
    assert!(lines[0].ends_with(" install: added to PATH: x"), "{}", lines[0]);
    assert!(lines[2].ends_with(" uninstall: removed from PATH: x"));
    assert_eq!(lines[0].as_bytes()[4], b'-');

    assert!(!installation::take_reopen(&home));
    installation::mark_reopen(&home);
    assert!(installation::take_reopen(&home));
    assert!(!installation::take_reopen(&home));
}

#[test]
fn uno_shim_sparito_si_riconosce_dal_nome() {
    assert!(installation::is_gone_shim(r"C:\Non\Esiste\clak-shim.exe"));
    assert!(installation::is_gone_shim("C:/Non/Esiste/CLAK-SHIM.EXE"));
    assert!(!installation::is_gone_shim(r"C:\Non\Esiste\altro-shim.exe"));
    assert!(!installation::is_gone_shim("clak-shim.exe"));
}

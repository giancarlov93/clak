//! A backup made on one computer and restored on another, on a real hub: what comes back
//! with its secrets, what is checked against the executables here, what the user must
//! point to or change, what was already here.
//!
//! The two computers share the disk of the test (a temporary folder): "the other one" is a
//! hub folder of its own, and what isn't on it is a file removed or changed.

use clak_apis::{ApiAuth, ApiDraft, ApiEndpoint, ApiHub, ApiStore, AuthKind};
use clak_backup::{Backup, Check, Contents, Cost, Done, Lock, Plan, Settings, Targets};
use clak_connectors::launch::Launch;
use clak_connectors::{ConnectorHub, ConnectorStore};
use clak_hub::launch::{PresetProgram, Target};
use clak_hub::permissions::{AppPermissions, Entry};
use clak_hub::rules::{Rule, Rules};
use clak_hub::{AgentIdentity, Hub, HubOptions, Platform};
use clak_platform::{PlainBox, SecretBox};
use serde_json::{Map, Value, json};
use sha2::Digest as _;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PASSWORD: &str = "a long password";

/// The system, faked: a program whose name says "unsigned" isn't signed (it is known by
/// its content), the others are signed by "Tests".
struct FakePlatform;

impl Platform for FakePlatform {
    fn agent_of(
        &self,
        _pid: Option<u32>,
        _request: &Map<String, Value>,
        _authorized: &HashSet<String>,
    ) -> Option<AgentIdentity> {
        None
    }

    fn is_alive(&self, _agent: &AgentIdentity) -> bool {
        true
    }

    fn attest_loopback(&self, _peer_port: u16, _local_port: u16) -> Value {
        json!({})
    }

    fn executable_hash(&self, path: &str) -> Option<String> {
        let bytes = std::fs::read(path).ok()?;
        Some(hex::encode(sha2::Sha256::digest(bytes)))
    }

    fn origin_of(&self, exe: &str) -> Value {
        if exe.contains("unsigned") {
            json!({ "exe": exe, "signed": "none" })
        } else {
            json!({ "exe": exe, "signed": "valid", "signer": "Tests" })
        }
    }
}

fn secrets(home: &Path, file: &str) -> Arc<dyn SecretBox> {
    Arc::new(PlainBox::open(home.join(file)))
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "clak-backup-{name}-{}",
        clak_wire::json::random_hex(4)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A file standing for a program.
fn program(dir: &Path, name: &str, content: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, content).unwrap();
    text(&path)
}

fn app(exe: &str, name: &str) -> Entry {
    let origin = FakePlatform.origin_of(exe);
    Entry {
        key: AppPermissions::key_for(&FakePlatform, &origin, name).unwrap(),
        exe: exe.to_owned(),
        name: name.to_owned(),
        signer: origin["signer"].as_str().map(str::to_owned),
        since: 1.0,
        label: None,
        command: Some(name.to_lowercase()),
    }
}

fn rule(agent_exe: &str, folder: Option<&Path>, app_key: &str, label: &str) -> Rule {
    let agent = AgentIdentity {
        pid: 1,
        started: 1,
        started_at: None,
        exe: Some(agent_exe.to_owned()),
        name: "Claude Code".into(),
        chain: Vec::new(),
        signed: "valid".into(),
        signer: Some("Tests".into()),
    };
    Rule {
        id: clak_wire::json::new_id("rule"),
        agent_key: clak_hub::rules::agent_key(&FakePlatform, &agent).unwrap(),
        agent: "Claude Code".into(),
        exe: agent_exe.to_owned(),
        folder: folder.map(text),
        program: PresetProgram {
            target: Target::App(app_key.to_owned()),
            tools: None,
        },
        label: label.to_owned(),
        since: 2.0,
    }
}

fn endpoint() -> ApiEndpoint {
    ApiEndpoint {
        name: "list".into(),
        method: "GET".into(),
        path: "/items".into(),
        description: None,
        parameters: Vec::new(),
        body: None,
        body_schema: None,
        body_required: false,
    }
}

/// The first computer: everything set up, written in its folder as the hub writes it.
struct First {
    home: PathBuf,
    disk: PathBuf,
    rhino: Entry,
    tool: Entry,
    connectors: ConnectorStore,
    apis: ApiStore,
}

fn first() -> First {
    let disk = temp("disk");
    let home = disk.join("first");
    std::fs::create_dir_all(disk.join("work")).unwrap();
    let rhino = app(&program(&disk, "rhino.exe", "rhino 8"), "Rhino");
    let tool = app(&program(&disk, "unsigned-tool.exe", "tool 1"), "Tool");
    let agent = program(&disk, "claude.exe", "claude");
    let server = program(&disk, "server.js", "// a server");

    let mut apps = AppPermissions::load(home.join("apps.json"));
    std::fs::create_dir_all(&home).unwrap();
    assert!(apps.restore(rhino.clone()));
    assert!(apps.restore(tool.clone()));
    let mut rules = Rules::load(home.join("rules.json"));
    assert!(rules.restore(rule(&agent, Some(&disk.join("work")), &rhino.key, "Rhino")));
    assert!(rules.restore(rule(&agent, None, &tool.key, "Tool")));
    assert!(rules.restore(rule(&agent, Some(&disk.join("gone")), &rhino.key, "Rhino")));

    let connectors = ConnectorStore::new(&home, secrets(&home, "connectors.secrets"));
    connectors
        .add("Notion", "http://127.0.0.1:9/mcp", Some("tok-notion"), None)
        .unwrap();
    let node = std::env::current_exe().unwrap();
    let files = connectors
        .add_stdio(
            "Files",
            &Launch {
                program: text(&node),
                args: vec!["--serve".into(), server],
            },
            &BTreeMap::from([("API_KEY".into(), "k-files".into())]),
            None,
        )
        .unwrap();
    connectors
        .update(&clak_connectors::ConnectorEntry {
            enabled: false,
            ..files
        })
        .unwrap();
    connectors
        .add_stdio(
            "Elsewhere",
            &Launch {
                program: "/nowhere/at/all/tool".into(),
                args: Vec::new(),
            },
            &BTreeMap::new(),
            None,
        )
        .unwrap();

    let apis = ApiStore::new(&home, secrets(&home, "apis.secrets"));
    let draft = ApiDraft {
        name: "Shop".into(),
        base_url: "http://127.0.0.1:9/api".into(),
        auth: ApiAuth {
            kind: AuthKind::Bearer,
            ..ApiAuth::default()
        },
        secret: Some("s-shop".into()),
        endpoints: vec![endpoint()],
        ..ApiDraft::default()
    };
    apis.add(draft.into_entry("api_1".into(), 3.0), Some("s-shop"))
        .unwrap();

    let preset = |name: &str, folder: &Path| {
        let mut p = Map::new();
        p.insert("name".into(), json!(name));
        p.insert("agent".into(), json!("claude-code"));
        p.insert("folder".into(), json!(text(folder)));
        p.insert(
            "programs".into(),
            json!([{ "app": tool.key, "label": "Tool" }]),
        );
        p
    };
    clak_setup::presets::save(&home, preset("Work", &disk.join("work"))).unwrap();
    clak_setup::presets::save(&home, preset("Gone", &disk.join("gone"))).unwrap();

    First {
        home,
        disk,
        rhino,
        tool,
        connectors,
        apis,
    }
}

/// The second computer: a hub of its own, running.
struct Second {
    home: PathBuf,
    hub: Option<Hub>,
    connectors: Arc<ConnectorHub>,
    apis: Arc<ApiHub>,
}

impl Second {
    async fn start(disk: &Path) -> Second {
        let home = disk.join("second");
        let mut options = HubOptions::new(Arc::new(FakePlatform));
        options.settings = false;
        let hub = Hub::try_start(&home, options, None)
            .await
            .unwrap()
            .expect("a hub already running");
        let broker = hub.broker().clone();
        let connectors = ConnectorHub::new(
            broker.clone(),
            Arc::new(ConnectorStore::new(
                &home,
                secrets(&home, "connectors.secrets"),
            )),
            Default::default(),
        );
        let apis = ApiHub::new(
            broker,
            Arc::new(ApiStore::new(&home, secrets(&home, "apis.secrets"))),
            clak_apis::rest::new_http_client().unwrap(),
        );
        Second {
            home,
            hub: Some(hub),
            connectors,
            apis,
        }
    }

    fn targets(&self) -> Targets<'_> {
        Targets {
            home: &self.home,
            broker: self.hub.as_ref().unwrap().broker(),
            connectors: &self.connectors,
            apis: Some(&self.apis),
        }
    }

    fn plan(&self, contents: Contents) -> Plan {
        let here = self
            .targets()
            .here(std::env::var_os("PATH").unwrap_or_default());
        Plan::check(clak_backup::Header::here(), contents, &here, &FakePlatform)
    }

    async fn stop(mut self) {
        self.connectors.stop("test over").await;
        self.apis.stop().await;
        self.hub.take().unwrap().stop().await;
    }
}

/// The first computer's backup, written by `back_up` and read back with the password.
fn backed_up(first: &First) -> Contents {
    let mut settings = Settings {
        enabled: true,
        folder: Some(first.disk.join("backups")),
        keep: 3,
        ..Settings::default()
    };
    settings.set_lock(Lock::new(PASSWORD, Cost::CHEAP).unwrap());
    settings.save(&first.home).unwrap();
    let Done::Written(path) =
        clak_backup::back_up(&first.home, &first.connectors, &first.apis, false).unwrap()
    else {
        panic!("nothing written");
    };
    // Nothing changed: no second file.
    assert_eq!(
        clak_backup::back_up(&first.home, &first.connectors, &first.apis, false).unwrap(),
        Done::Unchanged
    );
    let bytes = std::fs::read(&path).unwrap();
    let raw = String::from_utf8_lossy(&bytes);
    for secret in ["tok-notion", "k-files", "s-shop", "Rhino"] {
        assert!(!raw.contains(secret), "{secret} in clear in the backup");
    }
    let backup = Backup::decode(&bytes).unwrap();
    assert!(backup.open("not the password").is_err());
    let contents = backup.open(PASSWORD).unwrap();
    assert_eq!(
        contents,
        Contents::collect(&first.home, &first.connectors, &first.apis)
    );
    contents
}

fn state<'a>(plan: &'a Plan, name: &str, kind: clak_backup::Kind) -> &'a clak_backup::Item {
    plan.items
        .iter()
        .find(|i| i.name == name && i.kind == kind)
        .unwrap_or_else(|| panic!("no {name}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backup_comes_back_on_another_computer_after_its_checks() {
    use clak_backup::Kind::*;
    let first = first();
    let contents = backed_up(&first);
    assert_eq!(contents.connectors.len(), 3);
    assert_eq!(contents.rules.len(), 3);

    // On the other computer the unsigned tool is another version, and a folder is missing.
    std::fs::write(&first.tool.exe, "tool 2").unwrap();
    let second = Second::start(&first.disk).await;
    // A server already there, without its token.
    second
        .connectors
        .add("Notion here", "http://127.0.0.1:9/mcp", None, None)
        .unwrap();

    let mut plan = second.plan(contents);
    assert_eq!(state(&plan, "Rhino", App).check, Check::Ready);
    let tool = state(&plan, "Tool", App).clone();
    assert!(
        matches!(&tool.check, Check::Blocked(why) if why.contains("another version")),
        "{:?}",
        tool.check
    );
    assert_eq!(state(&plan, "Notion", Connector).check, Check::Secret);
    assert_eq!(state(&plan, "Files", Connector).check, Check::Ready);
    assert!(matches!(
        &state(&plan, "Elsewhere", Connector).check,
        Check::Warn(why) if why.contains("/nowhere/at/all/tool")
    ));
    assert_eq!(state(&plan, "Shop", Api).check, Check::Ready);
    assert_eq!(state(&plan, "Work", Preset).check, Check::Ready);
    assert!(matches!(state(&plan, "Gone", Preset).check, Check::Warn(_)));
    let rules: Vec<_> = plan.items.iter().filter(|i| i.kind == Rule).collect();
    assert_eq!(rules[0].check, Check::Ready);
    assert!(
        matches!(plan.effective(rules[1]), Check::Blocked(why) if why.contains("“Tool”")),
        "a rule follows its application"
    );
    assert!(matches!(&rules[2].check, Check::Blocked(why) if why.contains("gone")));

    // The panel's view has no secret, no key.
    let view = clak_wire::json::text(&plan.view());
    for secret in ["tok-notion", "k-files", "s-shop", "firma:"] {
        assert!(!view.contains(secret), "{secret} in the view");
    }
    assert!(view.contains("\"locate\":true"));

    // The user points the tool to where it is now: a new key, and its rule can come along.
    let moved = program(&first.disk, "unsigned-tool-2.exe", "tool 2");
    let here = second.targets().here(Default::default());
    let located = plan
        .locate(&tool.id, &moved, &here, &FakePlatform)
        .unwrap()
        .clone();
    assert_eq!(located.check, Check::Ready);
    assert!(located.chosen);
    let new_key = located.moved.as_ref().unwrap().0.clone();
    assert_ne!(new_key, first.tool.key);
    assert_eq!(plan.effective(rules_of(&plan)[1]), Check::Ready);

    // Everything ticked, the blocked ones too: they are skipped all the same.
    let all: HashSet<String> = plan.items.iter().map(|i| i.id.clone()).collect();
    let outcomes = clak_backup::restore(&plan, &all, &second.targets()).await;
    let said = |name: &str, kind: clak_backup::Kind| {
        outcomes
            .iter()
            .find(|o| o.name == name && o.kind == kind)
            .map(|o| (o.ok, o.message.clone()))
    };
    assert_eq!(said("Rhino", App), Some((true, "Allowed again.".into())));
    assert_eq!(said("Tool", App), Some((true, "Allowed again.".into())));
    assert_eq!(
        said("Notion", Connector),
        Some((true, "Its token restored.".into()))
    );
    assert_eq!(
        said("Gone", Preset).unwrap().1,
        "Restored: change its folder."
    );
    assert_eq!(
        outcomes.iter().filter(|o| o.kind == Rule).count(),
        2,
        "the rule without its folder isn't tried: {outcomes:?}"
    );
    assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");

    // What the second computer has now.
    let broker = second.hub.as_ref().unwrap().broker().clone();
    let (apps, rules) = broker.run(|c| (c.permissions().all(), c.rules.all().to_vec()));
    let keys: Vec<&str> = apps.iter().map(|a| a.key.as_str()).collect();
    assert_eq!(keys, vec![first.rhino.key.as_str(), new_key.as_str()]);
    assert_eq!(apps[1].exe, moved);
    assert_eq!(apps[1].command.as_deref(), Some("tool"));
    assert_eq!(rules.len(), 2);
    assert_eq!(rules[1].program.target, Target::App(new_key.clone()));

    let store = ConnectorStore::new(&second.home, secrets(&second.home, "connectors.secrets"));
    let entries = store.all();
    let notion = entries.iter().find(|e| e.name == "Notion here").unwrap();
    assert_eq!(store.token(&notion.id).as_deref(), Some("tok-notion"));
    assert!(!entries.iter().any(|e| e.name == "Notion"), "no duplicate");
    let files = entries.iter().find(|e| e.name == "Files").unwrap();
    assert!(!files.enabled, "turned off, as it was");
    assert_eq!(store.env(&files.id)["API_KEY"], "k-files");
    let elsewhere = entries.iter().find(|e| e.name == "Elsewhere").unwrap();
    assert!(!elsewhere.enabled, "its program isn't here: off");

    let apis = ApiStore::new(&second.home, secrets(&second.home, "apis.secrets"));
    let shop = apis.all().into_iter().find(|a| a.name == "Shop").unwrap();
    assert_eq!(apis.secret(&shop.id).as_deref(), Some("s-shop"));

    let presets = clak_setup::presets::all(&second.home);
    assert_eq!(presets.len(), 2);
    assert_eq!(presets[0]["programs"][0]["app"], json!(new_key));

    // Restored again: everything is already here.
    let again = second.plan(Contents::collect(
        &first.home,
        &first.connectors,
        &first.apis,
    ));
    for item in &again.items {
        let check = again.effective(item);
        assert!(
            !matches!(check, Check::Ready | Check::Secret),
            "{} again: {check:?}",
            item.name,
        );
    }

    second.stop().await;
    std::fs::remove_dir_all(&first.disk).ok();
}

fn rules_of(plan: &Plan) -> Vec<&clak_backup::Item> {
    plan.items
        .iter()
        .filter(|i| i.kind == clak_backup::Kind::Rule)
        .collect()
}

#[test]
fn what_changes_by_itself_does_not_make_a_new_backup() {
    let first = first();
    let key = [7u8; 32];
    let before = Contents::collect(&first.home, &first.connectors, &first.apis);
    let mut refreshed = before.clone();
    let grant = json!({
        "issuer": "https://auth.example.com", "authorization_endpoint": "https://auth.example.com/a",
        "token_endpoint": "https://auth.example.com/t", "client_id": "c",
        "redirect_uri": "http://127.0.0.1:1/callback", "resource": "https://mcp.example.com",
        "access_token": "a1", "refresh_token": "r1", "expires_at": 10.0,
    });
    refreshed.connectors[0].oauth = clak_connectors::OAuthGrant::from_json(&grant);
    assert!(refreshed.connectors[0].oauth.is_some());
    assert_ne!(before.fingerprint(&key), refreshed.fingerprint(&key));
    let mut later = refreshed.clone();
    let mut grant2 = grant.clone();
    grant2["access_token"] = json!("a2");
    grant2["expires_at"] = json!(3610.0);
    later.connectors[0].oauth = clak_connectors::OAuthGrant::from_json(&grant2);
    assert_eq!(refreshed.fingerprint(&key), later.fingerprint(&key));
    let mut rotated = grant2.clone();
    rotated["refresh_token"] = json!("r2");
    later.connectors[0].oauth = clak_connectors::OAuthGrant::from_json(&rotated);
    assert_ne!(refreshed.fingerprint(&key), later.fingerprint(&key));
    assert_ne!(before.fingerprint(&key), before.fingerprint(&[8u8; 32]));
    std::fs::remove_dir_all(&first.disk).ok();
}

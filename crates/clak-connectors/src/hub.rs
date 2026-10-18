//! The hub's connectors: they start with it, and the panel adds, probes, changes the
//! token or redoes the sign-in, turns off and removes them. The panel does it, in the same
//! process as the hub, or an agent through the settings (`clak settings`), with every
//! change confirmed by the user; only the panel opens an OAuth sign-in. A server can
//! also be a program on this computer (stdio), which the connector starts.
//!
//! The panel sees them in the hub's snapshot: on every change this object rebuilds their
//! view and hands it to the broker ([`Core::set_connectors`]). The token is never there:
//! only whether there is one.
//!
//! [`Core::set_connectors`]: clak_hub::Core::set_connectors

use crate::connector::{self, ConnectorProbe, same_origin, same_url};
use crate::launch::Launch;
use crate::live::{Changed, Connector, Row, phase};
use crate::mcp_http::{AccessToken, StaticToken, new_http_client};
use crate::oauth::{
    self, OAuthClient, OAuthError, OAuthGrant, OpenBrowser, SIGN_IN_TIMEOUT, Status,
};
use crate::store::{ConnectorEntry, ConnectorStore};
use crate::token::OAuthToken;
use clak_hub::texts::clean;
use clak_hub::{Broker, ConnectorTiming, Core, cli_names};
use clak_wire::json::float;
use reqwest::Client;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

/// A new credential for [`ConnectorHub::edit`]: a pasted token (or none: an empty `token`
/// removes it), or an OAuth sign-in just done.
#[derive(Debug, Clone, Default)]
pub struct ConnectorCredential {
    pub token: Option<String>,
    pub grant: Option<OAuthGrant>,
}

struct Inner {
    entries: Vec<ConnectorEntry>,
    running: HashMap<String, Arc<Connector>>,
}

pub struct ConnectorHub {
    broker: Broker,
    store: Arc<ConnectorStore>,
    timing: ConnectorTiming,
    http: Client,
    inner: Mutex<Inner>,
    open_browser: Mutex<OpenBrowser>,
    sign_in_timeout: Mutex<Duration>,
    /// One view at a time: two close changes don't deliver the old view last.
    view: Mutex<()>,
    me: Weak<ConnectorHub>,
}

impl ConnectorHub {
    pub fn new(
        broker: Broker,
        store: Arc<ConnectorStore>,
        timing: ConnectorTiming,
    ) -> Arc<ConnectorHub> {
        let entries = store.all();
        let hub = Arc::new_cyclic(|me| ConnectorHub {
            broker,
            store,
            timing,
            http: new_http_client(),
            inner: Mutex::new(Inner {
                entries,
                running: HashMap::new(),
            }),
            open_browser: Mutex::new(system_browser()),
            sign_in_timeout: Mutex::new(SIGN_IN_TIMEOUT),
            view: Mutex::new(()),
            me: me.clone(),
        });
        hub.push_view();
        hub
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// How the sign-in page is opened: by default the default browser; the app provides its
    /// own, the tests one that follows the page by itself.
    pub fn set_open_browser(&self, open: OpenBrowser) {
        *self.open_browser.lock().unwrap_or_else(|p| p.into_inner()) = open;
    }

    /// How long to wait for the user to finish signing in in the browser (5 minutes).
    pub fn set_sign_in_timeout(&self, timeout: Duration) {
        *self
            .sign_in_timeout
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = timeout;
    }

    pub fn start_all(&self) {
        let enabled: Vec<ConnectorEntry> = self
            .lock()
            .entries
            .iter()
            .filter(|e| e.enabled)
            .cloned()
            .collect();
        for entry in enabled {
            self.launch(entry);
        }
    }

    pub fn entries(&self) -> Vec<ConnectorEntry> {
        self.lock().entries.clone()
    }

    pub fn get(&self, id: &str) -> Option<ConnectorEntry> {
        self.lock().entries.iter().find(|e| e.id == id).cloned()
    }

    /// True if the connector has a token or a saved sign-in (which stays here: we only know it exists).
    pub fn has_credential(&self, id: &str) -> bool {
        self.store
            .get(id)
            .is_some_and(|e| self.stored_credential(&e))
    }

    fn stored_credential(&self, entry: &ConnectorEntry) -> bool {
        if entry.is_oauth() {
            self.store.oauth(&entry.id).is_some()
        } else {
            self.store.token(&entry.id).is_some()
        }
    }

    /// A connector with the same address, if there is one already: then its token is updated, no duplicate is created.
    pub fn find_by_url(&self, url: &str) -> Option<ConnectorEntry> {
        self.lock()
            .entries
            .iter()
            .find(|e| same_url(&e.url, url))
            .cloned()
    }

    /// A connector that starts the same program with the same arguments, if there is one already.
    pub fn find_by_launch(&self, launch: &Launch) -> Option<ConnectorEntry> {
        self.lock()
            .entries
            .iter()
            .find(|e| e.launch.as_ref() == Some(launch))
            .cloned()
    }

    /// A program's saved environment variables (see [`ConnectorStore::env`]).
    pub fn env(&self, id: &str) -> BTreeMap<String, String> {
        self.store.env(id)
    }

    /// What's wrong with a name and a program's environment variables, or `None`.
    pub fn problem_stdio(name: Option<&str>, env: &BTreeMap<String, String>) -> Option<String> {
        connector::problem_stdio(name, env)
    }

    /// Probes a program before saving it (see [`connector::probe_stdio`]).
    pub async fn probe_stdio(launch: &Launch, env: &BTreeMap<String, String>) -> ConnectorProbe {
        connector::probe_stdio(launch, env).await
    }

    /// What's wrong with a name and an address, or `None` if they're fine.
    pub fn problem(name: Option<&str>, url: Option<&str>) -> Option<String> {
        connector::problem(name, url)
    }

    /// An `http` address to another computer: token and data travel in the clear.
    pub fn unencrypted(url: &str) -> bool {
        connector::unencrypted(url)
    }

    /// Probes a server before saving it (see [`connector::probe`]).
    pub async fn probe(url: &str, token: Option<&str>) -> ConnectorProbe {
        connector::probe(url, token).await
    }

    /// The OAuth sign-in to a server, in the user's browser: for a new connector, or to redo
    /// it on an existing one (`id`, whose registered client is reused). `Ok(None)` if the
    /// server asks for no sign-in. Saves nothing: [`ConnectorHub::add_oauth`] or
    /// [`ConnectorHub::set_oauth`] does, only if it succeeded. `cancel` cancels it (from the
    /// panel): then the error has `canceled`.
    pub async fn sign_in(
        &self,
        url: &str,
        client: Option<&OAuthClient>,
        id: Option<&str>,
        status: Status<'_>,
        cancel: &CancellationToken,
    ) -> Result<Option<OAuthGrant>, OAuthError> {
        let previous = id.and_then(|id| self.store.oauth(id));
        let open = self
            .open_browser
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let timeout = *self
            .sign_in_timeout
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        tokio::select! {
            _ = cancel.cancelled() => Err(OAuthError::canceled()),
            result = connector::sign_in(&self.http, url, client, previous.as_ref(), &open, status, timeout) => result,
        }
    }

    pub fn add(
        &self,
        name: &str,
        url: &str,
        token: Option<&str>,
        command: Option<&str>,
    ) -> Result<ConnectorEntry, String> {
        let command = command_of(command);
        if let Some(problem) = connector::problem(Some(name), Some(url))
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(problem);
        }
        let entry = self
            .store
            .add(
                &clean(Some(name), 40),
                url.trim(),
                token,
                command.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        Ok(self.added(entry, String::new()))
    }

    /// A connector with the OAuth sign-in just done.
    pub fn add_oauth(
        &self,
        name: &str,
        url: &str,
        grant: &OAuthGrant,
        command: Option<&str>,
    ) -> Result<ConnectorEntry, String> {
        let command = command_of(command);
        if let Some(problem) = connector::problem(Some(name), Some(url))
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(problem);
        }
        let entry = self
            .store
            .add_oauth(
                &clean(Some(name), 40),
                url.trim(),
                grant,
                command.as_deref(),
            )
            .map_err(|e| e.to_string())?;
        Ok(self.added(entry, format!(", signed in at {}", grant.host())))
    }

    /// A connector that is a program on this computer: the command line and its environment
    /// variables.
    pub fn add_stdio(
        &self,
        name: &str,
        launch: &Launch,
        env: &BTreeMap<String, String>,
        command: Option<&str>,
    ) -> Result<ConnectorEntry, String> {
        let command = command_of(command);
        if let Some(problem) = connector::problem_stdio(Some(name), env)
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(problem);
        }
        let entry = self
            .store
            .add_stdio(&clean(Some(name), 40), launch, env, command.as_deref())
            .map_err(|e| e.to_string())?;
        let how = if env.is_empty() {
            String::new()
        } else {
            format!(", environment {}", names(env))
        };
        Ok(self.added(entry, how))
    }

    fn added(&self, entry: ConnectorEntry, how: String) -> ConnectorEntry {
        self.lock().entries.push(entry.clone());
        let unencrypted = if !entry.is_stdio() && connector::unencrypted(&entry.url) {
            ", unencrypted"
        } else {
            ""
        };
        let line = format!(
            "connector added: {} ({}{unencrypted}{how})",
            entry.name,
            entry.target()
        );
        self.broker.run(|c| c.log(line));
        self.launch(entry.clone());
        entry
    }

    /// Changes a connector's name, command and address, and if needed its credential
    /// (`credential`: `None` leaves it as is).
    ///
    /// Name and command only: it's the same server, the permissions stay. A new address is
    /// another server: the connector restarts and the permissions start over; and a token or
    /// a sign-in made for another host doesn't follow it, because it would end up with someone
    /// who never received it.
    pub async fn edit(
        &self,
        id: &str,
        name: &str,
        command: Option<&str>,
        url: &str,
        credential: Option<ConnectorCredential>,
    ) -> Result<ConnectorEntry, String> {
        let command = command_of(command);
        if let Some(problem) = connector::problem(Some(name), Some(url))
            .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(problem);
        }
        let old = self.get(id).ok_or("This connector no longer exists.")?;
        if old.is_stdio() {
            return Err(
                "This connector runs a program on this computer: it has no address.".into(),
            );
        }
        let mut entry = ConnectorEntry {
            name: clean(Some(name), 40),
            command: command.clone(),
            url: url.trim().to_owned(),
            ..old.clone()
        };
        let moved = !same_url(&old.url, &entry.url);
        let mut credential = credential;
        if credential.is_none() && !same_origin(&old.url, &entry.url) {
            credential = Some(ConnectorCredential::default());
        }

        let mut revoke = if credential.is_some() {
            self.store.oauth(id)
        } else {
            None
        };
        self.store.update(&entry).map_err(|e| e.to_string())?;
        match &credential {
            Some(ConnectorCredential {
                grant: Some(grant), ..
            }) => self.store.set_oauth(id, grant).map_err(|e| e.to_string())?,
            Some(c) => self
                .store
                .set_token(id, c.token.as_deref())
                .map_err(|e| e.to_string())?,
            None => {}
        }
        if credential.as_ref().is_some_and(|c| c.grant.is_some()) {
            revoke = None; // the new sign-in reuses the client: don't revoke the one just made
        }
        entry = self.store.get(id).unwrap_or(entry);
        self.replace(&entry);

        let mut what = Vec::new();
        if old.name != entry.name {
            what.push(format!("renamed “{}”", entry.name));
        }
        if old.command != entry.command {
            what.push(match &entry.command {
                None => "automatic command".to_owned(),
                Some(c) => format!("command “{c}”"),
            });
        }
        if moved {
            what.push(format!("address {}", entry.url));
        }
        if let Some(c) = &credential {
            what.push(match &c.grant {
                Some(g) => format!("signed in at {}", g.host()),
                None if c.token.as_deref().is_none_or(str::is_empty) => "token removed".to_owned(),
                None => "token changed".to_owned(),
            });
        }
        let what = what.join(", ");
        if !what.is_empty() {
            let line = format!("connector {}: {what}", old.name);
            self.broker.run(|c| c.log(line));
        }

        let running = self.lock().running.get(id).cloned();
        if moved {
            self.halt(id, "connector address changed").await;
            if entry.enabled {
                self.launch(entry.clone());
            }
        } else if credential.is_some() {
            if let Some(running) = running {
                running.set_credential(entry.clone(), self.credential(&entry), &what);
            }
        } else if let Some(running) = running {
            running.rename(entry.clone());
        }
        if let Some(grant) = revoke {
            self.revoke(grant);
        }
        self.push_view();
        Ok(entry)
    }

    /// Changes a program's name, command, command line and environment variables (`env`:
    /// `None` leaves them as they are).
    ///
    /// Name and command only: it's the same server, the permissions stay. Another command
    /// line is another program, and new variables count as a new token: the program
    /// restarts and the permissions start over.
    pub async fn edit_stdio(
        &self,
        id: &str,
        name: &str,
        command: Option<&str>,
        launch: &Launch,
        env: Option<BTreeMap<String, String>>,
    ) -> Result<ConnectorEntry, String> {
        let command = command_of(command);
        let old = self.get(id).ok_or("This connector no longer exists.")?;
        if !old.is_stdio() {
            return Err(
                "This connector is a server at an address: it doesn't run a program.".into(),
            );
        }
        let old_env = self.store.env(id);
        let env = env.filter(|e| *e != old_env);
        if let Some(problem) =
            connector::problem_stdio(Some(name), env.as_ref().unwrap_or(&old_env))
                .or_else(|| cli_names::problem(command.as_deref()))
        {
            return Err(problem);
        }
        let entry = ConnectorEntry {
            name: clean(Some(name), 40),
            command: command.clone(),
            launch: Some(launch.clone()),
            ..old.clone()
        };
        let moved = old.launch != entry.launch;
        self.store.update(&entry).map_err(|e| e.to_string())?;
        if let Some(env) = &env {
            self.store.set_env(id, env).map_err(|e| e.to_string())?;
        }
        let entry = self.store.get(id).unwrap_or(entry);
        self.replace(&entry);

        let mut what = Vec::new();
        if old.name != entry.name {
            what.push(format!("renamed “{}”", entry.name));
        }
        if old.command != entry.command {
            what.push(match &entry.command {
                None => "automatic command".to_owned(),
                Some(c) => format!("command “{c}”"),
            });
        }
        if moved {
            what.push(format!("program {}", launch.display()));
        }
        if let Some(env) = &env {
            what.push(if env.is_empty() {
                "environment removed".to_owned()
            } else {
                format!("environment {}", names(env))
            });
        }
        let what = what.join(", ");
        if !what.is_empty() {
            let line = format!("connector {}: {what}", old.name);
            self.broker.run(|c| c.log(line));
        }

        let running = self.lock().running.get(id).cloned();
        if moved {
            self.halt(id, "connector program changed").await;
            if entry.enabled {
                self.launch(entry.clone());
            }
        } else if let (Some(env), Some(running)) = (env, &running) {
            running.set_env(entry.clone(), env, &what);
        } else if let Some(running) = running {
            running.rename(entry.clone());
        }
        self.push_view();
        Ok(entry)
    }

    /// A new token (or none). Whoever had permission to use the connector must ask again.
    pub fn set_token(&self, id: &str, token: Option<&str>) -> Result<(), String> {
        let old = self.store.oauth(id);
        self.store.set_token(id, token).map_err(|e| e.to_string())?;
        let what = if token.is_none_or(str::is_empty) {
            "token removed"
        } else {
            "token changed"
        };
        self.changed(id, what);
        if let Some(old) = old {
            self.revoke(old);
        }
        Ok(())
    }

    /// A new OAuth sign-in. As with a new token, the permissions start over.
    pub fn set_oauth(&self, id: &str, grant: &OAuthGrant) -> Result<(), String> {
        self.store.set_oauth(id, grant).map_err(|e| e.to_string())?;
        self.changed(id, &format!("signed in at {}", grant.host()));
        Ok(())
    }

    fn changed(&self, id: &str, what: &str) {
        let entry = self.store.get(id);
        if let Some(entry) = &entry {
            self.replace(entry);
        }
        let running = self.lock().running.get(id).cloned();
        let line = format!("connector {}: {what}", self.name_of(id));
        self.broker.run(|c| c.log(line));
        if let (Some(entry), Some(running)) = (entry, running) {
            running.set_credential(entry.clone(), self.credential(&entry), what);
        }
        self.push_view();
    }

    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let entry = {
            let mut inner = self.lock();
            let Some(entry) = inner.entries.iter_mut().find(|e| e.id == id) else {
                return Ok(());
            };
            if entry.enabled == enabled {
                return Ok(());
            }
            entry.enabled = enabled;
            entry.clone()
        };
        self.store.update(&entry).map_err(|e| e.to_string())?;
        let line = format!(
            "connector {}: {}",
            entry.name,
            if enabled { "on" } else { "off" }
        );
        self.broker.run(|c| c.log(line));
        if enabled {
            self.launch(entry);
        } else {
            self.halt(id, "connector turned off").await;
        }
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let name = self.name_of(id);
        self.halt(id, "connector removed").await;
        let grant = self.store.oauth(id);
        self.store.remove(id).map_err(|e| e.to_string())?;
        self.lock().entries.retain(|e| e.id != id);
        let line = format!("connector removed: {name}");
        self.broker.run(|c| {
            c.log(line);
            c.touch();
        });
        if let Some(grant) = grant {
            self.revoke(grant);
        }
        self.push_view();
        Ok(())
    }

    /// Stops all connectors (the hub is closing).
    pub async fn stop(&self, reason: &str) {
        let all: Vec<Arc<Connector>> = self.lock().running.drain().map(|(_, c)| c).collect();
        for connector in all {
            connector.stop(reason).await;
        }
        self.push_view();
    }

    /// Revokes a sign-in no longer needed, without waiting for it: it's a courtesy to the service.
    fn revoke(&self, grant: OAuthGrant) {
        let http = self.http.clone();
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(Duration::from_secs(10), oauth::revoke(&http, &grant)).await;
        });
    }

    fn replace(&self, entry: &ConnectorEntry) {
        let mut inner = self.lock();
        if let Some(known) = inner.entries.iter_mut().find(|e| e.id == entry.id) {
            *known = entry.clone();
        }
    }

    fn name_of(&self, id: &str) -> String {
        self.get(id)
            .map(|e| e.name)
            .unwrap_or_else(|| id.to_owned())
    }

    /// How the connector presents itself: the pasted token, the OAuth sign-in (saved on every refresh), or nothing.
    fn credential(&self, entry: &ConnectorEntry) -> Option<Arc<dyn AccessToken>> {
        if !entry.is_oauth() {
            return self
                .store
                .token(&entry.id)
                .map(|t| Arc::new(StaticToken::new(t)) as Arc<dyn AccessToken>);
        }
        let grant = self.store.oauth(&entry.id)?;
        let (store, id) = (self.store.clone(), entry.id.clone());
        Some(Arc::new(OAuthToken::new(
            self.http.clone(),
            grant,
            Box::new(move |renewed| {
                let _ = store.set_oauth(&id, renewed);
            }),
        )))
    }

    fn launch(&self, entry: ConnectorEntry) {
        if self.lock().running.contains_key(&entry.id) {
            return;
        }
        let me = self.me.clone();
        let changed: Changed = Arc::new(move || {
            if let Some(hub) = me.upgrade() {
                hub.push_view();
            }
        });
        let env = if entry.is_stdio() {
            self.store.env(&entry.id)
        } else {
            BTreeMap::new()
        };
        let connector = Connector::new(
            self.broker.clone(),
            entry.clone(),
            self.credential(&entry),
            env,
            self.http.clone(),
            self.timing,
            changed,
        );
        {
            let mut inner = self.lock();
            if inner.running.contains_key(&entry.id) {
                return;
            }
            inner.running.insert(entry.id.clone(), connector.clone());
        }
        connector.start();
        self.push_view();
    }

    async fn halt(&self, id: &str, reason: &str) {
        let running = self.lock().running.remove(id);
        if let Some(running) = running {
            running.stop(reason).await;
        }
        self.push_view();
    }

    /// The connectors as the panel sees them, rebuilt now: the same JSON the broker receives on
    /// every change. The token isn't there: only whether there is one.
    pub fn view(&self) -> Value {
        let rows = self.rows();
        self.broker.run(|c| compose(&rows, c))
    }

    /// The panel's view, rebuilt and handed to the broker. One at a time: two close changes
    /// don't deliver the old view last.
    fn push_view(&self) {
        let _one = self.view.lock().unwrap_or_else(|p| p.into_inner());
        let rows = self.rows();
        self.broker.run(|c| {
            let view = compose(&rows, c);
            c.set_connectors(view);
        });
    }

    /// The connectors' state, read outside the broker's lock (the secret store may take its time)
    /// and outside the hub's lock; inside the broker only the program names are read.
    fn rows(&self) -> Vec<ViewRow> {
        let (entries, running) = {
            let inner = self.lock();
            (inner.entries.clone(), inner.running.clone())
        };
        entries
            .into_iter()
            .map(|e| {
                let live = running.get(&e.id).map(|c| c.row());
                let has_token = match &live {
                    Some(row) => row.has_credential,
                    None => self.stored_credential(&e),
                };
                let env = if e.is_stdio() {
                    self.store.env(&e.id).into_keys().collect()
                } else {
                    Vec::new()
                };
                let signin_host = live
                    .as_ref()
                    .and_then(|r| r.signin_host.clone())
                    .or_else(|| {
                        if e.is_oauth() {
                            self.store.oauth(&e.id).map(|g| g.host())
                        } else {
                            None
                        }
                    });
                (e, live, has_token, signin_host, env)
            })
            .collect()
    }
}

/// A connector for the view: the saved entry, the state if it's on, whether it has a
/// credential, where to sign in, the names of a program's environment variables.
type ViewRow = (
    ConnectorEntry,
    Option<Row>,
    bool,
    Option<String>,
    Vec<String>,
);

fn compose(rows: &[ViewRow], c: &Core) -> Value {
    let view: Vec<Value> = rows
        .iter()
        .map(|(e, live, has_token, signin_host, env)| {
            let app = live
                .as_ref()
                .and_then(|r| r.conn_id.as_deref())
                .and_then(|id| c.app_by_id(id));
            json!({
                "id": e.id,
                "name": e.name,
                "url": e.url,
                "transport": if e.is_stdio() { "stdio" } else { "http" },
                "launch": e.launch.as_ref().map(Launch::display),
                "env": env,
                "enabled": e.enabled,
                "auth": e.auth,
                "has_token": has_token,
                "signin_host": signin_host,
                "state": live.as_ref().map_or(phase::OFF, |r| r.phase),
                "error": live.as_ref().and_then(|r| r.error.clone()),
                "cli": app.map(|a| a.name.clone()),
                "command": e.command,
                "resource_id": app.map(|a| a.conn_id.clone()),
                "tools": app.map_or(0, |a| a.tools.len()),
                "since": float(e.since),
                "active_since": live.as_ref().and_then(|r| r.active_since).map(float),
                "icon": live.as_ref().and_then(|r| r.icon.clone()),
            })
        })
        .collect();
    Value::Array(view)
}

/// The environment variables' names, for the log: not the values, they're often keys.
fn names(env: &BTreeMap<String, String>) -> String {
    env.keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A hand-written command, cleaned up; empty means the automatic one.
fn command_of(command: Option<&str>) -> Option<String> {
    command
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

/// The default browser, on an http or https address and nothing else: the system opens it
/// (`FileProtocolHandler` on Windows, `open` on the Mac).
fn system_browser() -> OpenBrowser {
    Arc::new(|url: Url| {
        Box::pin(async move {
            oauth::browser_url_allowed(&url)?;
            let text = url.as_str().to_owned();
            #[cfg(windows)]
            let mut command = {
                let mut c = tokio::process::Command::new("rundll32.exe");
                c.args(["url.dll,FileProtocolHandler", &text]);
                c
            };
            #[cfg(target_os = "macos")]
            let mut command = {
                let mut c = tokio::process::Command::new("open");
                c.arg(&text);
                c
            };
            #[cfg(not(any(windows, target_os = "macos")))]
            let mut command = {
                let mut c = tokio::process::Command::new("xdg-open");
                c.arg(&text);
                c
            };
            command
                .spawn()
                .map(|_| ())
                .map_err(|e| OAuthError::new(e.to_string()))
        })
    })
}

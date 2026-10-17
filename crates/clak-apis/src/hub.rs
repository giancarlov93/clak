//! The hub's REST APIs: they start with it, and the panel adds them (from an OpenAPI or by
//! hand), changes them, turns them off and removes them. As with connectors, this is done
//! by the panel, in the same process, or by an agent through the settings (`clak settings`),
//! with every change confirmed by the user; only the panel opens an OAuth sign-in.
//!
//! Every enabled API is a hub program ([`RestApi`]): there's nothing to keep open, it
//! registers right away and answers until the user turns it off or removes it, and a
//! service that doesn't answer says so command by command.
//!
//! The panel sees them in the hub snapshot: on every change this object rebuilds their
//! view and hands it to the broker ([`Core::set_apis`]). The secret is never in it: only
//! whether there is one.
//!
//! [`Core::set_apis`]: clak_hub::Core::set_apis

use crate::model::{ApiAuth, ApiDraft, ApiEntry, AuthKind, Object, clean_command};
use crate::openapi::{self, ApiImport, OpenApiError};
use crate::rest::{self, AccessToken, StaticToken};
use crate::store::ApiStore;
use crate::text::clean;
use clak_connectors::oauth::{
    self, OAuthClient, OAuthError, OAuthGrant, OpenBrowser, SIGN_IN_TIMEOUT, Status,
};
use clak_connectors::{OAuthToken, same_origin, unencrypted};
use clak_hub::{Broker, Outbox, ResourceDef, ToolDef};
use clak_wire::json::{self, float, new_id, now};
use reqwest::Client;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use url::Url;

/// Whoever tells the panel that an OAuth access no longer renews: receives the API name.
type SignInLost = Arc<dyn Fn(&str) + Send + Sync>;

struct State {
    entries: Vec<ApiEntry>,
    running: HashMap<String, Arc<RestApi>>,
}

/// The hub's REST APIs. Built with [`ApiHub::new`] (or [`ApiHub::start`], which opens the
/// hub's store and turns the APIs on) and stopped with [`ApiHub::stop`].
pub struct ApiHub {
    broker: Broker,
    store: Arc<ApiStore>,
    http: Client,
    state: Mutex<State>,
    open_browser: Mutex<OpenBrowser>,
    sign_in_timeout: Mutex<Duration>,
    /// One view at a time: two close changes don't deliver the old view last.
    view: Mutex<()>,
    me: Weak<ApiHub>,
}

impl ApiHub {
    /// The APIs saved in `store`, still off: [`ApiHub::start_all`] turns them on. `http` is
    /// the client for calls ([`rest::new_http_client`]: no automatic redirects).
    pub fn new(broker: Broker, store: Arc<ApiStore>, http: Client) -> Arc<ApiHub> {
        let entries = store.all();
        let hub = Arc::new_cyclic(|me| ApiHub {
            broker,
            store,
            http,
            state: Mutex::new(State {
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

    /// The hub's APIs in its folder (`apis.json`, and the secrets in the system box),
    /// turned on.
    pub fn start(broker: Broker, home: &std::path::Path) -> Result<Arc<ApiHub>, String> {
        let http = rest::new_http_client().map_err(rest::describe)?;
        let hub = ApiHub::new(broker, Arc::new(ApiStore::open(home)), http);
        hub.start_all();
        Ok(hub)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// How the sign-in page is opened: by default the default browser; the app supplies its
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
        let enabled: Vec<ApiEntry> = self
            .lock()
            .entries
            .iter()
            .filter(|e| e.enabled)
            .cloned()
            .collect();
        for entry in enabled {
            self.launch(entry);
        }
        self.push_view();
    }

    pub fn entries(&self) -> Vec<ApiEntry> {
        self.lock().entries.clone()
    }

    pub fn get(&self, id: &str) -> Option<ApiEntry> {
        self.lock().entries.iter().find(|e| e.id == id).cloned()
    }

    /// True if a secret is saved: all that's known is that it exists.
    pub fn has_secret(&self, id: &str) -> bool {
        self.store.has_secret(id)
    }

    /// The client for calls, for whoever reads an OpenAPI or tests a service.
    pub fn http(&self) -> &Client {
        &self.http
    }

    /// Reads an OpenAPI, from a URL or a file. Saves nothing.
    pub async fn import(&self, source: &str) -> Result<ApiImport, OpenApiError> {
        openapi::load(&self.http, source).await
    }

    /// What's wrong with an API, or nothing.
    pub fn problem(draft: &ApiDraft) -> Option<String> {
        draft.problem()
    }

    /// The OAuth sign-in for an API: in the user's browser ([`AuthKind::OAuth`]) or "app
    /// only" ([`AuthKind::OAuthApp`]). `client`: the app registered with the service;
    /// without it, the one from the previous sign-in (`id`), if it's for the same client.
    /// Saves nothing: [`ApiHub::add`] or [`ApiHub::edit`] do, with [`ApiDraft::grant`], only
    /// if it succeeded. Cancelling (from the panel) means dropping the future.
    pub async fn sign_in(
        &self,
        auth: &ApiAuth,
        client: Option<OAuthClient>,
        id: Option<&str>,
        status: Status<'_>,
    ) -> Result<OAuthGrant, OAuthError> {
        if !auth.is_oauth() {
            return Err(OAuthError::new("This API doesn't use OAuth."));
        }
        if let Some(problem) = auth.problem().filter(|_| client.is_none()) {
            return Err(OAuthError::new(problem));
        }
        let token = auth.token_url.as_deref().unwrap_or("");
        let server = oauth::known(
            auth.authorization_url.as_deref().unwrap_or(token),
            token,
            auth.scope.as_deref(),
        )?;
        let previous = id.and_then(|id| self.grant(id));
        let mut client = client;
        if client.is_none()
            && let Some(previous) = &previous
            && Some(previous.client_id.as_str()) == auth.client_id.as_deref()
        {
            client = Some(OAuthClient::new(
                previous.client_id.clone(),
                previous.client_secret.clone(),
            ));
        }
        let client = client
            .or_else(|| {
                auth.client_id
                    .as_deref()
                    .filter(|c| !c.is_empty())
                    .map(|c| OAuthClient::new(c, None))
            })
            .ok_or_else(|| {
                OAuthError::new(
                    "OAuth needs the client ID of the app you registered with the service.",
                )
            })?;
        let result = if auth.kind == AuthKind::OAuthApp {
            if let Some(status) = status {
                status(&format!("Asking {} for a token…", server.host()));
            }
            oauth::client_credentials(&self.http, &server, &client).await
        } else {
            let open = self
                .open_browser
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            let timeout = *self
                .sign_in_timeout
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            oauth::authorize(
                &self.http,
                &server,
                Some(&client),
                previous.as_ref(),
                &open,
                status,
                timeout,
            )
            .await
        };
        result.map_err(|e| {
            if e.network {
                OAuthError::new(format!(
                    "The sign-in service is not reachable: {}",
                    e.message
                ))
            } else {
                e
            }
        })
    }

    /// The OAuth access saved for an API, or nothing.
    fn grant(&self, id: &str) -> Option<OAuthGrant> {
        let text = self.store.secret(id)?;
        OAuthGrant::from_json(&json::parse_str(&text).ok()?)
    }

    /// A new API: saved, turned on, and to agents it's a program right away.
    pub fn add(&self, draft: ApiDraft) -> Result<ApiEntry, String> {
        if let Some(problem) = draft.problem() {
            return Err(problem);
        }
        let secret = draft.secret_to_save();
        let entry = self
            .store
            .add(draft.into_entry(new_id("api"), now()), secret.as_deref())
            .map_err(|e| format!("{e:#}"))?;
        self.lock().entries.push(entry.clone());
        let count = entry.endpoints.len();
        let line = format!(
            "API added: {} ({}{}, {count} {})",
            entry.name,
            entry.base_url,
            if unencrypted(&entry.base_url) {
                ", unencrypted"
            } else {
                ""
            },
            if count == 1 { "endpoint" } else { "endpoints" }
        );
        self.broker.run(|c| c.log(line));
        self.launch(entry.clone());
        self.push_view();
        Ok(entry)
    }

    /// Changes an API. Name, command, endpoints: the same API, permissions stay (a new or
    /// changed command is asked again). A new base URL, access or secret: to agents it's
    /// another service, and permissions start over. A secret made for another host doesn't
    /// follow the new URL. `draft.secret` none keeps the saved one.
    pub async fn edit(&self, id: &str, draft: ApiDraft) -> Result<ApiEntry, String> {
        if let Some(problem) = draft.problem() {
            return Err(problem);
        }
        let old = self
            .get(id)
            .ok_or_else(|| "This API no longer exists.".to_owned())?;
        let mut secret = draft.secret_to_save();
        let entry = ApiEntry {
            name: clean(&draft.name, 40),
            base_url: draft.base_url.trim().trim_end_matches('/').to_owned(),
            auth: draft.auth.clone(),
            endpoints: draft.endpoints.clone(),
            command: clean_command(draft.command.as_deref()),
            source: draft.source.clone(),
            ..old.clone()
        };
        // A secret doesn't follow another host; an OAuth access does, since it's valid for its sign-in service.
        if secret.is_none()
            && !entry.auth.is_oauth()
            && !same_origin(&old.base_url, &entry.base_url)
            && self.has_secret(id)
        {
            secret = Some(String::new());
        }
        // Switching from OAuth to a key, or the reverse: what was there no longer applies.
        if secret.is_none() && old.auth.is_oauth() != entry.auth.is_oauth() && self.has_secret(id) {
            secret = Some(String::new());
        }
        let revoke = if secret.is_some() && old.auth.is_oauth() {
            self.grant(id)
        } else {
            None
        };
        let restart = !old.base_url.eq_ignore_ascii_case(&entry.base_url)
            || old.auth != entry.auth
            || secret.is_some();

        self.store.update(&entry).map_err(|e| format!("{e:#}"))?;
        if let Some(secret) = &secret {
            self.store
                .set_secret(id, Some(secret))
                .map_err(|e| format!("{e:#}"))?;
        }
        {
            let mut state = self.lock();
            for e in state.entries.iter_mut().filter(|e| e.id == id) {
                *e = entry.clone();
            }
        }

        let mut what = Vec::new();
        if old.name != entry.name {
            what.push(format!("renamed “{}”", entry.name));
        }
        if old.command != entry.command {
            what.push(match &entry.command {
                None => "automatic command".to_owned(),
                Some(command) => format!("command “{command}”"),
            });
        }
        if old.base_url != entry.base_url {
            what.push(format!("base URL {}", entry.base_url));
        }
        if old.auth != entry.auth {
            what.push(entry.auth.label());
        }
        if let Some(grant) = &draft.grant {
            what.push(format!("signed in at {}", grant.host()));
        } else if let Some(secret) = &secret {
            what.push(
                if secret.is_empty() {
                    "secret removed"
                } else {
                    "secret changed"
                }
                .to_owned(),
            );
        }
        let endpoints = |e: &ApiEntry| {
            e.endpoints
                .iter()
                .map(|e| json::text(&e.to_json()))
                .collect::<Vec<_>>()
        };
        if endpoints(&old) != endpoints(&entry) {
            what.push(format!("{} endpoints", entry.endpoints.len()));
        }
        if !what.is_empty() {
            let line = format!("API {}: {}", old.name, what.join(", "));
            self.broker.run(|c| c.log(line));
        }

        let running = self.lock().running.get(id).cloned();
        if restart {
            self.halt(id, "API address or access changed").await;
            if entry.enabled {
                self.launch(entry.clone());
            }
        } else if let Some(running) = running {
            running.update(&self.broker, entry.clone());
        }
        if let Some(revoke) = revoke
            && draft.grant.as_ref().map(|g| g.access_token.as_str())
                != Some(revoke.access_token.as_str())
        {
            self.revoke(revoke);
        }
        self.push_view();
        Ok(entry)
    }

    /// Revokes an access that's no longer needed, without waiting: it's a courtesy to the service.
    fn revoke(&self, grant: OAuthGrant) {
        let http = self.http.clone();
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(Duration::from_secs(10), oauth::revoke(&http, &grant)).await;
        });
    }

    /// Turns an API on or off: when off, it's gone for agents, and permissions end.
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let entry = {
            let mut state = self.lock();
            let Some(found) = state.entries.iter_mut().find(|e| e.id == id) else {
                return Ok(());
            };
            if found.enabled == enabled {
                return Ok(());
            }
            found.enabled = enabled;
            found.clone()
        };
        self.store.update(&entry).map_err(|e| format!("{e:#}"))?;
        let line = format!("API {}: {}", entry.name, if enabled { "on" } else { "off" });
        self.broker.run(|c| c.log(line));
        if enabled {
            self.launch(entry);
        } else {
            self.halt(id, "API turned off").await;
        }
        self.push_view();
        Ok(())
    }

    /// Removes an API, with its secret; an OAuth access is revoked, if the service allows it.
    pub async fn remove(&self, id: &str) -> Result<(), String> {
        let found = self.get(id);
        let name = found
            .as_ref()
            .map_or_else(|| id.to_owned(), |e| e.name.clone());
        let grant = if found.as_ref().is_some_and(|e| e.auth.is_oauth()) {
            self.grant(id)
        } else {
            None
        };
        self.halt(id, "API removed").await;
        let removed = self.store.remove(id).map_err(|e| format!("{e:#}"));
        if let Some(grant) = grant {
            self.revoke(grant);
        }
        self.lock().entries.retain(|e| e.id != id);
        self.broker.run(|c| c.log(format!("API removed: {name}")));
        self.push_view();
        removed
    }

    /// Turns off all APIs: the hub is closing.
    pub async fn stop(&self) {
        let all: Vec<Arc<RestApi>> = {
            let mut state = self.lock();
            state.running.drain().map(|(_, api)| api).collect()
        };
        for api in all {
            api.stop(&self.broker, "hub closed").await;
        }
        self.push_view();
    }

    fn launch(&self, entry: ApiEntry) {
        let api = Arc::new(RestApi::new(entry.clone(), self.credential(&entry)));
        {
            let mut state = self.lock();
            if state.running.contains_key(&entry.id) {
                return;
            }
            state.running.insert(entry.id.clone(), api.clone());
        }
        let me = self.me.clone();
        let lost: SignInLost = Arc::new(move |name: &str| {
            if let Some(hub) = me.upgrade() {
                hub.broker.run(|c| {
                    c.log(format!(
                        "{name}: sign-in required (the service no longer renews the access)"
                    ))
                });
                hub.push_view();
            }
        });
        api.start(&self.broker, self.http.clone(), lost);
    }

    async fn halt(&self, id: &str, reason: &str) {
        let running = self.lock().running.remove(id);
        if let Some(running) = running {
            running.stop(&self.broker, reason).await;
        }
        self.broker.run(|c| c.touch());
    }

    /// How an API identifies itself: the secret as is, or the OAuth access (saved on every renewal).
    fn credential(&self, entry: &ApiEntry) -> Option<Arc<dyn AccessToken>> {
        if !entry.auth.is_oauth() {
            return self
                .store
                .secret(&entry.id)
                .map(|s| Arc::new(StaticToken::new(s)) as Arc<dyn AccessToken>);
        }
        let grant = self.grant(&entry.id)?;
        let store = self.store.clone();
        let id = entry.id.clone();
        let save = Box::new(move |renewed: &OAuthGrant| {
            let _ = store.set_secret(&id, Some(&json::text(&renewed.to_json())));
        });
        Some(Arc::new(OAuthToken::new(self.http.clone(), grant, save)))
    }

    /// The APIs as the panel sees them. The secret isn't
    /// there: only whether there is one.
    pub fn view(&self) -> Value {
        let rows = self.rows();
        self.broker
            .run(|c| compose(&rows, |conn| c.app_by_id(conn).map(|a| a.name.clone())))
    }

    fn push_view(&self) {
        let _turn = self.view.lock().unwrap_or_else(|p| p.into_inner());
        let rows = self.rows();
        self.broker.run(|c| {
            let view = compose(&rows, |conn| c.app_by_id(conn).map(|a| a.name.clone()));
            c.set_apis(view);
        });
    }

    /// What the view needs, read outside the broker lock (the Keychain may take a while).
    fn rows(&self) -> Vec<Row> {
        let (entries, running) = {
            let state = self.lock();
            (state.entries.clone(), state.running.clone())
        };
        entries
            .into_iter()
            .map(|entry| {
                let live = running.get(&entry.id);
                let has_secret = entry.auth.kind != AuthKind::None
                    && live.map_or_else(|| self.store.has_secret(&entry.id), |l| l.has_secret());
                Row {
                    needs_sign_in: live.is_some_and(|l| l.needs_sign_in()),
                    conn_id: live.and_then(|l| l.conn_id()),
                    has_secret,
                    entry,
                }
            })
            .collect()
    }

    /// How many commands of an API are running right now (for tests and the panel).
    pub fn calls_in_flight(&self, id: &str) -> usize {
        self.lock().running.get(id).map_or(0, |api| {
            api.calls.lock().unwrap_or_else(|p| p.into_inner()).len()
        })
    }
}

struct Row {
    entry: ApiEntry,
    conn_id: Option<String>,
    has_secret: bool,
    needs_sign_in: bool,
}

fn compose(rows: &[Row], name_of: impl Fn(&str) -> Option<String>) -> Value {
    Value::Array(
        rows.iter()
            .map(|row| {
                let e = &row.entry;
                let state = if !e.enabled {
                    "off"
                } else if row.needs_sign_in {
                    "sign-in required"
                } else {
                    "active"
                };
                json!({
                    "id": e.id,
                    "name": e.name,
                    "base_url": e.base_url,
                    "enabled": e.enabled,
                    "auth": e.auth.kind.as_str(),
                    "auth_label": e.auth.label(),
                    "has_secret": row.has_secret,
                    "state": state,
                    "signin_host": if e.auth.is_oauth() { e.auth.sign_in_host() } else { None },
                    "cli": row.conn_id.as_deref().and_then(&name_of),
                    "command": e.command,
                    "resource_id": row.conn_id,
                    "endpoints": e.endpoints.len(),
                    "source": e.source,
                    "since": float(e.since),
                })
            })
            .collect(),
    )
}

// =========================================================================
// An enabled API: a hub program
// =========================================================================

/// A REST API that looks to agents like a connected program: every endpoint is a command,
/// and every `call` becomes an HTTP request. The secret is sent only to the origin of the
/// base URL ([`rest::send`] guarantees it).
struct RestApi {
    entry: Mutex<ApiEntry>,
    credential: Option<Arc<dyn AccessToken>>,
    /// The program in the broker, while it's registered.
    conn_id: Mutex<Option<String>>,
    stop: CancellationToken,
    /// The running commands, by `call_id`: a `cancel` from the hub stops them.
    calls: Arc<Mutex<HashMap<String, CancellationToken>>>,
    pump: Mutex<Option<JoinHandle<()>>>,
}

impl RestApi {
    fn new(entry: ApiEntry, credential: Option<Arc<dyn AccessToken>>) -> RestApi {
        RestApi {
            entry: Mutex::new(entry),
            credential,
            conn_id: Mutex::new(None),
            stop: CancellationToken::new(),
            calls: Arc::new(Mutex::new(HashMap::new())),
            pump: Mutex::new(None),
        }
    }

    fn entry(&self) -> ApiEntry {
        self.entry.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn conn_id(&self) -> Option<String> {
        self.conn_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn has_secret(&self) -> bool {
        self.credential.is_some()
    }

    /// An OAuth access that's missing or no longer renewed by the service: the user redoes it from the panel.
    fn needs_sign_in(&self) -> bool {
        self.entry().auth.is_oauth() && self.credential.as_ref().is_none_or(|c| c.dead())
    }

    /// Registers in the broker: from here on agents see it.
    fn start(self: &Arc<Self>, broker: &Broker, http: Client, lost: SignInLost) {
        let (outbox, rx) = Outbox::channel();
        let entry = self.entry();
        let conn_id = broker.run(|c| {
            let id = c.register(
                outbox,
                resource(&entry),
                tools(&entry),
                origin(&entry),
                entry.command.as_deref(),
            );
            c.touch();
            id
        });
        *self.conn_id.lock().unwrap_or_else(|p| p.into_inner()) = Some(conn_id);
        let pump = tokio::spawn(self.clone().pump(rx, broker.clone(), http, lost));
        *self.pump.lock().unwrap_or_else(|p| p.into_inner()) = Some(pump);
    }

    /// New name, command or endpoints: the same API, permissions stay (a new or changed
    /// command is asked again, as for every program).
    fn update(&self, broker: &Broker, entry: ApiEntry) {
        *self.entry.lock().unwrap_or_else(|p| p.into_inner()) = entry.clone();
        let Some(conn_id) = self.conn_id() else {
            return;
        };
        broker.run(|c| {
            c.rename(&conn_id, resource(&entry), entry.command.as_deref());
            c.tools_changed(&conn_id, tools(&entry));
        });
    }

    async fn stop(&self, broker: &Broker, reason: &str) {
        self.stop.cancel();
        for (_, call) in self.calls.lock().unwrap_or_else(|p| p.into_inner()).drain() {
            call.cancel();
        }
        let conn_id = self
            .conn_id
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        broker.run(|c| {
            if let Some(conn_id) = &conn_id {
                c.app_closed(conn_id, reason);
            }
            c.touch();
        });
        // A renewal still in flight must not save over the access of whoever comes next.
        if let Some(credential) = &self.credential {
            credential.retire();
        }
        let pump = self.pump.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(pump) = pump {
            let _ = tokio::time::timeout(Duration::from_secs(5), pump).await;
        }
    }

    /// What the hub sends to the program: `call` and `cancel`; the rest (`registered`…)
    /// asks for nothing.
    async fn pump(
        self: Arc<Self>,
        mut rx: UnboundedReceiver<Value>,
        broker: Broker,
        http: Client,
        lost: SignInLost,
    ) {
        loop {
            let message = tokio::select! {
                message = rx.recv() => message,
                _ = self.stop.cancelled() => None,
            };
            let Some(message) = message else { return };
            match message.get("type").and_then(Value::as_str) {
                Some("call") => {
                    let call_id = message
                        .get("call_id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let tool = message
                        .get("tool")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    let arguments = message
                        .get("arguments")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    let cancel = CancellationToken::new();
                    self.calls
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(call_id.clone(), cancel.clone());
                    let api = self.clone();
                    let (broker, http, lost) = (broker.clone(), http.clone(), lost.clone());
                    tokio::spawn(async move {
                        api.call(call_id, tool, arguments, cancel, broker, http, lost)
                            .await
                    });
                }
                Some("cancel") => {
                    if let Some(id) = message.get("call_id").and_then(Value::as_str) {
                        let call = self
                            .calls
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .remove(id);
                        if let Some(call) = call {
                            call.cancel();
                        }
                    }
                }
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn call(
        &self,
        call_id: String,
        tool: String,
        arguments: Object,
        cancel: CancellationToken,
        broker: Broker,
        http: Client,
        lost: SignInLost,
    ) {
        let entry = self.entry();
        let name = clean(&entry.name, 40);
        let signal = move || lost(&name);
        let result = tokio::select! {
            result = rest::call(&entry, &tool, &arguments, self.credential.as_deref(), &http, Some(&signal)) => Some(result),
            // The caller has left, or the API was turned off: nobody is waiting anymore.
            _ = cancel.cancelled() => None,
            _ = self.stop.cancelled() => None,
        };
        self.calls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&call_id);
        let Some(Value::Object(mut result)) = result else {
            return;
        };
        result.insert("type".into(), "result".into());
        result.insert("call_id".into(), call_id.into());
        if let Some(conn_id) = self.conn_id() {
            broker.run(|c| {
                let _ = c.result(&conn_id, &result);
            });
        }
    }
}

/// The program for the panel and the consent prompts: the API name, and where it lives.
fn resource(entry: &ApiEntry) -> ResourceDef {
    ResourceDef {
        app: entry.name.clone(),
        title: entry.authority(),
        instance_id: entry.id.clone(),
        publisher: None,
    }
}

/// The commands, one per endpoint, with their signature.
fn tools(entry: &ApiEntry) -> Vec<ToolDef> {
    entry
        .tools()
        .iter()
        .map(|t| {
            ToolDef::new(
                t["name"].as_str().unwrap_or(""),
                t["description"].as_str().unwrap_or(""),
                t["input_schema"].clone(),
            )
        })
        .collect()
}

/// Who is offering: a service reached by the hub (`connector`, protocol `rest`).
fn origin(entry: &ApiEntry) -> Value {
    entry.origin().unwrap_or_else(|| {
        json!({ "kind": "connector", "protocol": "rest", "url": entry.base_url, "host": "", "secure": false, "loopback": false })
    })
}

/// The default browser, on an http or https URL and nothing else: the system opens it
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

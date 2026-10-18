//! The MCP and APIs tabs: adding, changing, turning on, turning off and removing MCP servers and
//! REST APIs, with their dialogs. Only with the hub in this process: watching another
//! process's hub, nothing changes (the tabs are read-only).
//!
//! The dialogs are real windows, one at a time per kind, above the panel: "connector"
//! (an MCP server, also filled in from a `clak://` link), "api" (a REST API) and, above
//! the latter and modal, "endpoint". The page holds the form; here live what the form
//! changes (what was there, whether there was a secret), the checks and the saving. A
//! token or a secret enters from the page and never leaves again: of what is saved, the page
//! only knows whether it is there. A token that came from a link stays here until saved. A
//! server that is a program on this computer (stdio) has its command line and its
//! environment variables, which count as a token: only their names go back to the page.
//!
//! OAuth sign-ins are long: the current step reaches the dialog as an event
//! (`connector-status`, `api-status`), and `editor_stop` cancels them, like closing the window.

use crate::bridge;
use clak_apis::model::METHODS;
use clak_apis::{ApiAuth, ApiDraft, ApiEndpoint, ApiEntry, ApiParameter, AuthKind, BodyKind, ParamIn, ParamType};
use clak_connectors::launch::{self, Launch};
use clak_connectors::{ConnectorCredential, ConnectorEntry, ConnectorHub, ConnectorLink, OAuthClient, OAuthError};
use clak_hub::cli_names;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use tokio_util::sync::CancellationToken;

const PANEL: &str = "panel";
const CONNECTOR: &str = "connector";
const API: &str = "api";
const ENDPOINT: &str = "endpoint";
const READ_ONLY: &str = "Read-only: the hub runs in another process.";
/// From a link, the button lights up after a moment: a click already under way doesn't find it ready.
const ARMING: Duration = Duration::from_millis(800);
const STOPPED: &str = "Sign-in stopped.";

#[cfg(target_os = "macos")]
const THIS_COMPUTER: &str = "this Mac";
#[cfg(not(target_os = "macos"))]
const THIS_COMPUTER: &str = "this computer";

// =========================================================================
// The state of the open dialogs
// =========================================================================

/// What opens the connector window: a new one (empty, or from a link) or an existing
/// one (`id`). `sign_in`: to sign in again, which starts right away.
#[derive(Clone, Debug, PartialEq, Default)]
struct ConnectorDraft {
    id: Option<String>,
    name: String,
    url: String,
    token: Option<String>,
    from_link: bool,
    sign_in: bool,
}

/// The open connector window: what was there when it opened.
struct ConnectorOpen {
    draft: ConnectorDraft,
    opened: Instant,
    target: Option<ConnectorEntry>,
    had_credential: bool,
    /// The names of a program's saved environment variables (the values stay in the hub).
    env_names: Vec<String>,
}

impl ConnectorOpen {
    /// A link on a connector that already exists: only the sign-in changes.
    fn restricted(&self) -> bool {
        self.target.is_some() && self.draft.from_link
    }

    fn armed(&self) -> bool {
        !self.draft.from_link || self.opened.elapsed() >= ARMING
    }
}

struct ApiOpen {
    target: Option<ApiEntry>,
    had_secret: bool,
    sign_in: bool,
}

/// The endpoint dialog: for which window, what was there, the names already taken by the others.
struct EndpointOpen {
    owner: String,
    endpoint: Option<Value>,
    taken: Vec<String>,
    tag: Value,
}

#[derive(Default)]
struct Editors {
    connector: Option<ConnectorOpen>,
    /// A link that arrived while the window was open: it waits its turn (only the last one).
    queued: Option<ConnectorDraft>,
    api: Option<ApiOpen>,
    endpoint: Option<EndpointOpen>,
    /// What a window is doing and can be stopped: a sign-in, a document being read.
    busy: HashMap<String, (u64, CancellationToken)>,
    turn: u64,
}

static EDITORS: LazyLock<Mutex<Editors>> = LazyLock::new(Mutex::default);

fn editors() -> MutexGuard<'static, Editors> {
    EDITORS.lock().unwrap_or_else(|p| p.into_inner())
}

/// A window job that can be stopped; it ends with [`done`].
fn begin(label: &str) -> (u64, CancellationToken) {
    let token = CancellationToken::new();
    let mut editors = editors();
    editors.turn += 1;
    let turn = editors.turn;
    if let Some((_, old)) = editors.busy.insert(label.to_owned(), (turn, token.clone())) {
        old.cancel();
    }
    (turn, token)
}

/// Done: it is removed, unless another one has started in the meantime.
fn done(label: &str, turn: u64) {
    let mut editors = editors();
    if editors.busy.get(label).is_some_and(|(t, _)| *t == turn) {
        editors.busy.remove(label);
    }
}

fn connectors(app: &AppHandle) -> Result<std::sync::Arc<ConnectorHub>, String> {
    bridge::of(app).connectors().ok_or_else(|| READ_ONLY.to_owned())
}

fn apis(app: &AppHandle) -> Result<std::sync::Arc<clak_apis::ApiHub>, String> {
    bridge::of(app).apis().ok_or_else(|| READ_ONLY.to_owned())
}

fn trimmed(text: &str) -> Option<String> {
    Some(text.trim()).filter(|t| !t.is_empty()).map(str::to_owned)
}

/// A system alert above the panel: an error that has no dialog of its own.
fn tell(app: &AppHandle, text: &str) {
    let mut dialog = app.dialog().message(text).title("Clak").kind(MessageDialogKind::Warning);
    if let Some(panel) = app.get_webview_window(PANEL) {
        dialog = dialog.parent(&panel);
    }
    dialog.show(|_| {});
}

// =========================================================================
// The windows
// =========================================================================

/// A dialog: above its window, hidden until the page has taken its measure
/// (`editor_fit`), or anyway after a moment.
fn dialog(
    app: &AppHandle,
    label: &str,
    page: &str,
    title: &str,
    size: (f64, f64),
    parent: Option<&WebviewWindow>,
    resizable: bool,
) -> Result<WebviewWindow, String> {
    let mut builder = WebviewWindowBuilder::new(app, label, WebviewUrl::App(page.into()))
        .title(title)
        .inner_size(size.0, size.1)
        .resizable(resizable)
        .maximizable(resizable)
        .minimizable(false)
        .skip_taskbar(true)
        .visible(false)
        .focused(true);
    if let Some(parent) = parent {
        builder = builder.parent(parent).map_err(|e| e.to_string())?;
    }
    let window = crate::look::window(builder).build().map_err(|e| e.to_string())?;
    let late = window.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        if !late.is_visible().unwrap_or(true) {
            reveal(&late);
        }
    });
    Ok(window)
}

/// Centered on the window it comes from (like `CenterOwner`), and in front.
fn reveal(window: &WebviewWindow) {
    let parent = match window.label() {
        ENDPOINT => editors().endpoint.as_ref().map(|e| e.owner.clone()),
        _ => Some(PANEL.to_owned()),
    };
    if let Some(parent) = parent.and_then(|p| window.app_handle().get_webview_window(&p))
        && let (Ok(at), Ok(size), Ok(own)) = (parent.outer_position(), parent.outer_size(), window.outer_size())
    {
        let x = at.x + (size.width as i32 - own.width as i32) / 2;
        let y = at.y + (size.height as i32 - own.height as i32) / 2;
        let mut position = PhysicalPosition::new(x, y);
        // Inside the screen, even with the panel half off it.
        if let Ok(Some(monitor)) = parent.current_monitor() {
            let area = monitor.work_area();
            let right = area.position.x + area.size.width as i32 - own.width as i32;
            let bottom = area.position.y + area.size.height as i32 - own.height as i32;
            position.x = position.x.min(right).max(area.position.x);
            position.y = position.y.min(bottom).max(area.position.y);
        }
        let _ = window.set_position(position);
    } else {
        let _ = window.center();
    }
    let _ = window.show();
    let _ = window.set_focus();
}

/// What happens when a dialog closes, however it closes: whatever it was doing stops,
/// and the waiting link gets its turn.
fn watch(window: &WebviewWindow) {
    let app = window.app_handle().clone();
    let label = window.label().to_owned();
    window.on_window_event(move |event| {
        if !matches!(event, WindowEvent::Destroyed) {
            return;
        }
        let (next, owner) = {
            let mut editors = editors();
            if let Some((_, token)) = editors.busy.remove(&label) {
                token.cancel();
            }
            match label.as_str() {
                CONNECTOR => {
                    editors.connector = None;
                    (editors.queued.take(), None)
                }
                API => {
                    editors.api = None;
                    (None, None)
                }
                _ => (None, editors.endpoint.take().map(|e| e.owner)),
            }
        };
        if label == API
            && let Some(endpoint) = app.get_webview_window(ENDPOINT)
        {
            let _ = endpoint.destroy();
        }
        // The endpoint dialog is modal: once it is closed, the window below responds again.
        if let Some(owner) = owner.and_then(|o| app.get_webview_window(&o)) {
            let _ = owner.set_enabled(true);
            let _ = owner.set_focus();
        }
        if let Some(next) = next {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = open_connector(&app, next);
            });
        }
    });
}

/// The page says how tall it is (or, without a height, that it is ready): the window adapts and appears.
#[tauri::command]
pub fn editor_fit(window: WebviewWindow, height: Option<f64>) {
    if let Some(height) = height {
        let scale = window.scale_factor().unwrap_or(1.0);
        let screen = window
            .current_monitor()
            .ok()
            .flatten()
            .map_or(900.0, |m| m.work_area().size.height as f64 / scale * 0.92);
        let width = window.inner_size().map_or(560.0, |s| s.width as f64 / scale);
        let _ = window.set_size(LogicalSize::new(width, height.clamp(200.0, screen)));
    }
    if !window.is_visible().unwrap_or(true) {
        reveal(&window);
    }
}

/// Stops what the dialog is doing: a sign-in, a document being read.
#[tauri::command]
pub fn editor_stop(window: WebviewWindow) {
    if let Some((_, token)) = editors().busy.remove(window.label()) {
        token.cancel();
    }
}

#[tauri::command]
pub fn editor_close(window: WebviewWindow) {
    let _ = window.destroy();
}

// =========================================================================
// The MCP servers
// =========================================================================

/// The connector window. Only one: a link that arrives while it is open waits its
/// turn (only the last one), and an identical one doesn't open another. A site must not be able
/// to fill the screen.
fn open_connector(app: &AppHandle, draft: ConnectorDraft) -> Result<(), String> {
    let hub = connectors(app)?;
    if let Some(window) = app.get_webview_window(CONNECTOR) {
        let mut editors = editors();
        if draft.from_link && editors.connector.as_ref().is_some_and(|o| o.draft != draft) {
            editors.queued = Some(draft);
        }
        drop(editors);
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let target = match &draft.id {
        Some(id) => Some(hub.get(id).ok_or("This connector no longer exists.")?),
        None => hub.find_by_url(&draft.url),
    };
    let had_credential = target.as_ref().is_some_and(|t| hub.has_credential(&t.id));
    let env_names = target.as_ref().filter(|t| t.is_stdio()).map(|t| hub.env(&t.id).into_keys().collect()).unwrap_or_default();
    editors().connector = Some(ConnectorOpen { draft, opened: Instant::now(), target, had_credential, env_names });
    let panel = app.get_webview_window(PANEL);
    match dialog(app, CONNECTOR, "connector.html", "Clak — MCP server", (560.0, 640.0), panel.as_ref(), false) {
        Ok(window) => {
            watch(&window);
            Ok(())
        }
        Err(e) => {
            editors().connector = None;
            Err(e)
        }
    }
}

/// Adding (without `id`), changing, or signing in again (`sign_in`).
#[tauri::command]
pub async fn connector_open(app: AppHandle, id: Option<String>, sign_in: bool) -> Result<(), String> {
    let url = if id.is_none() { "https://".to_owned() } else { String::new() };
    open_connector(&app, ConnectorDraft { id, url, sign_in, ..ConnectorDraft::default() })
}

/// A `clak://connect?…` link: opens the connector window already filled in, on the MCP
/// tab. It connects nothing by itself: the user confirms.
pub fn open_link(app: &AppHandle, uri: &str) {
    crate::windows::panel(app, Some("mcp"));
    let link = ConnectorLink::parse(Some(uri));
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let link = match link {
            Ok(link) => link,
            Err(problem) => return tell(&app, &format!("The link can't be used: {problem}.")),
        };
        let draft = ConnectorDraft { id: None, name: link.name, url: link.url, token: link.token, from_link: true, sign_in: false };
        if let Err(problem) = open_connector(&app, draft) {
            tell(&app, &problem);
        }
    });
}

/// What the window shows when it opens. Not the saved token: only whether it is there.
#[tauri::command]
pub fn connector_draft(window: WebviewWindow) -> Option<Value> {
    if window.label() != CONNECTOR {
        return None;
    }
    let editors = editors();
    let open = editors.connector.as_ref()?;
    let draft = &open.draft;
    let target = open.target.as_ref();
    let link_token = draft.token.as_deref().is_some_and(|t| !t.is_empty());
    Some(json!({
        "target": target.map(|t| json!({ "id": t.id, "name": t.name, "url": t.url, "command": t.command, "oauth": t.is_oauth(), "stdio": t.is_stdio() })),
        "name": target.map_or(draft.name.as_str(), |t| t.name.as_str()),
        "command": target.and_then(|t| t.command.as_deref()).unwrap_or(""),
        "url": target.map_or(draft.url.as_str(), |t| t.url.as_str()),
        // A program can be chosen only for a new one, and never from a link.
        "can_choose": target.is_none() && !draft.from_link,
        "stdio": target.is_some_and(ConnectorEntry::is_stdio),
        "launch": target.and_then(|t| t.launch.as_ref()).map(Launch::display).unwrap_or_default(),
        "env_names": open.env_names,
        "from_link": draft.from_link,
        "link_token": link_token,
        "sign_in": draft.sign_in,
        "restricted": open.restricted(),
        "had_credential": open.had_credential,
        // A token from the link means token; opened to sign in, the sign-in (a server added
        // without one, e.g. by an agent, has no OAuth yet); otherwise what the connector
        // already uses, and for a new one the sign-in.
        "oauth": !link_token && (draft.sign_in || target.is_none_or(ConnectorEntry::is_oauth)),
        "reauth": draft.sign_in || open.restricted(),
        "armed_in": if draft.from_link { ARMING.saturating_sub(open.opened.elapsed()).as_millis() as u64 } else { 0 },
    }))
}

/// The connector window's form, as it is now.
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ConnectorForm {
    name: String,
    command: String,
    url: String,
    /// Sign-in with the service (OAuth), or a pasted token (even none).
    oauth: bool,
    token: String,
    /// The token box is still the link's one, untouched.
    link_token: bool,
    drop_token: bool,
    reauth: bool,
    client_id: String,
    client_secret: String,
    /// A program on this computer (stdio) instead of an address.
    stdio: bool,
    launch: String,
    /// The program's environment variables, one per line: `NAME=value`.
    env: String,
    drop_env: bool,
}

/// What the form means for a program, compared with what was there.
#[derive(Debug, PartialEq, Eq)]
struct ProgramPlan {
    problem: Option<String>,
    launch: Option<Launch>,
    /// The new variables (empty: they are removed); `None`: the saved ones stay.
    env: Option<BTreeMap<String, String>>,
    /// A new program, or a different command line: it will start at every Clak launch.
    program_changes: bool,
    permissions_restart: bool,
    /// The saved variables stay if the box is empty: to remove them, say so.
    keeps_env: bool,
}

/// What the form means, compared with what was there.
#[derive(Debug, PartialEq, Eq)]
struct ConnectorPlan {
    problem: Option<String>,
    origin_changed: bool,
    /// The sign-in page opens: a new connector, a switch to OAuth, a different host, or asked for.
    needs_sign_in: bool,
    /// The token changes: written, removed, a switch from OAuth, or a different host.
    token_changes: bool,
    /// The sign-in or the address changes: for the agents it is another server, and permissions start over.
    permissions_restart: bool,
    /// On a connector already using OAuth on the same host, the sign-in is redone only if asked for.
    show_reauth: bool,
    /// The saved token stays if the box is empty: to remove it, say so.
    keeps_token: bool,
}

impl ConnectorForm {
    fn url(&self) -> &str {
        self.url.trim()
    }

    fn command(&self) -> Option<String> {
        trimmed(&self.command)
    }

    /// The written token, or the link's one while the box is still its own.
    fn token(&self, open: &ConnectorOpen) -> Option<String> {
        match trimmed(&self.token) {
            Some(token) => Some(token),
            None if self.link_token => open.draft.token.clone().filter(|t| !t.is_empty()),
            None => None,
        }
    }

    fn client(&self) -> Option<OAuthClient> {
        trimmed(&self.client_id).map(|id| OAuthClient::new(id, trimmed(&self.client_secret)))
    }

    /// The written variables, `NAME=value` per line (not empty lines nor those with `#`);
    /// `None` if there are none. The error doesn't repeat the line: it might be a key.
    fn env(&self) -> Result<Option<BTreeMap<String, String>>, String> {
        let mut env = BTreeMap::new();
        for (n, line) in self.env.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(format!("Environment, line {}: write NAME=value.", n + 1));
            };
            env.insert(key.trim().to_owned(), value.trim().to_owned());
        }
        Ok((!env.is_empty()).then_some(env))
    }

    fn program_plan(&self, open: &ConnectorOpen) -> ProgramPlan {
        let target = open.target.as_ref();
        let launch = Launch::parse(&self.launch);
        let env = if self.drop_env { Ok(Some(BTreeMap::new())) } else { self.env() };
        let none = BTreeMap::new();
        let problem = if open.draft.from_link || target.is_some_and(|t| !t.is_stdio()) {
            Some("A server at an address can't become a program: remove it and add it again.".to_owned())
        } else {
            launch
                .as_ref()
                .err()
                .cloned()
                .or_else(|| env.as_ref().err().cloned())
                .or_else(|| ConnectorHub::problem_stdio(Some(&self.name), env.as_ref().ok().and_then(Option::as_ref).unwrap_or(&none)))
                .or_else(|| cli_names::problem(self.command().as_deref()))
        };
        let launch = launch.ok();
        let env = env.ok().flatten();
        let program_changes = launch.is_some() && target.is_none_or(|t| t.launch != launch);
        ProgramPlan {
            problem,
            permissions_restart: target.is_some() && (program_changes || env.is_some()),
            keeps_env: target.is_some() && !open.env_names.is_empty(),
            launch,
            env,
            program_changes,
        }
    }

    fn plan(&self, open: &ConnectorOpen) -> ConnectorPlan {
        let target = open.target.as_ref();
        let url = self.url();
        let problem = if open.restricted() {
            None
        } else {
            ConnectorHub::problem(Some(&self.name), Some(url)).or_else(|| cli_names::problem(self.command().as_deref()))
        };
        let origin_changed = target.is_some_and(|t| !clak_connectors::same_origin(&t.url, url));
        let url_changed = target.is_some_and(|t| t.url != url);
        let needs_sign_in = self.oauth
            && target.is_none_or(|t| !t.is_oauth() || origin_changed || self.reauth || !open.had_credential);
        let token_changes = !self.oauth
            && target.is_none_or(|t| t.is_oauth() || origin_changed || self.token(open).is_some() || self.drop_token);
        ConnectorPlan {
            problem,
            origin_changed,
            needs_sign_in,
            token_changes,
            permissions_restart: target.is_some() && (needs_sign_in || token_changes || url_changed),
            show_reauth: target.is_some_and(ConnectorEntry::is_oauth) && open.had_credential && !origin_changed && !open.restricted(),
            keeps_token: target.is_some_and(|t| !t.is_oauth()) && open.had_credential && !origin_changed,
        }
    }
}

/// The form checks, on every change: what is wrong and what will happen on saving.
#[tauri::command]
pub async fn connector_check(window: WebviewWindow, form: ConnectorForm) -> Value {
    if form.stdio {
        return program_check(&window, &form).await;
    }
    let editors = editors();
    let Some(open) = editors.connector.as_ref().filter(|_| window.label() == CONNECTOR) else {
        return json!({ "problem": "This window is no longer valid." });
    };
    let plan = form.plan(open);
    let url = form.url();
    let host = clak_connectors::uri::absolute(url).map(|u| clak_connectors::uri::host(&u)).filter(|h| !h.is_empty());
    json!({
        "problem": plan.problem,
        "host": host,
        "unencrypted": ConnectorHub::unencrypted(url),
        "auto": trimmed(&form.name).map(|n| cli_names::from(&n)),
        "can_probe": ConnectorHub::problem(Some("probe"), Some(url)).is_none(),
        "needs_sign_in": plan.needs_sign_in,
        "token_changes": plan.token_changes,
        "permissions_restart": plan.permissions_restart,
        "show_reauth": plan.show_reauth,
        "keeps_token": plan.keeps_token,
        "armed": open.armed(),
    })
}

/// The checks for a program: also where the file that will start is (looked up as the hub
/// will look it up), or that it isn't there.
async fn program_check(window: &WebviewWindow, form: &ConnectorForm) -> Value {
    let (plan, armed) = {
        let editors = editors();
        let Some(open) = editors.connector.as_ref().filter(|_| window.label() == CONNECTOR) else {
            return json!({ "problem": "This window is no longer valid." });
        };
        (form.program_plan(open), open.armed())
    };
    let program = match &plan.launch {
        Some(launch) => launch::find(&launch.program).await,
        None => None,
    };
    let missing = plan.launch.as_ref().filter(|_| program.is_none()).map(|l| {
        format!("“{}” was not found: neither a file nor a program in the PATH.", l.program)
    });
    json!({
        "problem": plan.problem.or(missing),
        "program": program.as_ref().map(|p| p.display().to_string()),
        "auto": trimmed(&form.name).map(|n| cli_names::from(&n)),
        "can_probe": program.is_some(),
        "program_changes": plan.program_changes,
        "permissions_restart": plan.permissions_restart,
        "keeps_env": plan.keeps_env,
        "armed": armed,
    })
}

/// Tries the server without saving anything: "Reached (…): 12 commands.", or why not.
#[tauri::command]
pub async fn connector_probe(app: AppHandle, window: WebviewWindow, form: ConnectorForm) -> Value {
    if form.stdio {
        return program_probe(&app, &window, &form).await;
    }
    let token = {
        let editors = editors();
        match editors.connector.as_ref().filter(|_| window.label() == CONNECTOR) {
            Some(open) if !form.oauth => form.token(open),
            _ => None,
        }
    };
    let result = ConnectorHub::probe(form.url(), token.as_deref()).await;
    json!({ "ok": result.ok, "message": result.message, "sign_in": result.sign_in })
}

/// Tries a program: starts it with the written variables (or, if the box is empty, with
/// the saved ones), asks for the commands and stops it.
async fn program_probe(app: &AppHandle, window: &WebviewWindow, form: &ConnectorForm) -> Value {
    let failed = |message: String| json!({ "ok": false, "message": message, "sign_in": false });
    let launch = match Launch::parse(&form.launch) {
        Ok(launch) => launch,
        Err(problem) => return failed(problem),
    };
    let target = editors()
        .connector
        .as_ref()
        .filter(|_| window.label() == CONNECTOR)
        .and_then(|o| o.target.as_ref().map(|t| t.id.clone()));
    let env = match form.env() {
        Err(problem) => return failed(problem),
        Ok(Some(env)) => env,
        Ok(None) if form.drop_env => BTreeMap::new(),
        Ok(None) => match (target, connectors(app)) {
            (Some(id), Ok(hub)) => hub.env(&id),
            _ => BTreeMap::new(),
        },
    };
    let result = ConnectorHub::probe_stdio(&launch, &env).await;
    json!({ "ok": result.ok, "message": result.message, "sign_in": false })
}

/// Saves a program: new, or with a changed name, command line or variables.
async fn program_save(hub: &ConnectorHub, window: &WebviewWindow, form: &ConnectorForm) -> Result<(), String> {
    let (plan, target, armed) = {
        let editors = editors();
        let open = editors.connector.as_ref().filter(|_| window.label() == CONNECTOR).ok_or("This window is no longer valid.")?;
        (form.program_plan(open), open.target.clone(), open.armed())
    };
    if !armed {
        return Err(String::new());
    }
    if let Some(problem) = plan.problem {
        return Err(problem);
    }
    let launch = plan.launch.ok_or("A command line is required.")?;
    let command = form.command();
    match target {
        None => hub.add_stdio(&form.name, &launch, &plan.env.unwrap_or_default(), command.as_deref()).map(|_| ()),
        Some(target) => hub.edit_stdio(&target.id, &form.name, command.as_deref(), &launch, plan.env).await.map(|_| ()),
    }?;
    let _ = window.destroy();
    Ok(())
}

/// Saves. First the sign-in, if needed: if it fails, nothing is saved. Once done, the window closes.
#[tauri::command]
pub async fn connector_save(app: AppHandle, window: WebviewWindow, form: ConnectorForm) -> Result<(), String> {
    let hub = connectors(&app)?;
    if form.stdio {
        return program_save(&hub, &window, &form).await;
    }
    let (plan, target, restricted, token, armed) = {
        let editors = editors();
        let open = editors.connector.as_ref().filter(|_| window.label() == CONNECTOR).ok_or("This window is no longer valid.")?;
        (form.plan(open), open.target.clone(), open.restricted(), form.token(open), open.armed())
    };
    if !armed {
        return Err(String::new());
    }
    if let Some(problem) = plan.problem {
        return Err(problem);
    }
    let url = form.url().to_owned();
    let command = form.command();
    let mut credential = None;
    if plan.needs_sign_in {
        let (turn, cancel) = begin(CONNECTOR);
        let status_window = window.clone();
        let status = move |text: &str| {
            let _ = status_window.emit_to(CONNECTOR, "connector-status", text);
        };
        let result = hub
            .sign_in(&url, form.client().as_ref(), target.as_ref().map(|t| t.id.as_str()), Some(&status), &cancel)
            .await;
        done(CONNECTOR, turn);
        let _ = window.set_focus(); // from the browser, back here
        match result {
            Ok(grant) => credential = Some(ConnectorCredential { token: None, grant }),
            Err(OAuthError { canceled: true, .. }) => return Err(STOPPED.into()),
            Err(e) => return Err(e.message),
        }
    } else if plan.token_changes {
        credential = Some(ConnectorCredential { token: if form.drop_token { None } else { token }, grant: None });
    }

    match target {
        None => match credential.as_ref().and_then(|c| c.grant.as_ref()) {
            Some(grant) => hub.add_oauth(&form.name, &url, grant, command.as_deref()).map(|_| ()),
            None => hub.add(&form.name, &url, credential.as_ref().and_then(|c| c.token.as_deref()), command.as_deref()).map(|_| ()),
        },
        Some(target) if restricted => match credential.as_ref().and_then(|c| c.grant.as_ref()) {
            Some(grant) => hub.set_oauth(&target.id, grant),
            None => hub.set_token(&target.id, credential.as_ref().and_then(|c| c.token.as_deref())),
        },
        Some(target) => hub.edit(&target.id, &form.name, command.as_deref(), &url, credential).await.map(|_| ()),
    }?;
    let _ = window.destroy();
    Ok(())
}

/// Turned off, it no longer exists for the agents; it stays saved.
#[tauri::command]
pub async fn connector_enable(app: AppHandle, id: String, on: bool) {
    let Ok(hub) = connectors(&app) else { return };
    if let Err(problem) = hub.set_enabled(&id, on).await {
        tell(&app, &problem);
    }
}

#[tauri::command]
pub fn connector_remove(app: AppHandle, id: String) {
    let Ok(hub) = connectors(&app) else { return };
    let Some(entry) = hub.get(&id) else { return };
    let what = if entry.is_stdio() { "its environment variables are" } else { "its token or sign-in is" };
    let text = format!(
        "Agents will no longer see “{}”, the permissions they had end, and {what} deleted from {THIS_COMPUTER}.",
        entry.name
    );
    confirm_remove(&app, &entry.name, text, move |app| async move {
        if let Err(problem) = hub.remove(&id).await {
            tell(&app, &problem);
        }
    });
}

/// Confirming a step that can't be undone: whoever used it no longer sees it, and the secret goes away.
fn confirm_remove<F, Fut>(app: &AppHandle, name: &str, text: String, then: F)
where
    F: FnOnce(AppHandle) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut dialog = app
        .dialog()
        .message(text)
        .title(format!("Remove “{name}”?"))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom("Remove".into(), "Cancel".into()));
    if let Some(panel) = app.get_webview_window(PANEL) {
        dialog = dialog.parent(&panel);
    }
    let app = app.clone();
    dialog.show(move |yes| {
        if yes {
            tauri::async_runtime::spawn(then(app));
        }
    });
}

// =========================================================================
// The REST APIs
// =========================================================================

/// The APIs window. Only one: another request brings the open one back to the front.
#[tauri::command]
pub async fn api_open(app: AppHandle, id: Option<String>, sign_in: bool) -> Result<(), String> {
    let hub = apis(&app)?;
    crate::windows::panel(&app, Some("apis"));
    if let Some(window) = app.get_webview_window(API) {
        let _ = window.unminimize();
        let _ = window.set_focus();
        return Ok(());
    }
    let target = match &id {
        Some(id) => Some(hub.get(id).ok_or("This API no longer exists.")?),
        None => None,
    };
    let had_secret = target.as_ref().is_some_and(|t| hub.has_secret(&t.id));
    editors().api = Some(ApiOpen { target, had_secret, sign_in });
    let panel = app.get_webview_window(PANEL);
    match dialog(&app, API, "api.html", "Clak — REST API", (980.0, 720.0), panel.as_ref(), true) {
        Ok(window) => {
            let _ = window.set_min_size(Some(LogicalSize::new(820.0, 560.0)));
            watch(&window);
            Ok(())
        }
        Err(e) => {
            editors().api = None;
            Err(e)
        }
    }
}

/// What the window shows when it opens. Not the secret: only whether it is there.
#[tauri::command]
pub fn api_draft(window: WebviewWindow) -> Option<Value> {
    if window.label() != API {
        return None;
    }
    let editors = editors();
    let open = editors.api.as_ref()?;
    Some(json!({
        "target": open.target.as_ref().map(|t| json!({
            "id": t.id,
            "name": t.name,
            "command": t.command,
            "base_url": t.base_url,
            "auth": t.auth.to_json(),
            "source": t.source,
            "endpoints": t.endpoints.iter().map(ApiEndpoint::to_json).collect::<Vec<_>>(),
        })),
        "had_secret": open.had_secret,
        "sign_in": open.sign_in,
    }))
}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct AuthForm {
    kind: String,
    name: String,
    authorization_url: String,
    token_url: String,
    scope: String,
    client_id: String,
}

/// The APIs window's form, as it is now: the endpoints are the chosen ones.
#[derive(Deserialize, Default, Debug)]
#[serde(default)]
pub struct ApiForm {
    name: String,
    command: String,
    base_url: String,
    auth: AuthForm,
    secret: String,
    drop_secret: bool,
    client_secret: String,
    reauth: bool,
    endpoints: Vec<Value>,
    source: Option<String>,
}

#[derive(Debug)]
struct ApiPlan {
    draft: ApiDraft,
    /// The sign-in page opens (or the token is requested, "app only"): a new API, a
    /// differently configured sign-in, no saved sign-in, a new secret, or asked for.
    needs_sign_in: bool,
    /// A saved secret that stays if the box is empty.
    keeps_secret: bool,
    /// A new address, sign-in or secret: for the agents it is another service.
    permissions_restart: bool,
    show_reauth: bool,
    has_client_secret: bool,
}

/// An endpoint written in the page: like `ApiEndpoint::from_json`, but without discarding it if
/// it is wrong (the check says so).
fn endpoint_of(node: &Value) -> Option<ApiEndpoint> {
    let o = node.as_object()?;
    let text = |key: &str| o.get(key).and_then(Value::as_str);
    Some(ApiEndpoint {
        name: text("name").unwrap_or("").to_owned(),
        method: text("method").unwrap_or("GET").to_uppercase(),
        path: text("path").unwrap_or("").to_owned(),
        description: text("description").and_then(trimmed),
        parameters: o
            .get("parameters")
            .and_then(Value::as_array)
            .map(|list| list.iter().filter_map(ApiParameter::from_json).collect())
            .unwrap_or_default(),
        body: text("body").and_then(BodyKind::parse),
        body_schema: o.get("body_schema").and_then(Value::as_object).cloned(),
        body_required: o.get("body_required").and_then(Value::as_bool).unwrap_or(false),
    })
}

impl ApiForm {
    fn kind(&self) -> AuthKind {
        AuthKind::parse(&self.auth.kind).unwrap_or_default()
    }

    fn auth(&self) -> ApiAuth {
        let a = &self.auth;
        let t = |s: &str| trimmed(s);
        match self.kind() {
            kind @ (AuthKind::Header | AuthKind::Query | AuthKind::Basic) => ApiAuth::named(kind, a.name.trim()),
            AuthKind::OAuth => ApiAuth::oauth(
                AuthKind::OAuth,
                t(&a.authorization_url).as_deref(),
                t(&a.token_url).as_deref(),
                t(&a.scope).as_deref(),
                t(&a.client_id).as_deref(),
            ),
            AuthKind::OAuthApp => ApiAuth::oauth(
                AuthKind::OAuthApp,
                None,
                t(&a.token_url).as_deref(),
                t(&a.scope).as_deref(),
                t(&a.client_id).as_deref(),
            ),
            kind => ApiAuth::new(kind),
        }
    }

    /// The app registered with the service, if the client secret is written here.
    fn client(&self) -> Option<OAuthClient> {
        let id = trimmed(&self.auth.client_id)?;
        trimmed(&self.client_secret).map(|secret| OAuthClient::new(id, Some(secret)))
    }

    fn plan(&self, open: &ApiOpen) -> ApiPlan {
        let target = open.target.as_ref();
        let auth = self.auth();
        let kind = auth.kind;
        let url = self.base_url.trim();
        let same_origin = target.is_some_and(|t| clak_connectors::same_origin(&t.base_url, url));
        let keeps_secret = open.had_secret && kind != AuthKind::None && same_origin;
        // The secret to save: nothing keeps the existing one, empty removes it.
        let secret = if kind == AuthKind::None {
            open.had_secret.then(String::new)
        } else if auth.is_oauth() {
            None // access comes with the grant, after the sign-in
        } else if self.drop_secret && keeps_secret {
            Some(String::new())
        } else {
            trimmed(&self.secret)
        };
        let needs_sign_in = auth.is_oauth()
            && target.is_none_or(|t| t.auth != auth || !open.had_secret || self.reauth || !self.client_secret.is_empty());
        let permissions_restart = target.is_some_and(|t| {
            !t.base_url.eq_ignore_ascii_case(url.trim_end_matches('/')) || t.auth != auth || secret.is_some() || needs_sign_in
        });
        ApiPlan {
            needs_sign_in,
            keeps_secret,
            permissions_restart,
            show_reauth: target.is_some_and(|t| open.had_secret && t.auth == auth),
            has_client_secret: open.had_secret && target.is_some_and(|t| t.auth.is_oauth()),
            draft: ApiDraft {
                name: self.name.clone(),
                base_url: url.to_owned(),
                auth,
                secret,
                endpoints: self.endpoints.iter().filter_map(endpoint_of).collect(),
                command: trimmed(&self.command),
                source: self.source.clone().filter(|s| !s.trim().is_empty()),
                grant: None,
            },
        }
    }
}

/// The form checks, on every change.
#[tauri::command]
pub fn api_check(window: WebviewWindow, form: ApiForm) -> Value {
    let editors = editors();
    let Some(open) = editors.api.as_ref().filter(|_| window.label() == API) else {
        return json!({ "problem": "This window is no longer valid." });
    };
    let plan = form.plan(open);
    json!({
        "problem": plan.draft.problem(),
        "unencrypted": plan.draft.auth.kind != AuthKind::None && ConnectorHub::unencrypted(&plan.draft.base_url),
        "auto": trimmed(&form.name).map(|n| cli_names::from(&n)),
        "needs_sign_in": plan.needs_sign_in,
        "keeps_secret": plan.keeps_secret,
        "permissions_restart": plan.permissions_restart,
        "show_reauth": plan.show_reauth,
        "has_client_secret": plan.has_client_secret,
    })
}

/// Reads an OpenAPI document, from an address or a file. Saves nothing. When stopped
/// (`editor_stop`), the error is empty.
#[tauri::command]
pub async fn api_import(app: AppHandle, window: WebviewWindow, source: String) -> Result<Value, String> {
    let hub = apis(&app)?;
    let (turn, cancel) = begin(window.label());
    let result = tokio::select! {
        result = hub.import(&source) => Some(result),
        _ = cancel.cancelled() => None,
    };
    done(window.label(), turn);
    match result {
        None => Err(String::new()),
        Some(Err(e)) => Err(e.0),
        Some(Ok(import)) => Ok(json!({
            "name": import.name,
            "base_url": import.base_url,
            "auth": import.auth.to_json(),
            "endpoints": import.endpoints.iter().map(ApiEndpoint::to_json).collect::<Vec<_>>(),
            "skipped": import.skipped,
            "version": import.version,
        })),
    }
}

/// The file of an OpenAPI document, from the system picker.
#[tauri::command]
pub async fn api_browse(window: WebviewWindow) -> Option<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let picker = window
        .dialog()
        .file()
        .set_title("OpenAPI document")
        .add_filter("OpenAPI (*.json, *.yaml, *.yml)", &["json", "yaml", "yml"]);
    #[cfg(windows)]
    let picker = picker.add_filter("All files", &["*"]);
    picker.set_parent(&window).pick_file(move |path| {
        let _ = sender.send(path);
    });
    let path = receiver.await.ok().flatten()?;
    path.into_path().ok().map(|p| p.to_string_lossy().into_owned())
}

/// Saves. With OAuth, first the sign-in: if it fails, nothing is saved.
#[tauri::command]
pub async fn api_save(app: AppHandle, window: WebviewWindow, form: ApiForm) -> Result<(), String> {
    let hub = apis(&app)?;
    let (mut plan, target) = {
        let editors = editors();
        let open = editors.api.as_ref().filter(|_| window.label() == API).ok_or("This window is no longer valid.")?;
        (form.plan(open), open.target.as_ref().map(|t| t.id.clone()))
    };
    if let Some(problem) = plan.draft.problem() {
        return Err(problem);
    }
    if plan.needs_sign_in {
        let (turn, cancel) = begin(API);
        let status_window = window.clone();
        let status = move |text: &str| {
            let _ = status_window.emit_to(API, "api-status", text);
        };
        let result = tokio::select! {
            result = hub.sign_in(&plan.draft.auth, form.client(), target.as_deref(), Some(&status)) => Some(result),
            _ = cancel.cancelled() => None,
        };
        done(API, turn);
        let _ = window.set_focus(); // from the browser, back here
        match result {
            None => return Err(STOPPED.into()),
            Some(Err(e)) if e.canceled => return Err(STOPPED.into()),
            Some(Err(e)) => return Err(e.message),
            Some(Ok(grant)) => plan.draft.grant = Some(grant),
        }
    }
    match target {
        None => hub.add(plan.draft).map(|_| ()),
        Some(id) => hub.edit(&id, plan.draft).await.map(|_| ()),
    }?;
    let _ = window.destroy();
    Ok(())
}

#[tauri::command]
pub async fn api_enable(app: AppHandle, id: String, on: bool) {
    let Ok(hub) = apis(&app) else { return };
    if let Err(problem) = hub.set_enabled(&id, on).await {
        tell(&app, &problem);
    }
}

#[tauri::command]
pub fn api_remove(app: AppHandle, id: String) {
    let Ok(hub) = apis(&app) else { return };
    let Some(entry) = hub.get(&id) else { return };
    let text = format!(
        "Agents will no longer see “{}”, the permissions they had end, and its key or token is deleted from {THIS_COMPUTER}.",
        entry.name
    );
    confirm_remove(&app, &entry.name, text, move |app| async move {
        if let Err(problem) = hub.remove(&id).await {
            tell(&app, &problem);
        }
    });
}

// =========================================================================
// An endpoint
// =========================================================================

/// The dialog of an endpoint, modal over the APIs window. `tag` comes back as it is with the
/// result (`endpoint-result`), so the page knows which row it was.
#[tauri::command]
pub async fn endpoint_open(app: AppHandle, window: WebviewWindow, endpoint: Option<Value>, taken: Vec<String>, tag: Value) -> Result<(), String> {
    if window.label() != API {
        return Err("This window is no longer valid.".into());
    }
    if let Some(open) = app.get_webview_window(ENDPOINT) {
        let _ = open.set_focus();
        return Ok(());
    }
    editors().endpoint = Some(EndpointOpen { owner: API.into(), endpoint, taken, tag });
    match dialog(&app, ENDPOINT, "endpoint.html", "Clak — Endpoint", (820.0, 560.0), Some(&window), false) {
        Ok(dialog) => {
            watch(&dialog);
            let _ = window.set_enabled(false);
            Ok(())
        }
        Err(e) => {
            editors().endpoint = None;
            Err(e)
        }
    }
}

#[tauri::command]
pub fn endpoint_draft(window: WebviewWindow) -> Option<Value> {
    if window.label() != ENDPOINT {
        return None;
    }
    let editors = editors();
    let open = editors.endpoint.as_ref()?;
    Some(json!({
        "endpoint": open.endpoint,
        "methods": METHODS,
        "places": ParamIn::ALL.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
        "types": ParamType::ALL.iter().map(|t| t.as_str()).collect::<Vec<_>>(),
    }))
}

/// The endpoint as it is, with the automatic name if it has none; and what is wrong.
fn endpoint_problem(node: &Value, taken: &[String]) -> (Option<ApiEndpoint>, Option<String>, String) {
    let Some(mut endpoint) = endpoint_of(node) else {
        return (None, Some("The endpoint isn't valid.".into()), String::new());
    };
    let auto = ApiEndpoint::name_from(&endpoint.method, endpoint.path.trim());
    endpoint.path = endpoint.path.trim().to_owned();
    endpoint.name = trimmed(&endpoint.name).unwrap_or_else(|| auto.clone());
    let problem = endpoint
        .problem()
        .or_else(|| taken.contains(&endpoint.name).then(|| format!("Another endpoint is called “{}”.", endpoint.name)));
    (Some(endpoint), problem, auto)
}

#[tauri::command]
pub fn endpoint_check(window: WebviewWindow, endpoint: Value) -> Value {
    let taken = editors().endpoint.as_ref().filter(|_| window.label() == ENDPOINT).map(|e| e.taken.clone()).unwrap_or_default();
    let (_, problem, auto) = endpoint_problem(&endpoint, &taken);
    json!({ "problem": problem, "auto": auto })
}

/// OK: the endpoint goes back to the APIs window, and the dialog closes.
#[tauri::command]
pub fn endpoint_done(app: AppHandle, window: WebviewWindow, endpoint: Value) -> Result<(), String> {
    let (owner, taken, tag) = {
        let editors = editors();
        let open = editors.endpoint.as_ref().filter(|_| window.label() == ENDPOINT).ok_or("This window is no longer valid.")?;
        (open.owner.clone(), open.taken.clone(), open.tag.clone())
    };
    let (endpoint, problem, _) = endpoint_problem(&endpoint, &taken);
    if let Some(problem) = problem {
        return Err(problem);
    }
    let endpoint = endpoint.map(|e| e.to_json());
    let _ = app.emit_to(owner.as_str(), "endpoint-result", json!({ "tag": tag, "endpoint": endpoint }));
    let _ = window.destroy();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(url: &str, oauth: bool) -> ConnectorEntry {
        ConnectorEntry {
            id: "c1".into(),
            name: "Notion".into(),
            url: url.into(),
            enabled: true,
            since: 0.0,
            auth: if oauth { ConnectorEntry::OAUTH_AUTH } else { ConnectorEntry::TOKEN_AUTH }.into(),
            command: None,
            launch: None,
        }
    }

    fn open(target: Option<ConnectorEntry>, had_credential: bool, from_link: bool, token: Option<&str>) -> ConnectorOpen {
        ConnectorOpen {
            draft: ConnectorDraft { from_link, token: token.map(Into::into), ..ConnectorDraft::default() },
            opened: Instant::now() - ARMING,
            target,
            had_credential,
            env_names: vec![],
        }
    }

    fn form(url: &str, oauth: bool) -> ConnectorForm {
        ConnectorForm { name: "Notion".into(), url: url.into(), oauth, ..ConnectorForm::default() }
    }

    #[test]
    fn un_connettore_nuovo_con_oauth_chiede_l_accesso() {
        let plan = form("https://mcp.notion.com/mcp", true).plan(&open(None, false, false, None));
        assert!(plan.needs_sign_in && !plan.token_changes && !plan.permissions_restart);
        assert_eq!(plan.problem, None);
        let plan = form("https://mcp.notion.com/mcp", false).plan(&open(None, false, false, None));
        assert!(!plan.needs_sign_in && plan.token_changes);
    }

    #[test]
    fn rinominare_non_tocca_l_accesso_ne_i_permessi() {
        let target = entry("https://mcp.notion.com/mcp", true);
        let mut f = form("https://mcp.notion.com/mcp", true);
        f.name = "Notion lavoro".into();
        let plan = f.plan(&open(Some(target.clone()), true, false, None));
        assert!(!plan.needs_sign_in && !plan.permissions_restart && plan.show_reauth);
        // Another host: the sign-in is redone, and permissions start over.
        let plan = form("https://altro.it/mcp", true).plan(&open(Some(target), true, false, None));
        assert!(plan.needs_sign_in && plan.permissions_restart && plan.origin_changed && !plan.show_reauth);
    }

    #[test]
    fn il_token_salvato_resta_se_la_casella_e_vuota() {
        let target = entry("https://x.it/mcp", false);
        let plan = form("https://x.it/mcp", false).plan(&open(Some(target.clone()), true, false, None));
        assert!(plan.keeps_token && !plan.token_changes && !plan.permissions_restart);
        let mut f = form("https://x.it/mcp", false);
        f.drop_token = true;
        assert!(f.plan(&open(Some(target), true, false, None)).token_changes);
    }

    #[test]
    fn un_link_su_un_connettore_che_c_e_cambia_solo_l_accesso() {
        let target = entry("https://x.it/mcp", false);
        let o = open(Some(target), true, true, Some("abc"));
        assert!(o.restricted());
        let mut f = form("https://x.it/mcp", false);
        f.name = String::new(); // name and address don't matter: they aren't changed
        f.link_token = true;
        let plan = f.plan(&o);
        assert_eq!(plan.problem, None);
        assert!(plan.token_changes);
        assert_eq!(f.token(&o).as_deref(), Some("abc"));
        f.link_token = false;
        assert_eq!(f.token(&o), None);
    }

    #[test]
    fn da_un_link_il_salvataggio_aspetta_un_attimo() {
        let mut o = open(None, false, true, None);
        o.opened = Instant::now();
        assert!(!o.armed());
        assert!(open(None, false, false, None).armed());
    }

    fn program(line: &str, env: &str) -> ConnectorForm {
        ConnectorForm { name: "Browser".into(), stdio: true, launch: line.into(), env: env.into(), ..ConnectorForm::default() }
    }

    #[test]
    fn un_programma_nuovo_con_le_sue_variabili() {
        let plan = program("agent-browser mcp --tools core", "# chiavi\nAPI_KEY = abc\n\nDEBUG=1").program_plan(&open(None, false, false, None));
        assert_eq!(plan.problem, None);
        assert_eq!(plan.launch.unwrap().args, ["mcp", "--tools", "core"]);
        let env = plan.env.unwrap();
        assert_eq!((env["API_KEY"].as_str(), env["DEBUG"].as_str()), ("abc", "1"));
        assert!(plan.program_changes && !plan.permissions_restart);
        // A line without =: the error doesn't repeat it, it might be a key.
        let plan = program("x", "sk-segreta").program_plan(&open(None, false, false, None));
        assert_eq!(plan.problem.as_deref(), Some("Environment, line 1: write NAME=value."));
        assert!(program("", "").program_plan(&open(None, false, false, None)).problem.is_some());
    }

    #[test]
    fn un_programma_salvato_tiene_le_variabili_se_la_casella_e_vuota() {
        let mut target = entry("", false);
        target.launch = Some(Launch::parse("agent-browser mcp").unwrap());
        let mut o = open(Some(target.clone()), false, false, None);
        o.env_names = vec!["API_KEY".into()];
        let plan = program("agent-browser mcp", "").program_plan(&o);
        assert!(plan.keeps_env && plan.env.is_none() && !plan.program_changes && !plan.permissions_restart);
        let plan = program("agent-browser mcp --tools all", "").program_plan(&o);
        assert!(plan.program_changes && plan.permissions_restart);
        let mut f = program("agent-browser mcp", "");
        f.drop_env = true;
        let plan = f.program_plan(&o);
        assert_eq!(plan.env, Some(BTreeMap::new()));
        assert!(plan.permissions_restart);
        // An address doesn't become a program, and a link doesn't add one.
        assert!(program("x", "").program_plan(&open(Some(entry("https://x.it/mcp", false)), false, false, None)).problem.is_some());
        assert!(program("x", "").program_plan(&open(None, false, true, None)).problem.is_some());
    }

    fn api_open(target: Option<ApiEntry>, had_secret: bool) -> ApiOpen {
        ApiOpen { target, had_secret, sign_in: false }
    }

    fn api_entry(auth: ApiAuth) -> ApiEntry {
        ApiEntry {
            id: "a1".into(),
            name: "Pets".into(),
            base_url: "https://api.pets.it/v1".into(),
            enabled: true,
            since: 0.0,
            auth,
            endpoints: vec![ApiEndpoint::new("list", "GET", "/pets", None, vec![])],
            command: None,
            source: None,
        }
    }

    fn api_form(kind: &str) -> ApiForm {
        ApiForm {
            name: "Pets".into(),
            base_url: "https://api.pets.it/v1/".into(),
            auth: AuthForm { kind: kind.into(), ..AuthForm::default() },
            endpoints: vec![ApiEndpoint::new("list", "GET", "/pets", None, vec![]).to_json()],
            ..ApiForm::default()
        }
    }

    #[test]
    fn un_api_con_il_segreto_salvato_lo_tiene() {
        let target = api_entry(ApiAuth::new(AuthKind::Bearer));
        let plan = api_form("bearer").plan(&api_open(Some(target.clone()), true));
        assert!(plan.keeps_secret && plan.draft.secret.is_none() && !plan.permissions_restart);
        assert_eq!(plan.draft.problem(), None);
        // Switching to no sign-in removes the secret, and permissions start over.
        let plan = api_form("none").plan(&api_open(Some(target), true));
        assert_eq!(plan.draft.secret.as_deref(), Some(""));
        assert!(plan.permissions_restart);
    }

    #[test]
    fn un_api_oauth_nuova_chiede_l_accesso() {
        let mut f = api_form("oauth");
        f.auth.authorization_url = "https://x.it/authorize".into();
        f.auth.token_url = "https://x.it/token".into();
        f.auth.client_id = "app".into();
        let plan = f.plan(&api_open(None, false));
        assert!(plan.needs_sign_in);
        assert_eq!(plan.draft.problem(), None);
        assert!(f.client().is_none());
        f.client_secret = "s".into();
        assert!(f.client().is_some());
    }

    #[test]
    fn un_endpoint_senza_nome_prende_quello_automatico() {
        let node = json!({ "name": "", "method": "GET", "path": "/users/{id}", "parameters": [{ "name": "id", "in": "path" }] });
        let (endpoint, problem, auto) = endpoint_problem(&node, &[]);
        assert_eq!(problem, None);
        assert_eq!(auto, "get_users_id");
        assert_eq!(endpoint.unwrap().name, "get_users_id");
        let (_, problem, _) = endpoint_problem(&node, &["get_users_id".into()]);
        assert_eq!(problem.as_deref(), Some("Another endpoint is called “get_users_id”."));
        let missing = json!({ "name": "x", "method": "GET", "path": "/users/{id}" });
        assert!(endpoint_problem(&missing, &[]).1.is_some());
    }
}

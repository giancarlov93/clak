//! OAuth for MCP servers, as the MCP specification (Authorization) requires: a native
//! client, with the user's browser and a loopback redirect (RFC 8252).
//!
//! 1. discovery: from the server's 401 to the resource metadata (RFC 9728) and from there
//!    to the authorization server's (RFC 8414, or OpenID);
//! 2. the client: the one the user registered by hand, or one registered now
//!    (RFC 7591), reused on later sign-ins;
//! 3. the sign-in in the browser, with PKCE S256, `state`, `resource` (RFC 8707) and, if
//!    the server declares it, the `iss` check (RFC 9207);
//! 4. refreshing with the refresh token, and revoking when the connector is removed.
//!
//! All the service's addresses must be https (or http on loopback): a code or a token
//! never travel in the clear. The browser opens only on a user gesture in the panel,
//! never from a connector working on its own: that's why the app decides how to open it
//! ([`OpenBrowser`]).
//!
//! For REST APIs there are the variants without discovery ([`known`]): the addresses come
//! from an OpenAPI or the user, no dynamic registration, no `resource`; and the "app
//! only" sign-in ([`client_credentials`]).

use crate::BoxFuture;
use crate::loopback::LoopbackCallback;
use crate::mcp_http::{PROTOCOL_VERSION, network_message, www_authenticate};
use crate::text::escape_data;
use crate::uri::{self, absolute, secure};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use clak_hub::texts::clean;
use clak_wire::json as wire;
use regex::Regex;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use url::Url;

pub const CALLBACK_PATH: &str = "/callback";
const MAX_DOCUMENT: usize = 1 << 20;

/// How long to wait, usually, for the user to finish signing in in the browser.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How the sign-in page is opened: the app decides (the default browser; in tests, an
/// HTTP request that follows the page by itself). An error stops the sign-in.
pub type OpenBrowser = Arc<dyn Fn(Url) -> BoxFuture<'static, Result<(), OAuthError>> + Send + Sync>;

/// How far the sign-in has got, for the panel: one line at a time.
pub type Status<'a> = Option<&'a (dyn Fn(&str) + Send + Sync)>;

/// The OAuth client registered by hand by the user in the service's settings, for
/// servers that don't allow dynamic registration.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthClient {
    pub id: String,
    pub secret: Option<String>,
}

impl OAuthClient {
    pub fn new(id: impl Into<String>, secret: Option<String>) -> OAuthClient {
        OAuthClient {
            id: id.into(),
            secret,
        }
    }
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OAuthClient({})", self.id)
    }
}

/// A completed OAuth sign-in: what's needed to use and refresh it. Stored encrypted
/// like a token, and never printed (`Debug` doesn't show the secrets).
#[derive(Clone, PartialEq)]
pub struct OAuthGrant {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: Option<String>,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub client_auth: String,
    pub registered: bool,
    pub redirect_uri: String,
    pub resource: String,
    pub scope: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<f64>,
    pub flow: Option<String>,
}

impl OAuthGrant {
    /// The "app only" sign-in (client credentials): no browser, and it refreshes by asking for a new token.
    pub const CLIENT_FLOW: &'static str = "client";

    pub fn is_client_flow(&self) -> bool {
        self.flow.as_deref() == Some(Self::CLIENT_FLOW)
    }

    /// The sign-in service's computer, for the panel.
    pub fn host(&self) -> String {
        absolute(&self.issuer)
            .map(|u| uri::host(&u))
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| self.issuer.clone())
    }

    /// The saved (encrypted) form in `connectors.secrets`.
    pub fn to_json(&self) -> Value {
        json!({
            "issuer": self.issuer,
            "authorization_endpoint": self.authorization_endpoint,
            "token_endpoint": self.token_endpoint,
            "revocation_endpoint": self.revocation_endpoint,
            "client_id": self.client_id,
            "client_secret": self.client_secret,
            "client_auth": self.client_auth,
            "registered": self.registered,
            "redirect_uri": self.redirect_uri,
            "resource": self.resource,
            "scope": self.scope,
            "access_token": self.access_token,
            "refresh_token": self.refresh_token,
            "expires_at": self.expires_at.map(wire::float).unwrap_or(Value::Null),
            "flow": self.flow,
        })
    }

    pub fn from_json(node: &Value) -> Option<OAuthGrant> {
        let o = node.as_object()?;
        let s = |key: &str| wire::str(o.get(key)).map(str::to_owned);
        Some(OAuthGrant {
            issuer: s("issuer")?,
            authorization_endpoint: s("authorization_endpoint")?,
            token_endpoint: s("token_endpoint")?,
            revocation_endpoint: s("revocation_endpoint"),
            client_id: s("client_id")?,
            client_secret: s("client_secret"),
            client_auth: s("client_auth").unwrap_or_else(|| "none".into()),
            registered: wire::truthy(o.get("registered")),
            redirect_uri: s("redirect_uri")?,
            resource: s("resource")?,
            scope: s("scope"),
            access_token: s("access_token")?,
            refresh_token: s("refresh_token"),
            expires_at: o.get("expires_at").and_then(Value::as_f64),
            flow: s("flow"),
        })
    }
}

impl fmt::Debug for OAuthGrant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "OAuthGrant({})", self.issuer)
    }
}

/// A sign-in that failed. `rejected`: the service said no (sign-in revoked, client no
/// longer registered), and a new sign-in by the user is needed; otherwise it's a failure
/// that may pass. `network`: the service wasn't even reached (the network, a name that
/// doesn't resolve); the caller words it its own way ("not reachable: …").
/// `canceled`: the user canceled from the panel; there's nothing to show them.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct OAuthError {
    pub message: String,
    pub rejected: bool,
    pub network: bool,
    pub canceled: bool,
}

impl OAuthError {
    pub fn new(message: impl Into<String>) -> OAuthError {
        OAuthError {
            message: message.into(),
            rejected: false,
            network: false,
            canceled: false,
        }
    }

    pub fn rejected(message: impl Into<String>) -> OAuthError {
        OAuthError {
            message: message.into(),
            rejected: true,
            network: false,
            canceled: false,
        }
    }

    /// The sign-in canceled by whoever asked for it.
    pub fn canceled() -> OAuthError {
        OAuthError {
            message: "Sign-in canceled.".into(),
            rejected: false,
            network: false,
            canceled: true,
        }
    }

    fn network(error: &reqwest::Error) -> OAuthError {
        OAuthError {
            message: network_message(error),
            rejected: false,
            network: true,
            canceled: false,
        }
    }
}

/// The challenge of a 401: `WWW-Authenticate: Bearer resource_metadata="…", scope="…"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthChallenge {
    pub resource_metadata: Option<String>,
    pub scope: Option<String>,
    pub error: Option<String>,
}

static PARAM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"([A-Za-z0-9_\-]+)\s*=\s*(?:"((?:[^"\\]|\\.)*)"|([^\s,]+))"#).expect("regex")
});

impl OAuthChallenge {
    /// The first Bearer challenge among a response's `WWW-Authenticate` headers.
    pub fn from_headers(headers: &HeaderMap) -> Option<OAuthChallenge> {
        www_authenticate(headers).find_map(OAuthChallenge::parse)
    }

    /// The Bearer challenge of a header, which may contain more than one (`Basic …, Bearer …`).
    pub fn parse(header: &str) -> Option<OAuthChallenge> {
        let parameters = challenges(header)
            .into_iter()
            .find(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))?
            .1;
        let mut values: HashMap<String, String> = HashMap::new();
        for m in PARAM.captures_iter(&parameters) {
            let key = m[1].to_ascii_lowercase();
            let value = match (m.get(2), m.get(3)) {
                (Some(quoted), _) => unescape_quoted(quoted.as_str()),
                (None, Some(plain)) => plain.as_str().to_owned(),
                _ => String::new(),
            };
            values.entry(key).or_insert(value);
        }
        Some(OAuthChallenge {
            resource_metadata: values.remove("resource_metadata"),
            scope: values.remove("scope"),
            error: values.remove("error"),
        })
    }
}

/// The challenges of a header: (scheme, parameters). Commas inside quotes don't count;
/// after a comma, a word not followed by `=` starts a new challenge.
fn challenges(header: &str) -> Vec<(String, String)> {
    let mut items = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for c in header.chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' && quoted {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
        } else if c == ',' && !quoted {
            items.push(std::mem::take(&mut current));
            continue;
        }
        current.push(c);
    }
    items.push(current);

    let mut found: Vec<(String, String)> = Vec::new();
    for item in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
        let token_end = item
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(item.len());
        let (token, rest) = item.split_at(token_end);
        let starts = !rest.trim_start().starts_with('=');
        if starts {
            found.push((token.to_owned(), rest.trim().to_owned()));
        } else if let Some((_, parameters)) = found.last_mut() {
            if !parameters.is_empty() {
                parameters.push_str(", ");
            }
            parameters.push_str(item);
        }
    }
    found
}

fn unescape_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Where and how to sign in for an MCP server: what discovery found.
#[derive(Debug, Clone, PartialEq)]
pub struct OAuthServer {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub registration_endpoint: Option<String>,
    pub revocation_endpoint: Option<String>,
    pub auth_methods: Vec<String>,
    pub iss_parameter: bool,
    pub resource: String,
    pub scope: Option<String>,
}

impl OAuthServer {
    pub fn host(&self) -> String {
        absolute(&self.issuer)
            .map(|u| uri::host(&u))
            .unwrap_or_else(|| self.issuer.clone())
    }

    fn supports(&self, method: &str) -> bool {
        self.auth_methods.iter().any(|m| m == method)
    }
}

// =========================================================================
// Discovery
// =========================================================================

pub async fn discover(
    http: &Client,
    server: &Url,
    challenge: Option<&OAuthChallenge>,
) -> Result<OAuthServer, OAuthError> {
    let origin = uri::origin(server);
    let path = server.path().trim_end_matches('/');

    // The resource metadata: the one the 401 points to, then the two well-known places.
    let mut candidates: Vec<String> = Vec::new();
    if let Some(pointed) = challenge.and_then(|c| c.resource_metadata.as_deref())
        && absolute(pointed).is_some_and(|u| secure(&u))
    {
        candidates.push(pointed.to_owned());
    }
    if !path.is_empty() {
        candidates.push(format!(
            "{origin}/.well-known/oauth-protected-resource{path}"
        ));
    }
    candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
    let mut seen = Vec::new();
    candidates.retain(|c| {
        let fresh = !seen.contains(c);
        seen.push(c.clone());
        fresh
    });

    let mut resource_meta: Option<Map<String, Value>> = None;
    for candidate in &candidates {
        resource_meta = get_json(http, candidate).await?;
        if resource_meta.is_some() {
            break;
        }
    }

    let mut resource = canonical(server);
    let issuer: String;
    let mut scope = challenge.and_then(|c| c.scope.clone());
    if let Some(meta) = &resource_meta {
        if let Some(declared) = wire::str(meta.get("resource")).filter(|d| !d.is_empty()) {
            if !resource_allowed(declared, server) {
                return Err(OAuthError::new(format!(
                    "The service's sign-in information is for another address ({}).",
                    shorten(declared)
                )));
            }
            resource = declared.to_owned();
        }
        issuer = meta
            .get("authorization_servers")
            .and_then(Value::as_array)
            .and_then(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .find(|s| !s.is_empty())
            })
            .map(str::to_owned)
            .ok_or_else(|| OAuthError::new("The server doesn't say where to sign in."))?;
        if scope.is_none()
            && let Some(Value::Array(scopes)) = meta.get("scopes_supported")
            && !scopes.is_empty()
        {
            scope = Some(
                scopes
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
    } else {
        // A server of the previous specification (2025-03-26): the sign-in service is the server itself.
        issuer = origin.clone();
    }
    let issuer_url = absolute(&issuer).filter(secure).ok_or_else(|| {
        OAuthError::new(format!(
            "The sign-in service isn't on https ({}): codes and tokens would travel unencrypted.",
            shorten(&issuer)
        ))
    })?;
    let issuer_host = uri::host(&issuer_url);

    let meta = authorization_server(http, &issuer_url).await?.ok_or_else(|| {
        OAuthError::new(if resource_meta.is_none() {
            "This server doesn't support signing in (OAuth): use a token.".to_owned()
        } else {
            format!("The sign-in service at {issuer_host} doesn't describe itself (no OAuth metadata).")
        })
    })?;

    if !same_issuer(wire::str(meta.get("issuer")), Some(&issuer)) {
        return Err(OAuthError::new(format!(
            "The sign-in service at {issuer_host} declares a different identity: refused."
        )));
    }
    let authorize = endpoint(&meta, "authorization_endpoint")?
        .ok_or_else(|| OAuthError::new("The sign-in service has no authorization address."))?;
    let token = endpoint(&meta, "token_endpoint")?
        .ok_or_else(|| OAuthError::new("The sign-in service has no token address."))?;
    let pkce = matches!(meta.get("code_challenge_methods_supported"), Some(Value::Array(methods)) if methods.iter().any(|m| m.as_str() == Some("S256")));
    if !pkce {
        return Err(OAuthError::new(
            "The sign-in service doesn't support PKCE (S256), which MCP requires: refused.",
        ));
    }
    if let Some(Value::Array(types)) = meta.get("response_types_supported")
        && !types.iter().any(|t| t.as_str() == Some("code"))
    {
        return Err(OAuthError::new(
            "The sign-in service doesn't support the authorization code flow.",
        ));
    }

    let auth = match meta.get("token_endpoint_auth_methods_supported") {
        Some(Value::Array(supported)) => supported
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => vec!["client_secret_basic".to_owned()], // the RFC 8414 default
    };
    Ok(OAuthServer {
        issuer,
        authorization_endpoint: authorize,
        token_endpoint: token,
        registration_endpoint: endpoint(&meta, "registration_endpoint")?,
        revocation_endpoint: endpoint(&meta, "revocation_endpoint")?,
        auth_methods: auth,
        iss_parameter: wire::truthy(meta.get("authorization_response_iss_parameter_supported")),
        resource,
        scope: scope.filter(|s| !s.is_empty()),
    })
}

/// The sign-in service's metadata: OAuth (RFC 8414) or OpenID, with and without a path.
async fn authorization_server(
    http: &Client,
    issuer: &Url,
) -> Result<Option<Map<String, Value>>, OAuthError> {
    let origin = uri::origin(issuer);
    let path = issuer.path().trim_end_matches('/');
    let candidates = if !path.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{path}"),
            format!("{origin}/.well-known/openid-configuration{path}"),
            format!("{origin}{path}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    };
    for candidate in &candidates {
        if let Some(meta) = get_json(http, candidate).await? {
            return Ok(Some(meta));
        }
    }
    Ok(None)
}

/// A JSON document, or `None` if there is none (404 and the like). No redirects.
async fn get_json(http: &Client, url: &str) -> Result<Option<Map<String, Value>>, OAuthError> {
    let response = http
        .get(url)
        .header(ACCEPT, "application/json")
        .header("MCP-Protocol-Version", PROTOCOL_VERSION)
        .send()
        .await
        .map_err(|e| OAuthError::network(&e))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    Ok(json_object(&read_limited(response).await?))
}

// =========================================================================
// The sign-in
// =========================================================================

/// The sign-in in the user's browser. `previous` is the previous sign-in, if any: its
/// client (and its port) are reused instead of registering another one.
#[allow(clippy::too_many_arguments)]
pub async fn sign_in(
    http: &Client,
    server: &Url,
    challenge: Option<&OAuthChallenge>,
    client: Option<&OAuthClient>,
    previous: Option<&OAuthGrant>,
    open_browser: &OpenBrowser,
    status: Status<'_>,
    timeout: Duration,
) -> Result<OAuthGrant, OAuthError> {
    say(status, "Looking for the service's sign-in…");
    let found = discover(http, server, challenge).await?;
    authorize(
        http,
        &found,
        client,
        previous,
        open_browser,
        status,
        timeout,
    )
    .await
}

/// A sign-in service whose addresses are already known (those of an OpenAPI, or
/// written by the user): no discovery, no dynamic registration, no `resource`.
/// The addresses must be https (or http on loopback).
pub fn known(authorize: &str, token: &str, scope: Option<&str>) -> Result<OAuthServer, OAuthError> {
    for (what, text) in [("sign-in", authorize), ("token", token)] {
        if !absolute(text).is_some_and(|u| secure(&u)) {
            return Err(OAuthError::new(format!(
                "The {what} address must be https: codes and tokens would travel unencrypted."
            )));
        }
    }
    let issuer = uri::origin(&absolute(authorize).expect("checked above"));
    Ok(OAuthServer {
        issuer,
        authorization_endpoint: authorize.to_owned(),
        token_endpoint: token.to_owned(),
        registration_endpoint: None,
        revocation_endpoint: None,
        auth_methods: vec!["client_secret_post".to_owned()],
        iss_parameter: false,
        resource: String::new(),
        scope: scope.filter(|s| !s.is_empty()).map(str::to_owned),
    })
}

/// The sign-in in the browser, with a service already found: PKCE, `state`, loopback return.
/// The limit is `timeout`; canceling (from the panel) is dropping the future.
pub async fn authorize(
    http: &Client,
    found: &OAuthServer,
    client: Option<&OAuthClient>,
    previous: Option<&OAuthGrant>,
    open_browser: &OpenBrowser,
    status: Status<'_>,
    timeout: Duration,
) -> Result<OAuthGrant, OAuthError> {
    let reuse = previous.filter(|p| {
        same_issuer(Some(&p.issuer), Some(&found.issuer)) && client.is_none() && !p.is_client_flow()
    });

    let preferred = reuse
        .and_then(|r| absolute(&r.redirect_uri))
        .and_then(|u| u.port_or_known_default())
        .unwrap_or(0);
    let callback = LoopbackCallback::start(preferred)
        .await
        .map_err(|e| OAuthError::new(e.to_string()))?;
    let redirect = format!("http://127.0.0.1:{}{CALLBACK_PATH}", callback.port());

    let (client_id, client_secret, client_auth, registered);
    if let Some(client) = client {
        client_id = client.id.clone();
        client_secret = client.secret.clone().filter(|s| !s.is_empty());
        registered = false;
        client_auth = if client_secret.is_none() {
            "none".to_owned()
        } else if found.supports("client_secret_basic") {
            "client_secret_basic".to_owned()
        } else {
            "client_secret_post".to_owned()
        };
    } else if let Some(reuse) = reuse.filter(|r| !r.registered || r.redirect_uri == redirect) {
        client_id = reuse.client_id.clone();
        client_secret = reuse.client_secret.clone();
        client_auth = reuse.client_auth.clone();
        registered = reuse.registered;
    } else if let Some(registration) = &found.registration_endpoint {
        say(status, &format!("Registering Clak with {}…", found.host()));
        (client_id, client_secret, client_auth) =
            register(http, registration, &redirect, found).await?;
        registered = true;
    } else {
        return Err(OAuthError::new(format!(
            "{} doesn't let apps register by themselves: create an OAuth client in the service's settings \
             (redirect URI http://127.0.0.1{CALLBACK_PATH}, any port) and enter its client ID under “OAuth client”.",
            found.host()
        )));
    }

    let verifier = random_text(32);
    let state = random_text(24);
    let mut query: Vec<(&str, String)> = vec![
        ("response_type", "code".into()),
        ("client_id", client_id.clone()),
        ("redirect_uri", redirect.clone()),
        (
            "code_challenge",
            URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
        ),
        ("code_challenge_method", "S256".into()),
        ("state", state.clone()),
    ];
    if !found.resource.is_empty() {
        query.push(("resource", found.resource.clone()));
    }
    if let Some(scope) = &found.scope {
        query.push(("scope", scope.clone()));
    }
    let separator = if found.authorization_endpoint.contains('?') {
        "&"
    } else {
        "?"
    };
    let joined: Vec<String> = query
        .iter()
        .map(|(k, v)| format!("{}={}", escape_data(k), escape_data(v)))
        .collect();
    let page = Url::parse(&format!(
        "{}{separator}{}",
        found.authorization_endpoint,
        joined.join("&")
    ))
    .map_err(|_| OAuthError::new("The sign-in address isn't a web address: refused."))?;

    let host = found.host();
    say(
        status,
        &format!("Opening your browser to sign in at {host}…"),
    );
    open_browser(page).await?;
    say(
        status,
        &format!("Waiting for you to sign in at {host}, in your browser…"),
    );

    let answer = tokio::time::timeout(timeout, callback.wait(&state))
        .await
        .map_err(|_| {
            OAuthError::new(format!(
                "No sign-in within {:.0} minutes: try again.",
                timeout.as_secs_f64() / 60.0
            ))
        })?;
    drop(callback);

    if let Some(error) = answer.get("error") {
        return Err(OAuthError::new(if error == "access_denied" {
            "Sign-in was refused or cancelled in the browser.".to_owned()
        } else {
            format!(
                "The sign-in service answered with an error: {}.",
                shorten(answer.get("error_description").unwrap_or(error))
            )
        }));
    }
    let wrong_issuer = match answer.get("iss") {
        Some(iss) => !same_issuer(Some(iss), Some(&found.issuer)),
        None => found.iss_parameter,
    };
    if wrong_issuer {
        return Err(OAuthError::new(
            "The sign-in answer doesn't come from the expected service: refused.",
        ));
    }
    let code = answer
        .get("code")
        .filter(|c| !c.is_empty())
        .ok_or_else(|| OAuthError::new("The sign-in answer has no code."))?;

    say(status, "Finishing the sign-in…");
    let grant = OAuthGrant {
        issuer: found.issuer.clone(),
        authorization_endpoint: found.authorization_endpoint.clone(),
        token_endpoint: found.token_endpoint.clone(),
        revocation_endpoint: found.revocation_endpoint.clone(),
        client_id,
        client_secret,
        client_auth,
        registered,
        redirect_uri: redirect.clone(),
        resource: found.resource.clone(),
        scope: found.scope.clone(),
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        flow: None,
    };
    let form = with_resource(
        vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.clone()),
            ("redirect_uri".into(), redirect),
            ("code_verifier".into(), verifier),
        ],
        &found.resource,
    );
    let tokens = token_request(http, &grant, form).await?;
    with(grant, &tokens)
}

/// The "app only" sign-in (RFC 6749, 4.4): the client presents its secret and receives a
/// token, with no user and no browser. It refreshes by doing it again.
pub async fn client_credentials(
    http: &Client,
    found: &OAuthServer,
    client: &OAuthClient,
) -> Result<OAuthGrant, OAuthError> {
    let secret = client
        .secret
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthError::new("The app-only sign-in needs the client secret."))?;
    let grant = OAuthGrant {
        issuer: found.issuer.clone(),
        authorization_endpoint: found.authorization_endpoint.clone(),
        token_endpoint: found.token_endpoint.clone(),
        revocation_endpoint: found.revocation_endpoint.clone(),
        client_id: client.id.clone(),
        client_secret: Some(secret),
        client_auth: "client_secret_post".into(),
        registered: false,
        redirect_uri: String::new(),
        resource: found.resource.clone(),
        scope: found.scope.clone(),
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        flow: Some(OAuthGrant::CLIENT_FLOW.into()),
    };
    client_token(http, grant).await
}

async fn client_token(http: &Client, grant: OAuthGrant) -> Result<OAuthGrant, OAuthError> {
    let mut form = vec![("grant_type".to_owned(), "client_credentials".to_owned())];
    if let Some(scope) = grant.scope.as_ref().filter(|s| !s.is_empty()) {
        form.push(("scope".into(), scope.clone()));
    }
    let form = with_resource(form, &grant.resource);
    let tokens = token_request(http, &grant, form).await?;
    with(grant, &tokens)
}

/// A new access token with the refresh token. `rejected` if a new sign-in is needed.
pub async fn refresh(http: &Client, grant: &OAuthGrant) -> Result<OAuthGrant, OAuthError> {
    if grant.is_client_flow() {
        return client_token(http, grant.clone()).await;
    }
    let refresh = grant
        .refresh_token
        .clone()
        .ok_or_else(|| OAuthError::rejected("the sign-in has expired"))?;
    let form = with_resource(
        vec![
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh),
        ],
        &grant.resource,
    );
    let tokens = token_request(http, grant, form).await?;
    with(grant.clone(), &tokens)
}

/// `resource` (RFC 8707) only if present: an arbitrary service, outside MCP, might not know it.
fn with_resource(mut form: Vec<(String, String)>, resource: &str) -> Vec<(String, String)> {
    if !resource.is_empty() {
        form.push(("resource".into(), resource.to_owned()));
    }
    form
}

/// Revokes the refresh token (or the access token), if the service allows it. A failure doesn't count:
/// the connector is removed anyway, revoking is a courtesy to the service.
pub async fn revoke(http: &Client, grant: &OAuthGrant) {
    let Some(endpoint) = grant
        .revocation_endpoint
        .as_deref()
        .and_then(absolute)
        .filter(secure)
    else {
        return;
    };
    let (token, hint) = match &grant.refresh_token {
        Some(refresh) => (refresh.clone(), "refresh_token"),
        None => (grant.access_token.clone(), "access_token"),
    };
    let request = form(
        http,
        &endpoint,
        grant,
        vec![
            ("token".into(), token),
            ("token_type_hint".into(), hint.into()),
        ],
    );
    let _ = request.send().await;
}

fn with(mut grant: OAuthGrant, tokens: &Map<String, Value>) -> Result<OAuthGrant, OAuthError> {
    let access = wire::str(tokens.get("access_token"))
        .filter(|a| !a.is_empty())
        .ok_or_else(|| OAuthError::new("The sign-in service didn't return an access token."))?;
    if let Some(kind) = wire::str(tokens.get("token_type"))
        && !kind.eq_ignore_ascii_case("Bearer")
    {
        return Err(OAuthError::new(format!(
            "The sign-in service returned a token of type “{}”, not Bearer.",
            shorten(kind)
        )));
    }
    grant.access_token = access.to_owned();
    // A service that rotates refresh tokens sends a new one; one that doesn't keeps the old one.
    if let Some(refresh) = wire::str(tokens.get("refresh_token")).filter(|r| !r.is_empty()) {
        grant.refresh_token = Some(refresh.to_owned());
    }
    grant.expires_at = seconds(tokens.get("expires_in"))
        .filter(|s| *s > 0.0)
        .map(|s| wire::now() + s);
    if let Some(scope) = wire::str(tokens.get("scope")).filter(|s| !s.is_empty()) {
        grant.scope = Some(scope.to_owned());
    }
    Ok(grant)
}

/// `expires_in`: a number, or (from some services) a number written as text.
fn seconds(node: Option<&Value>) -> Option<f64> {
    match node? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

async fn register(
    http: &Client,
    endpoint: &str,
    redirect: &str,
    found: &OAuthServer,
) -> Result<(String, Option<String>, String), OAuthError> {
    let method = if found.supports("none") {
        "none"
    } else if found.supports("client_secret_basic") {
        "client_secret_basic"
    } else {
        "client_secret_post"
    };
    let mut body = json!({
        "client_name": "Clak",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": method,
        "application_type": "native",
    });
    if let Some(scope) = &found.scope {
        body["scope"] = Value::String(scope.clone());
    }
    let response = http
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .header(ACCEPT, "application/json")
        .body(wire::encode(&body))
        .send()
        .await
        .map_err(|e| OAuthError::network(&e))?;
    let status = response.status();
    let answer = json_object(&read_limited(response).await?);
    let id = wire::str(answer.as_ref().and_then(|a| a.get("client_id"))).filter(|i| !i.is_empty());
    let (Some(id), true) = (id, status.is_success()) else {
        return Err(OAuthError::new(format!(
            "{} refused to register Clak: {}.",
            found.host(),
            problem(status, answer.as_ref())
        )));
    };
    let answer = answer.as_ref().expect("client_id came from it");
    let secret = wire::str(answer.get("client_secret"))
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let auth = match wire::str(answer.get("token_endpoint_auth_method")) {
        Some(auth) => auth.to_owned(),
        None if secret.is_none() => "none".to_owned(),
        None if method == "none" => "client_secret_basic".to_owned(),
        None => method.to_owned(),
    };
    Ok((id.to_owned(), secret, auth))
}

async fn token_request(
    http: &Client,
    grant: &OAuthGrant,
    fields: Vec<(String, String)>,
) -> Result<Map<String, Value>, OAuthError> {
    let uri = absolute(&grant.token_endpoint)
        .filter(secure)
        .ok_or_else(|| OAuthError::rejected("The token address isn't on https: refused."))?;
    let response = form(http, &uri, grant, fields)
        .send()
        .await
        .map_err(|e| OAuthError::network(&e))?;
    let status = response.status();
    let answer = json_object(&read_limited(response).await?);
    if status.is_success()
        && let Some(answer) = answer
    {
        return Ok(answer);
    }
    // invalid_grant: the refresh token is no longer valid; invalid_client: the client is no longer registered.
    let error = wire::str(answer.as_ref().and_then(|a| a.get("error")));
    let rejected = matches!(
        error,
        Some("invalid_grant" | "invalid_client" | "unauthorized_client")
    ) || status == StatusCode::BAD_REQUEST
        || status == StatusCode::UNAUTHORIZED;
    Err(OAuthError {
        message: format!(
            "the sign-in service refused: {}",
            problem(status, answer.as_ref())
        ),
        rejected,
        network: false,
        canceled: false,
    })
}

/// A form POST to the service, with the client authentication it needs.
fn form(
    http: &Client,
    uri: &Url,
    grant: &OAuthGrant,
    mut fields: Vec<(String, String)>,
) -> reqwest::RequestBuilder {
    let mut request = http.post(uri.clone()).header(ACCEPT, "application/json");
    match (
        &grant.client_secret,
        grant.client_auth == "client_secret_basic",
    ) {
        (Some(secret), true) => {
            let pair = format!("{}:{}", escape_data(&grant.client_id), escape_data(secret));
            request = request.header(AUTHORIZATION, format!("Basic {}", STANDARD.encode(pair)));
        }
        _ => {
            fields.push(("client_id".into(), grant.client_id.clone()));
            if let Some(posted) = &grant.client_secret {
                fields.push(("client_secret".into(), posted.clone()));
            }
        }
    }
    request.form(&fields)
}

fn json_object(bytes: &[u8]) -> Option<Map<String, Value>> {
    match serde_json::from_slice::<Value>(strip_bom(bytes)) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

pub(crate) fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)
}

async fn read_limited(mut response: Response) -> Result<Vec<u8>, OAuthError> {
    let mut buffer = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| OAuthError::network(&e))?
    {
        buffer.extend_from_slice(&chunk);
        if buffer.len() > MAX_DOCUMENT {
            return Err(OAuthError::new(
                "The sign-in service sent an answer that is too large.",
            ));
        }
    }
    Ok(buffer)
}

/// Why a service said no, without repeating anything that could be a secret.
fn problem(status: StatusCode, answer: Option<&Map<String, Value>>) -> String {
    let field = |key: &str| wire::str(answer.and_then(|a| a.get(key))).filter(|s| !s.is_empty());
    if let Some(description) = field("error_description") {
        shorten(description)
    } else if let Some(error) = field("error") {
        shorten(error)
    } else {
        format!("HTTP {}", status.as_u16())
    }
}

// =========================================================================
// Small tools
// =========================================================================

/// The server's address as the resource identifier (RFC 8707): no fragment, no query, no lone trailing slash.
pub fn canonical(server: &Url) -> String {
    let origin = uri::origin(server);
    if server.path() == "/" {
        origin
    } else {
        format!("{origin}{}", server.path())
    }
}

/// The declared resource is the server itself, or a prefix of it on the same computer.
pub fn resource_allowed(declared: &str, server: &Url) -> bool {
    let Some(resource) = absolute(declared) else {
        return false;
    };
    if !uri::same_server(&resource, server) {
        return false;
    }
    let prefix = resource.path().trim_end_matches('/');
    let path = server.path().trim_end_matches('/');
    prefix.is_empty() || path == prefix || path.starts_with(&format!("{prefix}/"))
}

fn same_issuer(a: Option<&str>, b: Option<&str>) -> bool {
    matches!((a, b), (Some(a), Some(b)) if a.trim_end_matches('/') == b.trim_end_matches('/'))
}

fn endpoint(meta: &Map<String, Value>, key: &str) -> Result<Option<String>, OAuthError> {
    let Some(text) = wire::str(meta.get(key)).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    let Some(url) = absolute(text) else {
        return Ok(None);
    };
    if secure(&url) {
        Ok(Some(text.to_owned()))
    } else {
        Err(OAuthError::new(format!(
            "The sign-in service's {} isn't on https: refused.",
            key.replace('_', " ")
        )))
    }
}

fn random_text(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("no system randomness");
    URL_SAFE_NO_PAD.encode(buffer)
}

fn shorten(text: &str) -> String {
    clean(Some(text), 160)
}

fn say(status: Status<'_>, text: &str) {
    if let Some(status) = status {
        status(text);
    }
}

/// Before opening the browser: only an http or https address, nothing else (a `file:` or
/// another app's scheme is not a sign-in page). The app calls it in its
/// [`OpenBrowser`], before passing the address to the system.
pub fn browser_url_allowed(url: &Url) -> Result<(), OAuthError> {
    if url.scheme() != "https" && url.scheme() != "http" {
        return Err(OAuthError::new(
            "The sign-in address isn't a web address: refused.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_risorsa_dichiarata_deve_essere_il_server_o_un_suo_prefisso() {
        let cases = [
            (
                "https://mcp.notion.com/mcp",
                "https://mcp.notion.com/mcp",
                true,
            ),
            ("https://mcp.notion.com", "https://mcp.notion.com/mcp", true),
            (
                "https://mcp.notion.com/",
                "https://mcp.notion.com/mcp",
                true,
            ),
            (
                "https://mcp.notion.com/mc",
                "https://mcp.notion.com/mcp",
                false,
            ),
            (
                "https://mcp.notion.com/mcp/x",
                "https://mcp.notion.com/mcp",
                false,
            ),
            ("https://altro.com/mcp", "https://mcp.notion.com/mcp", false),
            (
                "http://mcp.notion.com/mcp",
                "https://mcp.notion.com/mcp",
                false,
            ),
        ];
        for (declared, server, allowed) in cases {
            assert_eq!(
                resource_allowed(declared, &Url::parse(server).unwrap()),
                allowed,
                "{declared}"
            );
        }
    }

    #[test]
    fn la_sfida_di_un_401_dice_dove_accedere() {
        let challenge = OAuthChallenge::parse(
            "Bearer realm=\"OAuth\", resource_metadata=\"https://mcp.notion.com/.well-known/oauth-protected-resource/mcp\", scope=\"default\"",
        );
        assert_eq!(
            challenge,
            Some(OAuthChallenge {
                resource_metadata: Some(
                    "https://mcp.notion.com/.well-known/oauth-protected-resource/mcp".into()
                ),
                scope: Some("default".into()),
                error: None,
            })
        );
        assert_eq!(
            canonical(&Url::parse("https://mcp.notion.com/").unwrap()),
            "https://mcp.notion.com"
        );
        assert_eq!(
            canonical(&Url::parse("https://mcp.notion.com/mcp?x=1#y").unwrap()),
            "https://mcp.notion.com/mcp"
        );
    }

    #[test]
    fn la_sfida_bearer_si_trova_anche_dopo_un_altro_schema() {
        let challenge = OAuthChallenge::parse(
            "Basic realm=\"x, y\", Bearer error=invalid_token, scope=\"a b\"",
        )
        .unwrap();
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        assert_eq!(challenge.scope.as_deref(), Some("a b"));
        assert_eq!(challenge.resource_metadata, None);
        assert_eq!(OAuthChallenge::parse("Basic realm=\"x\""), None);
        assert_eq!(OAuthChallenge::parse("Bearer").unwrap().scope, None);
    }

    #[test]
    fn un_accesso_si_salva_e_si_rilegge_senza_stamparsi() {
        let grant = OAuthGrant {
            issuer: "https://auth.example".into(),
            authorization_endpoint: "https://auth.example/authorize".into(),
            token_endpoint: "https://auth.example/token".into(),
            revocation_endpoint: None,
            client_id: "c".into(),
            client_secret: None,
            client_auth: "none".into(),
            registered: true,
            redirect_uri: "http://127.0.0.1:5000/callback".into(),
            resource: "https://mcp.example/mcp".into(),
            scope: Some("read".into()),
            access_token: "at-segreto".into(),
            refresh_token: Some("rt-segreto".into()),
            expires_at: Some(1_700_000_000.5),
            flow: None,
        };
        let json = grant.to_json();
        assert_eq!(json["expires_at"], json!(1_700_000_000.5));
        assert_eq!(json["revocation_endpoint"], Value::Null);
        assert_eq!(OAuthGrant::from_json(&json), Some(grant.clone()));
        assert_eq!(format!("{grant:?}"), "OAuthGrant(https://auth.example)");
        assert_eq!(grant.host(), "auth.example");
        assert!(OAuthGrant::from_json(&json!({"issuer": "x"})).is_none());
    }

    #[test]
    fn gli_indirizzi_noti_devono_essere_https() {
        let server = known(
            "https://auth.example/oauth/authorize",
            "https://auth.example/oauth/token",
            Some(""),
        )
        .unwrap();
        assert_eq!(server.issuer, "https://auth.example");
        assert_eq!(server.resource, "");
        assert_eq!(server.scope, None);
        assert_eq!(server.auth_methods, vec!["client_secret_post"]);
        assert!(
            known(
                "http://127.0.0.1:8080/authorize",
                "http://localhost:8080/token",
                None
            )
            .is_ok()
        );
        assert_eq!(
            known("https://auth.example/a", "http://auth.example/t", None)
                .unwrap_err()
                .message,
            "The token address must be https: codes and tokens would travel unencrypted."
        );
        assert_eq!(
            known("nope", "https://auth.example/t", None)
                .unwrap_err()
                .message,
            "The sign-in address must be https: codes and tokens would travel unencrypted."
        );
        assert!(browser_url_allowed(&Url::parse("file:///C:/x").unwrap()).is_err());
    }
}

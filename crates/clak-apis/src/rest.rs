//! The call: an API command becomes an HTTP request, and the response becomes what the
//! agent receives (`{"is_error", "content"}`, the MCP blocks). There's nothing to keep
//! open: one function per command, which the hub program calls on every `call`.
//!
//! The secret (token, key, password) is sent only to the origin of the base URL: a
//! redirect to another host isn't followed, because the secret would go along with it.
//!
//! Access comes from an [`AccessToken`]: for a key, a token or a password a
//! [`StaticToken`]; for OAuth the `clak-connectors` token, which renews itself.

use crate::model::{ApiEndpoint, ApiEntry, AuthKind, BodyKind, Object, ParamIn};
use crate::text::{
    clean, escape_data, escape_form, left_part_path, one_line, same_origin, unescape_data,
};
use base64::Engine as _;
use clak_wire::json;
use regex::Regex;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

/// Beyond this, the response is truncated: 200 KB of text is already a lot for a model.
pub const MAX_RESPONSE: usize = 200 * 1024;

/// A file (an image, a PDF) passes through whole up to this size.
pub const MAX_BINARY: usize = 10 * 1024 * 1024;

/// How long a command may take: after that the agent learns the service didn't answer.
pub const CALL_LIMIT: Duration = Duration::from_secs(60);

// =========================================================================
// Access
// =========================================================================

// The secret an API identifies itself with is a `clak-connectors` [`AccessToken`], the
// same as for connectors: a [`StaticToken`] for a key, a token or a password; a
// [`clak_connectors::OAuthToken`] for OAuth, which renews shortly before expiry or after a
// 401, one request at a time, and saves every renewal.
pub use clak_connectors::{AccessToken, McpError, StaticToken};

/// A client without automatic redirects: [`send`] follows them, only on the same origin.
pub fn new_http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("clak")
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
}

// =========================================================================
// Commands
// =========================================================================

/// An API command, as an agent requests it: finds the endpoint, calls, and after
/// [`CALL_LIMIT`] says the service didn't answer. The result is
/// `{"is_error", "content"}`: the hub adds `type` and `call_id`. Cancelling a command means
/// dropping the future.
pub async fn call(
    api: &ApiEntry,
    tool: &str,
    arguments: &Object,
    credential: Option<&dyn AccessToken>,
    http: &reqwest::Client,
    sign_in_lost: Option<&(dyn Fn() + Send + Sync)>,
) -> Value {
    call_within(
        api,
        tool,
        arguments,
        credential,
        http,
        sign_in_lost,
        CALL_LIMIT,
    )
    .await
}

/// Like [`call`], with a chosen limit (tests don't wait a minute).
pub async fn call_within(
    api: &ApiEntry,
    tool: &str,
    arguments: &Object,
    credential: Option<&dyn AccessToken>,
    http: &reqwest::Client,
    sign_in_lost: Option<&(dyn Fn() + Send + Sync)>,
    limit: Duration,
) -> Value {
    let name = clean(&api.name, 40);
    let Some(endpoint) = api.endpoints.iter().find(|e| e.name == tool) else {
        return text_result(&format!("{name} has no command “{tool}”."), true);
    };
    match tokio::time::timeout(
        limit,
        send(api, endpoint, arguments, credential, http, sign_in_lost),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => text_result(
            &format!("{name} didn't answer within {} seconds.", limit.as_secs()),
            true,
        ),
    }
}

/// What the user must do to restore access: the agent tells them.
pub fn sign_in_text(api: &ApiEntry) -> String {
    let name = clean(&api.name, 40);
    if api.auth.kind == AuthKind::OAuthApp {
        format!(
            "{name} refused the app's access: ask the user to check its OAuth client in Clak, APIs tab, Edit on {name}."
        )
    } else {
        format!(
            "{name} needs the user to sign in: ask them to open Clak, APIs tab, and choose Sign in on {name}."
        )
    }
}

/// A request, and its response as an agent sees it. With OAuth, a 401 renews the access
/// and retries once; if the service doesn't renew it, the agent learns the user is needed,
/// and `sign_in_lost` tells the panel.
pub async fn send(
    api: &ApiEntry,
    endpoint: &ApiEndpoint,
    arguments: &Object,
    credential: Option<&dyn AccessToken>,
    http: &reqwest::Client,
    sign_in_lost: Option<&(dyn Fn() + Send + Sync)>,
) -> Value {
    let name = clean(&api.name, 40);
    let unreachable = |e: McpError| {
        text_result(
            &format!("{name}: the sign-in service isn't reachable ({e}): try again shortly."),
            true,
        )
    };
    let oauth = api.auth.is_oauth();
    let mut secret = match credential {
        None => None,
        Some(c) => match c.current().await {
            Ok(secret) => secret,
            Err(e) => return unreachable(e),
        },
    };
    if oauth && (secret.is_none() || credential.is_some_and(|c| c.dead())) {
        return text_result(&sign_in_text(api), true);
    }

    let mut request = match build(http, api, endpoint, arguments, secret.as_deref(), None) {
        Ok(request) => request,
        Err(message) => return text_result(&message, true),
    };
    let Ok(origin) = Url::parse(&api.base_url) else {
        return text_result(
            &format!("The address for {} isn't valid.", endpoint.name),
            true,
        );
    };
    let mut renewed = false;
    let mut hops = 0;
    loop {
        let url = request.url().clone();
        let response = match http.execute(request).await {
            Ok(response) => response,
            Err(e) => {
                return text_result(&format!("{name} is not reachable: {}", describe(e)), true);
            }
        };
        let status = response.status().as_u16();
        // An OAuth access that expired earlier than expected: renew it, and retry once.
        if let (401, true, Some(c), false) = (status, oauth, credential, renewed) {
            renewed = true;
            let fresh = match c.renew(secret.clone()).await {
                Ok(fresh) => fresh,
                Err(e) => return unreachable(e),
            };
            if !fresh {
                if c.dead()
                    && let Some(lost) = sign_in_lost
                {
                    lost();
                }
                return text_result(&sign_in_text(api), true);
            }
            secret = match c.current().await {
                Ok(secret) => secret,
                Err(e) => return unreachable(e),
            };
            request = match build(http, api, endpoint, arguments, secret.as_deref(), None) {
                Ok(retry) => retry,
                Err(message) => return text_result(&message, true),
            };
            *request.url_mut() = url;
            continue;
        }
        // A redirect is followed only on the same origin: elsewhere the secret would go with it.
        let location = response
            .headers()
            .get(header::LOCATION)
            .and_then(|l| l.to_str().ok())
            .and_then(|l| url.join(l).ok());
        if let (301 | 302 | 303 | 307 | 308, Some(next), true) = (status, location, hops < 5) {
            hops += 1;
            if !same_origin(&next, &origin) {
                return text_result(
                    &format!(
                        "HTTP {status}: {name} redirects to {}, another host. Not followed, so the credentials don't go with it.",
                        left_part_path(&next)
                    ),
                    true,
                );
            }
            let method = if matches!(status, 307 | 308) {
                None
            } else {
                Some(Method::GET)
            };
            request = match build(http, api, endpoint, arguments, secret.as_deref(), method) {
                Ok(again) => again,
                Err(message) => return text_result(&message, true),
            };
            *request.url_mut() = next;
            continue;
        }
        return match answer(response, endpoint).await {
            Ok(result) => result,
            Err(e) => text_result(&format!("{name} is not reachable: {}", describe(e)), true),
        };
    }
}

/// The request: the path with its values, the query, the headers, the body and the access.
/// `method`: instead of the endpoint's, without a body (a 303 becomes GET).
pub fn build(
    http: &reqwest::Client,
    api: &ApiEntry,
    endpoint: &ApiEndpoint,
    arguments: &Object,
    secret: Option<&str>,
    method: Option<Method>,
) -> Result<reqwest::Request, String> {
    let given = |name: &str| arguments.get(name).filter(|v| !v.is_null());
    for p in endpoint.parameters.iter().filter(|p| p.required) {
        if given(&p.name).is_none() && p.default.is_none() {
            return Err(format!("Missing --{}.", p.name));
        }
    }
    if endpoint.whole_body()
        && endpoint.body_required
        && given(ApiEndpoint::BODY_ARGUMENT).is_none()
    {
        return Err("Missing --body.".into());
    }
    let value =
        |p: &crate::model::ApiParameter| given(&p.name).map(plain).or_else(|| p.default.clone());

    let mut path = endpoint.path.clone();
    for p in endpoint
        .parameters
        .iter()
        .filter(|p| p.place == ParamIn::Path)
    {
        path = path.replace(
            &format!("{{{}}}", p.name),
            &escape_data(&value(p).unwrap_or_default()),
        );
    }

    let mut query: Vec<String> = Vec::new();
    for p in endpoint
        .parameters
        .iter()
        .filter(|p| p.place == ParamIn::Query)
    {
        if let Some(Value::Array(list)) = given(&p.name) {
            for item in list {
                let text = if item.is_null() {
                    String::new()
                } else {
                    plain(item)
                };
                query.push(format!("{}={}", escape_data(&p.name), escape_data(&text)));
            }
        } else if let Some(v) = value(p) {
            query.push(format!("{}={}", escape_data(&p.name), escape_data(&v)));
        }
    }
    let secret = secret.filter(|s| !s.is_empty());
    if let (AuthKind::Query, Some(key), Some(secret)) =
        (api.auth.kind, api.auth.name.as_deref(), secret)
    {
        query.push(format!("{}={}", escape_data(key), escape_data(secret)));
    }

    let address = format!(
        "{}/{}{}",
        api.base_url.trim_end_matches('/'),
        path.trim_start_matches('/'),
        if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        }
    );
    let url = Url::parse(&address)
        .map_err(|_| format!("The address for {} isn't valid.", endpoint.name))?;
    let has_override = method.is_some();
    let method = match method {
        Some(m) => m,
        None => Method::from_bytes(endpoint.method.as_bytes())
            .map_err(|_| format!("{}: unknown method {}.", endpoint.name, endpoint.method))?,
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::ACCEPT,
        HeaderValue::from_static("application/json, */*;q=0.8"),
    );
    for p in endpoint
        .parameters
        .iter()
        .filter(|p| p.place == ParamIn::Header)
    {
        if let Some(v) = value(p) {
            let set = HeaderName::from_bytes(p.name.as_bytes())
                .ok()
                .zip(HeaderValue::from_str(&one_line(&v)).ok())
                .filter(|(n, _)| *n != header::CONTENT_TYPE && *n != header::CONTENT_LENGTH);
            let Some((name, v)) = set else {
                return Err(format!("The header {} can't be set.", p.name));
            };
            headers.append(name, v);
        }
    }

    let mut body: Option<(&'static str, Vec<u8>)> = None;
    if !has_override && endpoint.has_body() {
        if endpoint.body == Some(BodyKind::Form) {
            let fields: Vec<String> = endpoint
                .parameters
                .iter()
                .filter(|p| p.place == ParamIn::Body)
                .filter_map(|p| {
                    value(p).map(|v| format!("{}={}", escape_form(&p.name), escape_form(&v)))
                })
                .collect();
            body = Some((
                "application/x-www-form-urlencoded",
                fields.join("&").into_bytes(),
            ));
        } else if endpoint.whole_body() {
            if let Some(whole) = given(ApiEndpoint::BODY_ARGUMENT) {
                body = Some((
                    "application/json; charset=utf-8",
                    json::encode(&whole_json(whole)),
                ));
            }
        } else {
            let mut object = Object::new();
            for p in endpoint
                .parameters
                .iter()
                .filter(|p| p.place == ParamIn::Body)
            {
                if let Some(v) = given(&p.name) {
                    object.insert(p.name.clone(), v.clone());
                } else if let Some(fallback) = &p.default {
                    let typed = p
                        .to_schema()
                        .get("default")
                        .cloned()
                        .unwrap_or_else(|| fallback.clone().into());
                    object.insert(p.name.clone(), typed);
                }
            }
            if !object.is_empty() || endpoint.body_required {
                body = Some((
                    "application/json; charset=utf-8",
                    json::encode(&Value::Object(object)),
                ));
            }
        }
    }

    let authorization = |value: String| {
        let mut v = HeaderValue::from_str(&value)
            .map_err(|_| "The header Authorization can't be set.".to_owned())?;
        v.set_sensitive(true);
        Ok::<_, String>(v)
    };
    match (api.auth.kind, secret) {
        (AuthKind::Bearer | AuthKind::OAuth | AuthKind::OAuthApp, Some(secret)) => {
            headers.insert(
                header::AUTHORIZATION,
                authorization(format!("Bearer {secret}"))?,
            );
        }
        (AuthKind::Basic, _) => {
            if let Some(user) = &api.auth.name {
                let pair = base64::engine::general_purpose::STANDARD
                    .encode(format!("{user}:{}", secret.unwrap_or("")));
                headers.insert(
                    header::AUTHORIZATION,
                    authorization(format!("Basic {pair}"))?,
                );
            }
        }
        (AuthKind::Header, Some(secret)) => {
            if let Some(name) = api
                .auth
                .name
                .as_deref()
                .and_then(|n| HeaderName::from_bytes(n.as_bytes()).ok())
                && let Ok(mut v) = HeaderValue::from_str(secret)
            {
                v.set_sensitive(true);
                headers.insert(name, v);
            }
        }
        _ => {}
    }

    let mut builder = http.request(method, url).headers(headers);
    if let Some((kind, bytes)) = body {
        builder = builder.header(header::CONTENT_TYPE, kind).body(bytes);
    }
    builder.build().map_err(describe)
}

/// The whole body as the agent passed it: from the command line it arrives as text, and
/// if it's JSON it's sent as such.
fn whole_json(body: &Value) -> Value {
    match body {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| body.clone()),
        other => other.clone(),
    }
}

/// A value for a URL or a header: text as is, everything else as JSON.
fn plain(node: &Value) -> String {
    match node {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        other => json::text(other),
    }
}

// =========================================================================
// The response
// =========================================================================

/// The response for the agent: the text (truncated beyond [`MAX_RESPONSE`]); an image or a
/// file as an attachment; an error code as an error, with the start of the body.
async fn answer(response: reqwest::Response, endpoint: &ApiEndpoint) -> reqwest::Result<Value> {
    let status = response.status();
    let line = status_line(status);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let mut params = content_type.split(';');
    let media = params.next().unwrap_or("").trim().to_owned();
    let charset = params
        .filter_map(|p| p.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, v)| v.trim().trim_matches('"').to_ascii_lowercase());
    let ok = status.is_success();
    let url = response.url().clone();
    let headers = response.headers().clone();

    if !is_text(&media) && !media.is_empty() && ok {
        let (data, truncated) = read_limited(response, MAX_BINARY).await?;
        if truncated {
            return Ok(text_result(
                &format!(
                    "{line}: {media}, larger than {} MB, not passed.",
                    MAX_BINARY / 1024 / 1024
                ),
                true,
            ));
        }
        let data = base64::engine::general_purpose::STANDARD.encode(data);
        let block = if media.to_ascii_lowercase().starts_with("image/") {
            serde_json::json!({"type": "image", "data": data, "mimeType": media})
        } else {
            serde_json::json!({"type": "resource", "resource": {"uri": left_part_path(&url), "blob": data, "mimeType": media}})
        };
        return Ok(serde_json::json!({"is_error": false, "content": [block]}));
    }

    let (data, truncated) = read_limited(response, if ok { MAX_RESPONSE } else { 4096 }).await?;
    let mut text = decode(&data, charset.as_deref());
    if truncated {
        text.push_str(&if ok {
            format!("\n[… truncated at {} KB]", MAX_RESPONSE / 1024)
        } else {
            " […]".into()
        });
    }
    if !ok {
        let message = if text.trim().is_empty() {
            line
        } else {
            format!("{line}\n{text}")
        };
        return Ok(text_result(&message, true));
    }
    let mut result = text_result(if text.is_empty() { &line } else { &text }, false);
    // Pages: a separate block, so a JSON response stays JSON.
    if let Some(more) = pages(&headers, &url, endpoint)
        && let Some(Value::Array(blocks)) = result.get_mut("content")
    {
        blocks.push(serde_json::json!({"type": "text", "text": more}));
    }
    Ok(result)
}

fn status_line(status: StatusCode) -> String {
    format!(
        "HTTP {} {}",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .trim_end()
    .to_owned()
}

/// The text in its encoding: UTF-8, Latin-1 and ASCII; anything else as UTF-8.
fn decode(data: &[u8], charset: Option<&str>) -> String {
    match charset {
        Some("iso-8859-1" | "latin1" | "latin-1" | "l1") => {
            data.iter().map(|&b| b as char).collect()
        }
        Some("us-ascii" | "ascii") => data
            .iter()
            .map(|&b| if b < 128 { b as char } else { '?' })
            .collect(),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}

static LINK_NEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<([^>]*)>\s*;[^,<]*?rel\s*=\s*"?([^";,]+)"?"#).expect("regex")
});

/// What the service says about pages: `Link: <…>; rel="next"` (RFC 8288) becomes the
/// arguments to request it — "--page 2" — when the next page changes only parameters the
/// command knows; and a total (`X-Total-Count`), if present.
pub fn pages(headers: &HeaderMap, url: &Url, endpoint: &ApiEndpoint) -> Option<String> {
    let mut notes = Vec::new();
    let links: Vec<&str> = headers
        .get_all("link")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if !links.is_empty() {
        let joined = links.join(",");
        let next = LINK_NEXT
            .captures_iter(&joined)
            .find(|m| m[2].split(' ').any(|r| r.eq_ignore_ascii_case("next")))
            .and_then(|m| url.join(&m[1]).ok());
        if let Some(next) = next {
            let now = query_of(url);
            let then = query_of(&next);
            let known: Vec<&str> = endpoint
                .parameters
                .iter()
                .filter(|p| p.place == ParamIn::Query)
                .map(|p| p.name.as_str())
                .collect();
            let changed: Vec<&(String, String)> = then
                .iter()
                .filter(|(k, v)| {
                    now.iter()
                        .find(|(nk, _)| nk == k)
                        .is_none_or(|(_, before)| before != v)
                })
                .collect();
            let same_path = next.path() == url.path();
            notes.push(if same_path && !changed.is_empty() && changed.iter().all(|(k, _)| known.contains(&k.as_str())) {
                let args: Vec<String> = changed.iter().map(|(k, v)| format!("--{k} {}", quote(v))).collect();
                format!("[Next page: {}]", args.join(" "))
            } else {
                "[There is a next page, but this command can't ask for it: its parameters don't cover the service's next-page address.]"
                    .to_owned()
            });
        }
    }
    for name in ["X-Total-Count", "X-Total", "Total-Count"] {
        if let Some(total) = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .filter(|t| (1..20).contains(&t.len()))
        {
            notes.push(format!("[Total: {total}]"));
            break;
        }
    }
    (!notes.is_empty()).then(|| notes.join("\n"))
}

/// The query pairs: if a key repeats, the last value wins.
fn query_of(url: &Url) -> Vec<(String, String)> {
    let mut values: Vec<(String, String)> = Vec::new();
    for pair in url
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (key, value) = match pair.find('=') {
            Some(at) => (
                &pair[..at],
                unescape_data(&pair[at + 1..].replace('+', " ")),
            ),
            None => (pair, String::new()),
        };
        let key = unescape_data(&key.replace('+', " "));
        match values.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => values.push((key, value)),
        }
    }
    values
}

/// A value for the command line: quoted only if needed.
fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/=+".contains(c))
    {
        value.to_owned()
    } else {
        format!("\"{}\"", value.replace('"', "\\\""))
    }
}

fn is_text(media: &str) -> bool {
    let t = media.to_ascii_lowercase();
    t.starts_with("text/")
        || t.ends_with("json")
        || t.ends_with("xml")
        || t.contains("yaml")
        || t.contains("javascript")
        || t == "application/x-www-form-urlencoded"
}

/// At most `max` bytes; the second value says whether there were more.
pub async fn read_limited(
    mut response: reqwest::Response,
    max: usize,
) -> reqwest::Result<(Vec<u8>, bool)> {
    let mut buffer = Vec::new();
    while buffer.len() <= max {
        match response.chunk().await? {
            Some(chunk) => buffer.extend_from_slice(&chunk),
            None => return Ok((buffer, false)),
        }
    }
    buffer.truncate(max);
    Ok((buffer, true))
}

/// A network error in words, with its causes, without the URL: the query may contain a
/// key.
pub fn describe(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut text = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        let more = cause.to_string();
        if !text.contains(&more) {
            text.push_str(": ");
            text.push_str(&more);
        }
        source = cause.source();
    }
    text
}

/// A text-only result.
pub fn text_result(text: &str, error: bool) -> Value {
    serde_json::json!({"is_error": error, "content": [{"type": "text", "text": text}]})
}

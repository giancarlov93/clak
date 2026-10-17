//! Reads an OpenAPI 3.x or Swagger 2.0 document, in JSON or YAML, and turns it into the
//! endpoints of an API: one command per operation, with the parameters and the body as
//! arguments. Internal references (`$ref`) are resolved; those to other files aren't.
//!
//! The document comes from outside and is text: names and descriptions end up in front of
//! a model, and the user sees and picks them before saving, like the commands of an MCP server.

use crate::model::{
    ApiAuth, ApiEndpoint, ApiParameter, AuthKind, BodyKind, Object, ParamIn, ParamType,
};
use crate::rest;
use crate::text::{authority, clean_opt, unescape_data};
use crate::yaml;
use clak_wire::json;
use regex::Regex;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;
use std::time::Duration;
use url::Url;

/// What's extracted from an OpenAPI document: the name, the base URL, how to authenticate
/// and the endpoints. `skipped`: the endpoints left out, and why (a file upload, a
/// parameter in a cookie), and the notes about access.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiImport {
    pub name: String,
    pub base_url: Option<String>,
    pub auth: ApiAuth,
    pub endpoints: Vec<ApiEndpoint>,
    pub skipped: Vec<String>,
    /// "OpenAPI 3.0.3", "Swagger 2.0".
    pub version: String,
}

/// Why a document can't be read, as told to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenApiError(pub String);

impl fmt::Display for OpenApiError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OpenApiError {}

fn fail<T>(message: impl Into<String>) -> Result<T, OpenApiError> {
    Err(OpenApiError(message.into()))
}

/// Beyond this, the document isn't read: 20 MB is enough even for the largest APIs.
pub const MAX_BYTES: usize = 20 * 1024 * 1024;

/// How many nested `$ref`s are followed in a schema; beyond that, "an object".
const MAX_DEPTH: usize = 6;

const VERBS: [&str; 7] = ["get", "put", "post", "delete", "options", "head", "patch"];

/// Downloads and reads a document, from an http(s) URL or from a file. `http` is the hub's
/// client ([`rest::new_http_client`]): it doesn't follow redirects.
pub async fn load(http: &reqwest::Client, source: &str) -> Result<ApiImport, OpenApiError> {
    let source = source.trim();
    if let Ok(url) = Url::parse(source).map_err(|_| ()).and_then(|u| {
        if matches!(u.scheme(), "http" | "https") {
            Ok(u)
        } else {
            Err(())
        }
    }) {
        let host = url.host_str().unwrap_or("").to_owned();
        let fetch = async {
            let response = http
                .get(url.clone())
                .header(
                    reqwest::header::ACCEPT,
                    "application/json, application/yaml, text/yaml, */*;q=0.5",
                )
                .send()
                .await
                .map_err(|e| OpenApiError(format!("Not reached: {}", rest::describe(e))))?;
            let status = response.status();
            if !status.is_success() {
                let reason = status.canonical_reason().unwrap_or("");
                return fail(
                    format!("HTTP {} {reason}", status.as_u16())
                        .trim_end()
                        .to_owned(),
                );
            }
            let (data, truncated) = rest::read_limited(response, MAX_BYTES)
                .await
                .map_err(|e| OpenApiError(format!("Not reached: {}", rest::describe(e))))?;
            if truncated {
                return fail("The document is larger than 20 MB.");
            }
            parse(&String::from_utf8_lossy(&data), Some(&url))
        };
        return match tokio::time::timeout(Duration::from_secs(30), fetch).await {
            Ok(result) => result,
            Err(_) => fail(format!("{host} didn't answer within 30 seconds.")),
        };
    }

    let metadata = match tokio::fs::metadata(source).await {
        Ok(m) if m.is_file() => m,
        Ok(_) => return fail("Neither an http(s) address nor an existing file."),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return fail("Neither an http(s) address nor an existing file.");
        }
        Err(e) => return fail(format!("The file can't be read: {e}")),
    };
    if metadata.len() > MAX_BYTES as u64 {
        return fail("The document is larger than 20 MB.");
    }
    let bytes = tokio::fs::read(source)
        .await
        .map_err(|e| OpenApiError(format!("The file can't be read: {e}")))?;
    parse(&String::from_utf8_lossy(&bytes), None)
}

/// Reads the text of a document. `source`: where it comes from, for relative URLs.
pub fn parse(text: &str, source: Option<&Url>) -> Result<ApiImport, OpenApiError> {
    let document = document(text)?;
    let Some(root) = document.as_object() else {
        return fail("The document isn't an object.");
    };
    let version = scalar(root.get("openapi")).or_else(|| scalar(root.get("swagger")));
    let v2 = scalar(root.get("swagger")).is_some_and(|s| s.starts_with('2'));
    let version = match version {
        Some(v) if v2 || v.starts_with('3') => v,
        _ => return fail("Not an OpenAPI document: there's no “openapi: 3.x” or “swagger: 2.0”."),
    };
    let reader = Reader { root, v2 };

    let title = clean_opt(
        root.get("info")
            .and_then(|i| i.get("title"))
            .and_then(Value::as_str),
        40,
    );
    let name = title
        .or_else(|| source.and_then(Url::host_str).map(str::to_owned))
        .unwrap_or_else(|| "API".into());
    let mut skipped = Vec::new();
    let mut endpoints = Vec::new();
    let mut names = HashSet::new();
    if let Some(paths) = root.get("paths").and_then(Value::as_object) {
        for (path, node) in paths {
            let Some(item) = reader.resolve(Some(node)).and_then(Value::as_object) else {
                continue;
            };
            if !path.starts_with('/') {
                continue;
            }
            for verb in VERBS {
                let Some(operation) = reader.resolve(item.get(verb)).and_then(Value::as_object)
                else {
                    continue;
                };
                let method = verb.to_uppercase();
                match reader.endpoint(&method, path, item, operation, &names) {
                    Ok(endpoint) => {
                        names.insert(endpoint.name.clone());
                        endpoints.push(endpoint);
                    }
                    Err(reason) => skipped.push(format!("{method} {path}: {reason}")),
                }
            }
        }
    }
    if endpoints.is_empty() && skipped.is_empty() {
        return fail("The document has no endpoints.");
    }
    let base_url = base_url(root, v2, source);
    let auth = reader.auth(&mut skipped, source);
    Ok(ApiImport {
        name,
        base_url,
        auth,
        endpoints,
        skipped,
        version: format!("{}{version}", if v2 { "Swagger " } else { "OpenAPI " }),
    })
}

// =========================================================================
// The base URL
// =========================================================================

static BRACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{([^}]+)\}").expect("regex"));

fn base_url(root: &Object, v2: bool, source: Option<&Url>) -> Option<String> {
    let raw = if v2 {
        let host = root
            .get("host")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| source.map(authority))?;
        let schemes: Vec<&str> = root
            .get("schemes")
            .and_then(Value::as_array)
            .map(|s| s.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let scheme = if schemes.contains(&"https") {
            "https".to_owned()
        } else {
            schemes
                .first()
                .map(|s| (*s).to_owned())
                .or_else(|| source.map(|s| s.scheme().to_owned()))
                .unwrap_or_else(|| "https".into())
        };
        format!(
            "{scheme}://{host}{}",
            root.get("basePath").and_then(Value::as_str).unwrap_or("")
        )
    } else {
        let server = root
            .get("servers")
            .and_then(Value::as_array)
            .and_then(|s| s.first())
            .and_then(Value::as_object);
        let Some((server, url)) =
            server.and_then(|s| s.get("url").and_then(Value::as_str).map(|u| (s, u)))
        else {
            return source.map(|s| format!("{}://{}", s.scheme(), authority(s)));
        };
        // The {name} variables take their default value.
        BRACES
            .replace_all(url, |caps: &regex::Captures| {
                server
                    .get("variables")
                    .and_then(|v| v.get(&caps[1]))
                    .and_then(|v| v.get("default"))
                    .and_then(Value::as_str)
                    .map_or_else(|| caps[0].to_owned(), str::to_owned)
            })
            .into_owned()
    };
    if let Ok(absolute) = Url::parse(&raw)
        && matches!(absolute.scheme(), "http" | "https")
    {
        return Some(absolute.to_string().trim_end_matches('/').to_owned());
    }
    source
        .and_then(|s| s.join(&raw).ok())
        .map(|u| u.to_string().trim_end_matches('/').to_owned())
}

// =========================================================================
// Endpoints
// =========================================================================

struct Reader<'a> {
    root: &'a Object,
    v2: bool,
}

impl<'a> Reader<'a> {
    /// Follows an internal `$ref` (even several in a row); an external one leads nowhere.
    fn resolve<'n>(&self, node: Option<&'n Value>) -> Option<&'n Value>
    where
        'a: 'n,
    {
        let mut node = node;
        let mut hops = 0;
        while let Some(reference) = node.and_then(|n| n.get("$ref")).and_then(Value::as_str) {
            if hops >= 32 {
                break;
            }
            hops += 1;
            if !reference.starts_with("#/") {
                return None;
            }
            node = self.pointer(reference);
        }
        node
    }

    fn pointer(&self, reference: &str) -> Option<&'a Value> {
        let mut parts = reference[2..].split('/');
        let first = parts.next()?;
        let mut at = self.root.get(&unpoint(first))?;
        for raw in parts {
            let part = unpoint(raw);
            at = match at {
                Value::Object(o) => o.get(&part)?,
                Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
                _ => return None,
            };
        }
        Some(at)
    }

    /// The schema with references resolved, up to [`MAX_DEPTH`] levels; a circular or too
    /// deep reference becomes "an object". Examples and the like are dropped.
    fn schema(&self, node: Option<&Value>, depth: usize, seen: &HashSet<String>) -> Option<Object> {
        let node = node?;
        if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
            if depth >= MAX_DEPTH || seen.contains(reference) {
                let mut object = Object::new();
                object.insert("type".into(), "object".into());
                return Some(object);
            }
            let mut seen = seen.clone();
            seen.insert(reference.to_owned());
            return self.schema(self.resolve(Some(node)), depth + 1, &seen);
        }
        self.schema_object(node.as_object()?, depth, seen)
    }

    fn schema_object(
        &self,
        schema: &Object,
        depth: usize,
        seen: &HashSet<String>,
    ) -> Option<Object> {
        let mut copy = Object::new();
        for (key, value) in schema {
            if matches!(
                key.as_str(),
                "example" | "examples" | "xml" | "externalDocs" | "discriminator" | "x-examples"
            ) {
                continue;
            }
            let inner = |v: &Value| {
                self.schema(Some(v), depth + 1, seen)
                    .map_or(Value::Null, Value::Object)
            };
            let copied = match (key.as_str(), value) {
                ("properties" | "patternProperties", Value::Object(properties)) => Value::Object(
                    properties
                        .iter()
                        .map(|(k, v)| (k.clone(), inner(v)))
                        .collect(),
                ),
                ("items" | "additionalProperties" | "not", Value::Object(_)) => inner(value),
                ("allOf" | "anyOf" | "oneOf", Value::Array(parts)) => {
                    Value::Array(parts.iter().map(inner).collect())
                }
                _ => value.clone(),
            };
            copy.insert(key.clone(), copied);
        }
        Some(merge(copy))
    }

    fn schema_of(&self, node: Option<&Value>) -> Option<Object> {
        self.schema(node, 0, &HashSet::new())
    }

    fn endpoint(
        &self,
        method: &str,
        path: &str,
        item: &Object,
        operation: &Object,
        taken: &HashSet<String>,
    ) -> Result<ApiEndpoint, String> {
        // The path's parameters apply to all operations; the operation's own replace them.
        let mut raw: Vec<((String, String), &Object)> = Vec::new();
        for list in [item.get("parameters"), operation.get("parameters")] {
            for node in list.and_then(Value::as_array).into_iter().flatten() {
                let Some(p) = self.resolve(Some(node)).and_then(Value::as_object) else {
                    continue;
                };
                let (Some(name), Some(place)) = (str_of(p, "name"), str_of(p, "in")) else {
                    continue;
                };
                let key = (name.to_owned(), place.to_owned());
                match raw.iter_mut().find(|(k, _)| *k == key) {
                    Some(slot) => slot.1 = p,
                    None => raw.push((key, p)),
                }
            }
        }

        let mut parameters: Vec<ApiParameter> = Vec::new();
        let mut body_type: Option<BodyKind> = None;
        let mut body_schema: Option<Object> = None;
        let mut body_required = false;
        let mut form: Vec<&Object> = Vec::new();
        const PLAIN_KEYS: [&str; 6] = [
            "name",
            "in",
            "required",
            "description",
            "allowEmptyValue",
            "collectionFormat",
        ];
        for ((name, place), p) in &raw {
            let required = json::truthy(p.get("required"));
            match place.as_str() {
                "path" | "query" | "header" if !(place == "header" && is_reserved(name)) => {
                    let schema = if self.v2 {
                        self.schema_of(Some(&Value::Object(without(p, &PLAIN_KEYS))))
                    } else {
                        self.schema_of(p.get("schema"))
                    };
                    let place = ParamIn::parse(place).unwrap_or(ParamIn::Query);
                    parameters.push(parameter(
                        name,
                        place,
                        schema,
                        required || place == ParamIn::Path,
                        str_of(p, "description"),
                    ));
                }
                "header" => {} // Accept, Content-Type, Authorization: the hub sets them
                "cookie" => {
                    if required {
                        return Err(format!("needs the cookie “{name}”"));
                    }
                }
                "body" if self.v2 => {
                    body_type = Some(BodyKind::Json);
                    body_schema = self.schema_of(p.get("schema"));
                    body_required = required;
                }
                "formData" if self.v2 => {
                    if str_of(p, "type") == Some("file") {
                        return Err("uploads a file".into());
                    }
                    form.push(p);
                }
                _ => {}
            }
        }
        if !form.is_empty() {
            body_type = Some(BodyKind::Form);
            body_required = form.iter().any(|p| json::truthy(p.get("required")));
            for p in &form {
                let schema = self.schema_of(Some(&Value::Object(without(p, &PLAIN_KEYS))));
                parameters.push(parameter(
                    str_of(p, "name").unwrap_or(""),
                    ParamIn::Body,
                    schema,
                    json::truthy(p.get("required")),
                    str_of(p, "description"),
                ));
            }
        }

        if !self.v2
            && let Some(body) = self
                .resolve(operation.get("requestBody"))
                .and_then(Value::as_object)
        {
            body_required = json::truthy(body.get("required"));
            let empty = Object::new();
            let content = body
                .get("content")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let json_body = content.iter().find(|(k, _)| is_json(k));
            let urlencoded = content.iter().find(|(k, _)| {
                k.to_ascii_lowercase()
                    .starts_with("application/x-www-form-urlencoded")
            });
            if let Some((_, media)) = json_body {
                body_type = Some(BodyKind::Json);
                body_schema = self.schema_of(media.get("schema"));
            } else if let Some((_, media)) = urlencoded {
                body_type = Some(BodyKind::Form);
                body_schema = self.schema_of(media.get("schema"));
            } else if body_required {
                return Err(
                    if content
                        .keys()
                        .any(|k| k.to_ascii_lowercase().starts_with("multipart/"))
                    {
                        "uploads a file".into()
                    } else {
                        format!(
                            "sends {}, not JSON",
                            content.keys().next().map_or("a body", String::as_str)
                        )
                    },
                );
            }
        }

        // A body that is an object with fields: the fields become arguments, if they don't clash with the others.
        let fields = body_schema
            .as_ref()
            .filter(|s| {
                s.get("type")
                    .and_then(Value::as_str)
                    .is_none_or(|t| t == "object")
            })
            .and_then(|s| s.get("properties"))
            .and_then(Value::as_object)
            .filter(|f| !f.is_empty())
            .filter(|f| {
                f.keys()
                    .all(|k| !parameters.iter().any(|p| &p.name == k) && ApiEndpoint::valid_name(k))
            })
            .cloned();
        if let (Some(_), Some(fields)) = (body_type, fields) {
            let mandatory: HashSet<String> = body_schema
                .as_ref()
                .and_then(|s| s.get("required"))
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            for (field, node) in fields {
                if node
                    .as_object()
                    .is_some_and(|f| json::truthy(f.get("readOnly")))
                {
                    continue;
                }
                let required = mandatory.contains(&field);
                parameters.push(parameter(
                    &field,
                    ParamIn::Body,
                    node.as_object().cloned(),
                    required,
                    None,
                ));
            }
            body_schema = None;
        } else if body_type == Some(BodyKind::Form)
            && body_schema.is_some()
            && !parameters.iter().any(|p| p.place == ParamIn::Body)
        {
            return Err("sends a form without fields".into());
        }

        let base_name = str_of(operation, "operationId")
            .filter(|id| !id.is_empty())
            .map(ApiEndpoint::sanitize)
            .filter(|clean| !clean.is_empty())
            .unwrap_or_else(|| ApiEndpoint::name_from(method, path));
        let mut unique = base_name.clone();
        let mut n = 2;
        while taken.contains(&unique) {
            let stem = if base_name.len() > 60 {
                &base_name[..60]
            } else {
                base_name.as_str()
            };
            unique = format!("{stem}_{n}");
            n += 1;
        }

        let summary = clean_opt(str_of(operation, "summary"), 200);
        let detail = clean_opt(str_of(operation, "description"), 600);
        let detail = if detail == summary { None } else { detail };
        let mut description = [summary, detail]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
        if json::truthy(operation.get("deprecated")) {
            description = format!("Deprecated. {description}").trim().to_owned();
        }

        let endpoint = ApiEndpoint {
            name: unique,
            method: method.to_owned(),
            path: path.to_owned(),
            description: (!description.is_empty()).then_some(description),
            parameters,
            body: body_type,
            body_schema,
            body_required,
        };
        if let Some(problem) = endpoint.problem() {
            let after = problem
                .find(':')
                .map_or(problem.as_str(), |at| &problem[at + 1..]);
            return Err(after.trim().to_owned());
        }
        Ok(endpoint)
    }

    /// How to authenticate: the scheme the document requires first, or the first it declares.
    /// OAuth with the code (in the browser) or "app only" brings URLs and scopes; the other
    /// OAuth flows (implicit, password) and OpenID become a Bearer token to paste.
    fn auth(&self, notes: &mut Vec<String>, source: Option<&Url>) -> ApiAuth {
        let schemes = if self.v2 {
            self.root.get("securityDefinitions")
        } else {
            self.root
                .get("components")
                .and_then(|c| c.get("securitySchemes"))
        };
        let Some(schemes) = schemes.and_then(Value::as_object).filter(|s| !s.is_empty()) else {
            return ApiAuth::default();
        };
        let wanted = self
            .root
            .get("security")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .flat_map(|s| s.keys())
            .next();
        let key = match wanted {
            Some(w) if schemes.get(w).is_some_and(|v| !v.is_null()) => w.as_str(),
            _ => schemes.keys().next().map_or("", String::as_str),
        };
        let Some(scheme) = self.resolve(schemes.get(key)).and_then(Value::as_object) else {
            return ApiAuth::default();
        };
        let kind = str_of(scheme, "type");
        if kind == Some("oauth2")
            && let Some(oauth) = self.oauth2(scheme, key, source)
        {
            return oauth;
        }
        let name = str_of(scheme, "name").filter(|n| !n.is_empty());
        let place = str_of(scheme, "in");
        let http = str_of(scheme, "scheme").map(str::to_lowercase);
        match (kind, place, http.as_deref()) {
            (Some("apiKey"), Some("header"), _) if name.is_some_and(ApiEndpoint::header_name) => {
                let name = name.unwrap_or_default();
                if name.eq_ignore_ascii_case("Authorization") {
                    ApiAuth::new(AuthKind::Bearer)
                } else {
                    ApiAuth::named(AuthKind::Header, name)
                }
            }
            (Some("apiKey"), Some("query"), _) if name.is_some() => {
                ApiAuth::named(AuthKind::Query, name.unwrap_or_default())
            }
            (Some("apiKey"), Some("cookie"), _) => {
                notes.push(
                    "Authentication with a cookie isn't supported: choose another one.".into(),
                );
                ApiAuth::default()
            }
            (Some("basic"), _, _) | (Some("http"), _, Some("basic")) => {
                ApiAuth::new(AuthKind::Basic)
            }
            (Some("http"), _, Some("bearer"))
            | (Some("oauth2"), _, _)
            | (Some("openIdConnect"), _, _) => ApiAuth::new(AuthKind::Bearer),
            _ => ApiAuth::default(),
        }
    }

    /// The OAuth 2 flows the hub supports: the code in the browser (3.x `authorizationCode`,
    /// 2.0 `accessCode`), then the "app only" one (`clientCredentials`, `application`).
    fn oauth2(&self, scheme: &Object, key: &str, source: Option<&Url>) -> Option<ApiAuth> {
        let scope = self.scopes(key);
        let url = |node: Option<&Value>| -> Option<String> {
            let text = node.and_then(Value::as_str).filter(|t| !t.is_empty())?;
            Some(match Url::parse(text) {
                Ok(absolute) => absolute.to_string(),
                Err(_) => source
                    .and_then(|s| s.join(text).ok())
                    .map_or_else(|| text.to_owned(), |u| u.to_string()),
            })
        };
        let make = |kind, authorization: Option<String>, token: Option<String>| ApiAuth {
            kind,
            name: None,
            authorization_url: authorization,
            token_url: token,
            scope: scope.clone(),
            client_id: None,
        };
        if self.v2 {
            return match str_of(scheme, "flow") {
                Some("accessCode") => Some(make(
                    AuthKind::OAuth,
                    url(scheme.get("authorizationUrl")),
                    url(scheme.get("tokenUrl")),
                )),
                Some("application") => {
                    Some(make(AuthKind::OAuthApp, None, url(scheme.get("tokenUrl"))))
                }
                _ => None,
            };
        }
        let flows = scheme.get("flows").and_then(Value::as_object)?;
        if let Some(code) = flows.get("authorizationCode").and_then(Value::as_object) {
            return Some(make(
                AuthKind::OAuth,
                url(code.get("authorizationUrl")),
                url(code.get("tokenUrl")),
            ));
        }
        if let Some(app) = flows.get("clientCredentials").and_then(Value::as_object) {
            return Some(make(AuthKind::OAuthApp, None, url(app.get("tokenUrl"))));
        }
        None
    }

    /// The scopes the document requires for this scheme: the global ones, or the operations' ones.
    fn scopes(&self, key: &str) -> Option<String> {
        fn requirements<'v>(security: Option<&'v Value>, key: &str) -> Vec<&'v Value> {
            security
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object)
                .filter_map(|s| s.get(key))
                .filter(|s| !s.is_null())
                .collect()
        }
        let mut all = requirements(self.root.get("security"), key);
        if all.is_empty() {
            for item in self
                .root
                .get("paths")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|p| p.values())
            {
                let Some(item) = self.resolve(Some(item)).and_then(Value::as_object) else {
                    continue;
                };
                for verb in VERBS {
                    if let Some(operation) = self.resolve(item.get(verb)).and_then(Value::as_object)
                    {
                        all.extend(requirements(operation.get("security"), key));
                    }
                }
            }
        }
        let mut scopes: Vec<&str> = Vec::new();
        for scope in all
            .iter()
            .filter_map(|a| a.as_array())
            .flatten()
            .filter_map(Value::as_str)
        {
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
        (!scopes.is_empty()).then(|| scopes.join(" "))
    }
}

/// A segment of a JSON pointer: `%XX`, then `~1` is `/` and `~0` is `~`.
fn unpoint(raw: &str) -> String {
    unescape_data(raw).replace("~1", "/").replace("~0", "~")
}

/// An `allOf` of objects becomes a single object: that way the body's fields are visible.
fn merge(mut schema: Object) -> Object {
    let parts = match schema.get("allOf") {
        Some(Value::Array(parts)) if parts.iter().all(Value::is_object) => parts.clone(),
        _ => return schema,
    };
    let mut properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut required: Vec<String> = strings(schema.get("required"));
    for part in parts.iter().filter_map(Value::as_object) {
        if let Some(more) = part.get("properties").and_then(Value::as_object) {
            for (key, value) in more {
                properties.insert(key.clone(), value.clone());
            }
        }
        required.extend(strings(part.get("required")));
        if let Some(description) = part.get("description").filter(|d| !d.is_null())
            && schema.get("description").is_none_or(Value::is_null)
        {
            schema.insert("description".into(), description.clone());
        }
    }
    schema.shift_remove("allOf");
    if schema.get("type").is_none_or(Value::is_null) {
        schema.insert("type".into(), "object".into());
    }
    if !properties.is_empty() {
        schema.insert("properties".into(), Value::Object(properties));
    }
    if !required.is_empty() {
        let mut distinct: Vec<String> = Vec::new();
        for r in required {
            if !distinct.contains(&r) {
                distinct.push(r);
            }
        }
        schema.insert(
            "required".into(),
            Value::Array(distinct.into_iter().map(Value::String).collect()),
        );
    }
    schema
}

fn strings(node: Option<&Value>) -> Vec<String> {
    node.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn parameter(
    name: &str,
    place: ParamIn,
    schema: Option<Object>,
    required: bool,
    description: Option<&str>,
) -> ApiParameter {
    let kind = type_of(schema.as_ref());
    let fallback = schema
        .as_ref()
        .and_then(|s| s.get("default"))
        .filter(|d| !d.is_object() && !d.is_array())
        .and_then(|d| scalar(Some(d)));
    let choices = schema
        .as_ref()
        .and_then(|s| s.get("enum"))
        .and_then(Value::as_array)
        .map(|e| e.iter().filter_map(|v| scalar(Some(v))).collect::<Vec<_>>());
    let description = description.map(str::to_owned).or_else(|| {
        schema
            .as_ref()
            .and_then(|s| s.get("description"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    // The full schema is needed only when it says more than the type: an object, a list of objects, a format.
    let plain = match &schema {
        None => true,
        Some(s) => {
            s.keys().all(|k| {
                matches!(
                    k.as_str(),
                    "type" | "description" | "default" | "enum" | "nullable"
                )
            }) || (kind == ParamType::List
                && s.get("items")
                    .and_then(Value::as_object)
                    .is_some_and(|items| {
                        type_of(Some(items)) == ParamType::Text && items.len() <= 1
                    }))
        }
    };
    let schema = schema.map(|mut s| {
        s.shift_remove("description");
        s
    });
    ApiParameter {
        name: name.to_owned(),
        place,
        kind,
        required,
        description: clean_opt(description.as_deref(), 300),
        default: fallback,
        choices,
        schema: if plain { None } else { schema },
    }
}

fn type_of(schema: Option<&Object>) -> ParamType {
    let declared = match schema.and_then(|s| s.get("type")) {
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null"),
        Some(t) => t.as_str(),
        None => None,
    };
    let declared = declared.or_else(|| {
        schema
            .and_then(|s| s.get("properties"))
            .filter(|p| !p.is_null())
            .map(|_| "object")
    });
    declared
        .and_then(ParamType::parse)
        .unwrap_or(ParamType::Text)
}

fn is_json(content_type: &str) -> bool {
    let kind = content_type.split(';').next().unwrap_or("").trim();
    kind.eq_ignore_ascii_case("application/json")
        || kind.to_ascii_lowercase().ends_with("+json")
        || kind == "*/*"
}

fn is_reserved(header: &str) -> bool {
    ["Accept", "Content-Type", "Authorization"]
        .iter()
        .any(|h| header.eq_ignore_ascii_case(h))
}

fn without(node: &Object, keys: &[&str]) -> Object {
    node.iter()
        .filter(|(k, _)| !keys.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn str_of<'v>(o: &'v Object, key: &str) -> Option<&'v str> {
    o.get(key).and_then(Value::as_str)
}

// =========================================================================
// JSON or YAML
// =========================================================================

/// The document as JSON: it already is, or it's YAML (a superset of JSON). In JSON,
/// comments and trailing commas are accepted; duplicate keys aren't.
pub fn document(text: &str) -> Result<Value, OpenApiError> {
    let text = text.trim_start_matches('\u{FEFF}');
    if text.trim_start().starts_with('{') {
        return json::parse_str(&relax(text))
            .map_err(|e| OpenApiError(format!("The JSON isn't valid: {e}")));
    }
    match yaml::parse(text) {
        Ok(Value::Null)
            if text
                .lines()
                .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')) =>
        {
            fail("The document is empty.")
        }
        Ok(value) => Ok(value),
        Err(e) => fail(format!("Neither JSON nor valid YAML: {e}")),
    }
}

/// The JSON without comments (`//`, `/* */`) or commas before `}` or `]`. Outside of
/// strings only; comments become spaces, so error positions stay the same.
fn relax(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut plain: Vec<char> = Vec::with_capacity(chars.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            plain.push(c);
            if c == '\\' && i + 1 < chars.len() {
                plain.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_string = true;
                plain.push(c);
                i += 1;
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    plain.push(' ');
                    i += 1;
                }
            }
            ('/', Some('*')) => {
                plain.extend([' ', ' ']);
                i += 2;
                while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                    plain.push(if chars[i] == '\n' { '\n' } else { ' ' });
                    i += 1;
                }
                if i < chars.len() {
                    plain.extend([' ', ' ']);
                    i += 2;
                }
            }
            _ => {
                plain.push(c);
                i += 1;
            }
        }
    }
    // Trailing commas: a comma followed (after whitespace) by } or ], outside of strings.
    let mut out = String::with_capacity(plain.len());
    let mut in_string = false;
    let mut escaped = false;
    for (at, &c) in plain.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ','
            && matches!(
                plain[at + 1..].iter().find(|ch| !ch.is_whitespace()),
                Some('}' | ']')
            )
        {
            out.push(' ');
            continue;
        }
        out.push(c);
    }
    out
}

/// A simple value as text: "2.0" stays "2.0" even if the YAML read it as a number.
fn scalar(node: Option<&Value>) -> Option<String> {
    match node? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(if *b { "true" } else { "false" }.into()),
        _ => None,
    }
}

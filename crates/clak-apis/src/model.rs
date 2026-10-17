//! The model of a REST API: how it recognizes its caller, its endpoints with their
//! parameters, and how it's saved. Every JSON shape here (the file, the command for agents)
//! stays fixed: commands have a signature, and changing their shape
//! would mean asking permission again for all of them.

use crate::text::{self, clean, is_loopback, utf16_len, utf16_prefix};
use clak_connectors::OAuthGrant;
use clak_hub::cli_names;
use clak_wire::json;
use serde_json::{Map, Value};
use std::collections::HashSet;
use url::Url;

/// A JSON object (a schema, the arguments of a command).
pub type Object = Map<String, Value>;

/// A string, if the value is one (`Wire.Str`).
fn str_of<'a>(o: &'a Object, key: &str) -> Option<&'a str> {
    o.get(key).and_then(Value::as_str)
}

fn opt(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |v| Value::String(v.to_owned()))
}

fn names(list: &[String]) -> Value {
    Value::Array(list.iter().cloned().map(Value::String).collect())
}

// =========================================================================
// Access
// =========================================================================

/// How an API recognizes its caller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AuthKind {
    #[default]
    None,
    Bearer,
    /// A key in a header.
    Header,
    /// A key in the query.
    Query,
    Basic,
    /// Sign-in in the browser: authorization code, PKCE, redirect to loopback.
    OAuth,
    /// "App only": client credentials.
    OAuthApp,
}

impl AuthKind {
    pub const ALL: [AuthKind; 7] = [
        AuthKind::None,
        AuthKind::Bearer,
        AuthKind::Header,
        AuthKind::Query,
        AuthKind::Basic,
        AuthKind::OAuth,
        AuthKind::OAuthApp,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AuthKind::None => "none",
            AuthKind::Bearer => "bearer",
            AuthKind::Header => "header",
            AuthKind::Query => "query",
            AuthKind::Basic => "basic",
            AuthKind::OAuth => "oauth",
            AuthKind::OAuthApp => "oauth-app",
        }
    }

    pub fn parse(text: &str) -> Option<AuthKind> {
        AuthKind::ALL.into_iter().find(|k| k.as_str() == text)
    }
}

/// How an API recognizes its caller. The secret (token, key, password, the OAuth access)
/// isn't here: it's stored encrypted, separately. `name` is the header or parameter name
/// for a key, the user name for Basic.
///
/// With OAuth: `OAuth` is the sign-in in the browser, `OAuthApp` the "app only" one. The
/// URLs and scopes come from the OpenAPI or from the user; the client ID is that of the app
/// the user registered with the service.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiAuth {
    pub kind: AuthKind,
    pub name: Option<String>,
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    pub scope: Option<String>,
    pub client_id: Option<String>,
}

impl ApiAuth {
    pub fn new(kind: AuthKind) -> ApiAuth {
        ApiAuth {
            kind,
            ..ApiAuth::default()
        }
    }

    /// A key (the header or parameter name) or Basic (the user name).
    pub fn named(kind: AuthKind, name: &str) -> ApiAuth {
        ApiAuth {
            kind,
            name: Some(name.to_owned()),
            ..ApiAuth::default()
        }
    }

    pub fn oauth(
        kind: AuthKind,
        authorization_url: Option<&str>,
        token_url: Option<&str>,
        scope: Option<&str>,
        client_id: Option<&str>,
    ) -> ApiAuth {
        let own = |s: Option<&str>| s.map(str::to_owned);
        ApiAuth {
            kind,
            name: None,
            authorization_url: own(authorization_url),
            token_url: own(token_url),
            scope: own(scope),
            client_id: own(client_id),
        }
    }

    pub fn is_oauth(&self) -> bool {
        matches!(self.kind, AuthKind::OAuth | AuthKind::OAuthApp)
    }

    /// What's wrong, or nothing.
    pub fn problem(&self) -> Option<String> {
        let blank = |s: &Option<String>| s.as_deref().is_none_or(|s| s.trim().is_empty());
        let message = match self.kind {
            AuthKind::None | AuthKind::Bearer => return None,
            AuthKind::Header | AuthKind::Query if blank(&self.name) => {
                if self.kind == AuthKind::Header {
                    "The key needs the name of its header."
                } else {
                    "The key needs the name of its parameter."
                }
            }
            AuthKind::Header if !ApiEndpoint::header_name(self.name.as_deref().unwrap_or("")) => {
                "That header name isn't valid."
            }
            AuthKind::Basic if self.name.as_deref().is_none_or(str::is_empty) => {
                "Basic authentication needs a user name."
            }
            AuthKind::Header | AuthKind::Query | AuthKind::Basic => return None,
            AuthKind::OAuth if !secure_url(self.authorization_url.as_deref()) => {
                "OAuth needs the sign-in (authorization) address, on https."
            }
            _ if !secure_url(self.token_url.as_deref()) => {
                "OAuth needs the token address, on https."
            }
            _ if blank(&self.client_id) => {
                "OAuth needs the client ID of the app you registered with the service."
            }
            _ => return None,
        };
        Some(message.to_owned())
    }

    /// The host of the sign-in service, for the panel.
    pub fn sign_in_host(&self) -> Option<String> {
        let url = if self.kind == AuthKind::OAuth {
            &self.authorization_url
        } else {
            &self.token_url
        };
        Url::parse(url.as_deref()?)
            .ok()?
            .host_str()
            .map(str::to_owned)
    }

    /// In short, for the panel: "Bearer token", "key in header X-API-Key", "OAuth at github.com".
    pub fn label(&self) -> String {
        let name = self.name.as_deref().unwrap_or("");
        let host = self.sign_in_host().unwrap_or_default();
        match self.kind {
            AuthKind::Bearer => "Bearer token".into(),
            AuthKind::Header => format!("key in header {name}"),
            AuthKind::Query => format!("key in ?{name}="),
            AuthKind::Basic => format!("Basic, user {name}"),
            AuthKind::OAuth => format!("OAuth at {host}"),
            AuthKind::OAuthApp => format!("OAuth (app only) at {host}"),
            AuthKind::None => "no authentication".into(),
        }
    }

    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("kind".into(), self.kind.as_str().into());
        o.insert("name".into(), opt(self.name.as_deref()));
        o.insert(
            "authorization_url".into(),
            opt(self.authorization_url.as_deref()),
        );
        o.insert("token_url".into(), opt(self.token_url.as_deref()));
        o.insert("scope".into(), opt(self.scope.as_deref()));
        o.insert("client_id".into(), opt(self.client_id.as_deref()));
        Value::Object(o)
    }

    /// An access that can't be understood is no access.
    pub fn from_json(node: Option<&Value>) -> ApiAuth {
        let Some(o) = node.and_then(Value::as_object) else {
            return ApiAuth::default();
        };
        let Some(kind) = str_of(o, "kind").and_then(AuthKind::parse) else {
            return ApiAuth::default();
        };
        let s = |key: &str| str_of(o, key).filter(|v| !v.is_empty()).map(str::to_owned);
        ApiAuth {
            kind,
            name: s("name"),
            authorization_url: s("authorization_url"),
            token_url: s("token_url"),
            scope: s("scope"),
            client_id: s("client_id"),
        }
    }
}

/// https, or http to this computer: a code or a token never travels in plain text.
fn secure_url(url: Option<&str>) -> bool {
    match url.and_then(|u| Url::parse(u).ok()) {
        Some(u) => u.scheme() == "https" || (u.scheme() == "http" && is_loopback(&u)),
        None => false,
    }
}

// =========================================================================
// Parameters
// =========================================================================

/// Where a parameter goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ParamIn {
    /// In the path: `/users/{id}`.
    Path,
    Query,
    Header,
    /// A field of the body, JSON or form.
    Body,
}

impl ParamIn {
    pub const ALL: [ParamIn; 4] = [
        ParamIn::Path,
        ParamIn::Query,
        ParamIn::Header,
        ParamIn::Body,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ParamIn::Path => "path",
            ParamIn::Query => "query",
            ParamIn::Header => "header",
            ParamIn::Body => "body",
        }
    }

    pub fn parse(text: &str) -> Option<ParamIn> {
        ParamIn::ALL.into_iter().find(|p| p.as_str() == text)
    }
}

/// The type of a parameter, as a JSON schema states it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ParamType {
    #[default]
    Text,
    Integer,
    Number,
    Boolean,
    List,
    Object,
}

impl ParamType {
    pub const ALL: [ParamType; 6] = [
        ParamType::Text,
        ParamType::Integer,
        ParamType::Number,
        ParamType::Boolean,
        ParamType::List,
        ParamType::Object,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ParamType::Text => "string",
            ParamType::Integer => "integer",
            ParamType::Number => "number",
            ParamType::Boolean => "boolean",
            ParamType::List => "array",
            ParamType::Object => "object",
        }
    }

    pub fn parse(text: &str) -> Option<ParamType> {
        ParamType::ALL.into_iter().find(|t| t.as_str() == text)
    }
}

/// A parameter of an endpoint. `schema` is present when it comes from an OpenAPI and says
/// more than the type (an object, a format).
#[derive(Clone, Debug, PartialEq)]
pub struct ApiParameter {
    pub name: String,
    pub place: ParamIn,
    pub kind: ParamType,
    pub required: bool,
    pub description: Option<String>,
    pub default: Option<String>,
    pub choices: Option<Vec<String>>,
    pub schema: Option<Object>,
}

impl ApiParameter {
    /// A text parameter, optional (except in the path, where it can't be missing).
    pub fn new(name: &str, place: ParamIn) -> ApiParameter {
        ApiParameter {
            name: name.to_owned(),
            place,
            kind: ParamType::Text,
            required: false,
            description: None,
            default: None,
            choices: None,
            schema: None,
        }
    }

    pub fn of_type(mut self, kind: ParamType) -> ApiParameter {
        self.kind = kind;
        self
    }

    pub fn required(mut self) -> ApiParameter {
        self.required = true;
        self
    }

    /// The JSON schema for the command: the OpenAPI one, or one built from the type.
    pub fn to_schema(&self) -> Object {
        let mut schema = self.schema.clone().unwrap_or_else(|| {
            let mut s = Object::new();
            s.insert("type".into(), self.kind.as_str().into());
            s
        });
        if self.schema.is_none() && self.kind == ParamType::List {
            let mut items = Object::new();
            items.insert("type".into(), ParamType::Text.as_str().into());
            schema.insert("items".into(), Value::Object(items));
        }
        if let Some(description) = self.description.as_deref().filter(|d| !d.is_empty()) {
            schema.insert("description".into(), description.into());
        }
        if self.schema.is_none() {
            if let Some(choices) = self.choices.as_ref().filter(|c| !c.is_empty()) {
                schema.insert("enum".into(), names(choices));
            }
            if let Some(fallback) = &self.default {
                schema.insert("default".into(), typed_default(self.kind, fallback));
            }
        }
        schema
    }

    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("name".into(), self.name.clone().into());
        o.insert("in".into(), self.place.as_str().into());
        o.insert("type".into(), self.kind.as_str().into());
        o.insert("required".into(), self.required.into());
        o.insert("description".into(), opt(self.description.as_deref()));
        o.insert("default".into(), opt(self.default.as_deref()));
        o.insert(
            "choices".into(),
            self.choices
                .as_ref()
                .filter(|c| !c.is_empty())
                .map_or(Value::Null, |c| names(c)),
        );
        o.insert(
            "schema".into(),
            self.schema.clone().map_or(Value::Null, Value::Object),
        );
        Value::Object(o)
    }

    pub fn from_json(node: &Value) -> Option<ApiParameter> {
        let o = node.as_object()?;
        let name = str_of(o, "name").filter(|n| !n.is_empty())?;
        let place = str_of(o, "in").and_then(ParamIn::parse)?;
        Some(ApiParameter {
            name: name.to_owned(),
            place,
            kind: str_of(o, "type")
                .and_then(ParamType::parse)
                .unwrap_or(ParamType::Text),
            required: json::truthy(o.get("required")) || place == ParamIn::Path,
            description: str_of(o, "description").map(str::to_owned),
            default: str_of(o, "default").map(str::to_owned),
            choices: o.get("choices").and_then(Value::as_array).map(|c| {
                c.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            }),
            schema: o.get("schema").and_then(Value::as_object).cloned(),
        })
    }
}

/// The default value with the right type: "20" for an integer is 20.
fn typed_default(kind: ParamType, fallback: &str) -> Value {
    match kind {
        ParamType::Integer => fallback.trim().parse::<i64>().map(Value::from).ok(),
        ParamType::Number => fallback
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite())
            .map(json::float),
        ParamType::Boolean => match fallback.trim().to_ascii_lowercase().as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        _ => None,
    }
    .unwrap_or_else(|| fallback.into())
}

// =========================================================================
// Endpoints
// =========================================================================

/// What body a request has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BodyKind {
    Json,
    /// `application/x-www-form-urlencoded`.
    Form,
}

impl BodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyKind::Json => "json",
            BodyKind::Form => "form",
        }
    }

    pub fn parse(text: &str) -> Option<BodyKind> {
        match text {
            "json" => Some(BodyKind::Json),
            "form" => Some(BodyKind::Form),
            _ => None,
        }
    }
}

/// An endpoint, which for agents is a command: `clak github list_repos --owner x`.
///
/// The body: with `Json` the "body" parameters become the fields of a JSON object; if there
/// are none, the command takes a `body` argument with the whole JSON (described by
/// `body_schema`, if the OpenAPI says so). With `Form`, the "body" parameters go in a form.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiEndpoint {
    pub name: String,
    pub method: String,
    pub path: String,
    pub description: Option<String>,
    pub parameters: Vec<ApiParameter>,
    pub body: Option<BodyKind>,
    pub body_schema: Option<Object>,
    pub body_required: bool,
}

/// The HTTP methods an endpoint can have.
pub const METHODS: [&str; 7] = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

impl ApiEndpoint {
    /// The argument with the whole body, when the body has no fields of its own.
    pub const BODY_ARGUMENT: &'static str = "body";

    /// An endpoint without a body.
    pub fn new(
        name: &str,
        method: &str,
        path: &str,
        description: Option<&str>,
        parameters: Vec<ApiParameter>,
    ) -> ApiEndpoint {
        ApiEndpoint {
            name: name.to_owned(),
            method: method.to_owned(),
            path: path.to_owned(),
            description: description.map(str::to_owned),
            parameters,
            body: None,
            body_schema: None,
            body_required: false,
        }
    }

    pub fn with_body(mut self, body: BodyKind) -> ApiEndpoint {
        self.body = Some(body);
        self
    }

    pub fn has_body(&self) -> bool {
        self.body.is_some()
    }

    /// The body as a single argument: a JSON with no declared fields.
    pub fn whole_body(&self) -> bool {
        self.body == Some(BodyKind::Json)
            && !self.parameters.iter().any(|p| p.place == ParamIn::Body)
    }

    /// The names in braces in the path: `/users/{id}/repos` → `id`.
    pub fn path_names(path: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut from = 0;
        while let Some(at) = path[from..].find('{').map(|i| i + from) {
            let Some(end) = path[at..].find('}').map(|i| i + at) else {
                break;
            };
            if end > at + 1 {
                found.push(path[at + 1..end].to_owned());
            }
            from = at + 1;
        }
        found
    }

    /// A command name: letters, digits, `_ . -`, at most 64 characters.
    pub fn valid_name(name: &str) -> bool {
        (1..=64).contains(&name.len())
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
    }

    /// An HTTP header name: a token, with no spaces or colons.
    pub fn header_name(name: &str) -> bool {
        (1..=128).contains(&name.len())
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
    }

    /// A query parameter name: nothing that would break the URL.
    fn query_name(name: &str) -> bool {
        (1..=128).contains(&name.len())
            && name
                .bytes()
                .all(|c| c > b' ' && c < 127 && !matches!(c, b'&' | b'=' | b'#' | b'?'))
    }

    /// A command name from method and path: GET /users/{id} → `get_users_id`.
    pub fn name_from(method: &str, path: &str) -> String {
        let mut parts = vec![method.to_lowercase()];
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let mapped: String = part
                .encode_utf16()
                .map(|u| match char::from_u32(u as u32) {
                    Some(c) if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
                    _ => '_',
                })
                .collect();
            let trimmed = mapped.trim_matches('_');
            if !trimmed.is_empty() {
                parts.push(trimmed.to_owned());
            }
        }
        let name = collapse(&parts.join("_"));
        if name.chars().count() > 64 {
            name.chars()
                .take(64)
                .collect::<String>()
                .trim_end_matches('_')
                .to_owned()
        } else {
            name
        }
    }

    /// Any name (an operationId) made valid: odd characters become `_`.
    pub fn sanitize(raw: &str) -> String {
        let mapped: String = raw
            .trim()
            .encode_utf16()
            .map(|u| match char::from_u32(u as u32) {
                Some(c) if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') => c,
                _ => '_',
            })
            .collect();
        let name = collapse(mapped.trim_matches('_'));
        if name.len() > 64 {
            name[..64].to_owned()
        } else {
            name
        }
    }

    /// What's wrong with this endpoint, or nothing.
    pub fn problem(&self) -> Option<String> {
        let name = &self.name;
        if !Self::valid_name(name) {
            return Some(format!(
                "“{name}”: a command name has letters, digits, _ . - (64 at most)."
            ));
        }
        if !METHODS.contains(&self.method.as_str()) {
            return Some(format!("{name}: unknown method {}.", self.method));
        }
        let path = &self.path;
        if !path.starts_with('/')
            || path.contains('#')
            || path.contains('?')
            || path.chars().any(char::is_whitespace)
        {
            return Some(format!(
                "{name}: the path must start with / and have no spaces, ? or #."
            ));
        }
        let mut seen = HashSet::new();
        for p in &self.parameters {
            if !Self::valid_name(&p.name)
                && !(p.place == ParamIn::Header && Self::header_name(&p.name))
                && !(p.place == ParamIn::Query && Self::query_name(&p.name))
            {
                return Some(format!(
                    "{name}: the parameter “{}” has an invalid name.",
                    p.name
                ));
            }
            if !seen.insert(p.name.as_str()) {
                return Some(format!("{name}: two parameters are called “{}”.", p.name));
            }
            if p.place == ParamIn::Body && !self.has_body() {
                return Some(format!(
                    "{name}: “{}” goes in the body, but the endpoint has none.",
                    p.name
                ));
            }
        }
        if self.whole_body() && seen.contains(Self::BODY_ARGUMENT) {
            return Some(format!(
                "{name}: a parameter can't be called “body” when the body is whole."
            ));
        }
        let in_path = Self::path_names(path);
        for used in &in_path {
            if !self
                .parameters
                .iter()
                .any(|p| p.place == ParamIn::Path && &p.name == used)
            {
                return Some(format!(
                    "{name}: the path uses {{{used}}}, which isn't a parameter."
                ));
            }
        }
        for p in self.parameters.iter().filter(|p| p.place == ParamIn::Path) {
            if !in_path.contains(&p.name) {
                return Some(format!(
                    "{name}: “{0}” is a path parameter, but the path has no {{{0}}}.",
                    p.name
                ));
            }
        }
        None
    }

    /// The command for agents: the summary, then method and path, then the parameters.
    /// The shape is `{"name", "description", "input_schema"}`, like `ToolDef` on the hub.
    pub fn to_tool(&self) -> Value {
        let mut properties = Object::new();
        let mut required = Vec::new();
        for p in &self.parameters {
            properties.insert(p.name.clone(), Value::Object(p.to_schema()));
            if p.required {
                required.push(Value::String(p.name.clone()));
            }
        }
        if self.whole_body() {
            let mut schema = self.body_schema.clone().unwrap_or_default();
            let description = match schema.get("description") {
                Some(d) => format!("The JSON body. {}", d.as_str().unwrap_or("")),
                None => "The JSON body.".to_owned(),
            };
            schema.insert("description".into(), description.into());
            properties.insert(Self::BODY_ARGUMENT.into(), Value::Object(schema));
            if self.body_required {
                required.push(Self::BODY_ARGUMENT.into());
            }
        }
        let mut input = Object::new();
        input.insert("type".into(), "object".into());
        input.insert("properties".into(), Value::Object(properties));
        if !required.is_empty() {
            input.insert("required".into(), Value::Array(required));
        }
        let line = format!("{} {}", self.method, self.path);
        let mut text = match self.description.as_deref().filter(|d| !d.is_empty()) {
            Some(description) => format!("{}\n\n{line}", description.trim()),
            None => line,
        };
        if utf16_len(&text) > 1500 {
            text = format!("{}…", utf16_prefix(&text, 1500));
        }
        let mut tool = Object::new();
        tool.insert("name".into(), self.name.clone().into());
        tool.insert("description".into(), text.into());
        tool.insert("input_schema".into(), Value::Object(input));
        Value::Object(tool)
    }

    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("name".into(), self.name.clone().into());
        o.insert("method".into(), self.method.clone().into());
        o.insert("path".into(), self.path.clone().into());
        o.insert("description".into(), opt(self.description.as_deref()));
        o.insert(
            "parameters".into(),
            Value::Array(self.parameters.iter().map(ApiParameter::to_json).collect()),
        );
        o.insert("body".into(), opt(self.body.map(BodyKind::as_str)));
        o.insert(
            "body_schema".into(),
            self.body_schema.clone().map_or(Value::Null, Value::Object),
        );
        o.insert("body_required".into(), self.body_required.into());
        Value::Object(o)
    }

    /// From the file: an invalid endpoint is discarded.
    pub fn from_json(node: &Value) -> Option<ApiEndpoint> {
        let o = node.as_object()?;
        let endpoint = ApiEndpoint {
            name: str_of(o, "name")?.to_owned(),
            method: str_of(o, "method")?.to_uppercase(),
            path: str_of(o, "path")?.to_owned(),
            description: str_of(o, "description").map(str::to_owned),
            parameters: o
                .get("parameters")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(ApiParameter::from_json).collect())
                .unwrap_or_default(),
            body: str_of(o, "body").and_then(BodyKind::parse),
            body_schema: o.get("body_schema").and_then(Value::as_object).cloned(),
            body_required: json::truthy(o.get("body_required")),
        };
        endpoint.problem().is_none().then_some(endpoint)
    }

    /// An endpoint written by an agent (`clak settings`): like `from_json`, but instead of
    /// discarding what's wrong it says so, so whoever wrote it can fix it.
    pub fn read(node: &Value) -> Result<ApiEndpoint, String> {
        let Some(o) = node.as_object() else {
            return Err("an endpoint must be a JSON object".into());
        };
        let Some(name) = str_of(o, "name").filter(|n| !n.is_empty()) else {
            return Err("an endpoint needs a “name”".into());
        };
        let Some(method) = str_of(o, "method").filter(|m| !m.is_empty()) else {
            return Err(format!("{name}: “method” is missing (GET, POST…)"));
        };
        let Some(path) = str_of(o, "path").filter(|p| !p.is_empty()) else {
            return Err(format!("{name}: “path” is missing (/users/{{id}})"));
        };
        let list = match o.get("parameters") {
            None | Some(Value::Null) => &Vec::new(),
            Some(Value::Array(list)) => list,
            Some(_) => return Err(format!("{name}: “parameters” must be a list")),
        };
        let mut parameters = Vec::new();
        for item in list {
            let Some(p) = item.as_object() else {
                return Err(format!("{name}: every parameter needs a “name”"));
            };
            let Some(parameter) = str_of(p, "name").filter(|n| !n.is_empty()) else {
                return Err(format!("{name}: every parameter needs a “name”"));
            };
            if str_of(p, "in").and_then(ParamIn::parse).is_none() {
                let places: Vec<&str> = ParamIn::ALL.iter().map(|p| p.as_str()).collect();
                return Err(format!(
                    "{name}: “{parameter}” needs “in”: {}",
                    places.join(", ")
                ));
            }
            if let Some(kind) = str_of(p, "type").filter(|t| ParamType::parse(t).is_none()) {
                let types: Vec<&str> = ParamType::ALL.iter().map(|t| t.as_str()).collect();
                return Err(format!(
                    "{name}: “{parameter}” has an unknown type “{kind}” ({})",
                    types.join(", ")
                ));
            }
            parameters.extend(ApiParameter::from_json(item));
        }
        let body = match str_of(o, "body").filter(|b| !b.is_empty() && *b != "none") {
            None => None,
            Some(b) => match BodyKind::parse(b) {
                Some(kind) => Some(kind),
                None => return Err(format!("{name}: “body” is “json”, “form” or nothing")),
            },
        };
        let endpoint = ApiEndpoint {
            name: name.to_owned(),
            method: method.to_uppercase(),
            path: path.to_owned(),
            description: str_of(o, "description").map(str::to_owned),
            parameters,
            body,
            body_schema: o.get("body_schema").and_then(Value::as_object).cloned(),
            body_required: json::truthy(o.get("body_required")),
        };
        match endpoint.problem() {
            Some(problem) => Err(problem),
            None => Ok(endpoint),
        }
    }
}

/// Double underscores become one.
fn collapse(text: &str) -> String {
    let mut name = text.to_owned();
    while name.contains("__") {
        name = name.replace("__", "_");
    }
    name
}

// =========================================================================
// The API
// =========================================================================

/// A REST API as the user added it: for agents it's a program, with one command per
/// endpoint. `source`: the URL of the OpenAPI it comes from, to read it again.
#[derive(Clone, Debug, PartialEq)]
pub struct ApiEntry {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub enabled: bool,
    pub since: f64,
    pub auth: ApiAuth,
    pub endpoints: Vec<ApiEndpoint>,
    pub command: Option<String>,
    pub source: Option<String>,
}

impl ApiEntry {
    pub fn to_json(&self) -> Value {
        let mut o = Object::new();
        o.insert("id".into(), self.id.clone().into());
        o.insert("name".into(), self.name.clone().into());
        o.insert("base_url".into(), self.base_url.clone().into());
        o.insert("enabled".into(), self.enabled.into());
        o.insert("since".into(), json::float(self.since));
        o.insert("auth".into(), self.auth.to_json());
        o.insert("command".into(), opt(self.command.as_deref()));
        o.insert("source".into(), opt(self.source.as_deref()));
        o.insert(
            "endpoints".into(),
            Value::Array(self.endpoints.iter().map(ApiEndpoint::to_json).collect()),
        );
        Value::Object(o)
    }

    pub fn from_json(node: &Value) -> Option<ApiEntry> {
        let o = node.as_object()?;
        Some(ApiEntry {
            id: str_of(o, "id")?.to_owned(),
            name: str_of(o, "name")?.to_owned(),
            base_url: str_of(o, "base_url")?.to_owned(),
            enabled: o.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            since: o.get("since").and_then(Value::as_f64).unwrap_or(0.0),
            auth: ApiAuth::from_json(o.get("auth")),
            endpoints: o
                .get("endpoints")
                .and_then(Value::as_array)
                .map(|list| list.iter().filter_map(ApiEndpoint::from_json).collect())
                .unwrap_or_default(),
            command: str_of(o, "command")
                .filter(|c| !c.is_empty() && cli_names::problem(Some(c)).is_none())
                .map(str::to_owned),
            source: str_of(o, "source").map(str::to_owned),
        })
    }

    /// The commands for agents, one per endpoint.
    pub fn tools(&self) -> Vec<Value> {
        self.endpoints.iter().map(ApiEndpoint::to_tool).collect()
    }

    /// The service for consent prompts: where it is, whether it's encrypted, whether it's
    /// this computer. Nothing if the base URL can't be parsed.
    pub fn origin(&self) -> Option<Value> {
        let url = Url::parse(&self.base_url).ok()?;
        let mut o = Object::new();
        o.insert("kind".into(), "connector".into());
        o.insert("protocol".into(), "rest".into());
        o.insert(
            "url".into(),
            text::left_part_path(&url).trim_end_matches('/').into(),
        );
        o.insert("host".into(), url.host_str().unwrap_or("").into());
        o.insert("secure".into(), (url.scheme() == "https").into());
        o.insert("loopback".into(), is_loopback(&url).into());
        Some(Value::Object(o))
    }

    /// Host and port of the base URL, for the panel's "program" row.
    pub fn authority(&self) -> String {
        Url::parse(&self.base_url)
            .map(|u| text::authority(&u))
            .unwrap_or_else(|_| self.base_url.clone())
    }
}

/// What the panel (or an agent, with the user's confirmation) sends to add or change an
/// API. `secret`: none keeps the saved one (in an edit), empty removes it.
/// `grant`: a fresh OAuth sign-in, which takes the place of the secret.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ApiDraft {
    pub name: String,
    pub base_url: String,
    pub auth: ApiAuth,
    pub secret: Option<String>,
    pub endpoints: Vec<ApiEndpoint>,
    pub command: Option<String>,
    pub source: Option<String>,
    pub grant: Option<OAuthGrant>,
}

impl ApiDraft {
    /// What's wrong with an API, or nothing.
    pub fn problem(&self) -> Option<String> {
        if let Some(problem) = clak_connectors::problem(Some(&self.name), Some(&self.base_url)) {
            return Some(
                problem
                    .replace("An address is required", "A base URL is required")
                    .replace("The address", "The base URL"),
            );
        }
        if let Some(command) = cli_names::problem(clean_command(self.command.as_deref()).as_deref())
        {
            return Some(command);
        }
        if let Some(auth) = self.auth.problem() {
            return Some(auth);
        }
        if Url::parse(self.base_url.trim()).is_ok_and(|u| u.query().is_some_and(|q| !q.is_empty()))
        {
            return Some(
                "The base URL can't have a query (?…): parameters go in the endpoints.".into(),
            );
        }
        if self.endpoints.is_empty() {
            return Some("Add at least one endpoint.".into());
        }
        let mut seen = HashSet::new();
        for endpoint in &self.endpoints {
            if let Some(wrong) = endpoint.problem() {
                return Some(wrong);
            }
            if !seen.insert(endpoint.name.as_str()) {
                return Some(format!("Two endpoints are called “{}”.", endpoint.name));
            }
        }
        None
    }

    /// The secret to save: a fresh sign-in, or what the panel wrote.
    pub fn secret_to_save(&self) -> Option<String> {
        match &self.grant {
            Some(grant) => Some(clak_wire::json::text(&grant.to_json())),
            None => self.secret.clone(),
        }
    }

    /// The new API as the hub saves it: the cleaned-up name, the URL without the trailing
    /// `/`, enabled. The secret stays out: it goes in its own file.
    pub fn into_entry(self, id: String, since: f64) -> ApiEntry {
        ApiEntry {
            id,
            name: clean(&self.name, 40),
            base_url: self.base_url.trim().trim_end_matches('/').to_owned(),
            enabled: true,
            since,
            auth: self.auth,
            endpoints: self.endpoints,
            command: clean_command(self.command.as_deref()),
            source: self.source,
        }
    }
}

/// The chosen command, with no surrounding spaces; empty means automatic.
pub fn clean_command(command: Option<&str>) -> Option<String> {
    command
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

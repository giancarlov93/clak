//! The connectors: the hub as a client of MCP servers over HTTP, with a token or OAuth.
//!
//! - [`hub`]: the hub's connectors ([`ConnectorHub`]): add, probe, change token or
//!   credential, turn off, remove; each one is a program connected to the broker;
//! - [`mcp_http`]: the MCP "Streamable HTTP" client (JSON or SSE, session, server confirmations);
//! - [`oauth`]: the OAuth sign-in of MCP servers (discovery, registration, PKCE, loopback,
//!   refresh, revocation) and the variants for REST APIs (known addresses, "app only");
//! - [`token`]: the OAuth sign-in as a token that refreshes itself and saves every refresh;
//! - [`store`]: `connectors.json` and the secrets in the system's secret store;
//! - [`link`]: the `clak://connect?…` links;
//! - [`connector`]: the checks, probing a server, starting a sign-in.

use std::future::Future;
use std::pin::Pin;

pub mod connector;
pub mod hub;
pub mod launch;
pub mod link;
mod live;
mod loopback;
mod mcp;
pub mod mcp_http;
pub mod mcp_stdio;
pub mod oauth;
pub mod store;
mod text;
pub mod token;
pub mod uri;

/// A boxed future, for the traits that must stay usable as `dyn`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub use connector::{ConnectorProbe, fill, probe, problem, same_origin, same_url, unencrypted};
pub use hub::{ConnectorCredential, ConnectorHub};
pub use link::ConnectorLink;
pub use live::phase;
pub use mcp_http::{
    AccessToken, Elicit, McpError, McpHttp, PROTOCOL_VERSION, StaticToken, new_http_client,
};
pub use oauth::{
    OAuthChallenge, OAuthClient, OAuthError, OAuthGrant, OAuthServer, OpenBrowser, Status,
};
pub use store::{ConnectorEntry, ConnectorStore};
pub use token::OAuthToken;

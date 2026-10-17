//! A connector's MCP client, whatever the transport: an HTTP server, or a program on
//! this computer over stdio. The connector cares about the same things.

use crate::mcp_http::{Elicit, McpError, McpHttp};
use crate::mcp_stdio::McpStdio;
use serde_json::{Map, Value};

pub(crate) enum McpClient {
    Http(McpHttp),
    Stdio(McpStdio),
}

impl McpClient {
    pub async fn initialize(&self) -> Result<(), McpError> {
        match self {
            McpClient::Http(c) => c.initialize().await,
            McpClient::Stdio(c) => c.initialize().await,
        }
    }

    pub async fn list_tools(&self) -> Result<Vec<Map<String, Value>>, McpError> {
        match self {
            McpClient::Http(c) => c.list_tools().await,
            McpClient::Stdio(c) => c.list_tools().await,
        }
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &Map<String, Value>,
        elicit: Option<&Elicit>,
    ) -> Result<Map<String, Value>, McpError> {
        match self {
            McpClient::Http(c) => c.call_tool(name, arguments, elicit).await,
            McpClient::Stdio(c) => c.call_tool(name, arguments, elicit).await,
        }
    }

    pub fn instructions(&self) -> Option<String> {
        match self {
            McpClient::Http(c) => c.instructions(),
            McpClient::Stdio(c) => c.instructions(),
        }
    }

    /// Arrives when the server says its commands have changed. Never over HTTP: the list
    /// is re-read every so often.
    pub async fn changed(&self) {
        match self {
            McpClient::Http(_) => std::future::pending().await,
            McpClient::Stdio(c) => c.changed().await,
        }
    }

    /// Arrives when the program stops, with the reason. Never over HTTP.
    pub async fn ended(&self) -> String {
        match self {
            McpClient::Http(_) => std::future::pending().await,
            McpClient::Stdio(c) => c.ended().await,
        }
    }
}

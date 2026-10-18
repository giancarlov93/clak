//! A connector's MCP client, whatever the transport: an HTTP server, or a program on
//! this computer over stdio. The connector cares about the same things.

use crate::mcp_http::{Elicit, McpError, McpHttp};
use crate::mcp_stdio::McpStdio;
use serde_json::{Map, Value};

// Always behind an `Arc`, one per connector: the size of the larger variant costs nothing.
#[allow(clippy::large_enum_variant)]
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

    /// The address of the icon the server declares, if it has one the app can show.
    pub fn icon(&self) -> Option<String> {
        match self {
            McpClient::Http(c) => c.icon(),
            McpClient::Stdio(c) => c.icon(),
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

/// The icon to show among those a server declares in `serverInfo.icons`: one the app can
/// draw (PNG, ICO or SVG, over https or inline), the largest up to 256 pixels; an SVG without
/// sizes counts as a large one.
pub(crate) fn declared_icon(server_info: Option<&Value>) -> Option<String> {
    let icons = server_info?.get("icons")?.as_array()?;
    icons
        .iter()
        .filter_map(|icon| {
            let src = icon.get("src")?.as_str()?;
            let mime = icon.get("mimeType").and_then(Value::as_str).unwrap_or("");
            let lower = src.to_ascii_lowercase();
            let usable = lower.starts_with("https://") || lower.starts_with("data:image/");
            let path = lower.split(['?', '#']).next().unwrap_or("");
            let vector = mime.contains("svg")
                || lower.starts_with("data:image/svg")
                || path.ends_with(".svg");
            usable.then(|| (rank(icon, vector), src.to_owned()))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, src)| src)
}

/// Larger is better, up to 256 pixels; "any" or no size counts as a middling one, or as a
/// large one for an SVG.
fn rank(icon: &Value, vector: bool) -> u32 {
    let no_size = if vector { 192 } else { 48 };
    let sizes = icon.get("sizes").and_then(Value::as_array);
    let side = sizes
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|s| {
            if s == "any" {
                return Some(no_size);
            }
            let (w, _) = s.split_once(['x', 'X'])?;
            w.parse::<u32>().ok()
        })
        .filter(|&w| w <= 256)
        .max();
    side.unwrap_or(no_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_largest_drawable_icon_is_chosen() {
        let info = json!({"name": "demo", "icons": [
            {"src": "https://example.com/icon.svg", "mimeType": "image/svg+xml", "sizes": ["any"]},
            {"src": "https://example.com/16.png", "mimeType": "image/png", "sizes": ["16x16"]},
            {"src": "https://example.com/128.png", "mimeType": "image/png", "sizes": ["128x128"]},
            {"src": "http://example.com/plain.png", "sizes": ["256x256"]},
        ]});
        assert_eq!(
            declared_icon(Some(&info)).as_deref(),
            Some("https://example.com/icon.svg")
        );
        let info = json!({"icons": [
            {"src": "https://example.com/16.png", "sizes": ["16x16"]},
            {"src": "https://example.com/128.png", "sizes": ["128x128"]},
            {"src": "https://example.com/1024.png", "sizes": ["1024x1024"]},
        ]});
        assert_eq!(
            declared_icon(Some(&info)).as_deref(),
            Some("https://example.com/128.png")
        );
    }

    #[test]
    fn inline_icons_count_and_plain_http_does_not() {
        let inline = json!({"icons": [{"src": "data:image/png;base64,AAAA"}]});
        assert_eq!(
            declared_icon(Some(&inline)).as_deref(),
            Some("data:image/png;base64,AAAA")
        );
        let plain = json!({"icons": [{"src": "http://x.test/a.png"}, {"src": "file:///C:/a.png"}]});
        assert_eq!(declared_icon(Some(&plain)), None);
        assert_eq!(declared_icon(Some(&json!({"name": "no icons"}))), None);
        assert_eq!(declared_icon(None), None);
    }
}

//! A `clak://connect?name=…&url=…&token=…` link, parsed.
//! It's a request, not a permission: it fills in the connector window, and the user
//! confirms. Errors never repeat the link's values, because one of them may be a
//! token.

use crate::connector::problem;
use crate::text::{unescape_strict, utf16_len};
use crate::uri;
use clak_hub::texts::clean;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorLink {
    pub name: String,
    pub url: String,
    pub token: Option<String>,
}

impl ConnectorLink {
    pub const MAX_LENGTH: usize = 8192;

    pub const SCHEME: &'static str = "clak";

    /// True if the argument looks like a link, even a malformed one: it must not be mistaken for an option.
    pub fn looks(argument: Option<&str>) -> bool {
        let s = Self::SCHEME;
        argument.is_some_and(|a| {
            a.len() > s.len()
                && a.as_bytes()[s.len()] == b':'
                && a.as_bytes()[..s.len()].eq_ignore_ascii_case(s.as_bytes())
        })
    }

    /// The link, or why it's not valid.
    pub fn parse(raw: Option<&str>) -> Result<ConnectorLink, String> {
        let raw = raw.unwrap_or("");
        if raw.trim().is_empty() || !Self::looks(Some(raw.trim())) {
            return Err("not a clak:// link".into());
        }
        if utf16_len(raw) > Self::MAX_LENGTH {
            return Err("the link is too long".into());
        }
        let text = raw.trim();
        let rest = text[Self::SCHEME.len() + 1..].trim_start_matches('/');
        let (action, query) = match rest.find('?') {
            Some(question) => (&rest[..question], &rest[question + 1..]),
            None => (rest, ""),
        };
        if !action.trim_end_matches('/').eq_ignore_ascii_case("connect") {
            return Err("the link doesn't ask to add a connector (clak://connect?…)".into());
        }

        // Case-insensitive keys: "URL" and "url" are the same, and twice is not allowed.
        let mut values: HashMap<String, String> = HashMap::new();
        let query = query.find('#').map_or(query, |fragment| &query[..fragment]);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = match pair.find('=') {
                Some(equals) => (
                    unescape_strict(&pair[..equals]),
                    unescape_strict(&pair[equals + 1..]),
                ),
                None => (unescape_strict(pair), Some(String::new())),
            };
            let (Some(key), Some(value)) = (key, value) else {
                return Err("the link has an invalid encoding".into());
            };
            let folded = key.to_lowercase();
            if values.contains_key(&folded) {
                return Err(format!("the link repeats “{}”", clean(Some(&key), 20)));
            }
            values.insert(folded, value);
        }

        let url = values
            .get("url")
            .map(|u| u.trim())
            .filter(|u| !u.is_empty())
            .ok_or("the link has no address (url)")?;
        if let Some(wrong) = problem(Some("link"), Some(url)) {
            return Err(format!("the address in the link is not valid: {wrong}"));
        }
        // Without a name, the computer's: the address is valid, so there is one.
        let mut name = values
            .get("name")
            .map(|n| clean(Some(n), 40))
            .unwrap_or_default();
        if name.is_empty() {
            name = clean(uri::absolute(url).map(|u| uri::host(&u)).as_deref(), 40);
        }

        let mut token = None;
        if let Some(secret) = values.get("token").filter(|s| !s.is_empty()) {
            if utf16_len(secret) > 4096
                || secret.chars().any(|c| c.is_control() || c.is_whitespace())
            {
                return Err("the token in the link is not valid".into());
            }
            token = Some(secret.clone());
        }
        Ok(ConnectorLink {
            name,
            url: url.to_owned(),
            token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str, url: &str, token: Option<&str>) -> ConnectorLink {
        ConnectorLink {
            name: name.into(),
            url: url.into(),
            token: token.map(Into::into),
        }
    }

    #[test]
    fn a_clak_link_is_read_and_only_fills_the_form() {
        assert_eq!(
            ConnectorLink::parse(Some(
                "clak://connect?name=Jarvis&url=http%3A%2F%2Fmy-server%3A3000%2Fmcp&token=abc%2B1"
            )),
            Ok(link("Jarvis", "http://my-server:3000/mcp", Some("abc+1")))
        );
        // Without a name, the computer; without a token, no token; upper case and trailing slash are fine.
        assert_eq!(
            ConnectorLink::parse(Some("CLAK://Connect/?url=https://example.com/mcp")),
            Ok(link("example.com", "https://example.com/mcp", None))
        );
        // A name with control characters and text-direction tricks comes through cleaned up.
        assert_eq!(
            ConnectorLink::parse(Some(
                "clak://connect?name=Jar%E2%80%AEvis%0Aevil&url=https://x.it/mcp"
            ))
            .unwrap()
            .name,
            "Jarvis evil"
        );
    }

    #[test]
    fn it_looks_like_a_link_only_with_the_clak_scheme() {
        assert!(ConnectorLink::looks(Some(
            "clak://connect?url=https://x.it"
        )));
        assert!(ConnectorLink::looks(Some("CLAK:connect")));
        assert!(!ConnectorLink::looks(Some("clak")));
        assert!(!ConnectorLink::looks(Some(
            "clakk://connect?url=https://x.it"
        )));
        assert!(!ConnectorLink::looks(None));
    }

    #[test]
    fn a_malformed_link_says_why_without_repeating_its_values() {
        let cases = [
            ("clak://connect?name=X", "the link has no address (url)"),
            (
                "clak://delete?url=https://x.it",
                "the link doesn't ask to add a connector (clak://connect?…)",
            ),
            (
                "clak://connect?url=file:///C:/Windows",
                "the address in the link is not valid: The address must start with http:// or https://.",
            ),
            (
                "clak://connect?url=javascript:alert(1)",
                "the address in the link is not valid: The address must start with http:// or https://.",
            ),
            (
                "clak://connect?url=https://me:secret@x.it/mcp",
                "the address in the link is not valid: The address can't contain a user name or password: the token goes in its own field.",
            ),
            (
                "clak://connect?url=https://x.it&url=https://y.it",
                "the link repeats “url”",
            ),
            (
                "clak://connect?url=https://x.it&token=a%20b",
                "the token in the link is not valid",
            ),
            (
                "clak://connect?url=https://x.it&name=%FF",
                "the link has an invalid encoding",
            ),
            ("https://x.it", "not a clak:// link"),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                ConnectorLink::parse(Some(raw)),
                Err(expected.to_owned()),
                "{raw}"
            );
        }
    }

    #[test]
    fn a_too_long_link_is_not_read() {
        let raw = format!(
            "clak://connect?url=https://x.it&name={}",
            "a".repeat(ConnectorLink::MAX_LENGTH)
        );
        assert_eq!(
            ConnectorLink::parse(Some(&raw)),
            Err("the link is too long".to_owned())
        );
    }
}

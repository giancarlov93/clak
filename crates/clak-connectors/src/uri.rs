//! The questions about addresses (is it absolute? is it this computer? what's its
//! origin?), with the `url` crate.

use url::{Host, Url};

/// An absolute address, or `None`.
pub fn absolute(text: &str) -> Option<Url> {
    Url::parse(text).ok()
}

/// This computer: `localhost` or a loopback address.
pub fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// The address's host, without port; empty if there is none.
pub fn host(url: &Url) -> String {
    match url.host() {
        Some(Host::Ipv6(ip)) => format!("[{ip}]"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// Scheme, host, and port if it's not the default one.
pub fn origin(url: &Url) -> String {
    match url.port() {
        Some(port) => format!("{}://{}:{port}", url.scheme(), host(url)),
        None => format!("{}://{}", url.scheme(), host(url)),
    }
}

/// The same scheme, host and port, ignoring case.
pub fn same_server(a: &Url, b: &Url) -> bool {
    a.scheme().eq_ignore_ascii_case(b.scheme())
        && host(a).eq_ignore_ascii_case(&host(b))
        && a.port_or_known_default() == b.port_or_known_default()
}

/// https, or http to this computer: a code or a token never travel in the clear over the network.
pub fn secure(url: &Url) -> bool {
    url.scheme() == "https" || (url.scheme() == "http" && is_loopback(url))
}

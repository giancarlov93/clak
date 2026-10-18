//! The icons of the MCP servers and REST APIs, for the panel, the tray box and the tray menu:
//! the one an MCP server declares (`serverInfo.icons`), otherwise the one its website declares
//! in its home page (`apple-touch-icon`, then `icon`), otherwise `/favicon.ico`. A server at
//! `mcp.example.com` (or an API at `api.example.com`) also tries the site of `example.com`.
//!
//! Over the network only with the Server icons preference, off by default: it tells the sites
//! that Clak uses them. Without it, only an icon a server declares inline (`data:`) shows.
//!
//! The app fetches them, never the page, and only for the servers and APIs the hub has: the
//! page names one (`mcp:<id>`, `api:<id>`), not an address. What reaches the page is a PNG
//! `data:` URL; SVG is drawn into pixels here, because the tray menu needs them. The icons
//! are kept in `<home>/icons` for a week; one without an icon is asked again at the next start.

use crate::bridge;
use base64::Engine;
use reqwest::Url;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};
use tauri::AppHandle;

/// The side of the stored icon: larger ones are scaled down, smaller ones kept as they are.
const SIDE: u32 = 128;
const FRESH: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const PAGE_LIMIT: usize = 1 << 20;
const IMAGE_LIMIT: usize = 512 << 10;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
/// For all the attempts of one server together.
const BUDGET: Duration = Duration::from_secs(30);
/// The first labels of a host that name the service, not the company: `mcp.notion.com` is
/// Notion's.
const SERVICE_LABELS: [&str; 3] = ["mcp", "api", "server"];
/// An SVG without sizes counts as this many pixels: it draws sharp at any size.
const VECTOR: u32 = 192;

/// The hub's own programs (the settings): Clak's icon, the one built into the app.
pub const CLAK: &str = "clak";

/// By source: the stored PNG, or `None` for one without an icon.
static FOUND: LazyLock<Mutex<HashMap<String, Option<Vec<u8>>>>> = LazyLock::new(Mutex::default);
/// One server at a time: the panel and the tray asking together fetch once.
static FETCHING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Where an icon comes from: what the server declares, and the address of its site.
#[derive(Debug, PartialEq)]
struct Source {
    declared: Option<String>,
    site: Option<Url>,
    file: String,
}

/// The icon of `source` (`mcp:<id>` or `api:<id>`), `size` pixels wide (real pixels).
#[tauri::command]
pub async fn server_icon(app: AppHandle, source: String, size: u32) -> Option<String> {
    let png = icon(&app, &source).await?;
    let png = scaled(&png, size.clamp(16, 256))?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(png)
    ))
}

/// The same icon as PNG bytes, for the tray menu. Blocking: off the main thread.
pub fn png(app: &AppHandle, source: &str, size: u32) -> Option<Vec<u8>> {
    let png = tauri::async_runtime::block_on(icon(app, source))?;
    scaled(&png, size)
}

async fn icon(app: &AppHandle, key: &str) -> Option<Vec<u8>> {
    if key == CLAK {
        return own(app);
    }
    let bridge = bridge::of(app);
    let source = source_of(&bridge.state(), key)?;
    if !bridge.prefs.server_icons() {
        // Nothing over the network: only what the server already sent.
        return source
            .declared
            .as_deref()
            .and_then(inline)
            .and_then(|b| normalized(&b));
    }
    if let Some(found) = cached(key) {
        return found;
    }
    let _turn = FETCHING.lock().await;
    if let Some(found) = cached(key) {
        return found;
    }
    let file = bridge.home().join("icons").join(&source.file);
    let stored = std::fs::read(&file).ok();
    let fresh = std::fs::metadata(&file)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age < FRESH);
    let found = match stored {
        Some(png) if fresh => Some(png),
        stored => {
            let fetched = tokio::time::timeout(BUDGET, fetch(&source))
                .await
                .ok()
                .flatten();
            if let Some(png) = &fetched {
                let _ = std::fs::create_dir_all(file.parent().unwrap_or(Path::new(".")));
                let _ = std::fs::write(&file, png);
            }
            // Offline, the week-old one is better than none.
            fetched.or(stored)
        }
    };
    FOUND
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert(key.to_owned(), found.clone());
    found
}

fn cached(key: &str) -> Option<Option<Vec<u8>>> {
    FOUND
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(key)
        .cloned()
}

fn own(app: &AppHandle) -> Option<Vec<u8>> {
    let icon = app.default_window_icon()?;
    let image = image::RgbaImage::from_raw(icon.width(), icon.height(), icon.rgba().to_vec())?;
    normalized_image(image::DynamicImage::ImageRgba8(image))
}

/// What `mcp:<id>` or `api:<id>` names in the hub's state. Nothing else is fetched: the page
/// can't make the app reach an address of its choice.
fn source_of(state: &Value, key: &str) -> Option<Source> {
    let (kind, id) = key.split_once(':')?;
    let (list, address) = match kind {
        "mcp" => ("connectors", "url"),
        "api" => ("apis", "base_url"),
        _ => return None,
    };
    let entry = state
        .get(list)?
        .as_array()?
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(id))?;
    let stdio = entry.get("transport").and_then(Value::as_str) == Some("stdio");
    let site = if stdio {
        None
    } else {
        entry
            .get(address)
            .and_then(Value::as_str)
            .and_then(endpoint)
    };
    let declared = entry.get("icon").and_then(Value::as_str).map(str::to_owned);
    if site.is_none() && declared.is_none() {
        return None;
    }
    let file = file_name(&format!(
        "{kind}_{id}_{}",
        site.as_ref().map_or("", |s| s.as_str())
    ));
    Some(Source {
        declared,
        site,
        file,
    })
}

/// The address without query and fragment: the same server however it is written.
fn endpoint(url: &str) -> Option<Url> {
    let mut url = Url::parse(url.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn file_name(text: &str) -> String {
    let name: String = text
        .replace("https://", "")
        .replace("http://", "")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    format!("{name}.png")
}

async fn fetch(source: &Source) -> Option<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("Clak/", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .ok()?;
    if let Some(src) = &source.declared {
        let png = match inline(src) {
            Some(bytes) => normalized(&bytes),
            None => match Url::parse(src) {
                Ok(url) if url.scheme() == "https" => image_at(&client, &url).await,
                _ => None,
            },
        };
        if png.is_some() {
            return png;
        }
    }
    for site in source.site.iter().flat_map(sites) {
        if let Some((page, body)) = get(&client, &site, PAGE_LIMIT).await {
            for link in links(&String::from_utf8_lossy(&body), &page) {
                if let Some(png) = image_at(&client, &link).await {
                    return Some(png);
                }
            }
        }
        if let Ok(favicon) = site.join("/favicon.ico")
            && let Some(png) = image_at(&client, &favicon).await
        {
            return Some(png);
        }
    }
    None
}

/// The home pages to look at: the server's own site, then the company's.
fn sites(server: &Url) -> Vec<Url> {
    let mut sites = Vec::new();
    let Some(host) = server.host_str() else {
        return sites;
    };
    if let Ok(own) = Url::parse(&format!("{}/", server.origin().ascii_serialization())) {
        sites.push(own);
    }
    let labels: Vec<&str> = host.split('.').collect();
    if server.domain().is_some()
        && labels.len() >= 3
        && SERVICE_LABELS.contains(&labels[0].to_ascii_lowercase().as_str())
        && let Ok(company) = Url::parse(&format!("https://{}/", labels[1..].join(".")))
    {
        sites.push(company);
    }
    sites
}

/// The icons a page declares, the best first: the larger, up to 256 pixels, an SVG counting
/// as a large one. The Safari `mask-icon` is a one-colour silhouette: left out.
fn links(html: &str, page: &Url) -> Vec<Url> {
    let mut found: Vec<(u32, Url)> = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<link") {
        let start = from + at;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        from = end;
        let attributes = attributes(&html[start + 5..end]);
        let get = |name: &str| {
            attributes
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        let rel = get("rel").unwrap_or("").to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
        let apple = rels.iter().any(|r| r.starts_with("apple-touch-icon"));
        if !apple && !rels.contains(&"icon") {
            continue;
        }
        let Some(href) = get("href").filter(|h| !h.is_empty()) else {
            continue;
        };
        let kind = get("type").unwrap_or("").to_ascii_lowercase();
        let path = href
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let vector = kind.contains("svg") || path.ends_with(".svg");
        let side = get("sizes")
            .unwrap_or("")
            .split_ascii_whitespace()
            .filter_map(|s| {
                s.to_ascii_lowercase()
                    .split_once('x')?
                    .0
                    .parse::<u32>()
                    .ok()
            })
            .max()
            .unwrap_or(if vector {
                VECTOR
            } else if apple {
                180
            } else {
                32
            });
        if let Ok(url) = page.join(&href.replace("&amp;", "&")) {
            found.push((side.min(256), url));
        }
    }
    found.sort_by_key(|(side, _)| std::cmp::Reverse(*side));
    found.into_iter().map(|(_, url)| url).collect()
}

/// The attributes of a tag, names in lower case: `rel="icon" href=/a.png`.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut chars = tag.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        if c.is_whitespace() || c == '/' {
            chars.next();
            continue;
        }
        let mut end = i;
        while let Some(&(j, c)) = chars.peek() {
            if c.is_whitespace() || c == '=' || c == '/' {
                break;
            }
            end = j + c.len_utf8();
            chars.next();
        }
        let name = tag[i..end].to_ascii_lowercase();
        while chars.peek().is_some_and(|&(_, c)| c.is_whitespace()) {
            chars.next();
        }
        let mut value = String::new();
        if chars.peek().is_some_and(|&(_, c)| c == '=') {
            chars.next();
            while chars.peek().is_some_and(|&(_, c)| c.is_whitespace()) {
                chars.next();
            }
            let quote = chars
                .peek()
                .map(|&(_, c)| c)
                .filter(|&c| c == '"' || c == '\'');
            if quote.is_some() {
                chars.next();
            }
            while let Some(&(_, c)) = chars.peek() {
                if Some(c) == quote || (quote.is_none() && c.is_whitespace()) {
                    break;
                }
                value.push(c);
                chars.next();
            }
            chars.next();
        }
        if name.is_empty() {
            chars.next();
        } else {
            found.push((name, value));
        }
    }
    found
}

/// A page or an image, up to `limit` bytes, with the address it ended at (after redirects).
async fn get(client: &reqwest::Client, url: &Url, limit: usize) -> Option<(Url, Vec<u8>)> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let mut response = client.get(url.clone()).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|n| n as usize > limit)
    {
        return None;
    }
    let page = response.url().clone();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        body.extend_from_slice(&chunk);
        if body.len() > limit {
            return None;
        }
    }
    Some((page, body))
}

async fn image_at(client: &reqwest::Client, url: &Url) -> Option<Vec<u8>> {
    let (_, bytes) = get(client, url, IMAGE_LIMIT).await?;
    normalized(&bytes)
}

/// The bytes of a `data:image/…;base64,` address.
fn inline(src: &str) -> Option<Vec<u8>> {
    let rest = src.strip_prefix("data:image/")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()
}

/// Any PNG, ICO or SVG as a PNG of at most `SIDE` pixels; anything else, or a speck, as
/// nothing.
fn normalized(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = if svg(bytes) {
        drawn(bytes)?
    } else {
        image::load_from_memory(bytes).ok()?
    };
    normalized_image(image)
}

fn normalized_image(image: image::DynamicImage) -> Option<Vec<u8>> {
    if image.width() < 16 || image.height() < 16 {
        return None;
    }
    let image = if image.width() > SIDE || image.height() > SIDE {
        image.resize(SIDE, SIDE, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    encode(&image)
}

/// Whether the bytes are an SVG document: its root element within the first kilobyte.
fn svg(bytes: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    let head = head.trim_start_matches('\u{feff}').trim_start();
    (head.starts_with('<') && head.contains("<svg")) || head.starts_with("<svg")
}

/// An SVG drawn at `SIDE` pixels on its longer side. Without text (no fonts are loaded) and
/// without any file it points to: only what is inside it.
fn drawn(bytes: &[u8]) -> Option<image::DynamicImage> {
    use resvg::{tiny_skia, usvg};
    let mut options = usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new(|_, _| None);
    let tree = usvg::Tree::from_data(bytes, &options).ok()?;
    let size = tree.size();
    let scale = SIDE as f32 / size.width().max(size.height());
    let width = ((size.width() * scale).round() as u32).max(1);
    let height = ((size.height() * scale).round() as u32).max(1);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let pixels: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    image::RgbaImage::from_raw(width, height, pixels).map(image::DynamicImage::ImageRgba8)
}

/// The stored PNG at `size` pixels: square, the picture centred.
fn scaled(png: &[u8], size: u32) -> Option<Vec<u8>> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?;
    let fitted = image.resize(size, size, image::imageops::FilterType::Lanczos3);
    let mut square = image::RgbaImage::new(size, size);
    let (x, y) = ((size - fitted.width()) / 2, (size - fitted.height()) / 2);
    image::imageops::overlay(&mut square, &fitted.to_rgba8(), x.into(), y.into());
    encode(&image::DynamicImage::ImageRgba8(square))
}

fn encode(image: &image::DynamicImage) -> Option<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    image.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn png_of(side: u32) -> Vec<u8> {
        encode(&image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_pixel(side, side, image::Rgba([47, 91, 234, 255])),
        ))
        .unwrap()
    }

    #[test]
    fn only_the_hub_s_servers_and_apis_are_fetched() {
        let state = json!({
            "connectors": [
                {"id": "cn_1", "transport": "http", "url": "https://mcp.example.com/mcp?team=1", "icon": "https://mcp.example.com/i.png"},
                {"id": "cn_2", "transport": "stdio", "url": "", "icon": "data:image/png;base64,AAAA"},
                {"id": "cn_3", "transport": "stdio", "url": ""},
            ],
            "apis": [{"id": "api_1", "base_url": "https://api.shop.test/v2"}],
        });
        let http = source_of(&state, "mcp:cn_1").unwrap();
        assert_eq!(http.site.unwrap().as_str(), "https://mcp.example.com/mcp");
        assert_eq!(
            http.declared.as_deref(),
            Some("https://mcp.example.com/i.png")
        );
        assert_eq!(http.file, "mcp_cn_1_mcp.example.com_mcp.png");

        let stdio = source_of(&state, "mcp:cn_2").unwrap();
        assert_eq!(
            (stdio.site, stdio.declared.as_deref()),
            (None, Some("data:image/png;base64,AAAA"))
        );
        assert!(
            source_of(&state, "mcp:cn_3").is_none(),
            "a local program with nothing declared"
        );

        let api = source_of(&state, "api:api_1").unwrap();
        assert_eq!(api.site.unwrap().as_str(), "https://api.shop.test/v2");

        assert!(source_of(&state, "mcp:nobody").is_none());
        assert!(source_of(&state, "https://elsewhere.example/mcp").is_none());
        assert!(source_of(&state, "file:C:/Windows/win.ini").is_none());
        assert!(source_of(&json!({}), "mcp:cn_1").is_none());
    }

    #[test]
    fn a_service_host_also_tries_the_company_site() {
        let sites = |url: &str| -> Vec<String> {
            super::sites(&Url::parse(url).unwrap())
                .iter()
                .map(Url::to_string)
                .collect()
        };
        assert_eq!(
            sites("https://mcp.notion.com/mcp"),
            ["https://mcp.notion.com/", "https://notion.com/"]
        );
        assert_eq!(
            sites("https://api.github.com"),
            ["https://api.github.com/", "https://github.com/"]
        );
        assert_eq!(
            sites("https://hass.example.it/api/mcp"),
            ["https://hass.example.it/"]
        );
        assert_eq!(
            sites("http://127.0.0.1:8080/mcp"),
            ["http://127.0.0.1:8080/"]
        );
    }

    #[test]
    fn the_page_s_icons_come_best_first() {
        let page = Url::parse("https://www.example.com/home/").unwrap();
        let html = r##"<html><head>
            <link rel="stylesheet" href="/a.css">
            <LINK REL="icon" href="/favicon.ico">
            <link rel="mask-icon" href="/mask.svg" color="#000">
            <link rel="icon" type="image/svg+xml" href="/icon.svg">
            <link rel=icon sizes=64x64 href=icons/64.png>
            <link href="/apple.png?v=1&amp;x=2" rel="apple-touch-icon"/>
        </head></html>"##;
        let found: Vec<String> = links(html, &page).iter().map(Url::to_string).collect();
        assert_eq!(
            found,
            [
                "https://www.example.com/icon.svg",
                "https://www.example.com/apple.png?v=1&x=2",
                "https://www.example.com/home/icons/64.png",
                "https://www.example.com/favicon.ico",
            ]
        );
    }

    #[test]
    fn icons_become_pngs_of_the_size_asked() {
        let big = normalized(&png_of(512)).unwrap();
        let image = image::load_from_memory(&big).unwrap();
        assert_eq!((image.width(), image.height()), (SIDE, SIDE));
        let small = normalized(&png_of(32)).unwrap();
        assert_eq!(image::load_from_memory(&small).unwrap().width(), 32);
        assert!(normalized(&png_of(8)).is_none());
        assert!(normalized(b"<html>not an image</html>").is_none());

        let menu = scaled(&big, 20).unwrap();
        let image = image::load_from_memory(&menu).unwrap();
        assert_eq!((image.width(), image.height()), (20, 20));
    }

    #[test]
    fn an_svg_is_drawn_into_pixels() {
        let svg = br##"<?xml version="1.0"?>
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 16">
              <rect width="32" height="16" fill="#e65a28"/>
              <image href="C:/Windows/win.ini" width="8" height="8"/>
            </svg>"##;
        let png = normalized(svg).unwrap();
        let image = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!((image.width(), image.height()), (SIDE, SIDE / 2));
        assert_eq!(image.get_pixel(64, 32).0, [0xe6, 0x5a, 0x28, 255]);
        assert!(!super::svg(b"<html><body>no</body></html>"));
    }

    #[test]
    fn an_ico_and_an_inline_icon_are_read() {
        let mut ico = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            48,
            48,
            image::Rgba([0, 0, 0, 255]),
        ))
        .write_to(&mut ico, image::ImageFormat::Ico)
        .unwrap();
        let png = normalized(ico.get_ref()).unwrap();
        assert_eq!(image::load_from_memory(&png).unwrap().width(), 48);

        let src = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png_of(64))
        );
        assert!(normalized(&inline(&src).unwrap()).is_some());
        assert!(inline("data:image/png,raw").is_none());
        assert!(inline("https://example.com/a.png").is_none());
    }

    /// Over the network: `cargo test -p clak-app server_icon -- --ignored`.
    #[test]
    #[ignore]
    fn notion_s_icon_comes_from_its_website() {
        tauri::async_runtime::block_on(async {
            let source = Source {
                declared: None,
                site: Some(Url::parse("https://mcp.notion.com/mcp").unwrap()),
                file: String::new(),
            };
            let png = fetch(&source).await.expect("an icon");
            assert!(image::load_from_memory(&png).unwrap().width() >= 16);
        });
    }
}

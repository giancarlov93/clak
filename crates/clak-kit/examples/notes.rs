//! A test program: a notepad that agents can read and extend.
//!
//! ```text
//!     cargo run -p clak-kit --example notes [title]
//! ```

use clak_kit::{LiveResource, text};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

#[tokio::main]
async fn main() {
    let title = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "notes.txt".to_owned());
    let notes = Arc::new(Mutex::new(String::new()));
    let resource = LiveResource::new("Notes", &title, Some("Clak"), None);
    let read = notes.clone();
    resource.tool("read", "The whole text of the note.", None, move |_args| {
        let text_now = read.lock().unwrap().clone();
        async move { text(text_now) }
    });
    let append = notes.clone();
    resource.tool(
        "append",
        "Adds a line at the end of the note.",
        Some(json!({"type": "object", "properties": {"line": {"type": "string"}}, "required": ["line"]})),
        move |args| {
            let line = args.get("line").and_then(Value::as_str).unwrap_or("").to_owned();
            append.lock().unwrap().push_str(&format!("{line}\n"));
            async move { text("added") }
        },
    );
    resource
        .on_event(|kind, message| eprintln!("[notes] {kind}: {}", Value::Object(message.clone())));
    if let Err(e) = resource.connect().await {
        eprintln!("[notes] {}", e.0);
        std::process::exit(1);
    }
    eprintln!("[notes] connected as {:?}", resource.name());
    tokio::signal::ctrl_c().await.ok();
    resource.close("closed").await;
}

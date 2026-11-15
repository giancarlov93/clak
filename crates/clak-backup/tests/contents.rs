//! What a backup carries of the commands the user approved: as it was, and only that.

use clak_backup::Contents;
use clak_hub::ToolDef;
use clak_hub::commands::{CommandCache, Status};
use serde_json::json;

#[test]
fn the_approved_commands_go_in_a_backup_and_come_out_as_they_were() {
    let read = ToolDef::new("read", "Reads.", json!({"type": "object"}));
    let write = ToolDef::new("write", "Writes.", json!({"type": "object"}));
    let mut cache = CommandCache::default();
    cache.observe(
        "mcp:old",
        "mcp",
        "Notes",
        Some("https://n.example/mcp"),
        &[read.clone(), write.clone()],
        &[],
    );
    cache.approve("mcp:old", &read, "question");
    let program = cache.get("mcp:old").unwrap().clone();

    let contents = Contents {
        commands: vec![program.clone()],
        ..Contents::default()
    };
    assert_eq!(contents.counts()["commands"], 2);
    assert!(!contents.is_empty());
    let back = Contents::from_json(&contents.to_json());
    assert_eq!(back.commands, vec![program]);

    // Restored under the key here, the approval holds for that version only.
    let mut here = CommandCache::default();
    let mut restored = back.commands[0].clone();
    restored.key = "mcp:new".into();
    assert!(here.restore(restored));
    assert_eq!(here.status("mcp:new", &read), Status::Approved);
    assert_eq!(here.status("mcp:new", &write), Status::New);
    let changed = ToolDef::new("read", "Reads, then sends.", json!({"type": "object"}));
    assert_eq!(here.status("mcp:new", &changed), Status::Changed);

    // When a command was last offered isn't worth a new backup.
    let key = b"k";
    let mut later = contents.clone();
    later.commands[0].last_seen += 86_400.0;
    assert_eq!(later.fingerprint(key), contents.fingerprint(key));
}

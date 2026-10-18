//! Where APIs are saved: `apis.json` in plain text, the secrets in the system box; on
//! Windows `apis.secrets`.

use clak_apis::model::{ApiAuth, ApiEndpoint, ApiEntry, AuthKind};
use clak_apis::store::ApiStore;
use clak_platform::{BlobCipher, FileBox, MemoryBox, SecretError};
use std::path::PathBuf;
use std::sync::Arc;

/// A fake cipher: it "encrypts" by flipping the bytes, so the secret can't be read in the file.
struct Flip;

impl BlobCipher for Flip {
    fn protect(&self, data: &[u8]) -> Result<Vec<u8>, SecretError> {
        Ok(data.iter().map(|b| !b).collect())
    }

    fn unprotect(&self, data: &[u8]) -> Result<Vec<u8>, SecretError> {
        if data.first() == Some(&0) {
            return Err(SecretError("not ours".into()));
        }
        Ok(data.iter().map(|b| !b).collect())
    }
}

struct Folder(PathBuf);

impl Folder {
    fn new() -> Folder {
        Folder(std::env::temp_dir().join(format!(
            "clak-apis-store-{}",
            clak_wire::json::random_hex(6)
        )))
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn api(id: &str, name: &str) -> ApiEntry {
    ApiEntry {
        id: id.into(),
        name: name.into(),
        base_url: "https://x.example".into(),
        enabled: true,
        since: 1712345678.25,
        auth: ApiAuth::named(AuthKind::Header, "X-Key"),
        endpoints: vec![ApiEndpoint::new("ping", "GET", "/ping", None, vec![])],
        command: None,
        source: Some("https://x.example/openapi.json".into()),
    }
}

#[test]
fn apis_and_secrets_are_saved_apart() {
    let folder = Folder::new();
    let home = folder.0.join("hub");
    let store = ApiStore::new(
        &home,
        Arc::new(FileBox::new(home.join("apis.secrets"), Flip)),
    );
    assert!(store.all().is_empty());

    store.add(api("api_1", "One"), Some("s3cret")).unwrap();
    store.add(api("api_2", "Two"), None).unwrap();
    assert_eq!(store.all().len(), 2);
    assert_eq!(store.secret("api_1").as_deref(), Some("s3cret"));
    assert!(store.has_secret("api_1"));
    assert_eq!(store.secret("api_2"), None);

    let list = std::fs::read_to_string(home.join("apis.json")).unwrap();
    let secrets = std::fs::read_to_string(home.join("apis.secrets")).unwrap();
    assert!(!list.contains("s3cret"));
    assert!(!secrets.contains("s3cret"));
    assert!(list.starts_with("{\n  \"apis\": ["));
    let parsed: serde_json::Value = serde_json::from_str(&secrets).unwrap();
    assert!(parsed["api_1"].is_string()); // { "<id>": "<base64 of the blob>" }

    let mut renamed = api("api_1", "First");
    renamed.enabled = false;
    store.update(&renamed).unwrap();
    assert_eq!(store.get("api_1").unwrap(), renamed);
    assert_eq!(store.secret("api_1").as_deref(), Some("s3cret"));

    store.set_secret("api_2", Some("other")).unwrap();
    assert_eq!(store.secret("api_2").as_deref(), Some("other"));
    store.set_secret("api_2", Some("")).unwrap();
    assert_eq!(store.secret("api_2"), None);

    store.remove("api_1").unwrap();
    assert_eq!(
        store
            .all()
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        ["api_2"]
    );
    assert_eq!(store.secret("api_1"), None);
}

#[test]
fn the_windows_hub_files_are_read() {
    let folder = Folder::new();
    std::fs::create_dir_all(&folder.0).unwrap();
    // As System.Text.Json writes it: with the BOM if an editor touched it, and escaped characters.
    std::fs::write(
        folder.0.join("apis.json"),
        "\u{FEFF}{\r\n  \"apis\": [\r\n    {\r\n      \"id\": \"api_9\",\r\n      \"name\": \"Caf\\u00E9\",\r\n      \"base_url\": \"https://x.example\",\r\n      \"enabled\": true,\r\n      \"since\": 1712345678,\r\n      \"auth\": { \"kind\": \"bearer\", \"name\": null },\r\n      \"command\": \"cafe\",\r\n      \"source\": null,\r\n      \"endpoints\": [ { \"name\": \"a\", \"method\": \"GET\", \"path\": \"/a\", \"parameters\": [] } ]\r\n    }\r\n  ]\r\n}",
    )
    .unwrap();
    // A secret encrypted by another user (or corrupted) can't be decrypted: it's absent.
    std::fs::write(
        folder.0.join("apis.secrets"),
        r#"{"api_9": "AAAA", "api_8": "not base64!"}"#,
    )
    .unwrap();
    let store = ApiStore::new(
        &folder.0,
        Arc::new(FileBox::new(folder.0.join("apis.secrets"), Flip)),
    );
    let all = store.all();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "Café");
    assert_eq!(all[0].since, 1712345678.0);
    assert_eq!(all[0].command.as_deref(), Some("cafe"));
    assert_eq!(store.secret("api_9"), None);
    assert_eq!(store.secret("api_8"), None);

    // A broken file is an empty list, not an error.
    std::fs::write(folder.0.join("apis.json"), "{ nope").unwrap();
    assert!(store.all().is_empty());
}

#[test]
fn the_memory_box_writes_only_the_list_to_disk() {
    let folder = Folder::new();
    let memory = Arc::new(MemoryBox::new());
    let store = ApiStore::new(&folder.0, memory.clone());
    store.add(api("api_1", "One"), Some("tok")).unwrap();
    assert_eq!(memory.all().get("api_1").map(String::as_str), Some("tok"));
    assert!(!folder.0.join("apis.secrets").exists());
    store.remove("api_1").unwrap();
    assert!(memory.all().is_empty());
}

//! The fingerprint of an executable, to remember an unsigned program.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

type Key = (String, u128, u64);

fn cache() -> &'static Mutex<HashMap<Key, String>> {
    static HASHES: OnceLock<Mutex<HashMap<Key, String>>> = OnceLock::new();
    HASHES.get_or_init(Default::default)
}

/// The SHA-256 of the contents, in lowercase hex; None if the file can't be read.
///
/// Slow (it reads the whole file): never under the hub's lock. Cached by path,
/// modification time and size: an updated executable is a different executable,
/// and is read again.
pub fn executable_hash(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = metadata.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
    let key = (path.to_string(), modified, metadata.len());
    if let Some(known) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Some(known);
    }
    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let hash = hex::encode(hasher.finalize());
    if let Ok(mut known) = cache().lock() {
        known.insert(key, hash.clone());
    }
    Some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l_impronta_e_quella_del_contenuto_e_segue_le_modifiche() {
        let dir = std::env::temp_dir().join(format!("clak-platform-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("programma.bin");
        std::fs::write(&file, b"abc").unwrap();
        let path = file.to_str().unwrap();
        assert_eq!(
            executable_hash(path).as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        // Different (and longer) contents: another cache key, another fingerprint.
        std::fs::write(&file, b"abcd").unwrap();
        assert_eq!(
            executable_hash(path).as_deref(),
            Some("88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589")
        );
        assert_eq!(executable_hash(dir.join("non-esiste").to_str().unwrap()), None);
        assert_eq!(executable_hash(dir.to_str().unwrap()), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn l_eseguibile_di_questo_processo_ha_un_impronta() {
        let me = std::env::current_exe().unwrap();
        let hash = executable_hash(me.to_str().unwrap()).unwrap();
        assert_eq!(hash.len(), 64);
        assert_eq!(executable_hash(me.to_str().unwrap()), Some(hash));
    }
}

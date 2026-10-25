//! The web page's password. Only its hash is kept (Argon2id, as a PHC string), in
//! `web.password` in the hub's folder, readable by the user alone.
//!
//! An agent runs as the same user and can read that file: with the hash it can only try
//! passwords one by one, at Argon2's pace, so the password must be long, not a PIN. It can
//! also replace the file, but the daemon reads it once, when it starts.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use std::path::{Path, PathBuf};

pub const FILE: &str = "web.password";

/// Shorter than this, it could be guessed from the hash.
pub const MIN_LENGTH: usize = 12;

pub fn file(home: &Path) -> PathBuf {
    home.join(FILE)
}

pub fn hash(password: &str) -> Result<String, String> {
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|e| format!("no system randomness: {e}"))?;
    let salt = SaltString::encode_b64(&random).map_err(|e| e.to_string())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| format!("the password couldn't be hashed: {e}"))
}

/// Slow on purpose (Argon2): never under a lock, nor on the server's threads.
pub fn verify(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|parsed| {
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    })
}

/// The hash in the file, if there is a valid one.
pub fn load(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file(home)).ok()?;
    let line = text.trim();
    PasswordHash::new(line).ok().map(|_| line.to_string())
}

/// Written through a `.tmp` the user alone can read, from the moment it is created.
pub fn save(home: &Path, hash: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    let path = file(home);
    let draft = path.with_extension("password.tmp");
    match std::fs::remove_file(&draft) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut file = options.open(&draft)?;
        file.write_all(format!("{hash}\n").as_bytes())?;
    }
    std::fs::rename(&draft, &path)
}

/// Asks for the password twice on the terminal, without showing it, and keeps its hash.
pub fn choose(home: &Path) -> Result<(), String> {
    eprintln!(
        "The password for Clak's web page, at least {MIN_LENGTH} characters: a phrase is best."
    );
    let first = rpassword::prompt_password("Password: ").map_err(|e| e.to_string())?;
    if first.chars().count() < MIN_LENGTH {
        return Err(format!("it must be at least {MIN_LENGTH} characters long"));
    }
    let again = rpassword::prompt_password("Again: ").map_err(|e| e.to_string())?;
    if first != again {
        return Err("the two passwords are different".into());
    }
    save(home, &hash(&first)?)
        .map_err(|e| format!("couldn't write {}: {e}", file(home).display()))?;
    eprintln!("Saved: only its hash, in {}.", file(home).display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_hash_is_kept_and_only_the_user_can_read_it() {
        let home = std::env::temp_dir().join(format!("clak-password-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(load(&home), None);

        let hashed = hash("correct horse battery").unwrap();
        assert!(hashed.starts_with("$argon2id$"), "{hashed}");
        assert!(!hashed.contains("correct horse"));
        assert!(verify(&hashed, "correct horse battery"));
        assert!(!verify(&hashed, "correct horse batter"));
        assert!(!verify("not a hash", "correct horse battery"));

        save(&home, &hashed).unwrap();
        assert_eq!(load(&home).as_deref(), Some(hashed.as_str()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(file(&home)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A file that isn't a hash is no password.
        std::fs::write(file(&home), "hunter2\n").unwrap();
        assert_eq!(load(&home), None);
        std::fs::remove_dir_all(&home).ok();
    }
}

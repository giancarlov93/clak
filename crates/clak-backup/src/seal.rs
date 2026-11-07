//! How a backup is locked.
//!
//! The password is never kept. When the user chooses it, a key pair is made (X25519): the
//! public half stays in `backup.json`, so that the automatic backups can be written without
//! asking anything; the private half is kept only locked with the password (Argon2id, then
//! XChaCha20-Poly1305), and that locked copy goes into every backup. Restoring, the
//! password opens the private half, which opens the backup.
//!
//! Each backup is sealed for the public half alone: a key of its own (X25519 with a pair
//! made for it, through HKDF-SHA256) and XChaCha20-Poly1305. What comes back from a wrong
//! password, or from a file changed by hand, is an error, never something half read.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use serde_json::{Value, json};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

/// The shortest password accepted: Argon2 slows down whoever tries them one by one, it does
/// not make a short one long.
pub const MIN_PASSWORD: usize = 8;

/// What the authenticated data of each part starts with: a part of another kind, or of
/// another version, doesn't open.
const KEY_CONTEXT: &[u8] = b"clak-backup/1 key";
const DATA_CONTEXT: &[u8] = b"clak-backup/1 data";

/// What can go wrong opening a backup, as the user reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// Shorter than [`MIN_PASSWORD`].
    ShortPassword,
    WrongPassword,
    /// Not a backup of Clak, or one changed by hand.
    Damaged,
    /// Locked with a cost this computer refuses (a file made to keep it busy).
    TooCostly,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SealError::ShortPassword => "The password must be at least 8 characters long.",
            SealError::WrongPassword => "That isn't the password of this backup.",
            SealError::Damaged => "This backup is damaged: it can't be opened.",
            SealError::TooCostly => "This backup asks for more memory than Clak gives it.",
        })
    }
}

impl std::error::Error for SealError {}

/// How much Argon2 costs: memory in KiB, passes, lanes. About half a second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cost {
    pub memory: u32,
    pub passes: u32,
    pub lanes: u32,
}

impl Cost {
    pub const DEFAULT: Cost = Cost {
        memory: 64 * 1024,
        passes: 3,
        lanes: 1,
    };

    /// For the tests: fast.
    pub const CHEAP: Cost = Cost {
        memory: 64,
        passes: 1,
        lanes: 1,
    };

    /// What a file may ask for: up to 1 GiB, 16 passes, 8 lanes.
    fn acceptable(&self) -> bool {
        (8..=1024 * 1024).contains(&self.memory)
            && (1..=16).contains(&self.passes)
            && (1..=8).contains(&self.lanes)
    }
}

/// The private key locked with the password, and the public one it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lock {
    pub public: [u8; 32],
    pub cost: Cost,
    salt: [u8; 16],
    nonce: [u8; 24],
    locked: Vec<u8>,
}

/// One backup's content, sealed for a [`Lock`]'s public key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sealed {
    ephemeral: [u8; 32],
    nonce: [u8; 24],
    data: Vec<u8>,
}

/// The private key, opened: only while restoring.
pub struct Opened {
    secret: StaticSecret,
    public: [u8; 32],
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("no system randomness");
    bytes
}

fn password_key(password: &str, salt: &[u8], cost: Cost) -> Result<Zeroizing<[u8; 32]>, SealError> {
    if !cost.acceptable() {
        return Err(SealError::TooCostly);
    }
    let params = Params::new(cost.memory, cost.passes, cost.lanes, Some(32))
        .map_err(|_| SealError::TooCostly)?;
    let mut key = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut key[..])
        .map_err(|_| SealError::TooCostly)?;
    Ok(key)
}

fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key))
}

fn context(prefix: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut aad = prefix.to_vec();
    for part in parts {
        aad.extend_from_slice(part);
    }
    aad
}

impl Lock {
    /// A new key pair, its private half locked with `password`. Slow (Argon2): never under
    /// a lock, nor on an async thread.
    pub fn new(password: &str, cost: Cost) -> Result<Lock, SealError> {
        if password.chars().count() < MIN_PASSWORD {
            return Err(SealError::ShortPassword);
        }
        let private = Zeroizing::new(random::<32>());
        let public = PublicKey::from(&StaticSecret::from(*private)).to_bytes();
        let salt = random::<16>();
        let nonce = random::<24>();
        let key = password_key(password, &salt, cost)?;
        let locked = cipher(&key)
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: private.as_ref(),
                    aad: &context(KEY_CONTEXT, &[&public]),
                },
            )
            .map_err(|_| SealError::Damaged)?;
        Ok(Lock {
            public,
            cost,
            salt,
            nonce,
            locked,
        })
    }

    /// The private half, with the password. Slow (Argon2).
    pub fn open(&self, password: &str) -> Result<Opened, SealError> {
        let key = password_key(password, &self.salt, self.cost)?;
        let private = cipher(&key)
            .decrypt(
                XNonce::from_slice(&self.nonce),
                Payload {
                    msg: &self.locked,
                    aad: &context(KEY_CONTEXT, &[&self.public]),
                },
            )
            .map_err(|_| SealError::WrongPassword)?;
        let private = Zeroizing::new(private);
        let bytes: [u8; 32] = private
            .as_slice()
            .try_into()
            .map_err(|_| SealError::Damaged)?;
        let secret = StaticSecret::from(bytes);
        if PublicKey::from(&secret).to_bytes() != self.public {
            return Err(SealError::Damaged);
        }
        Ok(Opened {
            secret,
            public: self.public,
        })
    }

    /// Seals `plain` for this lock: no password needed.
    pub fn seal(&self, plain: &[u8]) -> Sealed {
        let ephemeral = StaticSecret::from(random::<32>());
        let ephemeral_public = PublicKey::from(&ephemeral).to_bytes();
        let shared = ephemeral.diffie_hellman(&PublicKey::from(self.public));
        let key = data_key(shared.as_bytes(), &ephemeral_public, &self.public);
        let nonce = random::<24>();
        let data = cipher(&key)
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plain,
                    aad: &context(DATA_CONTEXT, &[&self.public, &ephemeral_public]),
                },
            )
            .expect("XChaCha20-Poly1305 seals any length a backup has");
        Sealed {
            ephemeral: ephemeral_public,
            nonce,
            data,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "kdf": "argon2id",
            "memory": self.cost.memory,
            "passes": self.cost.passes,
            "lanes": self.cost.lanes,
            "salt": BASE64.encode(self.salt),
            "nonce": BASE64.encode(self.nonce),
            "locked": BASE64.encode(&self.locked),
            "public": BASE64.encode(self.public),
        })
    }

    pub fn from_json(value: &Value) -> Option<Lock> {
        if value.get("kdf").and_then(Value::as_str) != Some("argon2id") {
            return None;
        }
        let number = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        Some(Lock {
            public: bytes(value.get("public"))?,
            cost: Cost {
                memory: number("memory")?,
                passes: number("passes")?,
                lanes: number("lanes")?,
            },
            salt: bytes(value.get("salt"))?,
            nonce: bytes(value.get("nonce"))?,
            locked: decode(value.get("locked"))?,
        })
    }
}

impl Opened {
    /// What `sealed` holds, if it was sealed for this key and nobody changed it.
    pub fn unseal(&self, sealed: &Sealed) -> Result<Vec<u8>, SealError> {
        let shared = self
            .secret
            .diffie_hellman(&PublicKey::from(sealed.ephemeral));
        if !shared.was_contributory() {
            return Err(SealError::Damaged);
        }
        let key = data_key(shared.as_bytes(), &sealed.ephemeral, &self.public);
        cipher(&key)
            .decrypt(
                XNonce::from_slice(&sealed.nonce),
                Payload {
                    msg: &sealed.data,
                    aad: &context(DATA_CONTEXT, &[&self.public, &sealed.ephemeral]),
                },
            )
            .map_err(|_| SealError::Damaged)
    }
}

impl Sealed {
    pub fn to_json(&self) -> Value {
        json!({
            "ephemeral": BASE64.encode(self.ephemeral),
            "nonce": BASE64.encode(self.nonce),
            "data": BASE64.encode(&self.data),
        })
    }

    pub fn from_json(value: &Value) -> Option<Sealed> {
        Some(Sealed {
            ephemeral: bytes(value.get("ephemeral"))?,
            nonce: bytes(value.get("nonce"))?,
            data: decode(value.get("data"))?,
        })
    }
}

fn data_key(shared: &[u8], ephemeral: &[u8; 32], public: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(ephemeral);
    salt[32..].copy_from_slice(public);
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(&salt), shared)
        .expand(DATA_CONTEXT, &mut key[..])
        .expect("32 bytes are a valid HKDF length");
    key
}

fn decode(value: Option<&Value>) -> Option<Vec<u8>> {
    BASE64.decode(value?.as_str()?).ok()
}

fn bytes<const N: usize>(value: Option<&Value>) -> Option<[u8; N]> {
    decode(value)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_sealed_opens_with_the_password_only() {
        let lock = Lock::new("correct horse", Cost::CHEAP).unwrap();
        let sealed = lock.seal(b"the tokens");
        assert!(!sealed.data.windows(6).any(|w| w == b"tokens"));
        let opened = lock.open("correct horse").unwrap();
        assert_eq!(opened.unseal(&sealed).unwrap(), b"the tokens");
        assert_eq!(
            lock.open("correct horsE").err(),
            Some(SealError::WrongPassword)
        );
    }

    #[test]
    fn the_lock_and_the_seal_survive_their_json() {
        let lock = Lock::new("a long password", Cost::CHEAP).unwrap();
        let again = Lock::from_json(&lock.to_json()).unwrap();
        assert_eq!(again, lock);
        let sealed = lock.seal(b"x");
        let back = Sealed::from_json(&sealed.to_json()).unwrap();
        assert_eq!(back, sealed);
        assert_eq!(
            again
                .open("a long password")
                .unwrap()
                .unseal(&back)
                .unwrap(),
            b"x"
        );
        assert!(Lock::from_json(&json!({ "kdf": "scrypt" })).is_none());
        assert!(Sealed::from_json(&json!({ "ephemeral": "AAAA" })).is_none());
    }

    #[test]
    fn a_changed_byte_or_another_key_does_not_open() {
        let lock = Lock::new("a long password", Cost::CHEAP).unwrap();
        let opened = lock.open("a long password").unwrap();
        let sealed = lock.seal(b"payload");
        let mut changed = sealed.clone();
        changed.data[0] ^= 1;
        assert_eq!(opened.unseal(&changed), Err(SealError::Damaged));
        let mut moved = sealed.clone();
        moved.ephemeral[0] ^= 1;
        assert_eq!(opened.unseal(&moved), Err(SealError::Damaged));
        // Another password's key: the same password, another pair.
        let other = Lock::new("a long password", Cost::CHEAP).unwrap();
        assert_eq!(
            other.open("a long password").unwrap().unseal(&sealed),
            Err(SealError::Damaged)
        );
        // The locked key with someone else's public half.
        let mut swapped = lock.clone();
        swapped.public = other.public;
        assert_eq!(
            swapped.open("a long password").err(),
            Some(SealError::WrongPassword)
        );
    }

    #[test]
    fn short_passwords_and_costly_files_are_refused() {
        assert_eq!(
            Lock::new("short", Cost::CHEAP).err(),
            Some(SealError::ShortPassword)
        );
        assert_eq!(
            Lock::new("èèèèèèèè", Cost::CHEAP).map(|_| ()),
            Ok(()),
            "characters, not bytes"
        );
        let mut lock = Lock::new("a long password", Cost::CHEAP).unwrap();
        lock.cost.memory = 4 * 1024 * 1024;
        assert_eq!(
            lock.open("a long password").err(),
            Some(SealError::TooCostly)
        );
        assert!(SealError::TooCostly.to_string().contains("memory"));
    }
}

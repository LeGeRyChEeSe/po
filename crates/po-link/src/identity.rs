//! Who a device is: an Ed25519 key pair made on first launch, and an X25519 key derived from
//! it to receive the directory key. The public key *is* the identity; addresses are only hints.

use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Random bytes from the system. A failure there leaves no safe way on: we stop.
pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::getrandom(&mut out).expect("system randomness");
    out
}

/// What a device is to the others: its two public keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKeys {
    pub ed: [u8; 32],
    pub x: [u8; 32],
}

/// The short name of a key: 16 hex digits of its hash, for files and lists.
pub fn device_id(ed: &[u8; 32]) -> String {
    let hash = Sha256::digest(ed);
    hash[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Checks a signature made by `ed`.
pub fn verify(ed: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(ed) else { return false };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(signature) else { return false };
    key.verify(message, &signature).is_ok() && key.verify_strict(message, &signature).is_ok()
}

/// A device's own keys.
#[derive(Clone)]
pub struct Identity {
    signing: ed25519_dalek::SigningKey,
    x: x25519_dalek::StaticSecret,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Identity({})", self.id())
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    v: u32,
    seed: String,
}

impl Identity {
    pub fn generate() -> Self {
        Self::from_seed(random())
    }

    /// The X25519 secret comes from the same seed through HKDF: one secret to keep, and the
    /// two uses never share a key.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let mut x = [0u8; 32];
        hkdf::Hkdf::<Sha256>::new(Some(b"po-link x25519"), &seed).expand(b"v1", &mut x).expect("32 bytes");
        Self { signing, x: x25519_dalek::StaticSecret::from(x) }
    }

    /// The identity kept at `path`, made there the first time.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        if let Ok(text) = std::fs::read_to_string(path)
            && let Ok(stored) = serde_json::from_str::<Stored>(&text)
            && let Ok(bytes) = B64.decode(stored.seed.trim())
            && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
        {
            return Ok(Self::from_seed(seed));
        }
        let identity = Self::generate();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let stored = Stored { v: 1, seed: B64.encode(identity.signing.to_bytes()) };
        crate::write_private(path, serde_json::to_string_pretty(&stored).unwrap_or_default().as_bytes())?;
        Ok(identity)
    }

    pub fn public(&self) -> PublicKeys {
        PublicKeys { ed: self.signing.verifying_key().to_bytes(), x: x25519_dalek::PublicKey::from(&self.x).to_bytes() }
    }

    pub fn id(&self) -> String {
        device_id(&self.public().ed)
    }

    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.signing.sign(message).to_bytes().to_vec()
    }

    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The key as PKCS#8 (RFC 8410), for TLS.
    pub fn pkcs8(&self) -> Vec<u8> {
        let mut der = vec![0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20];
        der.extend_from_slice(&self.signing.to_bytes());
        der
    }

    pub(crate) fn x_secret(&self) -> &x25519_dalek::StaticSecret {
        &self.x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identity_is_kept_and_found_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("link").join("identity.json");
        let first = Identity::load_or_create(&path).unwrap();
        let again = Identity::load_or_create(&path).unwrap();
        assert_eq!(first.public(), again.public());
        assert_eq!(first.id().len(), 16);
        assert_ne!(Identity::generate().public(), first.public());
    }

    #[test]
    fn a_signature_holds_only_for_its_key_and_message() {
        let a = Identity::generate();
        let b = Identity::generate();
        let sig = a.sign(b"hello");
        assert!(verify(&a.public().ed, b"hello", &sig));
        assert!(!verify(&a.public().ed, b"hellp", &sig));
        assert!(!verify(&b.public().ed, b"hello", &sig));
        assert!(!verify(&a.public().ed, b"hello", &sig[..10]));
    }

    #[test]
    fn the_two_keys_come_from_one_seed() {
        let seed = random::<32>();
        assert_eq!(Identity::from_seed(seed).public(), Identity::from_seed(seed).public());
        let keys = Identity::from_seed(seed).public();
        assert_ne!(keys.ed, keys.x);
    }
}

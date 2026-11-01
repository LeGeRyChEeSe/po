//! Encryption of what goes on GitHub: the addresses under the directory key (XChaCha20-Poly1305),
//! and the directory key itself sealed to each device (X25519 + HKDF, like a sealed box).

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::Sha256;

use crate::identity::{Identity, random};

const NONCE: usize = 24;

/// Encrypts `plaintext` under `key`; `aad` binds it to where it belongs (the device, the key's
/// name), so that a blob copied elsewhere does not open.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let nonce = random::<NONCE>();
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad }).expect("encryption"));
    out
}

pub fn decrypt(key: &[u8; 32], sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < NONCE + 16 {
        return None;
    }
    let (nonce, body) = sealed.split_at(NONCE);
    XChaCha20Poly1305::new(key.into()).decrypt(XNonce::from_slice(nonce), Payload { msg: body, aad }).ok()
}

fn sealing_key(shared: &[u8], ephemeral: &[u8; 32], recipient: &[u8; 32]) -> [u8; 32] {
    let mut salt = ephemeral.to_vec();
    salt.extend_from_slice(recipient);
    let mut key = [0u8; 32];
    hkdf::Hkdf::<Sha256>::new(Some(&salt), shared).expand(b"po-link seal v1", &mut key).expect("32 bytes");
    key
}

/// Seals `plaintext` for the holder of the X25519 key `recipient`: only they can open it.
pub fn seal(recipient: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let ephemeral = x25519_dalek::StaticSecret::from(random::<32>());
    let public = x25519_dalek::PublicKey::from(&ephemeral).to_bytes();
    let shared = ephemeral.diffie_hellman(&x25519_dalek::PublicKey::from(*recipient));
    let key = sealing_key(shared.as_bytes(), &public, recipient);
    let mut out = public.to_vec();
    out.extend(encrypt(&key, plaintext, aad));
    out
}

pub fn open(identity: &Identity, sealed: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < 32 {
        return None;
    }
    let (public, rest) = sealed.split_at(32);
    let public: [u8; 32] = public.try_into().ok()?;
    let shared = identity.x_secret().diffie_hellman(&x25519_dalek::PublicKey::from(public));
    // A low-order point gives a shared secret anyone knows: refused.
    if !shared.was_contributory() {
        return None;
    }
    let key = sealing_key(shared.as_bytes(), &public, &identity.public().x);
    decrypt(&key, rest, aad)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_is_encrypted_opens_with_the_key_and_the_same_context_only() {
        let key = random::<32>();
        let blob = encrypt(&key, b"192.168.1.20:41234", b"dev-a");
        assert_eq!(decrypt(&key, &blob, b"dev-a").as_deref(), Some(&b"192.168.1.20:41234"[..]));
        assert!(decrypt(&key, &blob, b"dev-b").is_none(), "moved to another device, it does not open");
        assert!(decrypt(&random::<32>(), &blob, b"dev-a").is_none());
        let mut tampered = blob.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decrypt(&key, &tampered, b"dev-a").is_none());
        assert!(decrypt(&key, &blob[..10], b"dev-a").is_none());
        assert_ne!(encrypt(&key, b"x", b""), encrypt(&key, b"x", b""), "a fresh nonce each time");
    }

    #[test]
    fn a_sealed_key_opens_for_its_recipient_only() {
        let phone = Identity::generate();
        let other = Identity::generate();
        let sealed = seal(&phone.public().x, b"directory key", b"k1");
        assert_eq!(open(&phone, &sealed, b"k1").as_deref(), Some(&b"directory key"[..]));
        assert!(open(&other, &sealed, b"k1").is_none());
        assert!(open(&phone, &sealed, b"k2").is_none());
        // A zero point (low order) is refused rather than giving a key everyone knows.
        let mut forged = vec![0u8; 32];
        forged.extend(encrypt(&[0u8; 32], b"x", b"k1"));
        assert!(open(&phone, &forged, b"k1").is_none());
    }
}

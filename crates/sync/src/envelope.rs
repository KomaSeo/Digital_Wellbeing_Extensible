//! Responsibility: Encrypt and sign a message with a device's key, and open it again.
//!
//! XChaCha20-Poly1305: a wrong key, a changed byte or a message meant for another device fails
//! to open. The device id is bound in as associated data.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

/// Bytes of a device key.
pub const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 24;

/// Responsibility: Carry one sealed message between a device and the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub device_id: String,
    /// Base64.
    pub nonce: String,
    /// Base64.
    pub ciphertext: String,
}

/// A new random device key, base64.
pub fn new_key() -> Result<String, String> {
    let mut key = [0u8; KEY_BYTES];
    getrandom::getrandom(&mut key).map_err(|error| error.to_string())?;
    Ok(STANDARD.encode(key))
}

fn cipher(key: &str) -> Result<XChaCha20Poly1305, String> {
    let bytes = STANDARD
        .decode(key.trim())
        .map_err(|_| "the device key is not valid base64".to_string())?;
    if bytes.len() != KEY_BYTES {
        return Err("the device key has the wrong length".into());
    }
    XChaCha20Poly1305::new_from_slice(&bytes).map_err(|error| error.to_string())
}

/// Seal `plaintext` for (or from) `device_id` with its key.
pub fn seal(device_id: &str, key: &str, plaintext: &[u8]) -> Result<Envelope, String> {
    let mut nonce = [0u8; NONCE_BYTES];
    getrandom::getrandom(&mut nonce).map_err(|error| error.to_string())?;
    let ciphertext = cipher(key)?
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: device_id.as_bytes(),
            },
        )
        .map_err(|_| "could not encrypt".to_string())?;
    Ok(Envelope {
        device_id: device_id.to_string(),
        nonce: STANDARD.encode(nonce),
        ciphertext: STANDARD.encode(ciphertext),
    })
}

/// Open an envelope with the key of its device; fails for any other key or a changed byte.
pub fn open(envelope: &Envelope, key: &str) -> Result<Vec<u8>, String> {
    let nonce = STANDARD
        .decode(&envelope.nonce)
        .map_err(|_| "bad nonce".to_string())?;
    if nonce.len() != NONCE_BYTES {
        return Err("bad nonce".into());
    }
    let ciphertext = STANDARD
        .decode(&envelope.ciphertext)
        .map_err(|_| "bad ciphertext".to_string())?;
    cipher(key)?
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: envelope.device_id.as_bytes(),
            },
        )
        .map_err(|_| "the message could not be opened (wrong key or changed)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_right_key_and_device_open_a_message() {
        let key = new_key().unwrap();
        let other = new_key().unwrap();
        let sealed = seal("laptop", &key, b"hello").unwrap();
        assert_eq!(open(&sealed, &key).unwrap(), b"hello");
        assert!(open(&sealed, &other).is_err());
        // Claiming another device id breaks it too.
        let mut renamed = sealed.clone();
        renamed.device_id = "desktop".into();
        assert!(open(&renamed, &key).is_err());
        // So does a changed byte.
        let mut changed = sealed;
        let mut bytes = STANDARD.decode(&changed.ciphertext).unwrap();
        bytes[0] ^= 1;
        changed.ciphertext = STANDARD.encode(bytes);
        assert!(open(&changed, &key).is_err());
    }
}

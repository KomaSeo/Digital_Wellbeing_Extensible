//! Responsibility: Let a device join the sync server with a login name and password instead of
//! a pairing code: sign up on an empty server, or log in to an existing account, and get a
//! device key back.
//!
//! - **Account key**: PBKDF2-HMAC-SHA256 of the password, salted with the login name. The
//!   password itself never leaves the device; the server keeps only the account key
//!   (`accounts.json`, never sent to a device).
//! - **Log in**: the device seals a [`LoginRequest`] with the account key (envelope id
//!   `account:<name>`). Only someone who knows the password can make one the server opens, and
//!   the answer (a new device key) is sealed with the same key.
//! - **Sign up**: the server does not know the account key yet, so the device and the server
//!   first agree on a one-time key (X25519 with the server's key from `GET /public-key`). Only
//!   an empty server (no account yet) takes a sign-up: the first person to sign up owns it.
//! - The device keeps the account key in `sync.json` (the locked data folder, like the device
//!   key), so after the server was set up again it logs in again by itself.
//!
//! Walkthrough (Korean): `docs/sync.md` 5장.

use crate::envelope::{self, Envelope};
use crate::users::{clean_name, UserEntry};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// PBKDF2 rounds of the account key (about a tenth of a second).
const ACCOUNT_KEY_ROUNDS: u32 = 200_000;
/// Envelope id prefix of a login request: `account:<name>`.
pub const LOGIN_PREFIX: &str = "account:";
/// Envelope id prefix of a sign-up request: `signup:<the device's one-time public key>`.
pub const SIGNUP_PREFIX: &str = "signup:";

/// Responsibility: Name an account and hold the key its password makes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountCredentials {
    pub name: String,
    /// Base64 of [`account_key`].
    pub key: String,
}

/// The account key of `name` with `password` (base64, a valid envelope key).
pub fn account_key(name: &str, password: &str) -> Result<String, String> {
    let name = clean_name(name)?;
    let mut output = [0u8; envelope::KEY_BYTES];
    let salt = format!("digital-wellbeing-sync account {name}");
    pbkdf2::pbkdf2_hmac::<Sha256>(
        password.as_bytes(),
        salt.as_bytes(),
        ACCOUNT_KEY_ROUNDS,
        &mut output,
    );
    Ok(STANDARD.encode(output))
}

/// Responsibility: Carry a login: which device asks, sealed with the account key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub sent_at: DateTime<Utc>,
    /// The name of the computer (a new device id comes from it).
    pub device_name: String,
    /// The id this device had on this server, kept when it still exists (its key changes).
    #[serde(default)]
    pub previous_device_id: Option<String>,
    #[serde(default)]
    pub app_version: String,
}

/// Responsibility: Carry a sign-up: the new account and the first device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignupRequest {
    pub sent_at: DateTime<Utc>,
    /// Base64 of the account key ([`account_key`]).
    pub account_key: String,
    /// The login for `users.json` (its password hash, made on the device).
    pub login: UserEntry,
    pub device_name: String,
    #[serde(default)]
    pub app_version: String,
}

/// Responsibility: Carry the server's answer to a login or a sign-up: the device's identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinAnswer {
    /// The `sent_at` of the request this answers.
    pub request_sent_at: DateTime<Utc>,
    pub account: String,
    pub device_id: String,
    pub key: String,
}

/// The server's long-term X25519 key pair, from 32 secret bytes.
pub fn server_public_key(secret: &[u8; 32]) -> [u8; 32] {
    x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).to_bytes()
}

/// The one-time key of a sign-up, from the shared secret of the two key pairs.
fn signup_key(shared: &[u8; 32], device_public: &[u8; 32], server_public: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"digital-wellbeing-sync signup");
    hasher.update(shared);
    hasher.update(device_public);
    hasher.update(server_public);
    STANDARD.encode(hasher.finalize())
}

fn decode_32(text: &str) -> Result<[u8; 32], String> {
    STANDARD
        .decode(text.trim())
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
        .ok_or_else(|| "a key is not 32 bytes of base64".to_string())
}

/// The server side of a sign-up envelope: its one-time key, from the device's public key in
/// the envelope id and the server's secret.
pub fn server_signup_key(envelope_id: &str, server_secret: &[u8; 32]) -> Result<String, String> {
    let device_public = decode_32(
        envelope_id
            .strip_prefix(SIGNUP_PREFIX)
            .ok_or("not a sign-up")?,
    )?;
    let secret = x25519_dalek::StaticSecret::from(*server_secret);
    let shared = secret.diffie_hellman(&x25519_dalek::PublicKey::from(device_public));
    Ok(signup_key(
        shared.as_bytes(),
        &device_public,
        &server_public_key(server_secret),
    ))
}

/// Responsibility: Talk to the server for a sign-up or a login (`transport` carries one
/// sealed request to the server and returns its sealed answer).
pub struct Joiner<'transport> {
    pub transport: &'transport mut dyn FnMut(&Envelope) -> Result<Envelope, String>,
}

fn random_32() -> Result<[u8; 32], String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn check_answer(answer: &JoinAnswer, sent_at: DateTime<Utc>) -> Result<(), String> {
    if answer.request_sent_at != sent_at {
        return Err("the answer does not belong to this request".into());
    }
    envelope::seal(&answer.device_id, &answer.key, b"check")?;
    Ok(())
}

impl Joiner<'_> {
    /// Make the first account of an empty server, and join this device to it.
    pub fn sign_up(
        &mut self,
        server_public: &str,
        name: &str,
        password: &str,
        device_name: &str,
    ) -> Result<(JoinAnswer, AccountCredentials), String> {
        let name = clean_name(name)?;
        let server_public = decode_32(server_public)?;
        let device_secret = x25519_dalek::StaticSecret::from(random_32()?);
        let device_public = x25519_dalek::PublicKey::from(&device_secret).to_bytes();
        let shared = device_secret.diffie_hellman(&x25519_dalek::PublicKey::from(server_public));
        let key = signup_key(shared.as_bytes(), &device_public, &server_public);
        let credentials = AccountCredentials {
            name: name.clone(),
            key: account_key(&name, password)?,
        };
        let request = SignupRequest {
            sent_at: Utc::now(),
            account_key: credentials.key.clone(),
            login: UserEntry::new(&name, password)?,
            device_name: device_name.to_string(),
            app_version: crate::protocol::app_version().to_string(),
        };
        let envelope_id = format!("{SIGNUP_PREFIX}{}", STANDARD.encode(device_public));
        let answer = self.exchange(&envelope_id, &key, &request)?;
        check_answer(&answer, request.sent_at)?;
        Ok((answer, credentials))
    }

    /// Log in to an existing account with its key, and join this device to it.
    pub fn log_in(
        &mut self,
        credentials: &AccountCredentials,
        device_name: &str,
        previous_device_id: Option<String>,
    ) -> Result<JoinAnswer, String> {
        let request = LoginRequest {
            sent_at: Utc::now(),
            device_name: device_name.to_string(),
            previous_device_id,
            app_version: crate::protocol::app_version().to_string(),
        };
        let envelope_id = format!("{LOGIN_PREFIX}{}", credentials.name);
        let answer = self.exchange(&envelope_id, &credentials.key, &request)?;
        check_answer(&answer, request.sent_at)?;
        Ok(answer)
    }

    fn exchange<Request: Serialize>(
        &mut self,
        envelope_id: &str,
        key: &str,
        request: &Request,
    ) -> Result<JoinAnswer, String> {
        let plaintext = serde_json::to_vec(request).map_err(|error| error.to_string())?;
        let sealed = envelope::seal(envelope_id, key, &plaintext)?;
        let answer = (self.transport)(&sealed)?;
        if answer.device_id != envelope_id {
            return Err("the server answered another request".into());
        }
        serde_json::from_slice(&envelope::open(&answer, key)?)
            .map_err(|error| format!("bad answer: {error}"))
    }
}

/// The server's public key for a sign-up (`GET /public-key`).
pub fn fetch_public_key(server_url: &str) -> Result<String, String> {
    let url = format!("{}/public-key", server_url.trim_end_matches('/'));
    let answer: serde_json::Value = ureq::get(&url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|error| format!("could not reach the server: {error}"))?
        .into_json()
        .map_err(|error| format!("bad answer from the server: {error}"))?;
    answer["public_key"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "the server is too old for sign-ups (update it first)".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_account_key_depends_on_the_name_and_the_password() {
        let key = account_key("Me", "long password").unwrap();
        assert_eq!(key, account_key("me", "long password").unwrap());
        assert_ne!(key, account_key("me", "other password").unwrap());
        assert_ne!(key, account_key("you", "long password").unwrap());
        assert!(envelope::seal("x", &key, b"fits").is_ok());
    }

    #[test]
    fn both_sides_agree_on_the_signup_key() {
        let server_secret = random_32().unwrap();
        let server_public = server_public_key(&server_secret);
        let device_secret = x25519_dalek::StaticSecret::from(random_32().unwrap());
        let device_public = x25519_dalek::PublicKey::from(&device_secret).to_bytes();
        let shared = device_secret.diffie_hellman(&x25519_dalek::PublicKey::from(server_public));
        let device_side = signup_key(shared.as_bytes(), &device_public, &server_public);
        let envelope_id = format!("{SIGNUP_PREFIX}{}", STANDARD.encode(device_public));
        assert_eq!(
            server_signup_key(&envelope_id, &server_secret).unwrap(),
            device_side
        );
    }
}

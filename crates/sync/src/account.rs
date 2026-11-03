//! Responsibility: Let a device join the sync server with a login name and password, with
//! OPAQUE: the server checks the password without ever receiving it or keeping anything from
//! which it could be read (RFC 9807; the `opaque-ke` crate, Ristretto255, Argon2id).
//!
//! - **Sign up** (only while the server has no account; the first person to sign up owns it):
//!   the device and the server run OPAQUE's registration. The server keeps the "password file"
//!   (`accounts.json`): an envelope only the password opens, and the server's OPRF key.
//! - **Log in**: OPAQUE's login, two round trips. Only a device that knows the password ends
//!   it; both sides then share a session key, and the device sends its request (its name)
//!   sealed with it. The server answers with a new device key, sealed the same way.
//! - Someone who listens in learns nothing to guess the password with. Someone who has the
//!   server's files cannot log in with them; they can only try passwords one by one, each try
//!   costing an Argon2id run, so a long password keeps them out.
//! - **Export key**: a successful login also gives the device a key the server never sees.
//!   The device keeps a key made from it ([`Joined::vault_key`]) and seals the owner's
//!   Anthropic API key with it before it goes to the server ([`crate::vault`]).
//!
//! Wire: `POST /account` with an [`AccountRequest`] as JSON; the answer is an
//! [`AccountAnswer`]. Walkthrough (Korean): `docs/sync.md` 5장.

use crate::envelope::{self, Envelope};
use crate::users::clean_name;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use opaque_ke::{
    ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialFinalization, CredentialRequest,
    CredentialResponse, RegistrationRequest, RegistrationResponse, RegistrationUpload,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Responsibility: Name the OPAQUE primitives both sides use.
pub struct Suite;

impl opaque_ke::CipherSuite for Suite {
    type OprfCs = opaque_ke::Ristretto255;
    type KeyExchange = opaque_ke::TripleDh<opaque_ke::Ristretto255, sha2::Sha512>;
    type Ksf = opaque_ke::argon2::Argon2<'static>;
}

/// What a wrong name or password gets (the same for both, so names cannot be probed).
pub const WRONG_LOGIN: &str = "이름이나 비밀번호가 맞지 않습니다";

/// Responsibility: Carry one step of signing up or logging in, from the device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum AccountRequest {
    SignupStart {
        name: String,
        request: String,
    },
    SignupFinish {
        name: String,
        upload: String,
    },
    LoginStart {
        name: String,
        request: String,
    },
    LoginFinish {
        login_id: String,
        finalization: String,
        /// A [`DeviceRequest`] sealed with the session key (envelope id `login:<login_id>`).
        sealed: Envelope,
    },
}

/// Responsibility: Carry the server's answer to one step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum AccountAnswer {
    SignupStarted {
        response: String,
    },
    SignedUp,
    LoginStarted {
        login_id: String,
        response: String,
    },
    /// A [`JoinAnswer`] sealed with the session key.
    LoggedIn {
        sealed: Envelope,
    },
}

/// Responsibility: Say which device logs in (sealed with the session key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRequest {
    /// The name of the computer (a new device id comes from it).
    pub device_name: String,
    /// The id this device had on this server, kept when it still exists (its key changes).
    #[serde(default)]
    pub previous_device_id: Option<String>,
    #[serde(default)]
    pub app_version: String,
}

/// Responsibility: Carry the device's identity on the server (sealed with the session key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JoinAnswer {
    pub account: String,
    pub device_id: String,
    pub key: String,
}

/// Responsibility: Hold what a device keeps of its login (in `sync.json`, readable by SYSTEM
/// and administrators only) so it can log in again by itself after the server was set up
/// again, and seal the API key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountCredentials {
    pub name: String,
    /// The password, sealed for this PC ([`crate::secret`]).
    pub password: String,
    /// See [`Joined::vault_key`].
    pub vault_key: String,
}

/// Responsibility: Hold what a device has after logging in.
#[derive(Debug, Clone, PartialEq)]
pub struct Joined {
    pub answer: JoinAnswer,
    /// Base64; seals the API key on the server. Made from OPAQUE's export key, which only
    /// the password and the server's OPRF key give; the server never sees it.
    pub vault_key: String,
}

/// The envelope key of a login's messages, from OPAQUE's session key.
pub fn session_envelope_key(session_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"digital-wellbeing-sync session");
    hasher.update(session_key);
    STANDARD.encode(hasher.finalize())
}

fn vault_key_from(export_key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"digital-wellbeing-sync vault");
    hasher.update(export_key);
    STANDARD.encode(hasher.finalize())
}

pub fn encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

pub fn decode(text: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(text)
        .map_err(|_| "a message is not base64".to_string())
}

/// Responsibility: Run the device's side of signing up and logging in (`transport` carries
/// one step to the server and returns its answer).
pub struct Joiner<'transport> {
    pub transport: &'transport mut dyn FnMut(&AccountRequest) -> Result<AccountAnswer, String>,
}

impl Joiner<'_> {
    /// Make the first account of an empty server, then log this device in with it.
    pub fn sign_up(
        &mut self,
        name: &str,
        password: &str,
        device_name: &str,
    ) -> Result<Joined, String> {
        let name = clean_name(name)?;
        crate::users::check_password(password)?;
        let mut rng = rand_core::OsRng;
        let started = ClientRegistration::<Suite>::start(&mut rng, password.as_bytes())
            .map_err(|error| format!("sign-up: {error}"))?;
        let response = match (self.transport)(&AccountRequest::SignupStart {
            name: name.clone(),
            request: encode(&started.message.serialize()),
        })? {
            AccountAnswer::SignupStarted { response } => response,
            other => return Err(format!("unexpected answer: {other:?}")),
        };
        let response = RegistrationResponse::<Suite>::deserialize(&decode(&response)?)
            .map_err(|error| format!("bad answer: {error}"))?;
        let finished = started
            .state
            .finish(
                &mut rng,
                password.as_bytes(),
                response,
                ClientRegistrationFinishParameters::default(),
            )
            .map_err(|error| format!("sign-up: {error}"))?;
        match (self.transport)(&AccountRequest::SignupFinish {
            name: name.clone(),
            upload: encode(&finished.message.serialize()),
        })? {
            AccountAnswer::SignedUp => {}
            other => return Err(format!("unexpected answer: {other:?}")),
        }
        self.log_in(&name, password, device_name, None)
    }

    /// Log in to an existing account, and join this device to it.
    pub fn log_in(
        &mut self,
        name: &str,
        password: &str,
        device_name: &str,
        previous_device_id: Option<String>,
    ) -> Result<Joined, String> {
        let name = clean_name(name)?;
        let mut rng = rand_core::OsRng;
        let started = ClientLogin::<Suite>::start(&mut rng, password.as_bytes())
            .map_err(|error| format!("login: {error}"))?;
        let (login_id, response) = match (self.transport)(&AccountRequest::LoginStart {
            name,
            request: encode(&started.message.serialize()),
        })? {
            AccountAnswer::LoginStarted { login_id, response } => (login_id, response),
            other => return Err(format!("unexpected answer: {other:?}")),
        };
        let response = CredentialResponse::<Suite>::deserialize(&decode(&response)?)
            .map_err(|error| format!("bad answer: {error}"))?;
        // A wrong password fails here, on the device: the envelope does not open.
        let finished = started
            .state
            .finish(
                &mut rng,
                password.as_bytes(),
                response,
                ClientLoginFinishParameters::default(),
            )
            .map_err(|_| WRONG_LOGIN.to_string())?;
        let key = session_envelope_key(&finished.session_key);
        let envelope_id = format!("login:{login_id}");
        let request = DeviceRequest {
            device_name: device_name.to_string(),
            previous_device_id,
            app_version: crate::protocol::app_version().to_string(),
        };
        let plaintext = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
        let sealed = envelope::seal(&envelope_id, &key, &plaintext)?;
        let answer = match (self.transport)(&AccountRequest::LoginFinish {
            login_id,
            finalization: encode(&finished.message.serialize()),
            sealed,
        })? {
            AccountAnswer::LoggedIn { sealed } => sealed,
            other => return Err(format!("unexpected answer: {other:?}")),
        };
        if answer.device_id != envelope_id {
            return Err("the server answered another login".into());
        }
        let answer: JoinAnswer = serde_json::from_slice(&envelope::open(&answer, &key)?)
            .map_err(|error| format!("bad answer: {error}"))?;
        envelope::seal(&answer.device_id, &answer.key, b"check")?;
        Ok(Joined {
            answer,
            vault_key: vault_key_from(&finished.export_key),
        })
    }
}

/// The deserializers the server uses (kept here with the suite).
pub fn registration_request(text: &str) -> Result<RegistrationRequest<Suite>, String> {
    RegistrationRequest::deserialize(&decode(text)?)
        .map_err(|error| format!("bad request: {error}"))
}

pub fn registration_upload(text: &str) -> Result<RegistrationUpload<Suite>, String> {
    RegistrationUpload::deserialize(&decode(text)?).map_err(|error| format!("bad request: {error}"))
}

pub fn credential_request(text: &str) -> Result<CredentialRequest<Suite>, String> {
    CredentialRequest::deserialize(&decode(text)?).map_err(|error| format!("bad request: {error}"))
}

pub fn credential_finalization(text: &str) -> Result<CredentialFinalization<Suite>, String> {
    CredentialFinalization::deserialize(&decode(text)?)
        .map_err(|error| format!("bad request: {error}"))
}

/// Carry one step to `server_url` (`POST /account`).
pub fn http_transport(
    server_url: &str,
) -> impl FnMut(&AccountRequest) -> Result<AccountAnswer, String> {
    let url = format!("{}/account", server_url.trim_end_matches('/'));
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build();
    move |request: &AccountRequest| match agent.post(&url).send_json(request) {
        Ok(response) => response
            .into_json::<AccountAnswer>()
            .map_err(|error| format!("bad answer from the server: {error}")),
        Err(ureq::Error::Status(code, response)) => {
            let reason = response.into_string().unwrap_or_default();
            // The server's reason as it is (the Focus app shows it).
            Err(if reason.is_empty() {
                format!("the server refused ({code})")
            } else {
                reason
            })
        }
        Err(error) => Err(format!("could not reach the server: {error}")),
    }
}

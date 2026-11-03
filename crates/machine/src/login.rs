//! Responsibility: Tie a Windows account of this PC to a login of the owner's sync server, so
//! its access grants, blocks and AI usage follow the person on every device.
//!
//! - The logins (`users.json`) come from the sync server; only its administrator makes them
//!   (`sync-add-user`). Without sync there are none, and nothing here changes anything.
//! - `logins.json` (this PC only) says which login each Windows account used. The Focus app
//!   logs in once per account, with administrator approval (the service's `sync-login`); the
//!   agent then starts again with the login's files.
//! - While the settings are protected, an account cannot log out.
//! - Once the settings are protected, an account that has not logged in is locked (every
//!   app) until it does: otherwise not logging in would be a way around the shared blocks.
//!
//! Walkthrough (Korean): `docs/sync.md` 6장.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;
use sync::users::{Users, USERS_DOCUMENT};

pub const LOGINS_FILE: &str = "logins.json";

/// Responsibility: Remember which login each Windows account of this PC uses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Logins {
    /// Bare Windows account name (lower case) -> login name.
    pub accounts: BTreeMap<String, String>,
}

/// Responsibility: Tell the Focus app who is logged in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoginStatus {
    /// The sync server has logins, so this account can (and should) log in.
    pub available: bool,
    /// The login this account uses.
    pub user: Option<String>,
}

/// Responsibility: Carry what the Focus app asks the elevated `sync-login --request` to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginRequestFile {
    pub server_url: String,
    pub name: String,
    pub password: String,
    /// Make the first account of an empty server instead of logging in.
    #[serde(default)]
    pub signup: bool,
    /// The Windows account that logs in (its grants, blocks and usage become the login's).
    #[serde(default)]
    pub windows_user: Option<String>,
}

static SESSION_USER: OnceLock<String> = OnceLock::new();
type ChangeHook = Box<dyn Fn() + Send + Sync>;
static ON_CHANGE: OnceLock<ChangeHook> = OnceLock::new();

/// What to do after this session logs in or out (the agent starts again with the login's
/// files).
pub fn on_change(hook: impl Fn() + Send + Sync + 'static) {
    let _ = ON_CHANGE.set(Box::new(hook));
}

/// The Windows account of this session, once [`set_session_user`] was called.
pub fn session_user() -> Option<String> {
    SESSION_USER.get().cloned()
}

/// Run the [`on_change`] hook, if any.
pub fn changed() {
    if let Some(hook) = ON_CHANGE.get() {
        hook();
    }
}

/// The Windows account of this agent's session (set once at start).
pub fn set_session_user(user: &str) {
    let _ = SESSION_USER.set(account_key(user));
}

fn account_key(user: &str) -> String {
    crate::bare_user(user).to_lowercase()
}

fn read<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn users(data_directory: &Path) -> Users {
    read(&data_directory.join(USERS_DOCUMENT))
}

/// The login `windows_user` uses, if it still exists.
pub fn login_of(data_directory: &Path, windows_user: &str) -> Option<String> {
    let logins: Logins = read(&data_directory.join(LOGINS_FILE));
    logins
        .accounts
        .get(&account_key(windows_user))
        .filter(|name| users(data_directory).contains(name))
        .cloned()
}

/// Whose files `windows_user` uses: its login, or else the Windows account.
pub fn profile_for(data_directory: &Path, windows_user: &str) -> String {
    login_of(data_directory, windows_user)
        .unwrap_or_else(|| crate::bare_user(windows_user).to_string())
}

/// A file's (modified time, length), to tell whether it changed.
type FileSignature = Option<(std::time::SystemTime, u64)>;

fn signature(path: &Path) -> FileSignature {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// The last [`status`] and what it was read from. The lockdown asks several times a second,
/// and the two files change only on a login, a logout or a sync.
type StatusCache = Option<(
    std::path::PathBuf,
    FileSignature,
    FileSignature,
    bool,
    LoginStatus,
)>;
static STATUS_CACHE: std::sync::Mutex<StatusCache> = std::sync::Mutex::new(None);

pub fn status(data_directory: &Path) -> LoginStatus {
    let users_signature = signature(&data_directory.join(USERS_DOCUMENT));
    let logins_signature = signature(&data_directory.join(LOGINS_FILE));
    let session_known = SESSION_USER.get().is_some();
    let mut cache = STATUS_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((folder, users_seen, logins_seen, session_seen, cached)) = cache.as_ref() {
        if folder == data_directory
            && *users_seen == users_signature
            && *logins_seen == logins_signature
            && *session_seen == session_known
        {
            return cached.clone();
        }
    }
    let fresh = LoginStatus {
        available: !users(data_directory).users.is_empty(),
        user: SESSION_USER
            .get()
            .and_then(|user| login_of(data_directory, user)),
    };
    *cache = Some((
        data_directory.to_path_buf(),
        users_signature,
        logins_signature,
        session_known,
        fresh.clone(),
    ));
    fresh
}

/// This session must log in before it may use the PC (see the module comment).
pub fn required(data_directory: &Path) -> bool {
    let status = status(data_directory);
    SESSION_USER.get().is_some() && status.available && status.user.is_none()
}

/// Log `windows_user` in as `name`. Only the service's `sync-login` calls this, with
/// administrator rights, after the sync server (or the account key this PC keeps) checked the
/// password.
pub fn assign(data_directory: &Path, windows_user: &str, name: &str) -> Result<(), String> {
    let path = data_directory.join(LOGINS_FILE);
    let mut logins: Logins = read(&path);
    logins
        .accounts
        .insert(account_key(windows_user), name.trim().to_lowercase());
    write(&path, &logins)
}

/// Log `windows_user` out; refused while the settings are protected (logging out would be a
/// way out of the shared grants and blocks).
pub fn log_out(data_directory: &Path, windows_user: &str) -> Result<(), String> {
    if crate::settings::is_protected(data_directory) {
        return Err("설정이 보호되어 있는 동안에는 로그아웃할 수 없습니다".into());
    }
    let path = data_directory.join(LOGINS_FILE);
    let mut logins: Logins = read(&path);
    logins.accounts.remove(&account_key(windows_user));
    write(&path, &logins)
}

fn write(path: &Path, logins: &Logins) -> Result<(), String> {
    let text = serde_json::to_string_pretty(logins).map_err(|error| error.to_string())?;
    crate::settings::write_atomic(path, text.as_bytes()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_uses_its_login_once_it_logged_in() {
        let folder = std::env::temp_dir().join(format!("dw_login_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        // No logins on the server: the Windows account, as before.
        assert_eq!(profile_for(&folder, r"PC\Work"), "Work");
        assert!(!status(&folder).available);

        let mut users = Users::default();
        users.set("me", "password").unwrap();
        std::fs::write(
            folder.join(USERS_DOCUMENT),
            serde_json::to_string(&users).unwrap(),
        )
        .unwrap();
        assert!(status(&folder).available);
        assign(&folder, r"PC\Work", "Me").unwrap();
        assert_eq!(profile_for(&folder, "work"), "me");
        assert_eq!(profile_for(&folder, "other"), "other");

        // A login the server removed no longer counts.
        std::fs::write(folder.join(USERS_DOCUMENT), r#"{"users":[]}"#).unwrap();
        assert_eq!(profile_for(&folder, "work"), "work");
        std::fs::write(
            folder.join(USERS_DOCUMENT),
            serde_json::to_string(&users).unwrap(),
        )
        .unwrap();
        // While the settings are protected, the account cannot log out.
        let protection = folder.join(crate::settings::PROTECTION_FILE);
        std::fs::write(&protection, r#"{"protected_since":"2026-01-01T00:00:00Z"}"#).unwrap();
        assert!(log_out(&folder, "work").is_err());
        assert_eq!(profile_for(&folder, "work"), "me");
        std::fs::remove_file(&protection).unwrap();
        log_out(&folder, "work").unwrap();
        assert_eq!(profile_for(&folder, "work"), "work");
        let _ = std::fs::remove_dir_all(&folder);
    }
}

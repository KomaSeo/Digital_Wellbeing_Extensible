use directories::ProjectDirs;
use enforcer::normalize_process_name;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not resolve app data directory")]
    NoAppData,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Persisted settings for [`crate::AiFocusRule`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiFocusConfig {
    /// A focus session is running: the monitor samples activity and asks the judge.
    pub session_active: bool,
    /// What the user says they are working on right now.
    pub task: String,
    /// Seconds between AI checks of the foreground app.
    pub check_interval_secs: u64,
    pub model: String,
    /// Used when `ANTHROPIC_API_KEY` is not set in the environment.
    pub api_key: String,
    /// Attach a downscaled screenshot of the foreground monitor to each check.
    pub send_screenshot: bool,
    /// Consecutive off-task verdicts for the same app before it is blocked.
    pub strikes_to_block: u32,
    /// How long a blocked app stays blocked. `0` = until the session ends.
    pub block_minutes: u64,
    /// Never judged, never blocked.
    pub always_allow: BTreeSet<String>,
    /// Judged by process name + title only; never screenshotted.
    pub private_apps: BTreeSet<String>,
    /// Off-task tabs are closed (Ctrl+W) instead of killing the whole browser.
    pub browsers: BTreeSet<String>,
    /// Users may ask for access that lasts until revoked (not only for minutes).
    pub allow_permanent_grants: bool,
    /// Upper limit for temporary access, in minutes.
    pub max_grant_minutes: u32,
    /// Grant ids the admin revoked. Kept here (an admin-owned file) so the
    /// admin UI never writes the agent's `grants.json`; the monitor drops these.
    pub revoked_grants: BTreeSet<String>,
}

impl Default for AiFocusConfig {
    fn default() -> Self {
        let set = |names: &[&str]| names.iter().map(|n| n.to_string()).collect();
        Self {
            session_active: false,
            task: String::new(),
            check_interval_secs: 60,
            model: DEFAULT_MODEL.to_string(),
            api_key: String::new(),
            send_screenshot: true,
            strikes_to_block: 2,
            block_minutes: 30,
            always_allow: BTreeSet::new(),
            private_apps: set(&["keepass.exe", "1password.exe", "bitwarden.exe"]),
            browsers: set(&[
                "chrome.exe",
                "msedge.exe",
                "firefox.exe",
                "whale.exe",
                "brave.exe",
                "opera.exe",
            ]),
            allow_permanent_grants: true,
            max_grant_minutes: 120,
            revoked_grants: BTreeSet::new(),
        }
    }
}

impl AiFocusConfig {
    pub fn is_always_allowed(&self, process_name: &str) -> bool {
        self.always_allow.contains(&normalize_process_name(process_name))
    }

    pub fn is_private(&self, process_name: &str) -> bool {
        self.private_apps.contains(&normalize_process_name(process_name))
    }

    pub fn is_browser(&self, process_name: &str) -> bool {
        self.browsers.contains(&normalize_process_name(process_name))
    }

    /// API key from the environment, falling back to the saved one.
    pub fn resolve_api_key(&self) -> Option<String> {
        std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| Some(self.api_key.trim().to_string()).filter(|k| !k.is_empty()))
    }
}

/// Shared, optionally file-backed [`AiFocusConfig`].
#[derive(Clone)]
pub struct AiFocusStore {
    path: Option<PathBuf>,
    /// `false` for the per-session agent: the file belongs to the admin UI, and
    /// session/task changes from the user stay in memory only.
    writable: bool,
    inner: Arc<RwLock<AiFocusConfig>>,
}

impl AiFocusStore {
    /// `ai_focus.json` in the app config directory (next to `policy.json`).
    pub fn default() -> Result<Self, ConfigError> {
        let dirs = ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
            .ok_or(ConfigError::NoAppData)?;
        Self::open(dirs.config_dir().join("ai_focus.json"))
    }

    pub fn open(path: PathBuf) -> Result<Self, ConfigError> {
        let config = if path.exists() {
            serde_json::from_str(&fs::read_to_string(&path)?)?
        } else {
            AiFocusConfig::default()
        };
        let store = Self {
            path: Some(path),
            writable: true,
            inner: Arc::new(RwLock::new(config)),
        };
        store.save(&store.get())?;
        Ok(store)
    }

    /// Reads `path` (defaults if missing) and never writes it.
    pub fn open_read_only(path: PathBuf) -> Result<Self, ConfigError> {
        let store = Self {
            path: Some(path),
            writable: false,
            inner: Arc::new(RwLock::new(AiFocusConfig::default())),
        };
        store.reload_keeping_session()?;
        Ok(store)
    }

    /// Re-read the file but keep the in-memory `session_active` / `task`, which
    /// the user controls at runtime (see `open_read_only`).
    pub fn reload_keeping_session(&self) -> Result<(), ConfigError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut fresh: AiFocusConfig = if path.exists() {
            serde_json::from_str(&fs::read_to_string(path)?)?
        } else {
            AiFocusConfig::default()
        };
        let mut config = self.inner.write();
        fresh.session_active = config.session_active;
        fresh.task = std::mem::take(&mut config.task);
        *config = fresh;
        Ok(())
    }

    /// Path of the backing file, if any.
    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    /// In-memory store for tests.
    pub fn in_memory(config: AiFocusConfig) -> Self {
        Self {
            path: None,
            writable: true,
            inner: Arc::new(RwLock::new(config)),
        }
    }

    pub fn get(&self) -> AiFocusConfig {
        self.inner.read().clone()
    }

    /// Apply `f` to the config and persist the result.
    pub fn update(&self, f: impl FnOnce(&mut AiFocusConfig)) -> Result<AiFocusConfig, ConfigError> {
        let snapshot = {
            let mut config = self.inner.write();
            f(&mut config);
            config.clone()
        };
        self.save(&snapshot)?;
        Ok(snapshot)
    }

    fn save(&self, config: &AiFocusConfig) -> Result<(), ConfigError> {
        let Some(path) = self.path.as_ref().filter(|_| self.writable) else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(config)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_store_keeps_session_across_reload_and_never_writes() {
        let dir = std::env::temp_dir().join(format!("dw_ai_cfg_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ai_focus.json");
        let admin = AiFocusStore::open(path.clone()).unwrap();
        admin.update(|c| c.check_interval_secs = 120).unwrap();

        let agent = AiFocusStore::open_read_only(path.clone()).unwrap();
        assert_eq!(agent.get().check_interval_secs, 120);
        agent
            .update(|c| {
                c.session_active = true;
                c.task = "thesis".into();
            })
            .unwrap();
        let on_disk: AiFocusConfig =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!on_disk.session_active);

        admin.update(|c| c.check_interval_secs = 300).unwrap();
        agent.reload_keeping_session().unwrap();
        let c = agent.get();
        assert_eq!(c.check_interval_secs, 300);
        assert!(c.session_active);
        assert_eq!(c.task, "thesis");
        fs::remove_dir_all(&dir).unwrap();
    }
}

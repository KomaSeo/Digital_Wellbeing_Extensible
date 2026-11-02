//! Responsibility: Define the AI focus settings, and load, save and reload them from
//! ai_focus.json.

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
/// The cheaper model that rechecks windows already judged (see `recheck_model`).
pub const DEFAULT_RECHECK_MODEL: &str = "claude-haiku-5-5";

/// Responsibility: Report why the settings could not be loaded or saved.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not resolve app data directory")]
    NoAppData,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Responsibility: Hold every AI focus setting and answer questions about it (is this app
/// allowed, private, a browser; which API key to use).
///
/// Persisted settings for [`crate::AiFocusRule`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AiFocusConfig {
    /// A focus session is running: the monitor samples activity and asks the judge.
    pub session_active: bool,
    /// What the user says they are working on right now.
    pub task: String,
    /// Which to-do the task belongs to (its id; "free" for a task off the list; empty without
    /// a planner). Blocks belong to it: when it changes, the blocks of the new one apply.
    /// Runtime only, like `task` in the agent: never saved.
    #[serde(skip)]
    pub task_scope: String,
    /// Seconds after which a window judged unsure (or an app judged off_task that is
    /// still collecting strikes) is judged again; also the pause after a failed call.
    /// New windows are judged at once, whatever this says.
    pub check_interval_secs: u64,
    pub model: String,
    /// A cheaper model for checking again a window already judged under the current task.
    /// When its verdict is the same as before it stands; when it differs, `model` checks the
    /// window again and its verdict counts. Empty (or the same as `model`): `model` does
    /// every check.
    pub recheck_model: String,
    /// Used when `ANTHROPIC_API_KEY` is not set in the environment.
    pub api_key: String,
    /// Attach a downscaled screenshot of the foreground monitor to each check.
    pub send_screenshot: bool,
    /// Consecutive off-task verdicts for the same app before it is closed and
    /// blocked. Browser tabs are blocked at the first one. Blocks do not
    /// expire; the user asks for access to lift one.
    pub strikes_to_block: u32,
    /// Minutes after which a window judged on_task is judged again; `0` uses
    /// `check_interval_secs`. Changing the task forgets every verdict.
    pub recheck_minutes: u64,
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
    /// Estimated US dollars per day after which focus checks stop until the next day
    /// (`0` = no limit). Planner and access-request calls are still allowed: refusing
    /// them could leave the user stuck on a task. See `usage.rs`.
    pub daily_budget_usd: f64,
    /// Grant ids revoked in the Settings tab. Kept here (the settings file) so the
    /// settings panels never write an agent's grants file; the monitor drops these.
    pub revoked_grants: BTreeSet<String>,
    /// Block ids lifted in the Settings tab, for the same reason: the panel names
    /// them here and each agent drops them from its own blocks file. The panel
    /// forgets an id once no blocks file holds it any more.
    pub unblocked: BTreeSet<String>,
    /// Websites that are always blocked, as case-insensitive regular expressions searched
    /// in browser tab titles (an invalid pattern is matched as plain text). A matching tab is
    /// closed whether or not daily wellbeing is on, without asking the AI.
    pub always_blocked_sites: BTreeSet<String>,
    /// Programs the service may update as administrator, by their winget id
    /// (`Kakao.KakaoTalk`). The user asks in the Focus app, or the service updates them once
    /// a day; the user never gets administrator rights. See `machine::system_care`.
    pub updatable_programs: BTreeSet<String>,
    /// Update [`Self::updatable_programs`] once a day without being asked.
    pub auto_update_programs: bool,
    /// The Windows time zone the service keeps (`Korea Standard Time`, with `_dstoff` when
    /// daylight saving is off). A standard user may change the time zone; the service puts
    /// this one back within seconds, so the life rules' hours cannot be shifted. Empty: the
    /// service writes the PC's time zone here the first time it runs.
    pub time_zone: String,
}

impl Default for AiFocusConfig {
    fn default() -> Self {
        let set = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
        Self {
            session_active: false,
            task: String::new(),
            task_scope: String::new(),
            check_interval_secs: 60,
            model: DEFAULT_MODEL.to_string(),
            recheck_model: DEFAULT_RECHECK_MODEL.to_string(),
            api_key: String::new(),
            send_screenshot: true,
            strikes_to_block: 2,
            recheck_minutes: 15,
            always_allow: BTreeSet::new(),
            private_apps: set(&["keepass.exe", "1password.exe", "bitwarden.exe"]),
            browsers: set(&[
                "chrome.exe",
                "msedge.exe",
                "firefox.exe",
                "whale.exe",
                "brave.exe",
                "opera.exe",
                // Linux process names (normalized like every name here)
                "chromium.exe",
                "chromium-browser.exe",
                "firefox-bin.exe",
                "firefox-esr.exe",
                "microsoft-edge.exe",
                "vivaldi-bin.exe",
            ]),
            allow_permanent_grants: true,
            max_grant_minutes: 120,
            daily_budget_usd: 0.0,
            revoked_grants: BTreeSet::new(),
            unblocked: BTreeSet::new(),
            always_blocked_sites: BTreeSet::new(),
            updatable_programs: BTreeSet::new(),
            auto_update_programs: true,
            time_zone: String::new(),
        }
    }
}

/// `title` without a trailing " - Google Chrome", " — Mozilla Firefox" and the like.
fn without_browser_name(title: &str) -> &str {
    const BROWSER_NAMES: [&str; 10] = [
        "google chrome",
        "chromium",
        "microsoft edge",
        "microsoft\u{200b} edge",
        "mozilla firefox",
        "firefox",
        "whale",
        "brave",
        "opera",
        "vivaldi",
    ];
    for separator in [" - ", " — ", " – "] {
        if let Some((page, last)) = title.rsplit_once(separator) {
            let last = last.trim().to_lowercase();
            if BROWSER_NAMES.iter().any(|name| last.contains(name)) {
                return page;
            }
        }
    }
    title
}

/// Pages a browser may keep open while a broken AI connection locks every app: Claude and
/// Anthropic pages (to get or fix a key), an empty new tab (to type their address), and the
/// Google sign-in page (to log in to Claude). Judged by the tab title, like every website.
pub fn lockdown_allows_page(title: &str) -> bool {
    let page = without_browser_name(title).trim().to_lowercase();
    page.is_empty()
        || ["claude", "anthropic", "google accounts", "google 계정"]
            .iter()
            .any(|allowed| page.contains(allowed))
        || [
            "new tab",
            "새 탭",
            "새 탭 페이지",
            "about:blank",
            "untitled",
            "start page",
        ]
        .iter()
        .any(|empty| page == *empty || page.starts_with(&format!("{empty} - ")))
}

impl AiFocusConfig {
    pub fn is_always_allowed(&self, process_name: &str) -> bool {
        listed(&self.always_allow, process_name)
    }

    pub fn is_private(&self, process_name: &str) -> bool {
        listed(&self.private_apps, process_name)
    }

    pub fn is_browser(&self, process_name: &str) -> bool {
        listed(&self.browsers, process_name)
    }

    /// The always-blocked website pattern a browser tab title matches, if any. The browser's
    /// own name at the end of the title ("… - Google Chrome") is left out, so a pattern such
    /// as "google" does not match every tab.
    pub fn always_blocked_site(&self, title: &str) -> Option<&str> {
        let page = without_browser_name(title);
        self.always_blocked_sites
            .iter()
            .find(|pattern| enforcer::pattern::matches_part(pattern, page))
            .map(String::as_str)
    }

    /// API key from the environment, falling back to the saved one.
    pub fn resolve_api_key(&self) -> Option<String> {
        std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|key| !key.trim().is_empty())
            .or_else(|| Some(self.api_key.trim().to_string()).filter(|key| !key.is_empty()))
    }
}

/// Responsibility: Share one AiFocusConfig between threads and keep it in sync with its file
/// (read-only for agents).
#[derive(Clone)]
pub struct AiFocusStore {
    path: Option<PathBuf>,
    /// `false` for the per-session agent: the file is the shared settings, and
    /// session/task changes from the user stay in memory only.
    writable: bool,
    inner: Arc<RwLock<AiFocusConfig>>,
}

impl AiFocusStore {
    /// `ai_focus.json` in the app config directory (next to `policy.json`).
    pub fn default() -> Result<Self, ConfigError> {
        let project_directories = ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
            .ok_or(ConfigError::NoAppData)?;
        Self::open(project_directories.config_dir().join("ai_focus.json"))
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
        // A damaged file must not stop the agent: run on defaults (which
        // block nothing extra) and pick up the file once it is readable again.
        if let Err(error) = store.reload_keeping_session() {
            tracing::warn!("cannot read AI focus settings ({error}); using defaults for now");
        }
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
        fresh.task_scope = std::mem::take(&mut config.task_scope);
        *config = fresh;
        Ok(())
    }

    /// `grants.json` next to the settings file (standalone install).
    pub fn grants_path(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .map(|settings_path| settings_path.with_file_name("grants.json"))
    }

    /// Standalone mode: `usage.json` next to the settings (token usage, see `usage.rs`).
    pub fn usage_path(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .map(|settings_path| settings_path.with_file_name("usage.json"))
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

    /// Apply `change` to the config and persist the result.
    pub fn update(
        &self,
        change: impl FnOnce(&mut AiFocusConfig),
    ) -> Result<AiFocusConfig, ConfigError> {
        let snapshot = {
            let mut config = self.inner.write();
            change(&mut config);
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
        let temporary_path = path.with_extension("json.tmp");
        fs::write(&temporary_path, serde_json::to_string_pretty(config)?)?;
        fs::rename(&temporary_path, path)?;
        Ok(())
    }
}

/// Is `process_name` in `list`? Both sides are compared in `normalize_process_name` form, so a
/// hand-edited file with "Chrome" or "firefox" still matches.
fn listed(list: &BTreeSet<String>, process_name: &str) -> bool {
    let name = normalize_process_name(process_name);
    list.contains(&name)
        || list
            .iter()
            .any(|entry| normalize_process_name(entry) == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lockdown_keeps_only_claude_pages_and_new_tabs() {
        assert!(lockdown_allows_page("Claude - Google Chrome"));
        assert!(lockdown_allows_page(
            "API keys - Claude Console - Microsoft\u{200b} Edge"
        ));
        assert!(lockdown_allows_page("Anthropic"));
        assert!(lockdown_allows_page("새 탭 - Chrome"));
        assert!(lockdown_allows_page("New Tab"));
        assert!(!lockdown_allows_page("YouTube - Google Chrome"));
        assert!(!lockdown_allows_page("Gmail - Mozilla Firefox"));
    }

    #[test]
    fn lists_match_names_in_any_spelling() {
        let mut config = AiFocusConfig::default();
        assert!(
            config.is_browser("chromium"),
            "Linux browsers are in the defaults"
        );
        config.browsers.insert("MyBrowser".into()); // hand-edited, not normalized
        assert!(config.is_browser("mybrowser"));
        assert!(!config.is_browser("code"));
    }

    #[test]
    fn read_only_store_keeps_session_across_reload_and_never_writes() {
        let directory = std::env::temp_dir().join(format!("dw_ai_cfg_{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("ai_focus.json");
        let admin = AiFocusStore::open(path.clone()).unwrap();
        admin
            .update(|config| config.check_interval_secs = 120)
            .unwrap();

        let agent = AiFocusStore::open_read_only(path.clone()).unwrap();
        assert_eq!(agent.get().check_interval_secs, 120);
        agent
            .update(|config| {
                config.session_active = true;
                config.task = "thesis".into();
            })
            .unwrap();
        let on_disk: AiFocusConfig =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!on_disk.session_active);

        admin
            .update(|config| config.check_interval_secs = 300)
            .unwrap();
        agent.reload_keeping_session().unwrap();
        let reloaded = agent.get();
        assert_eq!(reloaded.check_interval_secs, 300);
        assert!(reloaded.session_active);
        assert_eq!(reloaded.task, "thesis");
        fs::remove_dir_all(&directory).unwrap();
    }
}

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use persistence::{FilePersistence, PersistError, Persistence};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

mod heartbeat;
mod http;
mod rule;

pub use content_filter::{
    ContentFilter, EvaluateItem, EvaluateMedia, EvaluateRequest, EvaluateResult,
};
pub use heartbeat::{
    Heartbeat, HeartbeatMonitor, HeartbeatPayload, DEFAULT_GRACE, DEFAULT_HEARTBEAT_TTL,
};
pub use http::{spawn_http, DEFAULT_PORT};
pub use rule::{BrowserRule, BrowserRuleHandle};

const DEFAULT_BROWSERS: &[&str] = &["chrome.exe", "brave.exe", "safari.exe"];

#[derive(Debug, Error)]
pub enum BrowserError {
    #[error(transparent)]
    Persist(#[from] PersistError),
    #[error("invalid host: {0}")]
    InvalidHost(String),
}

/// Persisted site blacklist + the process names this rule guards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrowserConfig {
    pub browsers: BTreeSet<String>,
    pub blacklist: BTreeSet<String>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        Self {
            browsers: DEFAULT_BROWSERS.iter().map(|n| (*n).to_string()).collect(),
            blacklist: BTreeSet::new(),
            updated_at: Utc::now(),
        }
    }
}

impl BrowserConfig {
    pub fn normalize_process_name(name: &str) -> String {
        let trimmed = name.trim().to_lowercase();
        let file_name = Path::new(&trimmed)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&trimmed);
        if file_name.ends_with(".exe") {
            file_name.to_string()
        } else if file_name.is_empty() {
            trimmed
        } else {
            format!("{file_name}.exe")
        }
    }

    /// `chrome.exe` -> `chrome`. Heartbeat payloads use this id.
    pub fn browser_id_from_process(name: &str) -> String {
        let n = Self::normalize_process_name(name);
        n.strip_suffix(".exe").unwrap_or(&n).to_string()
    }

    pub fn normalize_host(raw: &str) -> Result<String, BrowserError> {
        let mut s = raw.trim().to_lowercase();
        if let Some(rest) = s.strip_prefix("https://") {
            s = rest.to_string();
        } else if let Some(rest) = s.strip_prefix("http://") {
            s = rest.to_string();
        }
        if let Some(rest) = s.strip_prefix("www.") {
            s = rest.to_string();
        }
        if let Some(rest) = s.strip_prefix("*.") {
            s = rest.to_string();
        }
        if let Some(i) = s.find('/') {
            s.truncate(i);
        }
        if let Some(i) = s.find('?') {
            s.truncate(i);
        }
        if let Some(i) = s.find('#') {
            s.truncate(i);
        }
        if let Some(i) = s.rfind(':') {
            let after = &s[i + 1..];
            if !after.is_empty() && after.bytes().all(|b| b.is_ascii_digit()) {
                s.truncate(i);
            }
        }
        let s = s.trim_end_matches('.').trim().to_string();
        if s.is_empty() || s.contains(' ') || !s.contains('.') {
            return Err(BrowserError::InvalidHost(raw.to_string()));
        }
        Ok(s)
    }

    pub fn is_guarded(&self, process_name: &str) -> bool {
        self.browsers
            .contains(&Self::normalize_process_name(process_name))
    }
}

#[derive(Clone)]
pub struct BrowserStore {
    inner: Arc<RwLock<BrowserConfig>>,
    persistence: Arc<dyn Persistence<BrowserConfig>>,
}

impl BrowserStore {
    pub fn default() -> Result<Self, BrowserError> {
        Self::with_persistence(Arc::new(
            FilePersistence::<BrowserConfig>::app_file("browser_rule.json")?,
        ))
    }

    pub fn with_persistence(
        persistence: Arc<dyn Persistence<BrowserConfig>>,
    ) -> Result<Self, BrowserError> {
        let config = persistence.load()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            persistence,
        })
    }

    pub fn get(&self) -> BrowserConfig {
        self.inner.read().clone()
    }

    fn mutate<F, T>(&self, f: F) -> Result<T, BrowserError>
    where
        F: FnOnce(&mut BrowserConfig) -> Result<T, BrowserError>,
    {
        let mut guard = self.inner.write();
        let result = f(&mut guard)?;
        guard.updated_at = Utc::now();
        self.persistence.save(&guard)?;
        Ok(result)
    }

    pub fn add_blacklist(&self, host: &str) -> Result<BrowserConfig, BrowserError> {
        let host = BrowserConfig::normalize_host(host)?;
        self.mutate(|c| {
            c.blacklist.insert(host);
            Ok(c.clone())
        })
    }

    pub fn remove_blacklist(&self, host: &str) -> Result<BrowserConfig, BrowserError> {
        let host = match BrowserConfig::normalize_host(host) {
            Ok(h) => h,
            Err(_) => host.trim().to_lowercase(),
        };
        self.mutate(|c| {
            c.blacklist.remove(&host);
            Ok(c.clone())
        })
    }

    pub fn set_blacklist(&self, hosts: &[String]) -> Result<BrowserConfig, BrowserError> {
        let mut next = BTreeSet::new();
        for raw in hosts {
            if let Ok(host) = BrowserConfig::normalize_host(raw) {
                next.insert(host);
            }
        }
        self.mutate(|c| {
            c.blacklist = next;
            Ok(c.clone())
        })
    }

    pub fn blacklist_vec(&self) -> Vec<String> {
        self.get().blacklist.into_iter().collect()
    }
}

#[cfg(test)]
mod store_tests;

#[cfg(test)]
mod evaluate_tests;

#[cfg(test)]
mod http_tests;

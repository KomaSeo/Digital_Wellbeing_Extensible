//! Responsibility: Define the blacklist/whitelist configuration and ListStore, which changes
//! and persists it.

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;

mod persistence;
mod rule;
mod settings_file;

pub use persistence::{FileListPersistence, ListPersistence};
pub use rule::{ListBasedRule, ListBasedRuleHandle};

/// Processes that must never be terminated (OS stability + self-protection).
/// Delegates to [`enforcer::never_terminate`] so this rule and the enforcer stay aligned.
pub fn always_protected() -> &'static [&'static str] {
    enforcer::never_terminate()
}

/// Responsibility: Say whether the blacklist or the whitelist is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ListMode {
    #[default]
    Blacklist,
    Whitelist,
}

/// Responsibility: Keep a named snapshot of the lists so it can be applied again later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub name: String,
    pub mode: ListMode,
    pub blacklist: BTreeSet<String>,
    pub whitelist: BTreeSet<String>,
}

/// Responsibility: Hold the lists and the mode, and decide whether a process name must be
/// blocked.
///
/// Persisted blacklist / whitelist configuration for [`ListBasedRule`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListConfig {
    #[serde(default)]
    pub mode: ListMode,
    #[serde(default)]
    pub blacklist: BTreeSet<String>,
    #[serde(default)]
    pub whitelist: BTreeSet<String>,
    #[serde(default)]
    pub strategies: Vec<Strategy>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for ListConfig {
    fn default() -> Self {
        let mut whitelist = BTreeSet::new();
        for name in always_protected() {
            whitelist.insert((*name).to_string());
        }
        Self {
            mode: ListMode::Blacklist,
            blacklist: BTreeSet::new(),
            whitelist,
            strategies: Vec::new(),
            updated_at: Utc::now(),
        }
    }
}

impl ListConfig {
    pub fn normalize_name(name: &str) -> String {
        let trimmed = name.trim().to_lowercase();
        let file_name = Path::new(&trimmed)
            .file_name()
            .and_then(|os_file_name| os_file_name.to_str())
            .unwrap_or(&trimmed);
        if file_name.ends_with(".exe") {
            file_name.to_string()
        } else if file_name.is_empty() {
            trimmed
        } else {
            format!("{file_name}.exe")
        }
    }

    /// Bring config from any source (file, API, strategy) into the form the
    /// rule relies on: normalized names, no empty names, protected names never
    /// blacklisted and always whitelisted, no unnamed strategies.
    pub fn sanitize(&mut self) {
        fn clean(set: &mut BTreeSet<String>) {
            *set = set
                .iter()
                .map(|name| {
                    // A regular expression is kept as written (lower case): normalizing would
                    // append ".exe" or cut it at a backslash.
                    if enforcer::pattern::is_regex(name) {
                        name.trim().to_lowercase()
                    } else {
                        ListConfig::normalize_name(name)
                    }
                })
                .filter(|name| !name.is_empty())
                .collect();
        }
        fn fix_lists(blacklist: &mut BTreeSet<String>, whitelist: &mut BTreeSet<String>) {
            clean(blacklist);
            clean(whitelist);
            blacklist.retain(|name| !ListConfig::is_protected(name));
            for protected in always_protected() {
                whitelist.insert((*protected).to_string());
            }
        }
        fix_lists(&mut self.blacklist, &mut self.whitelist);
        self.strategies
            .retain(|strategy| !strategy.name.trim().is_empty());
        for strategy in &mut self.strategies {
            strategy.name = strategy.name.trim().to_string();
            fix_lists(&mut strategy.blacklist, &mut strategy.whitelist);
        }
    }

    pub fn is_protected(name: &str) -> bool {
        let normalized = Self::normalize_name(name);
        app_identity::is_own_program(&normalized)
            || always_protected()
                .iter()
                .any(|protected| *protected == normalized)
    }

    pub fn should_block(&self, process_name: &str) -> bool {
        let name = Self::normalize_name(process_name);
        if Self::is_protected(&name) {
            return false;
        }
        // An entry is a name, or a regular expression the whole name must match.
        let listed = |list: &BTreeSet<String>| {
            list.contains(&name)
                || list
                    .iter()
                    .filter(|entry| enforcer::pattern::is_regex(entry))
                    .any(|pattern| enforcer::pattern::matches_whole(pattern, &name))
        };
        match self.mode {
            ListMode::Blacklist => listed(&self.blacklist),
            ListMode::Whitelist => !listed(&self.whitelist),
        }
    }
}

/// Responsibility: Report why the list file could not be read.
#[derive(Debug, Error)]
pub enum ListError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Responsibility: Share the ListConfig between threads and re-read it when its file changes.
/// The agent's settings code (`machine::settings`) writes the file; this store only reads it.
#[derive(Clone)]
pub struct ListStore {
    inner: Arc<RwLock<ListConfig>>,
    persistence: Arc<dyn ListPersistence>,
}

impl ListStore {
    /// Inject a custom [`ListPersistence`] (e.g. memory backend in tests).
    pub fn with_persistence(persistence: Arc<dyn ListPersistence>) -> Result<Self, ListError> {
        let mut config = persistence.load()?;
        config.sanitize();
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            persistence,
        })
    }

    pub fn get(&self) -> ListConfig {
        self.inner.read().clone()
    }

    /// Re-read the persisted config, discarding the in-memory copy (no save).
    /// Used when another process (an agent saving the Settings tab's edits) changes the file.
    pub fn reload(&self) -> Result<(), ListError> {
        let mut config = self.persistence.load()?;
        config.sanitize();
        *self.inner.write() = config;
        Ok(())
    }
}

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod store_tests;

pub use settings_file::SETTINGS_FILE;

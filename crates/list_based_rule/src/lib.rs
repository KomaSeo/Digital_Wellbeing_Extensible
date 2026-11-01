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

pub use persistence::{FileListPersistence, ListPersistence, MemoryListPersistence};
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
        always_protected()
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

/// Responsibility: Report why a list change failed.
#[derive(Debug, Error)]
pub enum ListError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot modify protected process: {0}")]
    Protected(String),
    #[error("strategy not found: {0}")]
    StrategyNotFound(String),
    #[error("strategy name is empty")]
    EmptyStrategyName,
    #[error("app data directory unavailable")]
    NoAppData,
}

/// Responsibility: Share the ListConfig between threads, apply changes to it, and persist them.
#[derive(Clone)]
pub struct ListStore {
    inner: Arc<RwLock<ListConfig>>,
    persistence: Arc<dyn ListPersistence>,
}

impl ListStore {
    /// Default store backed by [`FileListPersistence::default`].
    pub fn default() -> Result<Self, ListError> {
        Self::with_persistence(Arc::new(FileListPersistence::default()?))
    }

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

    fn mutate<F, T>(&self, change: F) -> Result<T, ListError>
    where
        F: FnOnce(&mut ListConfig) -> Result<T, ListError>,
    {
        // Change a copy and save it first: if saving fails, memory keeps
        // matching the file (and what the agent enforces).
        let mut guard = self.inner.write();
        let mut next = guard.clone();
        let result = change(&mut next)?;
        next.updated_at = Utc::now();
        self.persistence.save(&next)?;
        *guard = next;
        Ok(result)
    }

    pub fn set_mode(&self, mode: ListMode) -> Result<ListConfig, ListError> {
        self.mutate(|config| {
            config.mode = mode;
            Ok(config.clone())
        })
    }

    pub fn add_blacklist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        if ListConfig::is_protected(&name) {
            return Err(ListError::Protected(name));
        }
        self.mutate(|config| {
            config.blacklist.insert(name);
            Ok(config.clone())
        })
    }

    pub fn remove_blacklist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        self.mutate(|config| {
            config.blacklist.remove(&name);
            Ok(config.clone())
        })
    }

    pub fn add_whitelist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        self.mutate(|config| {
            config.whitelist.insert(name);
            Ok(config.clone())
        })
    }

    pub fn remove_whitelist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        if ListConfig::is_protected(&name) {
            return Err(ListError::Protected(name));
        }
        self.mutate(|config| {
            config.whitelist.remove(&name);
            Ok(config.clone())
        })
    }

    /// Replace the entire blacklist in one shot. Protected system names are skipped.
    pub fn set_blacklist(&self, names: &[String]) -> Result<ListConfig, ListError> {
        let mut next = BTreeSet::new();
        for raw in names {
            let name = ListConfig::normalize_name(raw);
            if name.is_empty() || ListConfig::is_protected(&name) {
                continue;
            }
            next.insert(name);
        }
        self.mutate(|config| {
            config.blacklist = next;
            Ok(config.clone())
        })
    }

    /// Replace the entire whitelist in one shot. Protected system names are always kept.
    pub fn set_whitelist(&self, names: &[String]) -> Result<ListConfig, ListError> {
        let mut next = BTreeSet::new();
        for raw in names {
            let name = ListConfig::normalize_name(raw);
            if !name.is_empty() {
                next.insert(name);
            }
        }
        for protected in always_protected() {
            next.insert((*protected).to_string());
        }
        self.mutate(|config| {
            config.whitelist = next;
            Ok(config.clone())
        })
    }

    pub fn save_strategy(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err(ListError::EmptyStrategyName);
        }
        self.mutate(|config| {
            let strategy = Strategy {
                name: name.clone(),
                mode: config.mode,
                blacklist: config.blacklist.clone(),
                whitelist: config.whitelist.clone(),
            };
            if let Some(existing) = config
                .strategies
                .iter_mut()
                .find(|candidate| candidate.name == name)
            {
                *existing = strategy;
            } else {
                config.strategies.push(strategy);
            }
            Ok(config.clone())
        })
    }

    pub fn apply_strategy(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = name.trim().to_string();
        self.mutate(|config| {
            let strategy = config
                .strategies
                .iter()
                .find(|candidate| candidate.name == name)
                .cloned()
                .ok_or_else(|| ListError::StrategyNotFound(name.clone()))?;
            config.mode = strategy.mode;
            config.blacklist = strategy.blacklist;
            config.whitelist = strategy.whitelist;
            for protected in always_protected() {
                config.whitelist.insert((*protected).to_string());
            }
            Ok(config.clone())
        })
    }

    pub fn replace_config(&self, mut new_config: ListConfig) -> Result<ListConfig, ListError> {
        new_config.sanitize();
        self.mutate(|config| {
            *config = new_config;
            Ok(config.clone())
        })
    }
}

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod store_tests;

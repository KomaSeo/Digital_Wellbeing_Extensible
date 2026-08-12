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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ListMode {
    #[default]
    Blacklist,
    Whitelist,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Strategy {
    pub name: String,
    pub mode: ListMode,
    pub blacklist: BTreeSet<String>,
    pub whitelist: BTreeSet<String>,
}

/// Persisted blacklist / whitelist configuration for [`ListBasedRule`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListConfig {
    pub mode: ListMode,
    pub blacklist: BTreeSet<String>,
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

    pub fn is_protected(name: &str) -> bool {
        let n = Self::normalize_name(name);
        always_protected().iter().any(|p| *p == n)
    }

    pub fn should_block(&self, process_name: &str) -> bool {
        let name = Self::normalize_name(process_name);
        if Self::is_protected(&name) {
            return false;
        }
        match self.mode {
            ListMode::Blacklist => self.blacklist.contains(&name),
            ListMode::Whitelist => !self.whitelist.contains(&name),
        }
    }
}

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
    #[error("app data directory unavailable")]
    NoAppData,
}

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
        let config = persistence.load()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            persistence,
        })
    }

    pub fn get(&self) -> ListConfig {
        self.inner.read().clone()
    }

    fn mutate<F, T>(&self, f: F) -> Result<T, ListError>
    where
        F: FnOnce(&mut ListConfig) -> Result<T, ListError>,
    {
        let mut guard = self.inner.write();
        let result = f(&mut guard)?;
        guard.updated_at = Utc::now();
        self.persistence.save(&guard)?;
        Ok(result)
    }

    pub fn set_mode(&self, mode: ListMode) -> Result<ListConfig, ListError> {
        self.mutate(|p| {
            p.mode = mode;
            Ok(p.clone())
        })
    }

    pub fn add_blacklist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        if ListConfig::is_protected(&name) {
            return Err(ListError::Protected(name));
        }
        self.mutate(|p| {
            p.blacklist.insert(name);
            Ok(p.clone())
        })
    }

    pub fn remove_blacklist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        self.mutate(|p| {
            p.blacklist.remove(&name);
            Ok(p.clone())
        })
    }

    pub fn add_whitelist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        self.mutate(|p| {
            p.whitelist.insert(name);
            Ok(p.clone())
        })
    }

    pub fn remove_whitelist(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = ListConfig::normalize_name(name);
        if ListConfig::is_protected(&name) {
            return Err(ListError::Protected(name));
        }
        self.mutate(|p| {
            p.whitelist.remove(&name);
            Ok(p.clone())
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
        self.mutate(|p| {
            p.blacklist = next;
            Ok(p.clone())
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
        self.mutate(|p| {
            p.whitelist = next;
            Ok(p.clone())
        })
    }

    pub fn save_strategy(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = name.trim().to_string();
        self.mutate(|p| {
            let strategy = Strategy {
                name: name.clone(),
                mode: p.mode,
                blacklist: p.blacklist.clone(),
                whitelist: p.whitelist.clone(),
            };
            if let Some(existing) = p.strategies.iter_mut().find(|s| s.name == name) {
                *existing = strategy;
            } else {
                p.strategies.push(strategy);
            }
            Ok(p.clone())
        })
    }

    pub fn apply_strategy(&self, name: &str) -> Result<ListConfig, ListError> {
        let name = name.trim().to_string();
        self.mutate(|p| {
            let strategy = p
                .strategies
                .iter()
                .find(|s| s.name == name)
                .cloned()
                .ok_or_else(|| ListError::StrategyNotFound(name.clone()))?;
            p.mode = strategy.mode;
            p.blacklist = strategy.blacklist;
            p.whitelist = strategy.whitelist;
            for protected in always_protected() {
                p.whitelist.insert((*protected).to_string());
            }
            Ok(p.clone())
        })
    }

    pub fn replace_config(&self, mut config: ListConfig) -> Result<ListConfig, ListError> {
        for protected in always_protected() {
            config.whitelist.insert((*protected).to_string());
        }
        config.updated_at = Utc::now();
        let mut guard = self.inner.write();
        *guard = config.clone();
        self.persistence.save(&guard)?;
        Ok(config)
    }
}

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod store_tests;

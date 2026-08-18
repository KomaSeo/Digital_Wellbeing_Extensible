use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use persistence::{FilePersistence, PersistError, Persistence};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;
use thiserror::Error;

mod filter;

pub use filter::{WordBasedFilter, WordBasedFilterHandle};

fn default_blacklist() -> BTreeSet<String> {
    [
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// Persisted properties for [`WordBasedFilter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordBasedConfig {
    #[serde(default = "default_blacklist")]
    pub blacklist: BTreeSet<String>,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for WordBasedConfig {
    fn default() -> Self {
        Self {
            blacklist: default_blacklist(),
            updated_at: Utc::now(),
        }
    }
}

impl WordBasedConfig {
    pub fn normalize_word(raw: &str) -> String {
        raw.trim().to_lowercase()
    }
}

#[derive(Debug, Error)]
pub enum WordBasedError {
    #[error(transparent)]
    Persist(#[from] PersistError),
    #[error("empty word")]
    EmptyWord,
}

#[derive(Clone)]
pub struct WordBasedStore {
    inner: Arc<RwLock<WordBasedConfig>>,
    persistence: Arc<dyn Persistence<WordBasedConfig>>,
}

impl WordBasedStore {
    pub fn default() -> Result<Self, WordBasedError> {
        Self::with_persistence(Arc::new(FilePersistence::<WordBasedConfig>::app_file(
            "word_based_filter.json",
        )?))
    }

    pub fn with_persistence(
        persistence: Arc<dyn Persistence<WordBasedConfig>>,
    ) -> Result<Self, WordBasedError> {
        let config = persistence.load()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            persistence,
        })
    }

    pub fn get(&self) -> WordBasedConfig {
        self.inner.read().clone()
    }

    fn mutate<F, T>(&self, f: F) -> Result<T, WordBasedError>
    where
        F: FnOnce(&mut WordBasedConfig) -> Result<T, WordBasedError>,
    {
        let mut guard = self.inner.write();
        let result = f(&mut guard)?;
        guard.updated_at = Utc::now();
        self.persistence.save(&guard)?;
        Ok(result)
    }

    pub fn add_word(&self, raw: &str) -> Result<WordBasedConfig, WordBasedError> {
        let word = WordBasedConfig::normalize_word(raw);
        if word.is_empty() {
            return Err(WordBasedError::EmptyWord);
        }
        self.mutate(|c| {
            c.blacklist.insert(word);
            Ok(c.clone())
        })
    }

    pub fn remove_word(&self, raw: &str) -> Result<WordBasedConfig, WordBasedError> {
        let word = WordBasedConfig::normalize_word(raw);
        self.mutate(|c| {
            c.blacklist.remove(&word);
            Ok(c.clone())
        })
    }

    pub fn set_blacklist(&self, words: &[String]) -> Result<WordBasedConfig, WordBasedError> {
        let mut next = BTreeSet::new();
        for raw in words {
            let word = WordBasedConfig::normalize_word(raw);
            if !word.is_empty() {
                next.insert(word);
            }
        }
        self.mutate(|c| {
            c.blacklist = next;
            Ok(c.clone())
        })
    }
}

#[cfg(test)]
mod store_tests;

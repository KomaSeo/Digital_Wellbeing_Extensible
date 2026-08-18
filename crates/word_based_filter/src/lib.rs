use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use persistence::{FilePersistence, PersistError, Persistence};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

mod filter;

pub use filter::{WordBasedFilter, WordBasedFilterHandle};

fn default_true() -> bool {
    true
}

/// Persisted properties for [`WordBasedFilter`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WordBasedConfig {
    #[serde(default = "default_true")]
    pub block_violence: bool,
    #[serde(default = "default_true")]
    pub block_sexual: bool,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for WordBasedConfig {
    fn default() -> Self {
        Self {
            block_violence: true,
            block_sexual: true,
            updated_at: Utc::now(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WordBasedError {
    #[error(transparent)]
    Persist(#[from] PersistError),
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

    pub fn set_block_violence(&self, enabled: bool) -> Result<WordBasedConfig, WordBasedError> {
        self.mutate(|c| {
            c.block_violence = enabled;
            Ok(c.clone())
        })
    }

    pub fn set_block_sexual(&self, enabled: bool) -> Result<WordBasedConfig, WordBasedError> {
        self.mutate(|c| {
            c.block_sexual = enabled;
            Ok(c.clone())
        })
    }
}

#[cfg(test)]
mod store_tests;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use persistence::{FilePersistence, PersistError, Persistence};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

mod cache;
mod client;
mod filter;

pub use cache::LlmCacheStore;
pub use filter::{LlmBasedFilter, LlmBasedFilterHandle};

pub const DEFAULT_PROMPT: &str = "Block violent, sexual, self-harm, or otherwise harmful content. Allow ordinary conversation, news, education, and entertainment that does not match this policy.";

fn default_true() -> bool {
    true
}

/// Persisted properties for [`LlmBasedFilter`].
#[derive(Clone, Serialize, Deserialize)]
pub struct LlmBasedConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_true")]
    pub judge_text: bool,
    #[serde(default = "default_true")]
    pub judge_image: bool,
    #[serde(default)]
    pub prompt_enabled: bool,
    #[serde(default)]
    pub prompt: String,
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for LlmBasedConfig {
    fn default() -> Self {
        Self {
            api_key: String::new(),
            judge_text: true,
            judge_image: true,
            prompt_enabled: false,
            prompt: String::new(),
            updated_at: Utc::now(),
        }
    }
}

impl std::fmt::Debug for LlmBasedConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmBasedConfig")
            .field(
                "api_key",
                &if self.api_key.is_empty() {
                    "<empty>"
                } else {
                    "<redacted>"
                },
            )
            .field("judge_text", &self.judge_text)
            .field("judge_image", &self.judge_image)
            .field("prompt_enabled", &self.prompt_enabled)
            .field("prompt", &self.prompt)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl LlmBasedConfig {
    pub fn effective_prompt(&self) -> &str {
        let custom = self.prompt.trim();
        if self.prompt_enabled && !custom.is_empty() {
            custom
        } else {
            DEFAULT_PROMPT
        }
    }
}

#[derive(Debug, Error)]
pub enum LlmBasedError {
    #[error(transparent)]
    Persist(#[from] PersistError),
}

#[derive(Clone)]
pub struct LlmBasedStore {
    inner: Arc<RwLock<LlmBasedConfig>>,
    persistence: Arc<dyn Persistence<LlmBasedConfig>>,
}

impl LlmBasedStore {
    pub fn default() -> Result<Self, LlmBasedError> {
        Self::with_persistence(Arc::new(FilePersistence::<LlmBasedConfig>::app_file(
            "llm_based_filter.json",
        )?))
    }

    pub fn with_persistence(
        persistence: Arc<dyn Persistence<LlmBasedConfig>>,
    ) -> Result<Self, LlmBasedError> {
        let config = persistence.load()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(config)),
            persistence,
        })
    }

    pub fn get(&self) -> LlmBasedConfig {
        self.inner.read().clone()
    }

    fn mutate<F, T>(&self, f: F) -> Result<T, LlmBasedError>
    where
        F: FnOnce(&mut LlmBasedConfig) -> Result<T, LlmBasedError>,
    {
        let mut guard = self.inner.write();
        let result = f(&mut guard)?;
        guard.updated_at = Utc::now();
        self.persistence.save(&guard)?;
        Ok(result)
    }

    pub fn set_api_key(&self, api_key: &str) -> Result<LlmBasedConfig, LlmBasedError> {
        self.mutate(|c| {
            c.api_key = api_key.trim().to_string();
            Ok(c.clone())
        })
    }

    pub fn set_judge_text(&self, enabled: bool) -> Result<LlmBasedConfig, LlmBasedError> {
        self.mutate(|c| {
            c.judge_text = enabled;
            Ok(c.clone())
        })
    }

    pub fn set_judge_image(&self, enabled: bool) -> Result<LlmBasedConfig, LlmBasedError> {
        self.mutate(|c| {
            c.judge_image = enabled;
            Ok(c.clone())
        })
    }

    pub fn set_prompt_enabled(&self, enabled: bool) -> Result<LlmBasedConfig, LlmBasedError> {
        self.mutate(|c| {
            c.prompt_enabled = enabled;
            Ok(c.clone())
        })
    }

    pub fn set_prompt(&self, prompt: &str) -> Result<LlmBasedConfig, LlmBasedError> {
        self.mutate(|c| {
            c.prompt = prompt.to_string();
            Ok(c.clone())
        })
    }
}

#[cfg(test)]
mod store_tests;

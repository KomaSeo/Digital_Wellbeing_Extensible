use persistence::{FilePersistence, PersistError, Persistence};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

const MAX_ENTRIES: usize = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheVerdict {
    pub action: String,
    #[serde(default)]
    pub reason: Option<String>,
}

impl CacheVerdict {
    pub fn allow() -> Self {
        Self {
            action: "allow".into(),
            reason: None,
        }
    }

    pub fn block(reason: impl Into<String>) -> Self {
        Self {
            action: "block".into(),
            reason: Some(reason.into()),
        }
    }

    pub fn is_block(&self) -> bool {
        self.action == "block"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LlmCacheFile {
    #[serde(default)]
    pub entries: BTreeMap<String, CacheVerdict>,
}

#[derive(Clone)]
pub struct LlmCacheStore {
    inner: Arc<RwLock<LlmCacheFile>>,
    persistence: Arc<dyn Persistence<LlmCacheFile>>,
}

impl LlmCacheStore {
    pub fn default() -> Result<Self, PersistError> {
        Self::with_persistence(Arc::new(FilePersistence::<LlmCacheFile>::app_file(
            "llm_based_filter_cache.json",
        )?))
    }

    pub fn with_persistence(
        persistence: Arc<dyn Persistence<LlmCacheFile>>,
    ) -> Result<Self, PersistError> {
        let file = persistence.load()?;
        Ok(Self {
            inner: Arc::new(RwLock::new(file)),
            persistence,
        })
    }

    pub fn get(&self, hash: &str) -> Option<CacheVerdict> {
        self.inner.read().entries.get(hash).cloned()
    }

    pub fn insert_many(
        &self,
        entries: impl IntoIterator<Item = (String, CacheVerdict)>,
    ) -> Result<(), PersistError> {
        let mut guard = self.inner.write();
        guard.entries.extend(entries);
        if guard.entries.len() > MAX_ENTRIES {
            let extra = guard.entries.len() - MAX_ENTRIES;
            let keys: Vec<String> = guard.entries.keys().take(extra).cloned().collect();
            for key in keys {
                guard.entries.remove(&key);
            }
        }
        self.persistence.save(&guard)
    }

    pub fn len(&self) -> usize {
        self.inner.read().entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use persistence::MemoryPersistence;

    #[test]
    fn roundtrip_verdict() {
        let persistence = Arc::new(MemoryPersistence::<LlmCacheFile>::new());
        let store = LlmCacheStore::with_persistence(
            Arc::clone(&persistence) as Arc<dyn Persistence<LlmCacheFile>>,
        )
        .unwrap();
        store
            .insert_many([(
                "abc".into(),
                CacheVerdict::block("violence"),
            )])
            .unwrap();
        let reloaded = LlmCacheStore::with_persistence(persistence).unwrap();
        let hit = reloaded.get("abc").unwrap();
        assert!(hit.is_block());
        assert_eq!(hit.reason.as_deref(), Some("violence"));
        assert!(reloaded.get("missing").is_none());
    }
}

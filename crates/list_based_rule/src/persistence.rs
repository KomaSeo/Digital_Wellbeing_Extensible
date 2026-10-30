//! Responsibility: Load and save ListConfig, keeping file I/O out of ListStore.

use crate::{ListConfig, ListError};
use directories::ProjectDirs;
use parking_lot::RwLock;
use std::fs;
use std::path::{Path, PathBuf};

/// Responsibility: Load and save a ListConfig somewhere.
///
/// Loads and saves list configs. Keeps filesystem (or other I/O) out of [`crate::ListStore`].
pub trait ListPersistence: Send + Sync {
    fn load(&self) -> Result<ListConfig, ListError>;
    fn save(&self, config: &ListConfig) -> Result<(), ListError>;
}

/// Responsibility: Keep ListConfig in a JSON file, written atomically.
///
/// Persists list config as JSON on disk (atomic write via temp file + rename).
#[derive(Debug, Clone)]
pub struct FileListPersistence {
    path: PathBuf,
}

impl FileListPersistence {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// App config directory + `policy.json` (legacy filename kept for existing installs).
    pub fn default_config_path() -> Result<PathBuf, ListError> {
        let dirs = ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
            .ok_or(ListError::NoAppData)?;
        Ok(dirs.config_dir().join("policy.json"))
    }

    /// File persistence at [`Self::default_config_path`].
    pub fn default() -> Result<Self, ListError> {
        Ok(Self::new(Self::default_config_path()?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ListPersistence for FileListPersistence {
    fn load(&self) -> Result<ListConfig, ListError> {
        if self.path.exists() {
            let raw = fs::read_to_string(&self.path)?;
            Ok(serde_json::from_str(&raw)?)
        } else {
            let config = ListConfig::default();
            self.save(&config)?;
            Ok(config)
        }
    }

    fn save(&self, config: &ListConfig) -> Result<(), ListError> {
        let raw = serde_json::to_string_pretty(config)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, &raw)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Responsibility: Keep ListConfig in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryListPersistence {
    data: RwLock<ListConfig>,
}

impl MemoryListPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_config(config: ListConfig) -> Self {
        Self {
            data: RwLock::new(config),
        }
    }

    pub fn snapshot(&self) -> ListConfig {
        self.data.read().clone()
    }
}

impl ListPersistence for MemoryListPersistence {
    fn load(&self) -> Result<ListConfig, ListError> {
        Ok(self.data.read().clone())
    }

    fn save(&self, config: &ListConfig) -> Result<(), ListError> {
        *self.data.write() = config.clone();
        Ok(())
    }
}

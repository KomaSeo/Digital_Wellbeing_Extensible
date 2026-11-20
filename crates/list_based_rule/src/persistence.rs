//! Responsibility: Load ListConfig, keeping file I/O out of ListStore.

use crate::{ListConfig, ListError};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// Responsibility: Load a ListConfig from somewhere (a file, or memory in tests).
pub trait ListPersistence: Send + Sync {
    fn load(&self) -> Result<ListConfig, ListError>;
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
}

impl ListPersistence for FileListPersistence {
    fn load(&self) -> Result<ListConfig, ListError> {
        if self.path.exists() {
            let raw = fs::read_to_string(&self.path)?;
            match serde_json::from_str(&raw) {
                Ok(config) => Ok(config),
                Err(error) => {
                    // A damaged file (e.g. empty after a power cut) must not stop
                    // the app or the agent from starting: keep it aside, use defaults.
                    let aside = self.path.with_extension("json.corrupt");
                    tracing::warn!(
                        "{} is unreadable ({error}); moved to {} and starting from defaults",
                        self.path.display(),
                        aside.display()
                    );
                    fs::rename(&self.path, &aside)?;
                    let config = ListConfig::default();
                    self.save(&config)?;
                    Ok(config)
                }
            }
        } else {
            let config = ListConfig::default();
            self.save(&config)?;
            Ok(config)
        }
    }
}

impl FileListPersistence {
    /// Write `config` atomically (a missing or damaged file is replaced by the defaults).
    fn save(&self, config: &ListConfig) -> Result<(), ListError> {
        let raw = serde_json::to_string_pretty(config)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp_path = self.path.with_extension("json.tmp");
        {
            let mut file = fs::File::create(&temp_path)?;
            file.write_all(raw.as_bytes())?;
            // On disk before the rename, so a crash cannot leave an empty file.
            file.sync_all()?;
        }
        fs::rename(&temp_path, &self.path)?;
        Ok(())
    }
}

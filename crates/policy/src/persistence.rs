use crate::{Policy, PolicyError};
use directories::ProjectDirs;
use parking_lot::RwLock;
use std::fs;
use std::path::{Path, PathBuf};

/// Loads and saves policy documents. Keeps filesystem (or other I/O) out of [`crate::PolicyStore`].
pub trait PolicyPersistence: Send + Sync {
    fn load(&self) -> Result<Policy, PolicyError>;
    fn save(&self, policy: &Policy) -> Result<(), PolicyError>;
}

/// Persists policy as JSON on disk (atomic write via temp file + rename).
#[derive(Debug, Clone)]
pub struct FilePolicyPersistence {
    path: PathBuf,
}

impl FilePolicyPersistence {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// App config directory + `policy.json`.
    pub fn default_config_path() -> Result<PathBuf, PolicyError> {
        let dirs = ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
            .ok_or(PolicyError::NoAppData)?;
        Ok(dirs.config_dir().join("policy.json"))
    }

    /// File persistence at [`Self::default_config_path`].
    pub fn default() -> Result<Self, PolicyError> {
        Ok(Self::new(Self::default_config_path()?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl PolicyPersistence for FilePolicyPersistence {
    fn load(&self) -> Result<Policy, PolicyError> {
        if self.path.exists() {
            let raw = fs::read_to_string(&self.path)?;
            Ok(serde_json::from_str(&raw)?)
        } else {
            let policy = Policy::default();
            self.save(&policy)?;
            Ok(policy)
        }
    }

    fn save(&self, policy: &Policy) -> Result<(), PolicyError> {
        let raw = serde_json::to_string_pretty(policy)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, &raw)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// In-memory persistence for unit tests (no filesystem).
#[derive(Debug, Default)]
pub struct MemoryPolicyPersistence {
    data: RwLock<Policy>,
}

impl MemoryPolicyPersistence {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_policy(policy: Policy) -> Self {
        Self {
            data: RwLock::new(policy),
        }
    }

    pub fn snapshot(&self) -> Policy {
        self.data.read().clone()
    }
}

impl PolicyPersistence for MemoryPolicyPersistence {
    fn load(&self) -> Result<Policy, PolicyError> {
        Ok(self.data.read().clone())
    }

    fn save(&self, policy: &Policy) -> Result<(), PolicyError> {
        *self.data.write() = policy.clone();
        Ok(())
    }
}

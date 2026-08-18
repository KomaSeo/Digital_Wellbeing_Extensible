use directories::ProjectDirs;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use thiserror::Error;

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "DigitalWellbeing";
const APPLICATION: &str = "DigitalWellbeing";

#[derive(Debug, Error)]
pub enum PersistError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("app data directory unavailable")]
    NoAppData,
}

/// Load/save a typed config. Rules pick a filename and a `T`; they do not
/// implement filesystem or JSON themselves.
pub trait Persistence<T>: Send + Sync {
    fn load(&self) -> Result<T, PersistError>;
    fn save(&self, value: &T) -> Result<(), PersistError>;
}

pub fn app_config_dir() -> Result<PathBuf, PersistError> {
    let dirs = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or(PersistError::NoAppData)?;
    Ok(dirs.config_dir().to_path_buf())
}

pub fn app_config_file(filename: &str) -> Result<PathBuf, PersistError> {
    Ok(app_config_dir()?.join(filename))
}

/// JSON file with atomic write (temp file + rename).
#[derive(Debug, Clone)]
pub struct FilePersistence<T> {
    path: PathBuf,
    _ty: PhantomData<T>,
}

impl<T> FilePersistence<T> {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            _ty: PhantomData,
        }
    }

    /// `%AppData%\DigitalWellbeing\DigitalWellbeing\config\{filename}`
    pub fn app_file(filename: &str) -> Result<Self, PersistError> {
        Ok(Self::new(app_config_file(filename)?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl<T> Persistence<T> for FilePersistence<T>
where
    T: Serialize + DeserializeOwned + Default + Send + Sync,
{
    fn load(&self) -> Result<T, PersistError> {
        if self.path.exists() {
            let raw = fs::read_to_string(&self.path)?;
            Ok(serde_json::from_str(&raw)?)
        } else {
            let value = T::default();
            self.save(&value)?;
            Ok(value)
        }
    }

    fn save(&self, value: &T) -> Result<(), PersistError> {
        let raw = serde_json::to_string_pretty(value)?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, &raw)?;
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// In-memory backend for tests.
#[derive(Debug)]
pub struct MemoryPersistence<T> {
    data: RwLock<T>,
}

impl<T: Default> MemoryPersistence<T> {
    pub fn new() -> Self {
        Self {
            data: RwLock::new(T::default()),
        }
    }
}

impl<T> MemoryPersistence<T> {
    pub fn with_value(value: T) -> Self {
        Self {
            data: RwLock::new(value),
        }
    }

    pub fn snapshot(&self) -> T
    where
        T: Clone,
    {
        self.data.read().clone()
    }
}

impl<T: Default> Default for MemoryPersistence<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Persistence<T> for MemoryPersistence<T>
where
    T: Clone + Send + Sync,
{
    fn load(&self) -> Result<T, PersistError> {
        Ok(self.data.read().clone())
    }

    fn save(&self, value: &T) -> Result<(), PersistError> {
        *self.data.write() = value.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        name: String,
    }

    #[test]
    fn memory_roundtrip() {
        let persist = MemoryPersistence::<Sample>::new();
        persist
            .save(&Sample {
                name: "ok".into(),
            })
            .unwrap();
        assert_eq!(persist.load().unwrap().name, "ok");
    }
}

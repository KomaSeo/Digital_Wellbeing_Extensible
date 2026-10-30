use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A JSON file mirrored in memory.
///
/// `writable = false` means this process only reads the file (the agent reading
/// the admin's `planner.json`); `update` then changes memory only.
pub struct JsonStore<T> {
    path: Option<PathBuf>,
    writable: bool,
    inner: Arc<RwLock<T>>,
}

impl<T> Clone for JsonStore<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            writable: self.writable,
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T: Clone + Default + Serialize + DeserializeOwned> JsonStore<T> {
    pub fn open(path: PathBuf, writable: bool) -> io::Result<Self> {
        let value = read(&path)?.unwrap_or_default();
        Ok(Self {
            path: Some(path),
            writable,
            inner: Arc::new(RwLock::new(value)),
        })
    }

    pub fn in_memory(value: T) -> Self {
        Self {
            path: None,
            writable: false,
            inner: Arc::new(RwLock::new(value)),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn get(&self) -> T {
        self.inner.read().clone()
    }

    pub fn update<R>(&self, f: impl FnOnce(&mut T) -> R) -> io::Result<R> {
        let (result, snapshot) = {
            let mut value = self.inner.write();
            let result = f(&mut value);
            (result, value.clone())
        };
        if let Some(path) = self.path.as_ref().filter(|_| self.writable) {
            write(path, &snapshot)?;
        }
        Ok(result)
    }

    /// Replace memory with the file's current contents.
    pub fn reload(&self) -> io::Result<()> {
        if let Some(path) = &self.path {
            *self.inner.write() = read(path)?.unwrap_or_default();
        }
        Ok(())
    }
}

fn read<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(io::Error::other)
}

/// Temp file + rename, so a reader never sees a half-written file.
fn write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(value).map_err(io::Error::other)?,
    )?;
    fs::rename(&tmp, path)
}

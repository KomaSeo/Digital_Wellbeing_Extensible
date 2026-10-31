//! Responsibility: Mirror one JSON file in memory for any serde type, with atomic writes and an
//! optional read-only mode.

use parking_lot::{Mutex, RwLock};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::warn;

/// Responsibility: Share one value between threads and keep it in sync with its file.
///
/// `writable = false` means this process only reads the file (the agent reading
/// the admin's `planner.json`); `update` then changes memory only.
pub struct JsonStore<T> {
    path: Option<PathBuf>,
    writable: bool,
    inner: Arc<RwLock<T>>,
    /// Held for a whole update (change, save, commit), so two threads cannot
    /// save out of order. Readers are not blocked while the file is written.
    writer: Arc<Mutex<()>>,
}

impl<T> Clone for JsonStore<T> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            writable: self.writable,
            inner: Arc::clone(&self.inner),
            writer: Arc::clone(&self.writer),
        }
    }
}

impl<T: Clone + Default + Serialize + DeserializeOwned> JsonStore<T> {
    /// Open `path`; a missing file means `T::default()`. An unreadable file is
    /// moved to `<name>.corrupt` and the default is used, so one damaged file
    /// never stops the program from starting.
    pub fn open(path: PathBuf, writable: bool) -> io::Result<Self> {
        let value = match read(&path) {
            Ok(value) => value.unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                let aside = path.with_extension("json.corrupt");
                warn!(
                    "{} is unreadable ({error}); moved to {} and starting empty",
                    path.display(),
                    aside.display()
                );
                if writable {
                    fs::rename(&path, &aside)?;
                }
                T::default()
            }
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: Some(path),
            writable,
            inner: Arc::new(RwLock::new(value)),
            writer: Arc::new(Mutex::new(())),
        })
    }

    pub fn in_memory(value: T) -> Self {
        Self {
            path: None,
            writable: false,
            inner: Arc::new(RwLock::new(value)),
            writer: Arc::new(Mutex::new(())),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn get(&self) -> T {
        self.inner.read().clone()
    }

    /// Apply `change` to a copy, save the copy, then make it current. If saving
    /// fails, memory is unchanged, so memory and file never disagree.
    pub fn update<R>(&self, change: impl FnOnce(&mut T) -> R) -> io::Result<R> {
        let _writer = self.writer.lock();
        let mut next = self.get();
        let result = change(&mut next);
        if let Some(path) = self.path.as_ref().filter(|_| self.writable) {
            write(path, &next)?;
        }
        *self.inner.write() = next;
        Ok(result)
    }

    /// Replace memory with the file's current contents. On error (e.g. the
    /// file is being written in place), memory keeps the last good value.
    pub fn reload(&self) -> io::Result<()> {
        if let Some(path) = &self.path {
            let value = read(path)?.unwrap_or_default();
            *self.inner.write() = value;
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
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Temp file + rename, so a reader never sees a half-written file. The temp
/// name is unique per process and write, so two writers (e.g. agents of two
/// sessions) never rename each other's half-written file into place.
fn write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let temporary_path = path.with_extension(format!(
        "json.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = fs::File::create(&temporary_path)?;
        file.write_all(raw.as_bytes())?;
        // On disk before the rename, so a crash cannot leave an empty file.
        file.sync_all()
    })();
    match written.and_then(|_| fs::rename(&temporary_path, path)) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary_path);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::thread;

    #[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
    struct Counter {
        n: u32,
    }

    fn temp_dir(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("dw_store_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let directory = temp_dir("corrupt");
        let path = directory.join("state.json");
        fs::write(&path, "{ not json").unwrap();
        let store = JsonStore::<Counter>::open(path.clone(), true).unwrap();
        assert_eq!(store.get(), Counter::default());
        assert!(path.with_extension("json.corrupt").exists());
        store.update(|counter| counter.n = 1).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("\"n\": 1"));
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn failed_save_leaves_memory_unchanged() {
        let directory = temp_dir("fail");
        // A directory where the file should be: rename onto it fails.
        let path = directory.join("state.json");
        fs::create_dir_all(path.join("blocker")).unwrap();
        let store = JsonStore::<Counter> {
            path: Some(path),
            writable: true,
            inner: Arc::new(RwLock::new(Counter::default())),
            writer: Arc::new(Mutex::new(())),
        };
        assert!(store.update(|counter| counter.n = 7).is_err());
        assert_eq!(store.get().n, 0);
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn concurrent_writers_never_leave_a_bad_file() {
        let directory = temp_dir("race");
        let path = directory.join("state.json");
        let first_store = JsonStore::<Counter>::open(path.clone(), true).unwrap();
        // A second store on the same file, like a second agent process.
        let second_store = JsonStore::<Counter>::open(path.clone(), true).unwrap();
        let writers: Vec<_> = [first_store, second_store]
            .into_iter()
            .map(|store| {
                thread::spawn(move || {
                    for _ in 0..200 {
                        store.update(|counter| counter.n += 1).unwrap();
                    }
                })
            })
            .collect();
        for _ in 0..500 {
            if let Ok(raw) = fs::read_to_string(&path) {
                serde_json::from_str::<Counter>(&raw).expect("file is always valid JSON");
            }
        }
        for writer in writers {
            writer.join().unwrap();
        }
        fs::remove_dir_all(&directory).unwrap();
    }
}

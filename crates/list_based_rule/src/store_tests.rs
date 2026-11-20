//! Responsibility: Test ListStore loading and reloading its file.

#![cfg(test)]

use super::*;
use parking_lot::RwLock;
use std::sync::Arc;

/// Responsibility: Hand ListStore a config from memory.
#[derive(Default)]
struct MemoryListPersistence {
    data: RwLock<ListConfig>,
}

impl ListPersistence for MemoryListPersistence {
    fn load(&self) -> Result<ListConfig, ListError> {
        Ok(self.data.read().clone())
    }
}

#[test]
fn loaded_and_reloaded_files_are_normalized() {
    let persistence = Arc::new(MemoryListPersistence::default());
    let store = ListStore::with_persistence(persistence.clone()).unwrap();
    assert_eq!(store.get().mode, ListMode::Blacklist);
    {
        let mut config = persistence.data.write();
        config.mode = ListMode::Whitelist;
        config.whitelist.insert("Code.exe".into());
        config.blacklist.insert("steam".into());
        config.blacklist.insert("System".into());
        config.blacklist.insert("   ".into());
    }
    store.reload().unwrap();
    let loaded = store.get();
    assert!(loaded.whitelist.contains("code.exe"));
    assert!(!loaded.should_block("Code.exe"));
    assert!(loaded.blacklist.contains("steam.exe"));
    assert!(!loaded.blacklist.contains("system.exe"));
    assert!(!loaded.blacklist.contains(""));
}

#[test]
fn corrupt_file_is_moved_aside_and_defaults_are_used() {
    let temp_directory = std::env::temp_dir().join(format!("dw_list_{}", std::process::id()));
    std::fs::create_dir_all(&temp_directory).unwrap();
    let path = temp_directory.join("policy.json");
    std::fs::write(&path, "").unwrap();
    let store =
        ListStore::with_persistence(Arc::new(FileListPersistence::new(path.clone()))).unwrap();
    assert_eq!(store.get().mode, ListMode::Blacklist);
    assert!(path.with_extension("json.corrupt").exists());
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("blacklist"));
    std::fs::remove_dir_all(&temp_directory).unwrap();
}

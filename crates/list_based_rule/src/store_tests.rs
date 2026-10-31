//! Responsibility: Test ListStore with in-memory persistence.

#![cfg(test)]

use super::*;
use std::sync::Arc;

fn memory_store() -> ListStore {
    ListStore::with_persistence(Arc::new(MemoryListPersistence::new()))
        .expect("memory persistence load cannot fail")
}

#[test]
fn set_lists_replace_entirely() {
    let store = memory_store();

    store
        .set_blacklist(&["Steam".into(), "discord.EXE".into(), "explorer.exe".into()])
        .unwrap();
    let config = store.get();
    assert!(config.blacklist.contains("steam.exe"));
    assert!(config.blacklist.contains("discord.exe"));
    assert!(!config.blacklist.contains("explorer.exe"));

    store
        .set_whitelist(&["code.exe".into(), "notepad.exe".into()])
        .unwrap();
    let config = store.get();
    assert!(config.whitelist.contains("code.exe"));
    assert!(config.whitelist.contains("notepad.exe"));
    assert!(config.whitelist.contains("csrss.exe"));
}

#[test]
fn mutations_roundtrip_through_memory_persistence() {
    let persistence = Arc::new(MemoryListPersistence::new());
    let store =
        ListStore::with_persistence(Arc::clone(&persistence) as Arc<dyn ListPersistence>).unwrap();

    store.add_blacklist("steam.exe").unwrap();
    store.set_mode(ListMode::Whitelist).unwrap();

    let reloaded = ListStore::with_persistence(persistence).unwrap();
    let config = reloaded.get();
    assert!(config.blacklist.contains("steam.exe"));
    assert_eq!(config.mode, ListMode::Whitelist);
}

#[test]
fn protected_names_rejected_on_add_blacklist() {
    let store = memory_store();
    let error = store.add_blacklist("explorer.exe").unwrap_err();
    assert!(matches!(error, ListError::Protected(_)));
}

#[test]
fn replace_config_and_loaded_files_are_normalized() {
    let store = memory_store();
    let mut config = ListConfig {
        mode: ListMode::Whitelist,
        ..ListConfig::default()
    };
    config.whitelist.insert("Code.exe".into());
    config.blacklist.insert("steam".into());
    config.blacklist.insert("System".into());
    config.blacklist.insert("   ".into());
    let saved = store.replace_config(config).unwrap();
    assert!(saved.whitelist.contains("code.exe"));
    assert!(!saved.should_block("Code.exe"));
    assert!(saved.blacklist.contains("steam.exe"));
    assert!(!saved.blacklist.contains("system.exe"));
    assert!(!saved.blacklist.contains(""));
    // The UI's remove button (which normalizes) now works.
    let after = store.remove_whitelist("Code.exe").unwrap();
    assert!(!after.whitelist.contains("code.exe"));
}

#[test]
fn empty_strategy_name_is_rejected() {
    let store = memory_store();
    assert!(matches!(
        store.save_strategy("   "),
        Err(ListError::EmptyStrategyName)
    ));
}

/// Fails every save, to check that memory is left unchanged.
struct BrokenDisk(MemoryListPersistence);

impl ListPersistence for BrokenDisk {
    fn load(&self) -> Result<ListConfig, ListError> {
        self.0.load()
    }
    fn save(&self, _config: &ListConfig) -> Result<(), ListError> {
        Err(ListError::Io(std::io::Error::other("disk full")))
    }
}

#[test]
fn failed_save_leaves_memory_unchanged() {
    let store =
        ListStore::with_persistence(Arc::new(BrokenDisk(MemoryListPersistence::new()))).unwrap();
    assert!(store.add_blacklist("steam.exe").is_err());
    assert!(!store.get().blacklist.contains("steam.exe"));
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

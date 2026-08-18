#![cfg(test)]

use super::*;
use persistence::{MemoryPersistence, Persistence};
use std::sync::Arc;

fn memory_store() -> ListStore {
    ListStore::with_persistence(Arc::new(MemoryPersistence::<ListConfig>::new()))
        .expect("memory persistence load cannot fail")
}

#[test]
fn set_lists_replace_entirely() {
    let store = memory_store();

    store
        .set_blacklist(&["Steam".into(), "discord.EXE".into(), "explorer.exe".into()])
        .unwrap();
    let p = store.get();
    assert!(p.blacklist.contains("steam.exe"));
    assert!(p.blacklist.contains("discord.exe"));
    assert!(!p.blacklist.contains("explorer.exe"));

    store
        .set_whitelist(&["code.exe".into(), "notepad.exe".into()])
        .unwrap();
    let p = store.get();
    assert!(p.whitelist.contains("code.exe"));
    assert!(p.whitelist.contains("notepad.exe"));
    assert!(p.whitelist.contains("csrss.exe"));
}

#[test]
fn mutations_roundtrip_through_memory_persistence() {
    let persistence = Arc::new(MemoryPersistence::<ListConfig>::new());
    let store = ListStore::with_persistence(
        Arc::clone(&persistence) as Arc<dyn Persistence<ListConfig>>,
    )
    .unwrap();

    store.add_blacklist("steam.exe").unwrap();
    store.set_mode(ListMode::Whitelist).unwrap();

    let reloaded = ListStore::with_persistence(persistence).unwrap();
    let p = reloaded.get();
    assert!(p.blacklist.contains("steam.exe"));
    assert_eq!(p.mode, ListMode::Whitelist);
}

#[test]
fn protected_names_rejected_on_add_blacklist() {
    let store = memory_store();
    let err = store.add_blacklist("explorer.exe").unwrap_err();
    assert!(matches!(err, ListError::Protected(_)));
}

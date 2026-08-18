use super::*;
use persistence::{MemoryPersistence, Persistence};
use std::sync::Arc;

#[test]
fn blacklist_roundtrip() {
    let persistence = Arc::new(MemoryPersistence::<WordBasedConfig>::new());
    let store = WordBasedStore::with_persistence(
        Arc::clone(&persistence) as Arc<dyn Persistence<WordBasedConfig>>,
    )
    .unwrap();
    store.add_word("  Spoilers  ").unwrap();
    store.add_word("nude").unwrap();
    assert!(store.add_word("   ").is_err());
    store.remove_word("nude").unwrap();
    let reloaded = WordBasedStore::with_persistence(persistence).unwrap();
    let cfg = reloaded.get();
    assert!(cfg.blacklist.contains("spoilers"));
    assert!(!cfg.blacklist.contains("nude"));
}

#[test]
fn set_blacklist_replaces_and_skips_empty() {
    let store = WordBasedStore::with_persistence(Arc::new(
        MemoryPersistence::<WordBasedConfig>::new(),
    ))
    .unwrap();
    store
        .set_blacklist(&["Gore".into(), "  ".into(), "폭력".into()])
        .unwrap();
    let cfg = store.get();
    assert_eq!(cfg.blacklist.len(), 2);
    assert!(cfg.blacklist.contains("gore"));
    assert!(cfg.blacklist.contains("폭력"));
}

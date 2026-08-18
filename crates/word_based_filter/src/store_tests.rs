use super::*;
use persistence::{MemoryPersistence, Persistence};
use std::sync::Arc;

#[test]
fn properties_persist() {
    let persistence = Arc::new(MemoryPersistence::<WordBasedConfig>::new());
    let store = WordBasedStore::with_persistence(
        Arc::clone(&persistence) as Arc<dyn Persistence<WordBasedConfig>>,
    )
    .unwrap();
    store.set_block_violence(false).unwrap();
    store.set_block_sexual(true).unwrap();
    let reloaded = WordBasedStore::with_persistence(persistence).unwrap();
    let cfg = reloaded.get();
    assert!(!cfg.block_violence);
    assert!(cfg.block_sexual);
}

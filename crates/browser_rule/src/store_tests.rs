use super::*;
use persistence::{MemoryPersistence, Persistence};
use std::sync::Arc;

fn memory_store() -> BrowserStore {
    BrowserStore::with_persistence(Arc::new(MemoryPersistence::<BrowserConfig>::new()))
        .expect("memory persistence load cannot fail")
}

#[test]
fn normalize_host_strips_scheme_www_and_path() {
    assert_eq!(
        BrowserConfig::normalize_host("https://www.YouTube.com/watch?v=1").unwrap(),
        "youtube.com"
    );
    assert_eq!(
        BrowserConfig::normalize_host("*.twitter.com").unwrap(),
        "twitter.com"
    );
}

#[test]
fn normalize_host_rejects_garbage() {
    assert!(BrowserConfig::normalize_host("").is_err());
    assert!(BrowserConfig::normalize_host("not a host").is_err());
    assert!(BrowserConfig::normalize_host("localhost").is_err());
}

#[test]
fn default_browsers_include_chrome_brave_safari() {
    let config = BrowserConfig::default();
    assert!(config.is_guarded("chrome.exe"));
    assert!(config.is_guarded("Brave.EXE"));
    assert!(config.is_guarded("safari"));
    assert!(!config.is_guarded("notepad.exe"));
}

#[test]
fn blacklist_roundtrip_memory() {
    let persistence = Arc::new(MemoryPersistence::<BrowserConfig>::new());
    let store = BrowserStore::with_persistence(
        Arc::clone(&persistence) as Arc<dyn Persistence<BrowserConfig>>,
    )
    .unwrap();
    store.add_blacklist("https://www.youtube.com/foo").unwrap();
    let reloaded = BrowserStore::with_persistence(persistence).unwrap();
    assert!(reloaded.get().blacklist.contains("youtube.com"));
}

#[test]
fn set_blacklist_skips_invalid() {
    let store = memory_store();
    store
        .set_blacklist(&["youtube.com".into(), "nope".into(), "".into()])
        .unwrap();
    let cfg = store.get();
    assert!(cfg.blacklist.contains("youtube.com"));
    assert_eq!(cfg.blacklist.len(), 1);
}

#[test]
fn browser_id_from_process() {
    assert_eq!(
        BrowserConfig::browser_id_from_process("Chrome.EXE"),
        "chrome"
    );
}

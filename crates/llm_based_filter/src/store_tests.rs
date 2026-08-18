use super::*;
use persistence::{MemoryPersistence, Persistence};
use std::sync::Arc;

#[test]
fn properties_persist() {
    let persistence = Arc::new(MemoryPersistence::<LlmBasedConfig>::new());
    let store = LlmBasedStore::with_persistence(
        Arc::clone(&persistence) as Arc<dyn Persistence<LlmBasedConfig>>,
    )
    .unwrap();
    store.set_api_key("  sk-test  ").unwrap();
    store.set_judge_text(false).unwrap();
    store.set_judge_image(true).unwrap();
    store.set_prompt_enabled(true).unwrap();
    store.set_prompt("block spoilers").unwrap();
    let reloaded = LlmBasedStore::with_persistence(persistence).unwrap();
    let cfg = reloaded.get();
    assert_eq!(cfg.api_key, "sk-test");
    assert!(!cfg.judge_text);
    assert!(cfg.judge_image);
    assert!(cfg.prompt_enabled);
    assert_eq!(cfg.prompt, "block spoilers");
    assert_eq!(cfg.effective_prompt(), "block spoilers");
}

#[test]
fn default_prompt_when_toggle_off() {
    let mut cfg = LlmBasedConfig::default();
    cfg.prompt = "custom".into();
    cfg.prompt_enabled = false;
    assert_eq!(cfg.effective_prompt(), DEFAULT_PROMPT);
}

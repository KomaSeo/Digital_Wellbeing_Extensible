use crate::{ListMode, ListStore};
use enforcer::{BlockDecision, BlockRule, ProcessSnapshot};
use std::sync::Arc;

/// Blacklist / whitelist [`BlockRule`], backed by [`ListStore`].
pub struct ListBasedRule {
    store: ListStore,
}

impl ListBasedRule {
    pub fn new(store: ListStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &ListStore {
        &self.store
    }
}

impl BlockRule for ListBasedRule {
    fn id(&self) -> &str {
        "list_based_rule"
    }

    fn name(&self) -> &str {
        "List-based rule"
    }

    fn description(&self) -> &str {
        "Blocks processes according to the configured blacklist or whitelist"
    }

    fn evaluate(&self, process: &ProcessSnapshot) -> BlockDecision {
        let config = self.store.get();
        if config.should_block(&process.name) {
            let reason = match config.mode {
                ListMode::Blacklist => "blacklist",
                ListMode::Whitelist => "whitelist",
            };
            BlockDecision::block(reason)
        } else {
            BlockDecision::Allow
        }
    }
}

/// Shared handle so the app can register the rule and still reach [`ListStore`].
#[derive(Clone)]
pub struct ListBasedRuleHandle {
    inner: Arc<ListBasedRule>,
}

impl ListBasedRuleHandle {
    pub fn new(store: ListStore) -> Self {
        Self {
            inner: Arc::new(ListBasedRule::new(store)),
        }
    }

    pub fn rule(&self) -> Arc<dyn BlockRule> {
        Arc::clone(&self.inner) as Arc<dyn BlockRule>
    }

    pub fn store(&self) -> &ListStore {
        self.inner.store()
    }
}

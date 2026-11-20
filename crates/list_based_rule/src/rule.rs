//! Responsibility: Turn ListStore into a BlockRule.

use crate::{ListMode, ListStore};
use enforcer::{BlockEvent, BlockRule, ProcessSnapshot};
use std::sync::Arc;

/// Responsibility: Block processes by the configured blacklist or whitelist.
///
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

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        let config = self.store.get();
        let reason = match config.mode {
            ListMode::Blacklist => "blacklist",
            ListMode::Whitelist => "whitelist",
        };
        processes
            .iter()
            .filter(|process| config.should_block(&process.name))
            .map(|process| BlockEvent::new(process, reason))
            .collect()
    }

    /// A whitelisted program is never closed as another program's helper.
    fn keeps(&self, process: &ProcessSnapshot) -> bool {
        let config = self.store.get();
        config.mode == ListMode::Whitelist && !config.should_block(&process.name)
    }
}

/// Responsibility: Hand out a shared ListBasedRule as a block rule, with its store.
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

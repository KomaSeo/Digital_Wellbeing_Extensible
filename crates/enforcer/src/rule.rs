use policy::PolicyStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Snapshot of a running process passed to every [`BlockRule`].
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
}

/// Verdict from a single rule. Enforcer blocks if **any** registered rule returns [`Block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockDecision {
    Allow,
    Block { reason: String },
}

impl BlockDecision {
    pub fn block(reason: impl Into<String>) -> Self {
        Self::Block {
            reason: reason.into(),
        }
    }
}

/// Pluggable decision maker. Register instances on [`crate::Enforcer`].
///
/// Example: a browser-extension guard can return [`Allow`] for up to 60s after
/// heartbeat loss, then [`Block`], without touching blacklist/whitelist lists.
pub trait BlockRule: Send + Sync {
    fn id(&self) -> &str;

    fn evaluate(&self, process: &ProcessSnapshot) -> BlockDecision;
}

/// Classic blacklist / whitelist logic, backed by [`PolicyStore`].
pub struct ListPolicyRule {
    store: PolicyStore,
}

impl ListPolicyRule {
    pub fn new(store: PolicyStore) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &PolicyStore {
        &self.store
    }
}

impl BlockRule for ListPolicyRule {
    fn id(&self) -> &str {
        "list_policy"
    }

    fn evaluate(&self, process: &ProcessSnapshot) -> BlockDecision {
        let policy = self.store.get();
        if policy.should_block(&process.name) {
            BlockDecision::block(format!("list_policy:{:?}", policy.mode))
        } else {
            BlockDecision::Allow
        }
    }
}

/// Shared handle used when the app still needs [`PolicyStore`] for the UI / tool-api.
#[derive(Clone)]
pub struct ListPolicyHandle {
    inner: Arc<ListPolicyRule>,
}

impl ListPolicyHandle {
    pub fn new(store: PolicyStore) -> Self {
        Self {
            inner: Arc::new(ListPolicyRule::new(store)),
        }
    }

    pub fn rule(&self) -> Arc<dyn BlockRule> {
        Arc::clone(&self.inner) as Arc<dyn BlockRule>
    }

    pub fn store(&self) -> &PolicyStore {
        self.inner.store()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleInfo {
    pub id: String,
}

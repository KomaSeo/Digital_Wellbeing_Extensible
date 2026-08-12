use serde::{Deserialize, Serialize};

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
/// The enforcer crate has no knowledge of any particular store or policy type —
/// callers supply rules (e.g. list-based, browser-extension grace) from the outside.
pub trait BlockRule: Send + Sync {
    /// Stable machine id (used for register/replace/unregister).
    fn id(&self) -> &str;

    /// Short human-readable label for UI / API.
    fn name(&self) -> &str;

    /// Longer explanation of what this rule does.
    fn description(&self) -> &str;

    fn evaluate(&self, process: &ProcessSnapshot) -> BlockDecision;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

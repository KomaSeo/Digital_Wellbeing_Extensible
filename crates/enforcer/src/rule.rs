//! Responsibility: Define the BlockRule interface and the data passed between rules and the
//! enforcer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Responsibility: Describe one running process to a rule.
///
/// Snapshot of a running process passed to every [`BlockRule`].
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
}

/// Responsibility: Record one process a rule wants terminated, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockEvent {
    pub process_name: String,
    pub pid: u32,
    pub reason: String,
    pub at: DateTime<Utc>,
}

impl BlockEvent {
    pub fn new(process: &ProcessSnapshot, reason: impl Into<String>) -> Self {
        Self {
            process_name: process.name.clone(),
            pid: process.pid,
            reason: reason.into(),
            at: Utc::now(),
        }
    }
}

/// Responsibility: Decide which processes to block (one implementation per kind of rule).
///
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

    /// Inspect the full process snapshot list and return every process this rule
    /// wants blocked. Batching lets rules amortize shared work (lookups, heartbeats, …).
    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent>;
}

/// Responsibility: Describe a registered rule for listings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

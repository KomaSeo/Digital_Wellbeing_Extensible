//! Responsibility: Expose the AI focus rule's public API, and assemble a monitor wired to the
//! real desktop and Claude.
//!
//! AI-judged focus rule.
//!
//! While a focus session runs, [`FocusMonitor`] samples the foreground window
//! every few seconds into an activity history. On each check interval it sends
//! the declared task, that history and (optionally) a screenshot to a [`Judge`]
//! (Claude by default). Apps judged off-task enough times in a row are reported
//! to the enforcer through [`AiFocusRule`]; browser tabs are closed instead.
//!
//! Walkthrough of every module (Korean): `docs/ai_focus_rule.md`.

pub mod access_review;
pub mod claude;
mod config;
pub mod connection;
pub mod grants;
mod judge;
mod monitor;
pub mod platform;
mod rule;
pub mod store;
pub mod usage;
mod youtube;

pub use config::{lockdown_allows_page, AiFocusConfig, AiFocusStore, ConfigError, DEFAULT_MODEL};
pub use judge::{ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, Verdict};
pub use monitor::{
    claude_judge_provider, AccessError, ActivityEntry, BlockedEntry, FocusMonitor, JudgeProvider,
    MonitorState, SavedBlocks, VerdictLogEntry,
};
pub use rule::{AiFocusRule, AiFocusRuleHandle};

use grants::GrantState;
use std::sync::Arc;
use store::JsonStore;

/// Monitor wired to the native desktop and the Claude judge and reviewer,
/// keeping access grants in `grants`. With `usage`, the recorder is also
/// installed for every Claude call of this process (see `usage.rs`).
pub fn native_monitor(
    store: AiFocusStore,
    grants: JsonStore<GrantState>,
    usage: Option<Arc<usage::UsageRecorder>>,
) -> Arc<FocusMonitor> {
    let mut monitor =
        FocusMonitor::new(store, platform::native(), claude_judge_provider()).with_grants(grants);
    if let Some(usage) = usage {
        usage::install(Arc::clone(&usage));
        monitor = monitor.with_usage(usage);
    }
    Arc::new(monitor)
}

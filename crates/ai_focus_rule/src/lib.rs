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
pub mod grants;
mod judge;
mod monitor;
pub mod platform;
mod rule;
pub mod store;

pub use config::{AiFocusConfig, AiFocusStore, ConfigError, DEFAULT_MODEL};
pub use judge::{ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, Verdict};
pub use monitor::{
    claude_judge_provider, AccessError, ActivityEntry, BlockedEntry, FocusMonitor, JudgeProvider,
    MonitorState, VerdictLogEntry,
};
pub use rule::{AiFocusRule, AiFocusRuleHandle};

use std::sync::Arc;

/// Monitor wired to the native desktop and the Claude judge.
pub fn native_monitor(store: AiFocusStore) -> Arc<FocusMonitor> {
    Arc::new(FocusMonitor::new(
        store,
        platform::native(),
        claude_judge_provider(),
    ))
}

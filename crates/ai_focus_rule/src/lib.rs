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
pub mod comparison;
mod config;
pub mod connection;
pub mod grants;
mod judge;
mod monitor;
pub mod platform;
mod rule;
pub mod search;
pub mod store;
pub mod usage;
mod youtube;

pub use config::{
    lockdown_allows_page, AiFocusConfig, AiFocusStore, ConfigError, BUILT_IN_ALWAYS_ALLOW,
    DEFAULT_MODEL, DEFAULT_RECHECK_MODEL, EFFORT_LEVELS,
};
pub use judge::{ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, Verdict};
pub use monitor::{
    claude_judge_provider, strip_browser_noise, AccessError, ActivityEntry, BlockSet, BlockedEntry,
    FocusMonitor, JudgeProvider, MonitorState, NewBlock, SavedBlocks, VerdictLogEntry,
};
pub use rule::{AiFocusRule, AiFocusRuleHandle};

//! Responsibility: Expose the AI focus rule's public API.
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
mod config;
pub mod grants;
mod judge;
mod monitor;
pub mod platform;
mod rule;

// The Claude client and the JSON file store live in their own crates (`ai_client`,
// `json_store`); these names keep the paths inside this crate short.
pub(crate) use ai_client::{claude, comparison, connection, usage};
pub(crate) use json_store as store;

pub use config::{
    AiFocusConfig, AiFocusStore, ConfigError, BUILT_IN_ALWAYS_ALLOW, DEFAULT_MODEL,
    DEFAULT_RECHECK_MODEL, EFFORT_LEVELS,
};
pub use judge::{ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, Verdict};
pub use monitor::{
    claude_judge_provider, AccessError, ActivityEntry, BlockSet, BlockedEntry, FocusMonitor,
    JudgeProvider, MonitorState, NewBlock, SavedBlocks, VerdictLogEntry,
};
pub use rule::{AiFocusRule, AiFocusRuleHandle};

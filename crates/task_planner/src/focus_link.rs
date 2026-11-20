//! Responsibility: Say what the planner needs from the AI focus check, without knowing it.
//!
//! The agent implements [`FocusLink`] over the AI focus monitor (`machine::focus_link`); the
//! planner's tests use a fake. The planner never sees the focus check's settings or state.

use ai_client::AiSettings;

/// Responsibility: Name the task the focus check watches and the blocks that belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedTask {
    /// The sentence the focus check judges windows against.
    pub task: String,
    /// Whose blocks apply: the to-do's id, or "free" (see `ai_focus_rule::SavedBlocks`).
    pub scope: String,
}

/// Responsibility: Connect the planner to the AI focus check.
pub trait FocusLink: Send + Sync {
    /// The Claude key, model and decision effort the planner's questions use (the AI focus
    /// settings, so one key serves the whole program).
    fn ai_settings(&self) -> AiSettings;

    /// The recent activity log (which windows were in front), sent with reviews as evidence.
    fn recent_activity(&self) -> String;

    /// The task the focus check watches now, or `None` when it is off.
    fn watched(&self) -> Option<WatchedTask>;

    /// Watch `task` (start the focus check), or stop it with `None`.
    fn watch(&self, task: Option<WatchedTask>) -> Result<(), String>;
}

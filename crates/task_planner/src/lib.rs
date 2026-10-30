//! Responsibility: Expose the task planner's public API.
//!
//! Task planner: the AI picks what to work on from an admin-maintained to-do
//! list, and changing that task needs a reason the AI approves.
//!
//! - `planner.json` ([`PlannerConfig`]): the list; edited by the admin only.
//! - `planner_state.json` ([`PlannerState`]): current assignment, finished
//!   to-dos, change-request log; written by the agent.
//!
//! When the planner is active, the agent refuses free-text tasks and plain
//! "stop" from the Focus app; everything goes through [`TaskPlanner`].
//! Walkthrough (Korean): `docs/task_planner.md`.

mod admin_ui;
mod advisor;
mod model;
mod planner;

pub use admin_ui::PlannerAdminUi;
pub use advisor::{
    claude_advisor_provider, Advisor, AdvisorProvider, ClaudeAdvisor, Pick, PickRequest, Review,
    ReviewRequest,
};
pub use model::{
    Assignment, AssignmentSource, ChangeKind, ChangeRecord, DoneRecord, PlannerConfig,
    PlannerState, PlannerStatus, TodoItem,
};
pub use planner::{PlannerError, TaskPlanner};
pub use ai_focus_rule::store::JsonStore;

#[cfg(test)]
mod tests;

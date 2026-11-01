//! Responsibility: Expose the task planner's public API.
//!
//! Task planner: the AI picks what to work on from an admin-maintained to-do
//! list, and changing that task needs a reason the AI approves.
//!
//! - `planner.json` ([`PlannerConfig`]): the list; edited by the admin only.
//! - `planner_state.json` ([`PlannerState`]): current assignment (with the time the AI gave
//!   it; when that is up the AI picks again, unless the user got "notify only" approved), finished
//!   to-dos, change-request log, to-dos the user proposed and the AI approved;
//!   written by the agent.
//!
//! When the planner is active, the agent refuses free-text tasks and plain
//! "stop" from the Focus app; everything goes through [`TaskPlanner`].
//! Walkthrough (Korean): `docs/task_planner.md`.

mod admin_ui;
mod advisor;
mod deadline;
mod model;
mod planner;

pub use admin_ui::PlannerAdminUi;
pub use advisor::{
    change_summary, claude_advisor_provider, Advisor, AdvisorProvider, AppointmentRequest,
    AppointmentReview, ClaudeAdvisor, Pick, PickRequest, Review, ReviewRequest, ReviewedTodo,
    TodoChangeRequest, TodoReview, WeekPlanAnswer, WeekPlanAnswerDay, WeekPlanAnswerTodo,
    WeekPlanRequest, APPOINTMENT_PROMPT, PICK_PROMPT, PROPOSAL_PROMPT, REVIEW_PROMPT,
    WEEK_PLAN_PROMPT,
};
pub use ai_focus_rule::store::JsonStore;
pub use deadline::parse_deadline;
pub use model::{
    Appointment, AppointmentInput, Assignment, AssignmentSource, AssignmentSpan, ChangeKind,
    ChangeRecord, DoneRecord, Evidence, PlannerConfig, PlannerInstructions, PlannerState,
    PlannerStatus, PolishItem, ProposalInput, ProposalRecord, TaskNotes, TaskStep, TodoChange,
    TodoChangeRecord, TodoItem, TodoListWrite, WeekPlan, WeekPlanDay, WeekPlanTodo, WhenTimeIsUp,
};
pub use planner::{PlannerError, TaskPlanner};

#[cfg(test)]
mod tests;

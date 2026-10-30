//! Responsibility: Define the planner's data: the admin's to-do list, the current assignment,
//! change requests and finished to-dos.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, VecDeque};

/// Responsibility: Describe one to-do on the admin's list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    /// Stable id; generated when the admin adds the item.
    pub id: String,
    pub title: String,
    /// Free-form details: what "done" means, which tools/sites are needed.
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub deadline: Option<NaiveDate>,
    /// 1 (low) ..= 5 (urgent).
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub estimate_minutes: Option<u32>,
    /// Proposed by the user (from the Focus app or their AI assistant) and
    /// approved by the AI, rather than written by the admin.
    #[serde(default)]
    pub proposed: bool,
}

fn default_priority() -> u8 {
    3
}

impl TodoItem {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            id: format!("t{}", Utc::now().timestamp_millis()),
            title: title.into(),
            notes: String::new(),
            deadline: None,
            priority: default_priority(),
            estimate_minutes: None,
            proposed: false,
        }
    }
}

/// Responsibility: Hold the admin's planner settings and to-do list (planner.json).
///
/// Admin settings (`planner.json`). Standard users cannot edit this file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerConfig {
    /// When on and open to-dos exist, the user cannot type a task freely:
    /// the planner assigns one, and changing it needs an approved reason.
    pub enabled: bool,
    pub todos: Vec<TodoItem>,
    /// The owner's standing rules for how time is spent ("no leisure before
    /// 18:00 on weekdays", "a 30-minute break after two hours of work"). Every
    /// AI decision of the planner follows them. Only the admin can edit them,
    /// so a request made in a weak moment cannot change them.
    pub life_rules: String,
    /// Ids of proposed to-dos the admin removed. Proposals live in the
    /// agent's `planner_state.json`; the admin removes one by naming it here,
    /// so neither program writes the other's file.
    pub dismissed: BTreeSet<String>,
}

/// Responsibility: Say who chose the current task: the AI, or an approved change request.
///
/// Why the current task is what it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentSource {
    /// The AI picked it from the list.
    Planner,
    /// The user asked for it and the AI approved the reason.
    ApprovedChange,
}

/// Responsibility: Record the task currently given to the user, and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assignment {
    /// The to-do this belongs to; `None` for an approved free-text task.
    pub todo_id: Option<String>,
    /// What the focus checker judges against: concrete, names the tools needed.
    pub task: String,
    /// The AI's one-line explanation, shown to the user.
    pub why: String,
    pub source: AssignmentSource,
    pub at: DateTime<Utc>,
}

/// Responsibility: Name what a change request asks for.
///
/// What the user is asking for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeKind {
    /// The current to-do is finished.
    Done,
    /// Work on another to-do from the list.
    SwitchTodo { todo_id: String },
    /// Work on something that is not on the list.
    SwitchFree { task: String },
    /// Stop the focus session altogether.
    End,
}

/// Responsibility: Record one change request and the AI's decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeRecord {
    pub at: DateTime<Utc>,
    pub request: ChangeKind,
    /// Assigned task at the time of the request.
    pub from_task: String,
    pub reason: String,
    pub approved: bool,
    /// The AI's answer to the user.
    pub reply: String,
}

/// Responsibility: Carry a to-do the user proposes, as it arrives over the agent API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProposalInput {
    pub title: String,
    pub notes: String,
    pub deadline: Option<NaiveDate>,
    /// 1 (low) ..= 5 (urgent); 3 when missing.
    pub priority: Option<u8>,
    pub estimate_minutes: Option<u32>,
}

/// Responsibility: Record one proposed to-do and the AI's decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRecord {
    pub at: DateTime<Utc>,
    pub item: TodoItem,
    /// Who sent it: "focus app", "claude desktop", ...
    pub via: String,
    pub approved: bool,
    /// The AI's answer to the user.
    pub reply: String,
}

/// Responsibility: Record that a to-do was finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneRecord {
    pub todo_id: String,
    pub title: String,
    pub at: DateTime<Utc>,
}

/// Responsibility: Hold the planner's runtime record (planner_state.json): assignment, finished
/// to-dos, request log.
///
/// Runtime state (`planner_state.json`), written by the agent only.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerState {
    pub assignment: Option<Assignment>,
    pub done: Vec<DoneRecord>,
    /// Newest last, capped.
    pub requests: VecDeque<ChangeRecord>,
    /// Proposed to-dos the AI approved; they join the admin's list (see
    /// `TaskPlanner::open_todos`).
    pub proposed: Vec<TodoItem>,
    /// Every proposal and the AI's decision, newest last, capped.
    pub proposals: VecDeque<ProposalRecord>,
}

impl PlannerState {
    pub fn is_done(&self, todo_id: &str) -> bool {
        self.done.iter().any(|d| d.todo_id == todo_id)
    }
}

/// Responsibility: Carry what the Focus app shows about the planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannerStatus {
    /// Planner is enabled and has open to-dos: free-text tasks are refused.
    pub active: bool,
    pub assignment: Option<Assignment>,
    pub open_todos: Vec<TodoItem>,
    /// Newest first.
    pub recent_requests: Vec<ChangeRecord>,
    /// Newest first.
    #[serde(default)]
    pub recent_proposals: Vec<ProposalRecord>,
}

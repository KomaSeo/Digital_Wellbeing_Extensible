//! Responsibility: Define the planner's data: the admin's to-do list, the current assignment,
//! change requests and finished to-dos.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
/// Part of the settings (`planner.json`): edited in the Focus app's Settings tab (while the
/// settings are protected, an edit takes effect only after three confirmations two days
/// apart), never by a planner request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerConfig {
    /// When on and open to-dos exist, the user cannot type a task freely:
    /// the planner assigns one, and changing it needs an approved reason.
    pub enabled: bool,
    pub todos: Vec<TodoItem>,
    /// The owner's standing rules for how time is spent ("no leisure before
    /// 18:00 on weekdays", "a 30-minute break after two hours of work"). Every
    /// AI decision of the planner follows them. They change only in the settings
    /// (lockable), so a request made in a weak moment cannot change them.
    pub life_rules: String,
    /// Ids of proposed to-dos the admin removed. Proposals live in the
    /// agent's `planner_state.json`; the admin removes one by naming it here,
    /// so neither program writes the other's file.
    pub dismissed: BTreeSet<String>,
    /// The instructions the planner's AI follows; empty texts mean the built-in ones.
    pub instructions: PlannerInstructions,
}

/// Responsibility: Hold the owner's own versions of the planner's three AI instructions.
///
/// Like every setting they change only through the Settings tab (or a scheduled update from
/// Claude Desktop) and, once the settings are protected, only after three confirmations.
/// An empty text means the built-in instruction ([`crate::advisor::PICK_PROMPT`] and so on).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerInstructions {
    /// Which to-do to work on now, and for how long.
    pub pick: String,
    /// Whether a change request (done, switch, end, notify only) is approved.
    pub change_review: String,
    /// Whether a to-do change (add, edit, remove) is approved; fills in a new to-do.
    pub proposal_review: String,
    /// The suggested order for the rest of the day.
    pub day_plan: String,
}

impl PlannerInstructions {
    pub fn pick_or_default(&self) -> &str {
        or_default(&self.pick, crate::advisor::PICK_PROMPT)
    }

    pub fn change_review_or_default(&self) -> &str {
        or_default(&self.change_review, crate::advisor::REVIEW_PROMPT)
    }

    pub fn proposal_review_or_default(&self) -> &str {
        or_default(&self.proposal_review, crate::advisor::PROPOSAL_PROMPT)
    }

    pub fn day_plan_or_default(&self) -> &str {
        or_default(&self.day_plan, crate::advisor::DAY_PLAN_PROMPT)
    }
}

fn or_default<'a>(own: &'a str, built_in: &'a str) -> &'a str {
    if own.trim().is_empty() {
        built_in
    } else {
        own
    }
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

/// Responsibility: Say what happens when an assignment's time is up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhenTimeIsUp {
    /// The AI picks the next task (it may pick the same to-do again).
    #[default]
    Reassign,
    /// The task stays; the Focus app only says the time is up. Set by an approved request.
    NotifyOnly,
}

/// Responsibility: Record the task currently given to the user, until when, and why.
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
    /// When the time the AI gave this task is up; `None` for assignments made before
    /// assignments had an end.
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub when_time_is_up: WhenTimeIsUp,
}

impl Assignment {
    /// The time is up and a new task should be picked now.
    pub fn due_for_reassignment(&self, now: DateTime<Utc>) -> bool {
        self.when_time_is_up == WhenTimeIsUp::Reassign
            && self.until.is_some_and(|until| now >= until)
    }
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
    /// Change what happens when the current task's time is up. Asking for `NotifyOnly`
    /// (keep working past the planned end) needs the AI's approval; asking for `Reassign`
    /// is stricter and is approved without asking.
    SetWhenTimeIsUp { action: WhenTimeIsUp },
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

/// Responsibility: Say what the user asks to change in the to-do list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TodoChange {
    /// Put a new to-do on the list. Fields left empty are filled in by the AI.
    Add { todo: ProposalInput },
    /// Change an open to-do. Fields left empty stay as they are.
    Edit {
        todo_id: String,
        todo: ProposalInput,
    },
    /// Take an open to-do off the list.
    Remove { todo_id: String },
}

/// Responsibility: Record one to-do change, the AI's decision, and what the list holds if
/// it is applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoChangeRecord {
    pub id: String,
    pub at: DateTime<Utc>,
    pub change: TodoChange,
    pub reason: String,
    /// Who sent it: "to-do tab", "claude desktop", ...
    pub via: String,
    pub approved: bool,
    /// The AI's answer, or why it could not answer.
    pub reply: String,
    /// The AI could not review it (no key, no network); the user may schedule it.
    #[serde(default)]
    pub ai_failed: bool,
    /// The to-do as it was (edit, remove).
    #[serde(default)]
    pub before: Option<TodoItem>,
    /// The to-do after the change (add, edit), with the fields the AI filled in.
    #[serde(default)]
    pub after: Option<TodoItem>,
    /// Set when the user scheduled a change the AI did not approve: the settings update
    /// that applies it after three confirmations, two hours apart.
    #[serde(default)]
    pub scheduled_update: Option<String>,
}

/// Responsibility: Name one write to the to-do list that applies a [`TodoChangeRecord`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TodoListWrite {
    /// Put this to-do in `planner.json` (replacing the one with its id).
    Put(TodoItem),
    /// Remove the to-do with this id from `planner.json`.
    Remove(String),
    /// Hide a to-do an older version kept outside `planner.json` (a proposed one).
    Dismiss(String),
}

/// Responsibility: Carry a to-do the user proposes, as it arrives over the agent API.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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

/// Responsibility: Record how long one assignment ran, for the time spent per to-do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AssignmentSpan {
    pub todo_id: Option<String>,
    pub task: String,
    pub started: DateTime<Utc>,
    pub ended: DateTime<Utc>,
}

/// Responsibility: Hold the user's own working notes and steps for one to-do.
///
/// They change nothing the rules depend on, so they are saved at once; the AI sees them
/// when it reviews "I finished it".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskNotes {
    pub text: String,
    pub steps: Vec<TaskStep>,
}

/// Responsibility: Hold one step of a to-do and whether it is done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskStep {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

/// Responsibility: Hold the AI's suggested order for the rest of the day (a preview: the
/// planner still assigns one to-do at a time).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayPlan {
    pub at: Option<DateTime<Utc>>,
    pub items: Vec<DayPlanItem>,
    /// One or two sentences about the day.
    pub summary: String,
}

/// Responsibility: Hold one block of a day plan.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayPlanItem {
    pub todo_id: String,
    pub minutes: u32,
    pub why: String,
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
    /// Every proposal and the AI's decision, newest last, capped (older versions).
    pub proposals: VecDeque<ProposalRecord>,
    /// Every to-do change and the AI's decision, newest last, capped.
    #[serde(default)]
    pub todo_changes: VecDeque<TodoChangeRecord>,
    /// Finished assignments, newest last, capped: the time spent per to-do.
    #[serde(default)]
    pub assignment_log: VecDeque<AssignmentSpan>,
    /// The user's notes and steps per to-do id.
    #[serde(default)]
    pub task_notes: BTreeMap<String, TaskNotes>,
    /// The last day plan the user asked for.
    #[serde(default)]
    pub day_plan: Option<DayPlan>,
}

impl PlannerState {
    pub fn is_done(&self, todo_id: &str) -> bool {
        self.done
            .iter()
            .any(|done_record| done_record.todo_id == todo_id)
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
    /// Newest first.
    #[serde(default)]
    pub recent_todo_changes: Vec<TodoChangeRecord>,
    /// Finished to-dos of the last seven days, newest first.
    #[serde(default)]
    pub recent_done: Vec<DoneRecord>,
    /// Minutes spent today (local day) per to-do id, the current assignment included.
    #[serde(default)]
    pub minutes_today: BTreeMap<String, i64>,
    /// Minutes spent in total per to-do id (finished to-dos of the last week included).
    #[serde(default)]
    pub minutes_total: BTreeMap<String, i64>,
    /// The user's notes and steps of the open to-dos.
    #[serde(default)]
    pub task_notes: BTreeMap<String, TaskNotes>,
    #[serde(default)]
    pub day_plan: Option<DayPlan>,
}

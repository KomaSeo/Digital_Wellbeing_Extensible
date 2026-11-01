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
    /// A routine (sleep, getting ready, exercise): finishing it records it as done but keeps it
    /// on the list, so the AI can assign it again.
    #[serde(default)]
    pub repeat: bool,
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
            repeat: false,
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
    /// Ignored: the planner is always on, so the AI (never the user) decides what to do.
    /// Kept so files written by versions with an on/off switch still load.
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
    /// How much work each of the next seven days needs, and how much free time is left.
    pub week_plan: String,
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

    pub fn week_plan_or_default(&self) -> &str {
        or_default(&self.week_plan, crate::advisor::WEEK_PLAN_PROMPT)
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

/// Responsibility: Say what happened when an assignment's time was up (older versions).
///
/// No longer used: when the time is up the task stays until the user reports whether it is
/// finished ([`ChangeKind::Done`] or [`ChangeKind::NotFinished`]). Kept so older state files
/// and requests still load.
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
    /// The time the AI gave is up: the user reports whether the to-do is finished before
    /// anything else happens.
    pub fn time_is_up(&self, now: DateTime<Utc>) -> bool {
        self.until.is_some_and(|until| now >= until)
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
    /// Drop the current task and let the AI pick another one; the user gives the reason
    /// but does not choose what comes next (choosing it themselves makes them lazy).
    Repick,
    /// Stop the focus session altogether.
    End,
    /// The time is up and the to-do is not finished: it stays on the list, and the next task
    /// comes when the user asks for one. Needs no AI review.
    NotFinished,
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
    /// What the user offered as proof that the to-do is finished ("done").
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub screenshot_attached: bool,
    /// For "done": "decisive" (a clear result such as a sent mail), "open_ended" (work that
    /// can always be better) or empty, as the AI judged it.
    #[serde(default)]
    pub outcome: String,
}

/// Responsibility: Carry the proof the user offers with "I finished it": what they write,
/// and a screenshot the agent took for them. Not stored (the screenshot is not kept).
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    pub text: String,
    pub screenshot_jpeg: Option<Vec<u8>>,
}

/// Responsibility: Keep the time left to improve open-ended work the user finished early.
///
/// It shows up as an "Improve: …" to-do (id `polish:<id>`) that the AI picks in spare time;
/// time spent on it is taken off, and it goes away when the time is used up or "done" is
/// approved for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PolishItem {
    /// The finished to-do as it was.
    pub todo: TodoItem,
    pub minutes_left: i64,
    pub since: DateTime<Utc>,
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
    /// Set only these fields of the to-do with this id in `planner.json`, so edits made
    /// meanwhile to its other fields stay. When the to-do is not in the file, add `base` with
    /// the fields set (a to-do an older version kept outside the file), or do nothing.
    Merge {
        todo_id: String,
        changed_fields: serde_json::Map<String, serde_json::Value>,
        base: Option<TodoItem>,
    },
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
    /// A routine that stays on the list when finished.
    pub repeat: Option<bool>,
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

/// Responsibility: Hold the AI's overview of the coming week (today and the next six days):
/// for each day, how much work it
/// needs so that every to-do is done by its deadline, and how much free time is left. It
/// sets no clock times; the planner still assigns one to-do at a time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeekPlan {
    /// When it was made.
    pub at: Option<DateTime<Utc>>,
    /// Today first, then the six days after it.
    pub days: Vec<WeekPlanDay>,
    /// Sums over `days`.
    pub work_minutes_total: u32,
    pub free_minutes_total: u32,
    /// Two or three sentences: how much work the week needs, how much slack is left.
    pub summary: String,
    /// Deadlines at risk, days with more work than fits.
    pub warnings: Vec<String>,
}

/// Responsibility: Hold one day of a [`WeekPlan`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeekPlanDay {
    pub date: Option<NaiveDate>,
    /// Work the day needs.
    pub work_minutes: u32,
    /// Waking time left after that work, following the life rules.
    pub free_minutes: u32,
    /// Which to-dos the work is for.
    pub todos: Vec<WeekPlanTodo>,
    /// One short sentence.
    pub note: String,
}

/// Responsibility: Name a to-do and the minutes a day of the week plan gives it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WeekPlanTodo {
    pub todo_id: String,
    pub minutes: u32,
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
    /// Open-ended to-dos finished early, by to-do id, with the time left to improve them.
    #[serde(default)]
    pub polish: BTreeMap<String, PolishItem>,
    /// The last overview of this week the user asked for.
    #[serde(default)]
    pub week_plan: Option<WeekPlan>,
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
    /// The last overview of this week.
    #[serde(default)]
    pub week_plan: Option<WeekPlan>,
    /// The current assignment's time is up: the user reports whether it is finished.
    #[serde(default)]
    pub report_due: bool,
    /// Minutes spent per open to-do: in total, or since it was last finished for a routine.
    #[serde(default)]
    pub minutes_this_round: BTreeMap<String, i64>,
}

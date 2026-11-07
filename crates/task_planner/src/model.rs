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
    /// Done away from the computer (sleep, exercise, a walk): while it is the task, every app
    /// and website is locked, and changing it needs an urgent reason. `None` (to-dos made
    /// before this existed): the AI judges it when it assigns the to-do.
    #[serde(default)]
    pub away_from_computer: Option<bool>,
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
            away_from_computer: None,
        }
    }
}

/// Responsibility: Describe one appointment: something fixed at a clock time (a meeting, a
/// class, a doctor's visit). From its start to its end it is the user's task, whatever was
/// assigned before, and nothing else is assigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Appointment {
    /// Stable id (`a<milliseconds>`).
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Away from the computer: every app and website is locked while it runs.
    #[serde(default)]
    pub away_from_computer: bool,
}

/// How long an appointment goes on past its planned end: meetings with other people run
/// late, and cutting them off at the minute would hurt.
pub const APPOINTMENT_GRACE_MINUTES: i64 = 30;

/// Responsibility: Offer to extend an appointment that just ended (after its grace time);
/// the user gives a reason the AI must approve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionOffer {
    pub appointment: Appointment,
    /// When it ended (its planned end plus the grace time, or the end of an extension).
    pub ended_at: DateTime<Utc>,
}

/// Responsibility: Carry an appointment the user asks to add, as it arrives over the agent
/// API.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppointmentInput {
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    /// Left out: the AI judges it.
    #[serde(default)]
    pub away_from_computer: Option<bool>,
}

/// Responsibility: Hold the admin's planner settings and to-do list (planner.json).
///
/// Part of the settings (`planner.json`): edited in the Focus app's Settings tab (while the
/// settings are protected, a looser edit takes effect only after the confirmations set in
/// [`Confirmations`]), never by a planner request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PlannerConfig {
    /// Ignored: the planner is always on, so the AI (never the user) decides what to do.
    /// Kept so files written by versions with an on/off switch still load.
    pub enabled: bool,
    pub todos: Vec<TodoItem>,
    /// Appointments the AI accepted (past ones are dropped when the list changes).
    pub appointments: Vec<Appointment>,
    /// The owner's rules for which appointments the AI accepts ("no appointments for games",
    /// "at most two evenings a week"). Appointments are fixed and replace the assigned task,
    /// so they could be used to dodge work; like the life rules they change only in the
    /// settings.
    pub appointment_rules: String,
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
    /// How often, and how far apart, a scheduled update is confirmed before it takes effect.
    pub confirmations: Confirmations,
}

/// Responsibility: Hold how many "yes" answers a scheduled update needs, and the hours
/// between its questions: one pair for settings updates, one for to-do changes the user
/// makes without the AI.
///
/// Kept in planner.json (a protected settings file) so that, like every setting, loosening it
/// is itself a scheduled update confirmed under the numbers in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Confirmations {
    /// "Yes" answers a settings update needs.
    pub settings_times: u32,
    /// Hours from scheduling a settings update to its first question, and between questions.
    pub settings_interval_hours: u32,
    /// "Yes" answers a to-do change the AI did not approve needs.
    pub todo_change_times: u32,
    /// Hours between the questions about such a to-do change.
    pub todo_change_interval_hours: u32,
}

impl Confirmations {
    /// The fewest and most answers; fewer than one would be no confirmation at all.
    pub const TIMES_RANGE: std::ops::RangeInclusive<u32> = 1..=10;
    /// One hour to 60 days.
    pub const HOURS_RANGE: std::ops::RangeInclusive<u32> = 1..=1440;

    pub fn settings_times(&self) -> usize {
        clamp(self.settings_times, Self::TIMES_RANGE) as usize
    }

    pub fn settings_interval_seconds(&self) -> i64 {
        i64::from(clamp(self.settings_interval_hours, Self::HOURS_RANGE)) * 3600
    }

    pub fn todo_change_times(&self) -> usize {
        clamp(self.todo_change_times, Self::TIMES_RANGE) as usize
    }

    pub fn todo_change_interval_seconds(&self) -> i64 {
        i64::from(clamp(self.todo_change_interval_hours, Self::HOURS_RANGE)) * 3600
    }
}

impl Default for Confirmations {
    fn default() -> Self {
        Self {
            settings_times: 3,
            settings_interval_hours: 48,
            todo_change_times: 3,
            todo_change_interval_hours: 2,
        }
    }
}

fn clamp(value: u32, range: std::ops::RangeInclusive<u32>) -> u32 {
    value.clamp(*range.start(), *range.end())
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
    /// Whether an appointment the user adds is accepted.
    pub appointment_review: String,
    /// Whether the current task gets more time ("연장하기").
    pub extension_review: String,
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

    pub fn appointment_review_or_default(&self) -> &str {
        or_default(&self.appointment_review, crate::advisor::APPOINTMENT_PROMPT)
    }

    pub fn extension_review_or_default(&self) -> &str {
        or_default(&self.extension_review, crate::advisor::EXTENSION_PROMPT)
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
    /// Done away from the computer: every app and website is locked meanwhile, and changing
    /// it needs an urgent reason.
    #[serde(default)]
    pub away_from_computer: bool,
    /// Set for an appointment: it is fixed, ends by itself at `until`, and needs no report.
    #[serde(default)]
    pub appointment_id: Option<String>,
    /// Websites (https URLs) the task clearly needs; the Focus app opens them when the task
    /// starts.
    #[serde(default)]
    pub open: Vec<String>,
    /// The user has not touched the computer since this time, during a task done at the
    /// computer: the task's time stands still until they are back (then `until` moves later
    /// by the time they were away).
    #[serde(default)]
    pub paused_since: Option<DateTime<Utc>>,
    /// Earlier times during this task when the user did not touch the computer.
    #[serde(default)]
    pub idle: Vec<IdleSpan>,
}

impl Assignment {
    /// The time the AI gave is up: the user reports whether the to-do is finished before
    /// anything else happens. Never while the time stands still.
    pub fn time_is_up(&self, now: DateTime<Utc>) -> bool {
        self.paused_since.is_none() && self.until.is_some_and(|until| now >= until)
    }

    /// This assignment as a span up to `now`: the stretch away from the computer still going
    /// on ends at `now`.
    pub fn span_until(&self, now: DateTime<Utc>) -> AssignmentSpan {
        let ended = now.max(self.at);
        let mut idle = self.idle.clone();
        if let Some(since) = self.paused_since {
            idle.push(IdleSpan {
                start: since,
                end: ended.max(since),
            });
        }
        AssignmentSpan {
            todo_id: self.todo_id.clone(),
            task: self.task.clone(),
            started: self.at,
            ended,
            away_from_computer: self.away_from_computer,
            appointment: self.appointment_id.is_some(),
            idle,
        }
    }
}

/// Responsibility: Record a stretch of time when the user did not touch the computer during
/// a task done at the computer ("알 수 없음" in the timetable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleSpan {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// The part of `start..end` inside `from..to`.
fn overlap(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> chrono::Duration {
    (end.min(to) - start.max(from)).max(chrono::Duration::zero())
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
    /// More time for the current task ("연장하기"), also once its time is up. Reviewed with
    /// its own instruction (`instructions.extension_review`).
    Extend { minutes: u32 },
    /// An appointment ran past its planned end and the grace time: keep it going for
    /// `minutes` more. The AI approves only a fitting reason.
    ExtendAppointment {
        appointment_id: String,
        minutes: u32,
    },
    /// The current to-do had already been done before this assignment, at another time or
    /// place ("이미 처리했어요"): it is done, but this assignment's time is not counted as
    /// spent on it, and no improvement time follows.
    AlreadyDone,
    /// The current to-do itself changed ("일이 변경되었어요": its deadline, scope or content,
    /// or it is no longer needed), as the reason says. Approved, the AI's list changes edit
    /// or remove it and the next task is picked from the updated list.
    Changed,
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
    /// Approved only: the writes to the to-do list the AI made with this request (a follow-up
    /// to-do, a corrected title); the agent applies them to `planner.json`.
    #[serde(default)]
    pub list_writes: Vec<TodoListWrite>,
    /// Approved only: what the AI changed in the list, one line each, for the user.
    #[serde(default)]
    pub list_summary: Vec<String>,
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
    /// Add an appointment; the AI decides whether to accept it (owner's appointment rules).
    AddAppointment { appointment: AppointmentInput },
    /// Change an appointment on the list (keeps its id). In strict mode the AI decides by the
    /// owner's appointment rules, seeing the appointment it replaces.
    EditAppointment {
        appointment_id: String,
        appointment: AppointmentInput,
    },
    /// Take an appointment off the list. Needs no AI review: it only frees time for to-dos.
    RemoveAppointment { appointment_id: String },
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
    /// The appointment added, removed, or as it is after an edit.
    #[serde(default)]
    pub appointment: Option<Appointment>,
    /// The appointment as it was before an edit.
    #[serde(default)]
    pub appointment_before: Option<Appointment>,
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
    /// Put this appointment in `planner.json` (replacing the one with its id).
    PutAppointment(Appointment),
    /// Remove the appointment with this id from `planner.json`.
    RemoveAppointment(String),
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
    /// Done away from the computer; left out, the AI fills it in.
    pub away_from_computer: Option<bool>,
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
    /// Done away from the computer (spans logged before this field: `false`).
    #[serde(default)]
    pub away_from_computer: bool,
    #[serde(default)]
    pub appointment: bool,
    /// Stretches when the user did not touch the computer (they do not count as time spent).
    #[serde(default)]
    pub idle: Vec<IdleSpan>,
}

impl AssignmentSpan {
    /// Minutes of this span inside `from..to` that count as time spent on the task: the
    /// stretches away from the computer do not.
    pub fn minutes_within(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> i64 {
        let whole = overlap(self.started, self.ended, from, to);
        let idle = self
            .idle
            .iter()
            .map(|idle| {
                overlap(
                    idle.start,
                    idle.end,
                    from.max(self.started),
                    to.min(self.ended),
                )
            })
            .fold(chrono::Duration::zero(), |sum, part| sum + part);
        (whole - idle).num_minutes().max(0)
    }

    /// Minutes that count as time spent, the whole span.
    pub fn minutes(&self) -> i64 {
        self.minutes_within(self.started, self.ended)
    }
}

/// Responsibility: Name what one stretch of the timetable was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimetableKind {
    /// A task done at the computer.
    Task,
    /// A task done away from the computer.
    AwayFromComputer,
    Appointment,
    /// A task done at the computer, but the user did not touch the computer: what they did
    /// is not known.
    Unknown,
}

/// Responsibility: Hold one stretch of the timetable (the Statistics tab).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimetableEntry {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub kind: TimetableKind,
    /// The task (for "알 수 없음": the task it interrupted).
    pub task: String,
    pub todo_id: Option<String>,
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
    /// Appointments the user ended early ("done"): they do not start again.
    #[serde(default)]
    pub ended_appointments: BTreeSet<String>,
    /// The user pressed "사용 중지" (stop using the computer): every app is locked and
    /// nothing runs but the blocking, until they press "다시 사용하기".
    #[serde(default)]
    pub stopped_at: Option<DateTime<Utc>>,
    /// Approved extensions of appointments: the new end, by appointment id.
    #[serde(default)]
    pub appointment_extensions: BTreeMap<String, DateTime<Utc>>,
    /// What the user wrote about their situation when they last asked for a task (where they
    /// are, what they have with them, how they feel), in their words; every pick reads it
    /// until they write another.
    #[serde(default)]
    pub situation: String,
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
    /// [`PlannerState::situation`].
    #[serde(default)]
    pub situation: String,
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
    /// Appointments that have not ended, soonest first.
    #[serde(default)]
    pub appointments: Vec<Appointment>,
    /// The user stopped using the computer ("사용 중지") at this time.
    #[serde(default)]
    pub stopped_at: Option<DateTime<Utc>>,
    /// An appointment that just ended (within the last half hour) and may be extended.
    #[serde(default)]
    pub extension_offer: Option<ExtensionOffer>,
}

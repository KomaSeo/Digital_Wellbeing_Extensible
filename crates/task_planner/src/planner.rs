//! Responsibility: Assign the user's task from the list and arbitrate change requests, driving
//! the focus session.
//!
//! It drives the AI focus session: an assignment becomes the focus monitor's
//! task (and starts the session); an approved "end" stops it.
//!
//! Every assignment runs for the time the AI gave it (`until`). When that is up, the agent
//! calls [`TaskPlanner::reassign_if_time_is_up`] and the AI picks again, unless the user got
//! "only notify me" approved for that assignment.

use crate::advisor::{
    AdvisorProvider, PickRequest, Review, ReviewRequest, ReviewedTodo, TodoChangeRequest,
};
use crate::model::{
    Assignment, AssignmentSource, AssignmentSpan, ChangeKind, ChangeRecord, DayPlan, DoneRecord,
    PlannerConfig, PlannerState, PlannerStatus, ProposalInput, TaskNotes, TodoChange,
    TodoChangeRecord, TodoItem, TodoListWrite, WhenTimeIsUp,
};
use ai_focus_rule::store::JsonStore;
use ai_focus_rule::{FocusMonitor, JudgeError};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

const REQUEST_LOG_CAPACITY: usize = 50;
/// Most proposals per day (each costs an AI call), and most approved
/// proposals waiting on the list at once.
const PROPOSALS_PER_DAY: usize = 30;
const MAX_OPEN_PROPOSED: usize = 50;
const MAX_TITLE_CHARS: usize = 120;
const MAX_NOTES_CHARS: usize = 1000;
const MIN_REASON_CHARS: usize = 4;
/// An assignment older than this is not resumed after a restart; a new one is picked.
const RESUME_WITHIN: Duration = Duration::hours(12);
/// Bounds on the time the AI gives one assignment, and the time an approved switch gets
/// when the AI names none.
const MIN_ASSIGNMENT_MINUTES: u32 = 10;
const MAX_ASSIGNMENT_MINUTES: u32 = 480;
const DEFAULT_ASSIGNMENT_MINUTES: u32 = 60;
/// Finished assignments kept for the time spent per to-do (a few weeks).
const ASSIGNMENT_LOG_CAPACITY: usize = 1000;
const MAX_TASK_NOTES_CHARS: usize = 4000;
const MAX_TASK_STEPS: usize = 50;

/// Responsibility: Report why an assignment or a change request was refused.
#[derive(Debug, Error)]
pub enum PlannerError {
    #[error("the planner is off or every to-do is done")]
    NotActive,
    #[error("another request is still being processed")]
    Busy,
    #[error("unknown or finished to-do '{0}'")]
    UnknownTodo(String),
    #[error("please give a reason (at least {MIN_REASON_CHARS} characters)")]
    ReasonTooShort,
    #[error("the new task is empty")]
    EmptyTask,
    #[error("the AI picked a to-do that is not open ('{0}')")]
    BadPick(String),
    #[error("give the to-do a title (at most {MAX_TITLE_CHARS} characters; notes at most {MAX_NOTES_CHARS})")]
    BadProposal,
    #[error("\"{0}\" is already on the list")]
    Duplicate(String),
    #[error("too many proposals: at most {PROPOSALS_PER_DAY} a day and {MAX_OPEN_PROPOSED} open")]
    TooManyProposals,
    #[error("AI: {0}")]
    Ai(#[from] JudgeError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Responsibility: Own the planner's config and state stores, and run assign / request_change
/// against the AI advisor.
pub struct TaskPlanner {
    config: JsonStore<PlannerConfig>,
    state: JsonStore<PlannerState>,
    focus: Arc<FocusMonitor>,
    advisor: AdvisorProvider,
    /// Held for a whole assign/review, so two clicks cannot race. The AI call
    /// happens under it, but no data lock is held during the call.
    busy: Mutex<()>,
}

impl TaskPlanner {
    pub fn new(
        config: JsonStore<PlannerConfig>,
        state: JsonStore<PlannerState>,
        focus: Arc<FocusMonitor>,
        advisor: AdvisorProvider,
    ) -> Self {
        Self {
            config,
            state,
            focus,
            advisor,
            busy: Mutex::new(()),
        }
    }

    pub fn config_store(&self) -> &JsonStore<PlannerConfig> {
        &self.config
    }

    #[cfg(test)]
    pub(crate) fn state_store_for_tests(&self) -> &JsonStore<PlannerState> {
        &self.state
    }

    /// The admin's to-dos plus approved proposals the admin has not removed,
    /// minus finished ones.
    fn open_todos(&self) -> Vec<TodoItem> {
        let state = self.state.get();
        let config = self.config.get();
        config
            .todos
            .into_iter()
            .chain(
                state
                    .proposed
                    .iter()
                    .filter(|todo| !config.dismissed.contains(&todo.id))
                    .cloned(),
            )
            .filter(|todo| !state.is_done(&todo.id))
            .collect()
    }

    /// Enabled and something left to do: the user cannot type a task freely.
    pub fn is_active(&self) -> bool {
        self.config.get().enabled && !self.open_todos().is_empty()
    }

    pub fn status(&self) -> PlannerStatus {
        let state = self.state.get();
        let now = Utc::now();
        let open_todos = self.open_todos();
        // Finished spans plus the one running now.
        let mut spans: Vec<AssignmentSpan> = state.assignment_log.iter().cloned().collect();
        if let Some(current) = &state.assignment {
            spans.push(AssignmentSpan {
                todo_id: current.todo_id.clone(),
                task: current.task.clone(),
                started: current.at,
                ended: now,
            });
        }
        let start_of_today = Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
            .map(|midnight| midnight.with_timezone(&Utc))
            .unwrap_or(now - Duration::days(1));
        let mut minutes_today = BTreeMap::new();
        let mut minutes_total = BTreeMap::new();
        for span in &spans {
            let Some(todo_id) = &span.todo_id else {
                continue;
            };
            let total = (span.ended - span.started).num_minutes().max(0);
            *minutes_total.entry(todo_id.clone()).or_insert(0) += total;
            let today = (span.ended - span.started.max(start_of_today))
                .num_minutes()
                .max(0);
            if today > 0 {
                *minutes_today.entry(todo_id.clone()).or_insert(0) += today;
            }
        }
        PlannerStatus {
            active: self.is_active(),
            assignment: state.assignment.clone(),
            recent_requests: state.requests.iter().rev().take(10).cloned().collect(),
            recent_proposals: state.proposals.iter().rev().take(10).cloned().collect(),
            recent_todo_changes: state.todo_changes.iter().rev().take(20).cloned().collect(),
            recent_done: state
                .done
                .iter()
                .rev()
                .filter(|done| now - done.at < Duration::days(7))
                .cloned()
                .collect(),
            minutes_today,
            minutes_total,
            task_notes: state
                .task_notes
                .iter()
                .filter(|(todo_id, _)| open_todos.iter().any(|todo| &todo.id == *todo_id))
                .map(|(todo_id, notes)| (todo_id.clone(), notes.clone()))
                .collect(),
            day_plan: state.day_plan.clone(),
            open_todos,
        }
    }

    /// Save the user's notes and steps for an open to-do. They change nothing the rules
    /// depend on, so no AI is asked.
    pub fn put_task_notes(&self, todo_id: &str, mut notes: TaskNotes) -> Result<(), PlannerError> {
        if !self.open_todos().iter().any(|todo| todo.id == todo_id) {
            return Err(PlannerError::UnknownTodo(todo_id.to_string()));
        }
        notes.text = notes.text.chars().take(MAX_TASK_NOTES_CHARS).collect();
        notes.steps.truncate(MAX_TASK_STEPS);
        for step in &mut notes.steps {
            step.text = step.text.trim().chars().take(MAX_TITLE_CHARS).collect();
        }
        notes.steps.retain(|step| !step.text.is_empty());
        self.state.update(|planner_state| {
            planner_state.task_notes.insert(todo_id.to_string(), notes);
        })?;
        Ok(())
    }

    /// Ask the AI for an order for the rest of the day (a preview; one AI call), keep it
    /// and return it.
    pub fn plan_day(&self, now: DateTime<Utc>) -> Result<DayPlan, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        let open = self.open_todos();
        if open.is_empty() {
            return Err(PlannerError::NotActive);
        }
        let state = self.state.get();
        let recently_done: Vec<DoneRecord> = state.done.iter().rev().take(10).cloned().collect();
        let history = self.focus.state().history_text();
        let now_local = local_now_text();
        let advisor = (self.advisor)(&self.focus.store().get())?;
        let config = self.config.get();
        let mut plan = advisor.plan_day(&PickRequest {
            instructions: config.instructions.day_plan_or_default(),
            now_local: &now_local,
            life_rules: &config.life_rules,
            open: &open,
            recently_done: &recently_done,
            previous: state.assignment.as_ref(),
            history: &history,
        })?;
        plan.items
            .retain(|item| open.iter().any(|todo| todo.id == item.todo_id));
        for item in &mut plan.items {
            item.minutes = item.minutes.clamp(5, MAX_ASSIGNMENT_MINUTES);
        }
        plan.at = Some(now);
        self.state
            .update(|planner_state| planner_state.day_plan = Some(plan.clone()))?;
        Ok(plan)
    }

    /// The user asks to add, edit or remove a to-do (from the Focus app or their AI
    /// assistant, named by `via`). The AI reviews it against the life rules and fills in the
    /// fields of a new to-do the user left empty. The decision is logged and returned; the
    /// caller applies an approved one ([`Self::writes_for`]) to `planner.json`. When the AI
    /// cannot answer, the record says so (`ai_failed`) and nothing is approved. Works
    /// whether or not the planner is on.
    pub fn review_todo_change(
        &self,
        now: DateTime<Utc>,
        change: TodoChange,
        reason: &str,
        via: &str,
    ) -> Result<TodoChangeRecord, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        let reason = reason
            .trim()
            .chars()
            .take(MAX_NOTES_CHARS)
            .collect::<String>();
        let open = self.open_todos();
        let state = self.state.get();
        let today = state
            .todo_changes
            .iter()
            .filter(|record| now - record.at < Duration::days(1))
            .count();
        if today >= PROPOSALS_PER_DAY {
            return Err(PlannerError::TooManyProposals);
        }
        let change = clean_change(change)?;
        let target = match &change {
            TodoChange::Add { todo } => {
                if let Some(duplicate) = open
                    .iter()
                    .find(|existing| existing.title.trim().eq_ignore_ascii_case(&todo.title))
                {
                    return Err(PlannerError::Duplicate(duplicate.title.clone()));
                }
                if open.iter().filter(|existing| existing.proposed).count() >= MAX_OPEN_PROPOSED {
                    return Err(PlannerError::TooManyProposals);
                }
                None
            }
            TodoChange::Edit { todo_id, .. } | TodoChange::Remove { todo_id } => {
                if reason.chars().count() < MIN_REASON_CHARS {
                    return Err(PlannerError::ReasonTooShort);
                }
                Some(
                    open.iter()
                        .find(|existing| &existing.id == todo_id)
                        .cloned()
                        .ok_or_else(|| PlannerError::UnknownTodo(todo_id.clone()))?,
                )
            }
        };

        let recent: Vec<TodoChangeRecord> = state.todo_changes.iter().rev().cloned().collect();
        let history = self.focus.state().history_text();
        let now_local = local_now_text();
        let config = self.config.get();
        let review = (self.advisor)(&self.focus.store().get()).and_then(|advisor| {
            advisor.review_todo_change(&TodoChangeRequest {
                instructions: config.instructions.proposal_review_or_default(),
                now_local: &now_local,
                life_rules: &config.life_rules,
                change: &change,
                target: target.as_ref(),
                reason: &reason,
                open: &open,
                recent_changes: &recent,
                history: &history,
            })
        });
        let (approved, reply, ai_failed, filled) = match review {
            Ok(review) => (review.approved, review.reply, false, Some(review.todo)),
            Err(error) => (
                false,
                format!("The AI could not review this: {error}"),
                true,
                None,
            ),
        };
        let after = match &change {
            TodoChange::Add { todo } => Some(completed_todo(now, todo, filled.as_ref())),
            TodoChange::Edit { todo, .. } => target.as_ref().map(|before| edited(before, todo)),
            TodoChange::Remove { .. } => None,
        };
        let record = TodoChangeRecord {
            id: format!("c{}", now.timestamp_millis()),
            at: now,
            change,
            reason,
            via: via.trim().chars().take(40).collect(),
            approved,
            reply,
            ai_failed,
            before: target,
            after,
            scheduled_update: None,
        };
        self.state.update(|planner_state| {
            if planner_state.todo_changes.len() >= REQUEST_LOG_CAPACITY {
                planner_state.todo_changes.pop_front();
            }
            planner_state.todo_changes.push_back(record.clone());
        })?;
        info!(
            "to-do change {} via {} ({}): {}",
            crate::advisor::change_summary(&record.change, record.before.as_ref()),
            record.via,
            if record.approved {
                "approved"
            } else if record.ai_failed {
                "not reviewed"
            } else {
                "rejected"
            },
            record.reply
        );
        Ok(record)
    }

    /// A logged to-do change by id.
    pub fn todo_change(&self, id: &str) -> Option<TodoChangeRecord> {
        self.state
            .get()
            .todo_changes
            .into_iter()
            .find(|record| record.id == id)
    }

    /// The writes to the to-do list that apply `record`.
    pub fn writes_for(&self, record: &TodoChangeRecord) -> Vec<TodoListWrite> {
        let in_settings = |todo_id: &str| {
            self.config
                .get()
                .todos
                .iter()
                .any(|todo| todo.id == todo_id)
        };
        match &record.change {
            TodoChange::Add { .. } => record
                .after
                .clone()
                .map(TodoListWrite::Put)
                .into_iter()
                .collect(),
            TodoChange::Edit { todo_id, .. } => {
                let mut writes = Vec::new();
                if !in_settings(todo_id) {
                    writes.push(TodoListWrite::Dismiss(todo_id.clone()));
                }
                writes.extend(record.after.clone().map(TodoListWrite::Put));
                writes
            }
            TodoChange::Remove { todo_id } if in_settings(todo_id) => {
                vec![TodoListWrite::Remove(todo_id.clone())]
            }
            TodoChange::Remove { todo_id } => vec![TodoListWrite::Dismiss(todo_id.clone())],
        }
    }

    /// Note that the user scheduled a change the AI did not approve.
    pub fn mark_scheduled(&self, record_id: &str, update_id: &str) -> Result<(), PlannerError> {
        self.state.update(|planner_state| {
            if let Some(record) = planner_state
                .todo_changes
                .iter_mut()
                .find(|record| record.id == record_id)
            {
                record.scheduled_update = Some(update_id.to_string());
            }
        })?;
        Ok(())
    }

    /// Start (or resume) the session with an assigned task.
    pub fn assign(&self, now: DateTime<Utc>) -> Result<Assignment, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        if !self.is_active() {
            return Err(PlannerError::NotActive);
        }
        // Resume a recent assignment instead of asking again (e.g. after logon).
        if let Some(previous) = self.state.get().assignment {
            let still_open = previous
                .todo_id
                .as_deref()
                .is_none_or(|id| self.open_todos().iter().any(|todo| todo.id == id));
            if still_open
                && now - previous.at < RESUME_WITHIN
                && !previous.due_for_reassignment(now)
            {
                self.start_focus(&previous)?;
                return Ok(previous);
            }
        }
        self.pick_and_apply(now)
    }

    /// When the current assignment's time is up (and the user did not get "notify only"
    /// approved for it), ask the AI for the next task. Returns the new assignment, or
    /// `None` when nothing was due or another request is being handled.
    pub fn reassign_if_time_is_up(
        &self,
        now: DateTime<Utc>,
    ) -> Result<Option<Assignment>, PlannerError> {
        let Some(_busy) = self.busy.try_lock() else {
            return Ok(None);
        };
        let due = self
            .state
            .get()
            .assignment
            .is_some_and(|assignment| assignment.due_for_reassignment(now));
        if !due || !self.is_active() {
            return Ok(None);
        }
        self.pick_and_apply(now).map(Some)
    }

    /// The user asks to change the assignment. Returns the AI's decision;
    /// an approved request is applied before returning.
    pub fn request_change(
        &self,
        now: DateTime<Utc>,
        request: ChangeKind,
        reason: &str,
    ) -> Result<ChangeRecord, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        if !self.is_active() {
            return Err(PlannerError::NotActive);
        }
        let reason = reason.trim();
        if reason.chars().count() < MIN_REASON_CHARS {
            return Err(PlannerError::ReasonTooShort);
        }
        if request
            == (ChangeKind::SetWhenTimeIsUp {
                action: WhenTimeIsUp::Reassign,
            })
        {
            // Stricter than the default it restores: no need to ask the AI.
            return self.record_and_apply(
                now,
                request,
                reason,
                Review {
                    approved: true,
                    reply: "A new task will be picked when the time is up.".into(),
                    minutes: 0,
                },
                &self.open_todos(),
            );
        }
        let open = self.open_todos();
        let target_title = match &request {
            ChangeKind::SwitchTodo { todo_id } => Some(
                open.iter()
                    .find(|todo| &todo.id == todo_id)
                    .map(|todo| todo.title.clone())
                    .ok_or_else(|| PlannerError::UnknownTodo(todo_id.clone()))?,
            ),
            ChangeKind::SwitchFree { task } if task.trim().is_empty() => {
                return Err(PlannerError::EmptyTask)
            }
            _ => None,
        };

        let state = self.state.get();
        let recent: Vec<ChangeRecord> = state.requests.iter().rev().cloned().collect();
        let history = self.focus.state().history_text();
        let now_local = local_now_text();
        let advisor = (self.advisor)(&self.focus.store().get())?;
        let config = self.config.get();
        let review = advisor.review(&ReviewRequest {
            instructions: config.instructions.change_review_or_default(),
            now_local: &now_local,
            life_rules: &config.life_rules,
            current: state.assignment.as_ref(),
            request: &request,
            target_title: target_title.as_deref(),
            reason,
            open: &open,
            recent_requests: &recent,
            history: &history,
            notes: state
                .assignment
                .as_ref()
                .and_then(|assignment| assignment.todo_id.as_ref())
                .and_then(|todo_id| state.task_notes.get(todo_id)),
        })?;
        self.record_and_apply(now, request, reason, review, &open)
    }

    /// Log the decision on a change request and apply it when approved.
    fn record_and_apply(
        &self,
        now: DateTime<Utc>,
        request: ChangeKind,
        reason: &str,
        review: Review,
        open: &[TodoItem],
    ) -> Result<ChangeRecord, PlannerError> {
        let state = self.state.get();
        let record = ChangeRecord {
            at: now,
            request: request.clone(),
            from_task: state
                .assignment
                .as_ref()
                .map(|assignment| assignment.task.clone())
                .unwrap_or_default(),
            reason: reason.to_string(),
            approved: review.approved,
            reply: review.reply.clone(),
        };
        self.state.update(|planner_state| {
            if planner_state.requests.len() >= REQUEST_LOG_CAPACITY {
                planner_state.requests.pop_front();
            }
            planner_state.requests.push_back(record.clone());
        })?;
        info!(
            "change request {:?} ({}): {}",
            request,
            if review.approved {
                "approved"
            } else {
                "rejected"
            },
            review.reply
        );
        if review.approved {
            self.apply_change(now, &request, &review, open)?;
        }
        Ok(record)
    }

    fn apply_change(
        &self,
        now: DateTime<Utc>,
        request: &ChangeKind,
        review: &Review,
        open: &[TodoItem],
    ) -> Result<(), PlannerError> {
        let reply = review.reply.as_str();
        match request {
            ChangeKind::Done => {
                let current = self.state.get().assignment;
                if let Some(todo) = current
                    .as_ref()
                    .and_then(|assignment| assignment.todo_id.as_deref())
                    .and_then(|id| open.iter().find(|todo| todo.id == id))
                {
                    let record = DoneRecord {
                        todo_id: todo.id.clone(),
                        title: todo.title.clone(),
                        at: now,
                    };
                    self.state
                        .update(|planner_state| planner_state.done.push(record))?;
                }
                // Next one, or stop when the list is empty. If picking fails
                // (AI or network error), do not stay on the finished to-do:
                // end the session so "Get my task" can be used again.
                if self.is_active() {
                    if let Err(error) = self.pick_and_apply(now) {
                        tracing::warn!("could not pick the next to-do: {error}");
                        self.end(now)?;
                    }
                } else {
                    self.end(now)?;
                }
            }
            ChangeKind::SwitchTodo { todo_id } => {
                let todo = open
                    .iter()
                    .find(|todo| &todo.id == todo_id)
                    .ok_or_else(|| PlannerError::UnknownTodo(todo_id.clone()))?;
                let task = if todo.notes.trim().is_empty() {
                    todo.title.clone()
                } else {
                    format!("{} — {}", todo.title, todo.notes.trim())
                };
                let minutes = if review.minutes > 0 {
                    review.minutes
                } else {
                    todo.estimate_minutes.unwrap_or(DEFAULT_ASSIGNMENT_MINUTES)
                };
                self.apply(Assignment {
                    todo_id: Some(todo.id.clone()),
                    task,
                    why: reply.to_string(),
                    source: AssignmentSource::ApprovedChange,
                    at: now,
                    until: Some(end_after(now, minutes)),
                    when_time_is_up: WhenTimeIsUp::Reassign,
                })?;
            }
            ChangeKind::SwitchFree { task } => {
                let minutes = if review.minutes > 0 {
                    review.minutes
                } else {
                    DEFAULT_ASSIGNMENT_MINUTES
                };
                self.apply(Assignment {
                    todo_id: None,
                    task: task.trim().to_string(),
                    why: reply.to_string(),
                    source: AssignmentSource::ApprovedChange,
                    at: now,
                    until: Some(end_after(now, minutes)),
                    when_time_is_up: WhenTimeIsUp::Reassign,
                })?;
            }
            ChangeKind::End => self.end(now)?,
            ChangeKind::SetWhenTimeIsUp { action } => {
                self.state.update(|planner_state| {
                    if let Some(assignment) = planner_state.assignment.as_mut() {
                        assignment.when_time_is_up = *action;
                    }
                })?;
            }
        }
        Ok(())
    }

    fn pick_and_apply(&self, now: DateTime<Utc>) -> Result<Assignment, PlannerError> {
        let open = self.open_todos();
        let state = self.state.get();
        let recently_done: Vec<DoneRecord> = state.done.iter().rev().take(10).cloned().collect();
        let history = self.focus.state().history_text();
        let now_local = local_now_text();
        let advisor = (self.advisor)(&self.focus.store().get())?;
        let config = self.config.get();
        let pick = advisor.pick(&PickRequest {
            instructions: config.instructions.pick_or_default(),
            now_local: &now_local,
            life_rules: &config.life_rules,
            open: &open,
            recently_done: &recently_done,
            previous: state.assignment.as_ref(),
            history: &history,
        })?;
        if !open.iter().any(|todo| todo.id == pick.todo_id) {
            return Err(PlannerError::BadPick(pick.todo_id));
        }
        let assignment = Assignment {
            todo_id: Some(pick.todo_id),
            task: pick.task,
            why: pick.why,
            source: AssignmentSource::Planner,
            at: now,
            until: Some(end_after(now, pick.minutes)),
            when_time_is_up: WhenTimeIsUp::Reassign,
        };
        info!(
            "assigned for {} min: {}",
            pick.minutes
                .clamp(MIN_ASSIGNMENT_MINUTES, MAX_ASSIGNMENT_MINUTES),
            assignment.task
        );
        self.apply(assignment.clone())?;
        Ok(assignment)
    }

    fn apply(&self, assignment: Assignment) -> Result<(), PlannerError> {
        self.state.update(|planner_state| {
            close_span(planner_state, assignment.at);
            planner_state.assignment = Some(assignment.clone());
        })?;
        self.start_focus(&assignment)
    }

    fn start_focus(&self, assignment: &Assignment) -> Result<(), PlannerError> {
        let task = assignment.task.clone();
        self.focus
            .store()
            .update(|config| {
                config.task = task;
                config.session_active = true;
            })
            .map_err(|error| PlannerError::Io(std::io::Error::other(error.to_string())))?;
        Ok(())
    }

    fn end(&self, now: DateTime<Utc>) -> Result<(), PlannerError> {
        self.state.update(|planner_state| {
            close_span(planner_state, now);
            planner_state.assignment = None;
        })?;
        self.focus
            .store()
            .update(|config| config.session_active = false)
            .map_err(|error| PlannerError::Io(std::io::Error::other(error.to_string())))?;
        Ok(())
    }
}

/// Log how long the current assignment ran, as it ends at `ended`.
fn close_span(planner_state: &mut PlannerState, ended: DateTime<Utc>) {
    if let Some(current) = &planner_state.assignment {
        if planner_state.assignment_log.len() >= ASSIGNMENT_LOG_CAPACITY {
            planner_state.assignment_log.pop_front();
        }
        planner_state.assignment_log.push_back(AssignmentSpan {
            todo_id: current.todo_id.clone(),
            task: current.task.clone(),
            started: current.at,
            ended: ended.max(current.at),
        });
    }
}

/// `now` plus `minutes`, kept within the bounds an assignment may run.
fn end_after(now: DateTime<Utc>, minutes: u32) -> DateTime<Utc> {
    now + Duration::minutes(minutes.clamp(MIN_ASSIGNMENT_MINUTES, MAX_ASSIGNMENT_MINUTES) as i64)
}

/// Trim the texts of a change and refuse an addition without a usable title.
fn clean_change(change: TodoChange) -> Result<TodoChange, PlannerError> {
    let clean = |mut todo: ProposalInput| {
        todo.title = todo.title.split_whitespace().collect::<Vec<_>>().join(" ");
        todo.notes = todo.notes.trim().to_string();
        todo
    };
    let fits = |todo: &ProposalInput| {
        todo.title.chars().count() <= MAX_TITLE_CHARS
            && todo.notes.chars().count() <= MAX_NOTES_CHARS
    };
    let change = match change {
        TodoChange::Add { todo } => TodoChange::Add { todo: clean(todo) },
        TodoChange::Edit { todo_id, todo } => TodoChange::Edit {
            todo_id,
            todo: clean(todo),
        },
        remove @ TodoChange::Remove { .. } => remove,
    };
    match &change {
        TodoChange::Add { todo } if todo.title.chars().count() < 2 || !fits(todo) => {
            Err(PlannerError::BadProposal)
        }
        TodoChange::Edit { todo, .. } if !fits(todo) => Err(PlannerError::BadProposal),
        _ => Ok(change),
    }
}

/// A new to-do: what the user filled in, completed by the AI where the user left a field
/// empty (and only there).
fn completed_todo(
    now: DateTime<Utc>,
    draft: &ProposalInput,
    filled: Option<&ReviewedTodo>,
) -> TodoItem {
    let filled = filled.cloned().unwrap_or_default();
    let deadline = draft
        .deadline
        .or_else(|| filled.deadline.trim().parse::<NaiveDate>().ok());
    let notes = if draft.notes.is_empty() {
        filled.notes.trim().chars().take(MAX_NOTES_CHARS).collect()
    } else {
        draft.notes.clone()
    };
    let priority = draft
        .priority
        .or((filled.priority > 0).then_some(filled.priority))
        .unwrap_or(3)
        .clamp(1, 5);
    let estimate_minutes = draft
        .estimate_minutes
        .or((filled.estimate_minutes > 0).then_some(filled.estimate_minutes))
        .map(|minutes| minutes.min(6000));
    TodoItem {
        id: format!("p{}", now.timestamp_millis()),
        title: draft.title.clone(),
        notes,
        deadline,
        priority,
        estimate_minutes,
        proposed: true,
    }
}

/// `before` with the fields the user set in `draft`.
fn edited(before: &TodoItem, draft: &ProposalInput) -> TodoItem {
    let mut after = before.clone();
    if !draft.title.is_empty() {
        after.title = draft.title.clone();
    }
    if !draft.notes.is_empty() {
        after.notes = draft.notes.clone();
    }
    if draft.deadline.is_some() {
        after.deadline = draft.deadline;
    }
    if let Some(priority) = draft.priority {
        after.priority = priority.clamp(1, 5);
    }
    if let Some(minutes) = draft.estimate_minutes {
        after.estimate_minutes = (minutes > 0).then_some(minutes.min(6000));
    }
    after
}

fn local_now_text() -> String {
    Local::now().format("%Y-%m-%d %A %H:%M").to_string()
}

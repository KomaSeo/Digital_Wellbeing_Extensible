//! [`TaskPlanner`]: assigns a task from the list and arbitrates change requests.
//!
//! It drives the AI focus session: an assignment becomes the focus monitor's
//! task (and starts the session); an approved "end" stops it.

use crate::advisor::{AdvisorProvider, PickRequest, ReviewRequest};
use crate::model::{
    Assignment, AssignmentSource, ChangeKind, ChangeRecord, DoneRecord, PlannerConfig,
    PlannerState, PlannerStatus, TodoItem,
};
use crate::store::JsonStore;
use ai_focus_rule::{FocusMonitor, JudgeError};
use chrono::{DateTime, Duration, Local, Utc};
use parking_lot::Mutex;
use std::sync::Arc;
use thiserror::Error;
use tracing::info;

const REQUEST_LOG_CAPACITY: usize = 50;
const MIN_REASON_CHARS: usize = 4;
/// An assignment older than this is not resumed after a restart; a new one is picked.
const RESUME_WITHIN: Duration = Duration::hours(12);

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
    #[error("AI: {0}")]
    Ai(#[from] JudgeError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

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

    fn open_todos(&self) -> Vec<TodoItem> {
        let state = self.state.get();
        self.config
            .get()
            .todos
            .into_iter()
            .filter(|t| !state.is_done(&t.id))
            .collect()
    }

    /// Enabled and something left to do: the user cannot type a task freely.
    pub fn is_active(&self) -> bool {
        self.config.get().enabled && !self.open_todos().is_empty()
    }

    pub fn status(&self) -> PlannerStatus {
        let state = self.state.get();
        PlannerStatus {
            active: self.is_active(),
            assignment: state.assignment,
            open_todos: self.open_todos(),
            recent_requests: state.requests.iter().rev().take(10).cloned().collect(),
        }
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
                .is_none_or(|id| self.open_todos().iter().any(|t| t.id == id));
            if still_open && now - previous.at < RESUME_WITHIN {
                self.start_focus(&previous)?;
                return Ok(previous);
            }
        }
        self.pick_and_apply(now)
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
        let open = self.open_todos();
        let target_title = match &request {
            ChangeKind::SwitchTodo { todo_id } => Some(
                open.iter()
                    .find(|t| &t.id == todo_id)
                    .map(|t| t.title.clone())
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
        let review = advisor.review(&ReviewRequest {
            now_local: &now_local,
            current: state.assignment.as_ref(),
            request: &request,
            target_title: target_title.as_deref(),
            reason,
            open: &open,
            recent_requests: &recent,
            history: &history,
        })?;

        let record = ChangeRecord {
            at: now,
            request: request.clone(),
            from_task: state
                .assignment
                .as_ref()
                .map(|a| a.task.clone())
                .unwrap_or_default(),
            reason: reason.to_string(),
            approved: review.approved,
            reply: review.reply.clone(),
        };
        self.state.update(|s| {
            if s.requests.len() >= REQUEST_LOG_CAPACITY {
                s.requests.pop_front();
            }
            s.requests.push_back(record.clone());
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
            self.apply_change(now, &request, &review.reply, &open)?;
        }
        Ok(record)
    }

    fn apply_change(
        &self,
        now: DateTime<Utc>,
        request: &ChangeKind,
        reply: &str,
        open: &[TodoItem],
    ) -> Result<(), PlannerError> {
        match request {
            ChangeKind::Done => {
                let current = self.state.get().assignment;
                if let Some(todo) = current
                    .as_ref()
                    .and_then(|a| a.todo_id.as_deref())
                    .and_then(|id| open.iter().find(|t| t.id == id))
                {
                    let record = DoneRecord {
                        todo_id: todo.id.clone(),
                        title: todo.title.clone(),
                        at: now,
                    };
                    self.state.update(|s| s.done.push(record))?;
                }
                // Next one, or stop when the list is empty.
                if self.is_active() {
                    self.pick_and_apply(now)?;
                } else {
                    self.end(now)?;
                }
            }
            ChangeKind::SwitchTodo { todo_id } => {
                let todo = open
                    .iter()
                    .find(|t| &t.id == todo_id)
                    .ok_or_else(|| PlannerError::UnknownTodo(todo_id.clone()))?;
                let task = if todo.notes.trim().is_empty() {
                    todo.title.clone()
                } else {
                    format!("{} — {}", todo.title, todo.notes.trim())
                };
                self.apply(Assignment {
                    todo_id: Some(todo.id.clone()),
                    task,
                    why: reply.to_string(),
                    source: AssignmentSource::ApprovedChange,
                    at: now,
                })?;
            }
            ChangeKind::SwitchFree { task } => {
                self.apply(Assignment {
                    todo_id: None,
                    task: task.trim().to_string(),
                    why: reply.to_string(),
                    source: AssignmentSource::ApprovedChange,
                    at: now,
                })?;
            }
            ChangeKind::End => self.end(now)?,
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
        let pick = advisor.pick(&PickRequest {
            now_local: &now_local,
            open: &open,
            recently_done: &recently_done,
            previous: state.assignment.as_ref(),
            history: &history,
        })?;
        if !open.iter().any(|t| t.id == pick.todo_id) {
            return Err(PlannerError::BadPick(pick.todo_id));
        }
        let assignment = Assignment {
            todo_id: Some(pick.todo_id),
            task: pick.task,
            why: pick.why,
            source: AssignmentSource::Planner,
            at: now,
        };
        info!("assigned: {}", assignment.task);
        self.apply(assignment.clone())?;
        Ok(assignment)
    }

    fn apply(&self, assignment: Assignment) -> Result<(), PlannerError> {
        self.state
            .update(|s| s.assignment = Some(assignment.clone()))?;
        self.start_focus(&assignment)
    }

    fn start_focus(&self, assignment: &Assignment) -> Result<(), PlannerError> {
        let task = assignment.task.clone();
        self.focus
            .store()
            .update(|c| {
                c.task = task;
                c.session_active = true;
            })
            .map_err(|e| PlannerError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    fn end(&self, _now: DateTime<Utc>) -> Result<(), PlannerError> {
        self.state.update(|s| s.assignment = None)?;
        self.focus
            .store()
            .update(|c| c.session_active = false)
            .map_err(|e| PlannerError::Io(std::io::Error::other(e.to_string())))?;
        Ok(())
    }
}

fn local_now_text() -> String {
    Local::now().format("%Y-%m-%d %A %H:%M").to_string()
}

//! Responsibility: Assign the user's task from the list and arbitrate change requests, driving
//! the focus session.
//!
//! It drives the AI focus session: an assignment becomes the focus monitor's
//! task (and starts the session); an approved "end" stops it.

use crate::advisor::{AdvisorProvider, PickRequest, ProposalRequest, ReviewRequest};
use crate::model::{
    Assignment, AssignmentSource, ChangeKind, ChangeRecord, DoneRecord, PlannerConfig,
    PlannerState, PlannerStatus, ProposalInput, ProposalRecord, TodoItem,
};
use ai_focus_rule::store::JsonStore;
use ai_focus_rule::{FocusMonitor, JudgeError};
use chrono::{DateTime, Duration, Local, Utc};
use parking_lot::Mutex;
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
        PlannerStatus {
            active: self.is_active(),
            assignment: state.assignment,
            open_todos: self.open_todos(),
            recent_requests: state.requests.iter().rev().take(10).cloned().collect(),
            recent_proposals: state.proposals.iter().rev().take(10).cloned().collect(),
        }
    }

    /// The user proposes a to-do (from the Focus app or their AI assistant,
    /// named by `via`). The AI reviews it against the life rules; an approved
    /// one joins the open list. Works whether or not the planner is on.
    pub fn propose(
        &self,
        now: DateTime<Utc>,
        input: ProposalInput,
        via: &str,
    ) -> Result<ProposalRecord, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        let title = input.title.split_whitespace().collect::<Vec<_>>().join(" ");
        let notes = input.notes.trim().to_string();
        if title.chars().count() < 2
            || title.chars().count() > MAX_TITLE_CHARS
            || notes.chars().count() > MAX_NOTES_CHARS
        {
            return Err(PlannerError::BadProposal);
        }
        let open = self.open_todos();
        if let Some(duplicate) = open
            .iter()
            .find(|todo| todo.title.trim().eq_ignore_ascii_case(&title))
        {
            return Err(PlannerError::Duplicate(duplicate.title.clone()));
        }
        let state = self.state.get();
        let today = state
            .proposals
            .iter()
            .filter(|proposal| now - proposal.at < Duration::days(1))
            .count();
        if today >= PROPOSALS_PER_DAY
            || open.iter().filter(|todo| todo.proposed).count() >= MAX_OPEN_PROPOSED
        {
            return Err(PlannerError::TooManyProposals);
        }

        let item = TodoItem {
            id: format!("p{}", now.timestamp_millis()),
            title,
            notes,
            deadline: input.deadline,
            priority: input.priority.unwrap_or(3).clamp(1, 5),
            estimate_minutes: input.estimate_minutes.map(|minutes| minutes.min(6000)),
            proposed: true,
        };
        let recent: Vec<ProposalRecord> = state.proposals.iter().rev().cloned().collect();
        let history = self.focus.state().history_text();
        let now_local = local_now_text();
        let life_rules = self.config.get().life_rules;
        let advisor = (self.advisor)(&self.focus.store().get())?;
        let review = advisor.review_proposal(&ProposalRequest {
            now_local: &now_local,
            life_rules: &life_rules,
            item: &item,
            open: &open,
            recent_proposals: &recent,
            history: &history,
        })?;

        let record = ProposalRecord {
            at: now,
            item: item.clone(),
            via: via.trim().chars().take(40).collect(),
            approved: review.approved,
            reply: review.reply,
        };
        self.state.update(|planner_state| {
            if planner_state.proposals.len() >= REQUEST_LOG_CAPACITY {
                planner_state.proposals.pop_front();
            }
            planner_state.proposals.push_back(record.clone());
            if record.approved {
                planner_state.proposed.push(item.clone());
            }
        })?;
        info!(
            "proposal \"{}\" via {} ({}): {}",
            record.item.title,
            record.via,
            if record.approved {
                "approved"
            } else {
                "rejected"
            },
            record.reply
        );
        Ok(record)
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
        let life_rules = self.config.get().life_rules;
        let review = advisor.review(&ReviewRequest {
            now_local: &now_local,
            life_rules: &life_rules,
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
        let life_rules = self.config.get().life_rules;
        let pick = advisor.pick(&PickRequest {
            now_local: &now_local,
            life_rules: &life_rules,
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
        };
        info!("assigned: {}", assignment.task);
        self.apply(assignment.clone())?;
        Ok(assignment)
    }

    fn apply(&self, assignment: Assignment) -> Result<(), PlannerError> {
        self.state
            .update(|planner_state| planner_state.assignment = Some(assignment.clone()))?;
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

    fn end(&self, _now: DateTime<Utc>) -> Result<(), PlannerError> {
        self.state
            .update(|planner_state| planner_state.assignment = None)?;
        self.focus
            .store()
            .update(|config| config.session_active = false)
            .map_err(|error| PlannerError::Io(std::io::Error::other(error.to_string())))?;
        Ok(())
    }
}

fn local_now_text() -> String {
    Local::now().format("%Y-%m-%d %A %H:%M").to_string()
}

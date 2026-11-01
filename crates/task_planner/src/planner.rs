//! Responsibility: Assign the user's task from the list and arbitrate change requests, driving
//! the focus session.
//!
//! It drives the AI focus session: an assignment becomes the focus monitor's
//! task (and starts the session); an approved "end" stops it.
//!
//! Every assignment runs for the time the AI gave it (`until`). When that is up, the task
//! stays until the user reports whether it is finished ("done", reviewed by the AI, or "not
//! finished", which keeps the to-do). After a report, or when nothing is assigned, the next
//! task comes only when the user asks for it ([`TaskPlanner::assign`]); meanwhile the agent
//! locks every app (`machine::lockdown`). Routines (`repeat`) stay on the list when finished.
//!
//! Appointments (accepted by the AI under the owner's appointment rules) are fixed: from
//! their start to their end they are the task, whatever ran before, and they end by
//! themselves. A task done away from the computer (sleep, exercise; or such an appointment)
//! locks every app too, and getting off it early needs an urgent reason.

use crate::advisor::{
    AdvisorProvider, AppointmentRequest, PickRequest, Review, ReviewRequest, ReviewedTodo,
    TodoChangeRequest, WeekPlanAnswer, WeekPlanRequest,
};
use crate::model::{
    Appointment, Assignment, AssignmentSource, AssignmentSpan, ChangeKind, ChangeRecord,
    DoneRecord, Evidence, PlannerConfig, PlannerState, PlannerStatus, PolishItem, ProposalInput,
    TaskNotes, TodoChange, TodoChangeRecord, TodoItem, TodoListWrite, WeekPlan, WeekPlanDay,
    WeekPlanTodo, WhenTimeIsUp,
};
use ai_focus_rule::store::JsonStore;
use ai_focus_rule::{FocusMonitor, JudgeError};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
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
/// Bounds on the time the AI gives one assignment, and the time an approved switch gets
/// when the AI names none.
const MIN_ASSIGNMENT_MINUTES: u32 = 10;
const MAX_ASSIGNMENT_MINUTES: u32 = 720;
const DEFAULT_ASSIGNMENT_MINUTES: u32 = 60;
/// Finished assignments kept for the time spent per to-do (a few weeks).
const ASSIGNMENT_LOG_CAPACITY: usize = 1000;
const MAX_TASK_NOTES_CHARS: usize = 4000;
const MAX_TASK_STEPS: usize = 50;
/// The longest appointment.
const MAX_APPOINTMENT_MINUTES: i64 = 24 * 60;
/// Ended appointments stay in `planner.json` this long, then the next change drops them.
const APPOINTMENTS_KEPT_FOR: Duration = Duration::days(7);

/// Responsibility: Report why an assignment or a change request was refused.
#[derive(Debug, Error)]
pub enum PlannerError {
    #[error("할 일 목록이 비어 있습니다. 먼저 할 일을 추가하세요")]
    NotActive,
    #[error("다른 요청을 아직 처리하고 있습니다")]
    Busy,
    #[error("없거나 이미 끝낸 할 일입니다('{0}')")]
    UnknownTodo(String),
    #[error("이유를 적어 주세요({MIN_REASON_CHARS}글자 이상)")]
    ReasonTooShort,
    #[error("새로 할 일이 비어 있습니다")]
    EmptyTask,
    #[error("AI가 목록에 없는 할 일을 골랐습니다('{0}')")]
    BadPick(String),
    #[error("할 일에 제목을 적어 주세요(제목은 {MAX_TITLE_CHARS}글자, 메모는 {MAX_NOTES_CHARS}글자까지)")]
    BadProposal,
    #[error("\"{0}\"은(는) 이미 목록에 있습니다")]
    Duplicate(String),
    #[error("할 일 변경이 너무 많습니다: 하루 {PROPOSALS_PER_DAY}개, 남은 추가 할 일 {MAX_OPEN_PROPOSED}개까지입니다")]
    TooManyProposals,
    #[error("\"{0}\"은(는) 일찍 끝낸 일을 다듬도록 앱이 만든 할 일입니다. 끝내거나 다듬기 시간을 다 쓰면 없어집니다")]
    MadeByTheApp(String),
    #[error("할 일 시간이 다 됐습니다. 먼저 끝냈는지 보고하세요(끝냈다 또는 다 못 했다)")]
    ReportFirst,
    #[error(
        "\"다 못 했다\"는 할 일 시간이 다 된 뒤에 보고합니다. 그 전에는 \"다른 일 받기\"를 쓰세요"
    )]
    NotTimeYet,
    #[error(
        "시간이 다 됐을 때의 동작은 더 이상 고르지 않습니다. 시간이 다 되면 완료 보고를 합니다"
    )]
    NoLongerUsed,
    #[error("지금 할 일이 없습니다")]
    NothingAssigned,
    #[error("약속에 제목(2~{MAX_TITLE_CHARS}글자)과 시간을 적어 주세요. 끝은 시작보다 늦고 지금보다 뒤여야 하며, 약속은 24시간을 넘지 못합니다")]
    BadAppointment,
    #[error("\"{0}\" 약속과 시간이 겹칩니다")]
    AppointmentOverlaps(String),
    #[error("없는 약속입니다('{0}')")]
    UnknownAppointment(String),
    #[error(
        "사용을 멈춘 동안에는 AI를 쓰는 일을 하지 않습니다. 먼저 \"다시 사용하기\"를 누르세요"
    )]
    Stopped,
    #[error("AI: {0}")]
    Ai(#[from] JudgeError),
    #[error("저장하지 못했습니다: {0}")]
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
    pub(crate) fn open_todos(&self) -> Vec<TodoItem> {
        let state = self.state.get();
        let config = self.config.get();
        let now = Utc::now();
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
            .filter(|todo| todo.repeat || !state.is_done(&todo.id))
            .chain(
                state
                    .polish
                    .iter()
                    .filter(|(_, polish)| now - polish.since < POLISH_KEPT_FOR)
                    .map(|(todo_id, polish)| polish_todo(todo_id, polish)),
            )
            .collect()
    }

    /// Minutes spent on the to-do `todo_id` so far, the running assignment included.
    fn minutes_spent_on(&self, todo_id: &str, now: DateTime<Utc>) -> i64 {
        let state = self.state.get();
        let finished: i64 = state
            .assignment_log
            .iter()
            .filter(|span| span.todo_id.as_deref() == Some(todo_id))
            .map(|span| (span.ended - span.started).num_minutes().max(0))
            .sum();
        let running = state
            .assignment
            .as_ref()
            .filter(|assignment| assignment.todo_id.as_deref() == Some(todo_id))
            .map_or(0, |assignment| (now - assignment.at).num_minutes().max(0));
        finished + running
    }

    /// Appointments that have not ended (and were not ended early), soonest first.
    pub fn appointments(&self, now: DateTime<Utc>) -> Vec<Appointment> {
        let ended = self.state.get().ended_appointments;
        let mut appointments: Vec<Appointment> = self
            .config
            .get()
            .appointments
            .into_iter()
            .filter(|appointment| appointment.end > now && !ended.contains(&appointment.id))
            .collect();
        appointments.sort_by_key(|appointment| appointment.start);
        appointments
    }

    /// The user stopped using the computer ("사용 중지"): every app is locked, and nothing
    /// but the blocking runs (no AI checks, no picks, no appointments starting).
    pub fn stopped(&self) -> bool {
        self.state.get().stopped_at.is_some()
    }

    /// "사용 중지": end the current task (its to-do stays on the list, as if not finished)
    /// and stop everything but the blocking until [`Self::resume_use`].
    pub fn stop_use(&self, now: DateTime<Utc>) -> Result<(), PlannerError> {
        let _busy = self.busy.lock();
        if let Some(appointment_id) = self
            .state
            .get()
            .assignment
            .and_then(|assignment| assignment.appointment_id)
        {
            self.state.update(|planner_state| {
                planner_state.ended_appointments.insert(appointment_id);
            })?;
        }
        self.end(now)?;
        self.state
            .update(|planner_state| planner_state.stopped_at = Some(now))?;
        info!("the user stopped using the computer");
        Ok(())
    }

    /// "다시 사용하기": the stop ends; the next task comes when the user asks for it.
    pub fn resume_use(&self) -> Result<(), PlannerError> {
        self.state
            .update(|planner_state| planner_state.stopped_at = None)?;
        info!("the user uses the computer again");
        Ok(())
    }

    /// The task when it is done away from the computer: the agent locks every app meanwhile.
    pub fn away_from_computer_task(&self) -> Option<String> {
        self.state
            .get()
            .assignment
            .filter(|assignment| assignment.away_from_computer)
            .map(|assignment| assignment.task)
    }

    /// An assignment that ends at `until`, cut off at the start of the next appointment.
    fn cut_at_next_appointment(&self, now: DateTime<Utc>, until: DateTime<Utc>) -> DateTime<Utc> {
        self.appointments(now)
            .iter()
            .map(|appointment| appointment.start)
            .filter(|start| *start > now)
            .min()
            .map_or(until, |start| until.min(start))
    }

    /// Start the appointment that runs now, if it is not the task yet: it replaces whatever
    /// runs (the replaced to-do stays on the list). Ends an appointment that is over or was
    /// removed. Returns the appointment's assignment when one runs.
    fn keep_appointments(&self, now: DateTime<Utc>) -> Result<Option<Assignment>, PlannerError> {
        let appointments = self.appointments(now);
        let current = self.state.get().assignment;
        if let Some(appointment_id) = current
            .as_ref()
            .and_then(|assignment| assignment.appointment_id.as_ref())
        {
            if !appointments
                .iter()
                .any(|appointment| &appointment.id == appointment_id && appointment.runs_at(now))
            {
                info!("appointment {appointment_id} is over");
                self.end(now)?;
            }
        }
        let Some(running) = appointments
            .into_iter()
            .find(|appointment| appointment.runs_at(now))
        else {
            return Ok(None);
        };
        let current = self.state.get().assignment;
        if let Some(current) = current
            .filter(|assignment| assignment.appointment_id.as_deref() == Some(running.id.as_str()))
        {
            return Ok(Some(current));
        }
        let task = if running.notes.trim().is_empty() {
            running.title.clone()
        } else {
            format!("{} — {}", running.title, running.notes.trim())
        };
        let assignment = Assignment {
            todo_id: None,
            task,
            why: "약속 시간입니다".into(),
            source: AssignmentSource::Planner,
            at: now,
            until: Some(running.end),
            when_time_is_up: WhenTimeIsUp::Reassign,
            away_from_computer: running.away_from_computer,
            appointment_id: Some(running.id.clone()),
        };
        info!("appointment started: {}", running.title);
        self.apply(assignment.clone())?;
        Ok(Some(assignment))
    }

    /// There are to-dos but none is assigned: the agent locks every app until the user asks
    /// for their task.
    pub fn waiting_for_task(&self) -> bool {
        self.state.get().assignment.is_none() && self.is_active()
    }

    /// Something left to do: the user cannot type a task freely.
    pub fn is_active(&self) -> bool {
        self.is_active_with(&self.open_todos())
    }

    /// The planner is always on; it has nothing to do only when no to-do is open.
    fn is_active_with(&self, open: &[TodoItem]) -> bool {
        !open.is_empty()
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
        // A routine counts again from its last finish.
        let mut minutes_this_round: BTreeMap<String, i64> = BTreeMap::new();
        for todo in &open_todos {
            let last_done = todo.repeat.then(|| {
                state
                    .done
                    .iter()
                    .filter(|done| done.todo_id == todo.id)
                    .map(|done| done.at)
                    .max()
            });
            let spent: i64 = spans
                .iter()
                .filter(|span| span.todo_id.as_deref() == Some(todo.id.as_str()))
                .map(|span| {
                    let started = match last_done.flatten() {
                        Some(done_at) => span.started.max(done_at),
                        None => span.started,
                    };
                    (span.ended - started).num_minutes().max(0)
                })
                .sum();
            minutes_this_round.insert(todo.id.clone(), spent);
        }
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
            stopped_at: state.stopped_at,
            active: self.is_active_with(&open_todos),
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
            week_plan: state.week_plan.clone(),
            report_due: state
                .assignment
                .as_ref()
                .is_some_and(|assignment| assignment.time_is_up(now)),
            minutes_this_round,
            appointments: self.appointments(now),
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

    /// Ask the AI how much work each of the next seven days needs and how much free time is
    /// left (one AI call), keep the answer and return it. An overview, not a timetable: the
    /// planner still assigns one to-do at a time.
    pub fn plan_week(&self, now: DateTime<Utc>) -> Result<WeekPlan, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        if self.stopped() {
            return Err(PlannerError::Stopped);
        }
        let open = self.open_todos();
        if open.is_empty() {
            return Err(PlannerError::NotActive);
        }
        let state = self.state.get();
        let config = self.config.get();
        // A rolling week (today and the next six days), so that on a Sunday the overview
        // still covers a whole week.
        let today = now.with_timezone(&Local).date_naive();
        let days_left: Vec<NaiveDate> = (0..7)
            .map(|offset| today + Duration::days(offset))
            .collect();
        let with_spent: Vec<(TodoItem, i64)> = open
            .iter()
            .map(|todo| (todo.clone(), self.minutes_spent_on(&todo.id, now)))
            .collect();
        let worked_this_week: Vec<(NaiveDate, i64)> = (0..7)
            .rev()
            .map(|days_ago| {
                let day = today - Duration::days(days_ago);
                (day, minutes_on_day(&state, day, now))
            })
            .collect();
        let recently_done: Vec<DoneRecord> = state.done.iter().rev().take(10).cloned().collect();
        let now_local = local_now_text();
        let advisor = (self.advisor)(&self.focus.store().get())?;
        let answer = advisor.plan_week(&WeekPlanRequest {
            instructions: config.instructions.week_plan_or_default(),
            now_local: &now_local,
            life_rules: &config.life_rules,
            days_left: &days_left,
            open: &with_spent,
            worked_this_week: &worked_this_week,
            recently_done: &recently_done,
            appointments: &self.appointments(now),
        })?;
        let plan = checked_week_plan(now, answer, &days_left, &open);
        self.state
            .update(|planner_state| planner_state.week_plan = Some(plan.clone()))?;
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
        if self.stopped() {
            return Err(PlannerError::Stopped);
        }
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
        let change = clean_change(change, now)?;
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
            TodoChange::AddAppointment { appointment } => {
                if let Some(overlapping) = self.appointments(now).into_iter().find(|existing| {
                    existing.start < appointment.end && appointment.start < existing.end
                }) {
                    return Err(PlannerError::AppointmentOverlaps(overlapping.title));
                }
                None
            }
            TodoChange::RemoveAppointment { appointment_id } => {
                if !self
                    .config
                    .get()
                    .appointments
                    .iter()
                    .any(|existing| &existing.id == appointment_id)
                {
                    return Err(PlannerError::UnknownAppointment(appointment_id.clone()));
                }
                None
            }
            TodoChange::Edit { todo_id, .. } | TodoChange::Remove { todo_id } => {
                if is_polish(todo_id) {
                    return Err(PlannerError::MadeByTheApp(todo_id.clone()));
                }
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
        if matches!(
            change,
            TodoChange::AddAppointment { .. } | TodoChange::RemoveAppointment { .. }
        ) {
            return self.review_appointment_change(now, change, reason, via, &open, &recent);
        }
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
            _ => None,
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
            appointment: None,
        };
        self.log_todo_change(&record)?;
        Ok(record)
    }

    /// The appointment half of [`Self::review_todo_change`]: the AI decides on an addition
    /// by the owner's appointment rules; a removal only frees time and needs no AI.
    fn review_appointment_change(
        &self,
        now: DateTime<Utc>,
        change: TodoChange,
        reason: String,
        via: &str,
        open: &[TodoItem],
        recent: &[TodoChangeRecord],
    ) -> Result<TodoChangeRecord, PlannerError> {
        let config = self.config.get();
        let (approved, reply, ai_failed, appointment) = match &change {
            TodoChange::AddAppointment { appointment } => {
                let now_local = local_now_text();
                let state = self.state.get();
                let review = (self.advisor)(&self.focus.store().get()).and_then(|advisor| {
                    advisor.review_appointment(&AppointmentRequest {
                        instructions: config.instructions.appointment_review_or_default(),
                        now_local: &now_local,
                        appointment_rules: &config.appointment_rules,
                        life_rules: &config.life_rules,
                        appointment,
                        reason: &reason,
                        appointments: &self.appointments(now),
                        open,
                        current: state.assignment.as_ref(),
                        recent_changes: recent,
                    })
                });
                let (approved, reply, ai_failed, judged_away) = match review {
                    Ok(review) => (
                        review.approved,
                        review.reply,
                        false,
                        review.away_from_computer,
                    ),
                    Err(error) => (
                        false,
                        format!("The AI could not review this: {error}"),
                        true,
                        false,
                    ),
                };
                let accepted = Appointment {
                    id: format!("a{}", now.timestamp_millis()),
                    title: appointment.title.clone(),
                    notes: appointment.notes.clone(),
                    start: appointment.start,
                    end: appointment.end,
                    away_from_computer: appointment.away_from_computer.unwrap_or(judged_away),
                };
                (approved, reply, ai_failed, Some(accepted))
            }
            TodoChange::RemoveAppointment { appointment_id } => (
                true,
                "약속을 지웠습니다.".to_string(),
                false,
                config
                    .appointments
                    .iter()
                    .find(|existing| &existing.id == appointment_id)
                    .cloned(),
            ),
            _ => unreachable!("only appointment changes come here"),
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
            before: None,
            after: None,
            scheduled_update: None,
            appointment,
        };
        self.log_todo_change(&record)?;
        Ok(record)
    }

    fn log_todo_change(&self, record: &TodoChangeRecord) -> Result<(), PlannerError> {
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
        Ok(())
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
            // Only the fields the user changed: a change scheduled for hours later must not
            // undo edits made meanwhile.
            TodoChange::Edit { todo_id, todo } => {
                let outside_the_file = !in_settings(todo_id);
                let mut writes = Vec::new();
                if outside_the_file {
                    writes.push(TodoListWrite::Dismiss(todo_id.clone()));
                }
                writes.push(TodoListWrite::Merge {
                    todo_id: todo_id.clone(),
                    changed_fields: edited_fields(todo),
                    base: record.before.clone().filter(|_| outside_the_file),
                });
                writes
            }
            TodoChange::Remove { todo_id } if in_settings(todo_id) => {
                vec![TodoListWrite::Remove(todo_id.clone())]
            }
            // Kept outside the file, unless an edit has put it there by the time this applies.
            TodoChange::Remove { todo_id } => vec![
                TodoListWrite::Dismiss(todo_id.clone()),
                TodoListWrite::Remove(todo_id.clone()),
            ],
            // Long-past appointments go with the change.
            TodoChange::AddAppointment { .. } => self
                .config
                .get()
                .appointments
                .iter()
                .filter(|existing| record.at - existing.end > APPOINTMENTS_KEPT_FOR)
                .map(|existing| TodoListWrite::RemoveAppointment(existing.id.clone()))
                .chain(
                    record
                        .appointment
                        .clone()
                        .map(TodoListWrite::PutAppointment),
                )
                .collect(),
            TodoChange::RemoveAppointment { appointment_id } => {
                vec![TodoListWrite::RemoveAppointment(appointment_id.clone())]
            }
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

    /// The user asks for their task ("할 일 받기"): the current one when there is one, or a new
    /// one the AI picks. A task whose time is up must be reported first.
    pub fn assign(&self, now: DateTime<Utc>) -> Result<Assignment, PlannerError> {
        let _busy = self.busy.lock();
        // Asking for a task ends a stop.
        if self.stopped() {
            self.state
                .update(|planner_state| planner_state.stopped_at = None)?;
        }
        if let Some(appointment) = self.keep_appointments(now)? {
            self.start_focus(&appointment)?;
            return Ok(appointment);
        }
        if !self.is_active() {
            return Err(PlannerError::NotActive);
        }
        if let Some(previous) = self.state.get().assignment {
            let still_open = previous
                .todo_id
                .as_deref()
                .is_none_or(|id| self.open_todos().iter().any(|todo| todo.id == id));
            if still_open {
                if previous.time_is_up(now) {
                    return Err(PlannerError::ReportFirst);
                }
                self.start_focus(&previous)?;
                return Ok(previous);
            }
            // Its to-do left the list (removed, or finished elsewhere): pick anew.
            self.end(now)?;
        }
        self.pick_and_apply(now, None)
    }

    /// Run every few seconds by the agent: start an appointment whose time has come and end
    /// one that is over, and after logon (an agent restart) go on with the assignment there
    /// is. Never picks a to-do.
    pub fn resume(&self, now: DateTime<Utc>) -> Result<(), PlannerError> {
        let Some(_busy) = self.busy.try_lock() else {
            return Ok(());
        };
        if self.stopped() {
            return Ok(());
        }
        self.keep_appointments(now)?;
        if let Some(current) = self.state.get().assignment {
            if !self.focus.store().get().session_active {
                self.start_focus(&current)?;
            }
        }
        Ok(())
    }

    /// The user asks to change the assignment. Returns the AI's decision;
    /// an approved request is applied before returning.
    pub fn request_change(
        &self,
        now: DateTime<Utc>,
        request: ChangeKind,
        reason: &str,
        evidence: Evidence,
    ) -> Result<ChangeRecord, PlannerError> {
        let _busy = self.busy.try_lock().ok_or(PlannerError::Busy)?;
        self.check_change(now, &request, reason)?;
        let reason = reason.trim();
        let evidence_text: String = evidence.text.trim().chars().take(MAX_NOTES_CHARS).collect();
        if request == ChangeKind::NotFinished {
            // Keeping the to-do on the list loosens nothing: no need to ask the AI.
            return self.record_and_apply(
                now,
                request,
                reason,
                Review {
                    approved: true,
                    reply: "할 일은 목록에 남습니다. 준비되면 \"할 일 받기\"를 누르세요.".into(),
                    minutes: 0,
                    outcome: String::new(),
                },
                &self.open_todos(),
                (evidence_text, false),
            );
        }
        let open = self.open_todos();
        let target_title = match &request {
            ChangeKind::SwitchTodo { todo_id } => open
                .iter()
                .find(|todo| &todo.id == todo_id)
                .map(|todo| todo.title.clone()),
            _ => None,
        };

        let state = self.state.get();
        let recent: Vec<ChangeRecord> = state.requests.iter().rev().cloned().collect();
        let history = self.focus.state().history_text();
        let now_local = local_now_text();
        let advisor = (self.advisor)(&self.focus.store().get())?;
        let config = self.config.get();
        let current_todo = state
            .assignment
            .as_ref()
            .and_then(|assignment| assignment.todo_id.clone());
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
            evidence: &evidence_text,
            screenshot: evidence.screenshot_jpeg.as_deref(),
            planned_minutes: state.assignment.as_ref().and_then(|assignment| {
                assignment
                    .until
                    .map(|until| (until - assignment.at).num_minutes())
            }),
            elapsed_minutes: state
                .assignment
                .as_ref()
                .map_or(0, |assignment| (now - assignment.at).num_minutes().max(0)),
            spent_on_todo_minutes: current_todo
                .as_deref()
                .map_or(0, |todo_id| self.minutes_spent_on(todo_id, now)),
            estimate_minutes: current_todo
                .as_deref()
                .and_then(|todo_id| open.iter().find(|todo| todo.id == todo_id))
                .and_then(|todo| todo.estimate_minutes),
        })?;
        let screenshot_attached = evidence.screenshot_jpeg.is_some();
        self.record_and_apply(
            now,
            request,
            reason,
            review,
            &open,
            (evidence_text, screenshot_attached),
        )
    }

    /// Refuse a change request that cannot be sent to the AI: the planner is off, the reason
    /// is too short, or the request names no open to-do or an empty task. Callers may check
    /// before doing slow work for the request (such as waiting for a proof screenshot).
    pub fn check_change(
        &self,
        now: DateTime<Utc>,
        request: &ChangeKind,
        reason: &str,
    ) -> Result<(), PlannerError> {
        let open = self.open_todos();
        let assignment = self.state.get().assignment;
        // An appointment runs even with an empty list.
        if !self.is_active_with(&open) && assignment.is_none() {
            return Err(PlannerError::NotActive);
        }
        if reason.trim().chars().count() < MIN_REASON_CHARS {
            return Err(PlannerError::ReasonTooShort);
        }
        let time_is_up = assignment
            .as_ref()
            .is_some_and(|assignment| assignment.time_is_up(now));
        match request {
            ChangeKind::SetWhenTimeIsUp { .. } => return Err(PlannerError::NoLongerUsed),
            ChangeKind::Done | ChangeKind::NotFinished | ChangeKind::Repick | ChangeKind::End
                if assignment.is_none() =>
            {
                return Err(PlannerError::NothingAssigned)
            }
            ChangeKind::NotFinished if !time_is_up => return Err(PlannerError::NotTimeYet),
            // Nothing else before the report.
            ChangeKind::Done | ChangeKind::NotFinished => {}
            _ if time_is_up => return Err(PlannerError::ReportFirst),
            _ => {}
        }
        match request {
            ChangeKind::SwitchTodo { todo_id } if !open.iter().any(|todo| &todo.id == todo_id) => {
                Err(PlannerError::UnknownTodo(todo_id.clone()))
            }
            ChangeKind::SwitchFree { task } if task.trim().is_empty() => {
                Err(PlannerError::EmptyTask)
            }
            _ => Ok(()),
        }
    }

    /// Log the decision on a change request and apply it when approved.
    fn record_and_apply(
        &self,
        now: DateTime<Utc>,
        request: ChangeKind,
        reason: &str,
        review: Review,
        open: &[TodoItem],
        (evidence, screenshot_attached): (String, bool),
    ) -> Result<ChangeRecord, PlannerError> {
        let state = self.state.get();
        let record = ChangeRecord {
            evidence,
            screenshot_attached,
            outcome: review.outcome.clone(),
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
            self.apply_change(now, &request, reason, &review, open)?;
        }
        Ok(record)
    }

    fn apply_change(
        &self,
        now: DateTime<Utc>,
        request: &ChangeKind,
        reason: &str,
        review: &Review,
        open: &[TodoItem],
    ) -> Result<(), PlannerError> {
        let reply = review.reply.as_str();
        // An appointment left early (approved) does not start again.
        if let Some(appointment_id) = self
            .state
            .get()
            .assignment
            .and_then(|assignment| assignment.appointment_id)
        {
            self.state.update(|planner_state| {
                planner_state.ended_appointments.insert(appointment_id);
                let still_listed: Vec<String> = self
                    .config
                    .get()
                    .appointments
                    .iter()
                    .map(|appointment| appointment.id.clone())
                    .collect();
                planner_state
                    .ended_appointments
                    .retain(|id| still_listed.contains(id));
            })?;
        }
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
                    // Open-ended work finished early keeps the planned time that is left,
                    // to be spent improving it later.
                    // The to-do's estimate against all the time spent on it; without an
                    // estimate, this assignment's planned time against the time it ran.
                    let polish_left =
                        if review.outcome == "open_ended" && !is_polish(&todo.id) && !todo.repeat {
                            match todo.estimate_minutes {
                                Some(estimate) => {
                                    i64::from(estimate) - self.minutes_spent_on(&todo.id, now)
                                }
                                None => current
                                    .as_ref()
                                    .and_then(|assignment| {
                                        assignment.until.map(|until| {
                                            (until - assignment.at).num_minutes()
                                                - (now - assignment.at).num_minutes().max(0)
                                        })
                                    })
                                    .unwrap_or(0),
                            }
                        } else {
                            0
                        };
                    let finished = todo.clone();
                    self.state.update(|planner_state| {
                        planner_state.done.push(record);
                        if let Some(base_id) = finished.id.strip_prefix(POLISH_PREFIX) {
                            planner_state.polish.remove(base_id);
                        } else if polish_left >= MIN_POLISH_MINUTES {
                            planner_state.polish.insert(
                                finished.id.clone(),
                                PolishItem {
                                    todo: finished.clone(),
                                    minutes_left: polish_left,
                                    since: now,
                                },
                            );
                        }
                    })?;
                }
                // The next task comes when the user asks for it ("할 일 받기").
                self.end(now)?;
            }
            ChangeKind::NotFinished => self.end(now)?,
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
                    until: Some(self.cut_at_next_appointment(now, end_after(now, minutes))),
                    when_time_is_up: WhenTimeIsUp::Reassign,
                    away_from_computer: todo.away_from_computer.unwrap_or(false),
                    appointment_id: None,
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
                    until: Some(self.cut_at_next_appointment(now, end_after(now, minutes))),
                    when_time_is_up: WhenTimeIsUp::Reassign,
                    away_from_computer: false,
                    appointment_id: None,
                })?;
            }
            ChangeKind::Repick => {
                if let Err(error) = self.pick_and_apply(now, Some(reason)) {
                    tracing::warn!("could not pick another task: {error}");
                    return Err(error);
                }
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

    /// Ask the AI for the next task and assign it; `repick_reason` is set when the user asked
    /// for something else.
    fn pick_and_apply(
        &self,
        now: DateTime<Utc>,
        repick_reason: Option<&str>,
    ) -> Result<Assignment, PlannerError> {
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
            repick_reason,
            appointments: &self.appointments(now),
        })?;
        let Some(picked) = open.iter().find(|todo| todo.id == pick.todo_id) else {
            return Err(PlannerError::BadPick(pick.todo_id));
        };
        let assignment = Assignment {
            away_from_computer: picked.away_from_computer.unwrap_or(pick.away_from_computer),
            todo_id: Some(pick.todo_id),
            task: pick.task,
            why: pick.why,
            source: AssignmentSource::Planner,
            at: now,
            until: Some(self.cut_at_next_appointment(now, end_after(now, pick.minutes))),
            when_time_is_up: WhenTimeIsUp::Reassign,
            appointment_id: None,
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
        // Blocks belong to the to-do (see `SavedBlocks`).
        let scope = assignment
            .todo_id
            .clone()
            .unwrap_or_else(|| "free".to_string());
        self.focus
            .store()
            .update(|config| {
                config.task = task;
                config.task_scope = scope;
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

/// Log how long the current assignment ran, as it ends at `ended`; time spent improving a
/// finished to-do comes off its time left.
fn close_span(planner_state: &mut PlannerState, ended: DateTime<Utc>) {
    planner_state
        .polish
        .retain(|_, polish| ended - polish.since < POLISH_KEPT_FOR);
    if let Some(current) = &planner_state.assignment {
        if let Some(base_id) = current
            .todo_id
            .as_deref()
            .and_then(|todo_id| todo_id.strip_prefix(POLISH_PREFIX))
        {
            let spent = (ended - current.at).num_minutes().max(0);
            if let Some(polish) = planner_state.polish.get_mut(base_id) {
                polish.minutes_left -= spent;
                if polish.minutes_left < MIN_POLISH_MINUTES {
                    planner_state.polish.remove(base_id);
                }
            }
        }
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

/// Minutes of assignments (the running one included) on the local day `day`.
fn minutes_on_day(state: &PlannerState, day: NaiveDate, now: DateTime<Utc>) -> i64 {
    let Some(start) = day
        .and_hms_opt(0, 0, 0)
        .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
        .map(|midnight| midnight.with_timezone(&Utc))
    else {
        return 0;
    };
    let end = start + Duration::days(1);
    let running = state
        .assignment
        .as_ref()
        .map(|assignment| (assignment.at, now));
    state
        .assignment_log
        .iter()
        .map(|span| (span.started, span.ended))
        .chain(running)
        .map(|(started, ended)| (ended.min(end) - started.max(start)).num_minutes().max(0))
        .sum()
}

/// The AI's week answer, kept to what can be trusted: the seven days asked about only (each once), to-dos
/// from the list only, minutes within a day, totals summed here.
fn checked_week_plan(
    now: DateTime<Utc>,
    answer: WeekPlanAnswer,
    days_left: &[NaiveDate],
    open: &[TodoItem],
) -> WeekPlan {
    const MINUTES_PER_DAY: i64 = 24 * 60;
    let mut days: Vec<WeekPlanDay> = Vec::new();
    for answer_day in answer.days {
        let Ok(date) = answer_day.date.trim().parse::<NaiveDate>() else {
            continue;
        };
        if !days_left.contains(&date) || days.iter().any(|day| day.date == Some(date)) {
            continue;
        }
        let todos = answer_day
            .todos
            .into_iter()
            .filter(|planned| open.iter().any(|todo| todo.id == planned.todo_id))
            .map(|planned| WeekPlanTodo {
                todo_id: planned.todo_id,
                minutes: planned.minutes.clamp(0, MINUTES_PER_DAY) as u32,
            })
            .collect();
        let work_minutes = answer_day.work_minutes.clamp(0, MINUTES_PER_DAY);
        let free_minutes = answer_day
            .free_minutes
            .clamp(0, MINUTES_PER_DAY - work_minutes);
        days.push(WeekPlanDay {
            date: Some(date),
            work_minutes: work_minutes as u32,
            free_minutes: free_minutes as u32,
            todos,
            note: answer_day.note.trim().chars().take(300).collect(),
        });
    }
    days.sort_by_key(|day| day.date);
    WeekPlan {
        at: Some(now),
        work_minutes_total: days.iter().map(|day| day.work_minutes).sum(),
        free_minutes_total: days.iter().map(|day| day.free_minutes).sum(),
        days,
        summary: answer.summary.trim().chars().take(1000).collect(),
        warnings: answer
            .warnings
            .into_iter()
            .map(|warning| warning.trim().chars().take(300).collect::<String>())
            .filter(|warning| !warning.is_empty())
            .take(10)
            .collect(),
    }
}

/// Ids of "Improve: …" to-dos start with this, followed by the finished to-do's id.
const POLISH_PREFIX: &str = "polish:";
/// Less time left than this is not worth an "Improve: …" to-do.
const MIN_POLISH_MINUTES: i64 = 10;
/// An "Improve: …" to-do not done within this time is dropped: work finished that long ago
/// has usually been handed in or no longer matters.
const POLISH_KEPT_FOR: Duration = Duration::days(14);

fn is_polish(todo_id: &str) -> bool {
    todo_id.starts_with(POLISH_PREFIX)
}

/// The "Improve: …" to-do for a finished to-do with time left.
fn polish_todo(todo_id: &str, polish: &PolishItem) -> TodoItem {
    TodoItem {
        id: format!("{POLISH_PREFIX}{todo_id}"),
        title: format!("Improve: {}", polish.todo.title),
        notes: format!(
            "Finished earlier than planned; up to {} more minutes to make it better. {}",
            polish.minutes_left, polish.todo.notes
        )
        .trim()
        .to_string(),
        deadline: None,
        priority: polish.todo.priority,
        estimate_minutes: u32::try_from(polish.minutes_left).ok(),
        proposed: polish.todo.proposed,
        repeat: false,
        away_from_computer: polish.todo.away_from_computer,
    }
}

/// `now` plus `minutes`, kept within the bounds an assignment may run.
fn end_after(now: DateTime<Utc>, minutes: u32) -> DateTime<Utc> {
    now + Duration::minutes(minutes.clamp(MIN_ASSIGNMENT_MINUTES, MAX_ASSIGNMENT_MINUTES) as i64)
}

/// Trim the texts of a change and refuse an addition without a usable title.
fn clean_change(change: TodoChange, now: DateTime<Utc>) -> Result<TodoChange, PlannerError> {
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
        TodoChange::AddAppointment { mut appointment } => {
            appointment.title = appointment
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            appointment.notes = appointment.notes.trim().to_string();
            let minutes = (appointment.end - appointment.start).num_minutes();
            let title_length = appointment.title.chars().count();
            if !(2..=MAX_TITLE_CHARS).contains(&title_length)
                || appointment.notes.chars().count() > MAX_NOTES_CHARS
                || minutes < 1
                || minutes > MAX_APPOINTMENT_MINUTES
                || appointment.end <= now
            {
                return Err(PlannerError::BadAppointment);
            }
            TodoChange::AddAppointment { appointment }
        }
        other @ (TodoChange::Remove { .. } | TodoChange::RemoveAppointment { .. }) => other,
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
    let judged_away = filled.map(|filled| filled.away_from_computer);
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
        repeat: draft.repeat.unwrap_or(false),
        away_from_computer: draft.away_from_computer.or(judged_away),
    }
}

/// `before` with the fields the user set in `draft`.
fn edited(before: &TodoItem, draft: &ProposalInput) -> TodoItem {
    let mut item = serde_json::to_value(before).unwrap_or(Value::Null);
    if let Value::Object(item_fields) = &mut item {
        item_fields.extend(edited_fields(draft));
    }
    serde_json::from_value(item).unwrap_or_else(|_| before.clone())
}

/// The fields of a to-do that `draft` sets, as `planner.json` stores them: an empty title or
/// notes and a missing value leave the field as it is; an estimate of 0 clears it.
fn edited_fields(draft: &ProposalInput) -> Map<String, Value> {
    let mut changed_fields = Map::new();
    if !draft.title.is_empty() {
        changed_fields.insert("title".into(), json!(draft.title));
    }
    if !draft.notes.is_empty() {
        changed_fields.insert("notes".into(), json!(draft.notes));
    }
    if let Some(deadline) = draft.deadline {
        changed_fields.insert("deadline".into(), json!(deadline));
    }
    if let Some(priority) = draft.priority {
        changed_fields.insert("priority".into(), json!(priority.clamp(1, 5)));
    }
    if let Some(minutes) = draft.estimate_minutes {
        changed_fields.insert(
            "estimate_minutes".into(),
            json!((minutes > 0).then_some(minutes.min(6000))),
        );
    }
    if let Some(repeat) = draft.repeat {
        changed_fields.insert("repeat".into(), json!(repeat));
    }
    if let Some(away) = draft.away_from_computer {
        changed_fields.insert("away_from_computer".into(), json!(away));
    }
    changed_fields
}

fn local_now_text() -> String {
    Local::now().format("%Y-%m-%d %A %H:%M").to_string()
}

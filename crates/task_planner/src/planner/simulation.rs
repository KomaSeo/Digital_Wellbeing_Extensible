//! Responsibility: Run the planner in a sandbox with a virtual clock, so the owner can try the
//! life rules before and after applying them: the AI picks tasks and reviews requests and
//! to-do changes as it would, while the user plays what happens (the owner: "사용자가
//! 시뮬레이션을 돌리면서 가상으로 이 프로그램을 사용해 볼거야 중간에 사용자가 일을 안 해버릴 수도
//! 있고, 주어진 일이 생각보다 오래/적게 걸릴 수도 있고 …").
//!
//! - Each sandbox is a [`TaskPlanner`] over copies of `planner.json` and `planner_state.json`
//!   in memory, with a focus check that watches nothing: nothing a simulation does reaches the
//!   real files, the window check or the blocks.
//! - When the rules being edited differ from the rules in force, two sandboxes run side by
//!   side, one with each; every action goes to both, each at its own virtual clock.
//! - The AI is the real one: the calls cost money and are labeled "simulation: …" in the AI
//!   cost statistics.

use super::{TaskPlanner, MAX_NOTES_CHARS};
use crate::advisor::{
    Advisor, AdvisorProvider, AppointmentRequest, AppointmentReview, Pick, PickRequest, Review,
    ReviewRequest, TodoChangeRequest, TodoReview, WeekPlanAnswer, WeekPlanRequest,
};
use crate::focus_link::{FocusLink, WatchedTask};
use crate::model::{
    Assignment, ChangeKind, Evidence, PlannerConfig, PlannerInstructions, PlannerState, TodoChange,
    TodoListWrite,
};
use ai_client::{AiSettings, JudgeError};
use chrono::{DateTime, Duration, Local, Utc};
use json_store::JsonStore;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Steps one automatic run takes at most (each is one or two AI calls per sandbox).
pub const MAX_AUTO_STEPS: usize = 40;
/// The virtual clock moves in steps of this many minutes, so appointments start and end on
/// the way.
const STEP_MINUTES: i64 = 5;
/// The extra time a "takes longer" event asks for.
const LATE_EXTRA_MINUTES: u32 = 30;
/// The source the sandbox's records name.
const VIA: &str = "simulation";

/// Responsibility: Hold the rules a simulation runs with: the life rules, the appointment
/// rules and the AI instructions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleSet {
    pub life_rules: String,
    pub appointment_rules: String,
    pub instructions: PlannerInstructions,
}

impl RuleSet {
    /// The rules of `config`.
    pub fn of(config: &PlannerConfig) -> Self {
        Self {
            life_rules: config.life_rules.clone(),
            appointment_rules: config.appointment_rules.clone(),
            instructions: config.instructions.clone(),
        }
    }

    fn apply_to(&self, config: &mut PlannerConfig) {
        config.life_rules = self.life_rules.clone();
        config.appointment_rules = self.appointment_rules.clone();
        config.instructions = self.instructions.clone();
    }
}

/// Responsibility: Say how a simulation starts: when, in which situation, and with which
/// rules besides the ones in force.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationSetup {
    /// The virtual time it starts at.
    pub at: DateTime<Utc>,
    /// The situation the user describes next to "할 일 받기".
    #[serde(default)]
    pub situation: String,
    /// The rules being edited. `None`, or the same as in force: only the rules in force.
    #[serde(default)]
    pub draft: Option<RuleSet>,
}

/// Responsibility: Name something that happens to the current task, which the simulation
/// plays as time passing and a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SimulationEvent {
    /// Done after about 60% of the time given.
    Early,
    /// Done when the time is up.
    OnTime,
    /// The time is up and 30 more minutes are asked for.
    Late,
    /// The time passes without the work ("다 못 했다").
    Skipped,
    /// After 15 minutes the user does not know how to go on and asks for another task.
    Stuck,
}

/// Responsibility: Say how often an automatic run plays something other than "done on time".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Variability {
    /// Every task is done on time.
    #[default]
    None,
    /// About one task in four goes otherwise.
    Some,
    /// About one task in two goes otherwise.
    Often,
}

/// Responsibility: Name one thing the user does in a simulation; it goes to every sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SimulationAction {
    /// "할 일 받기".
    Assign,
    /// The virtual clock moves on.
    Advance { minutes: i64 },
    /// A change request, as in the Work tab.
    Request {
        request: ChangeKind,
        #[serde(default)]
        reason: String,
    },
    /// A to-do or appointment change, as in the To-do tab.
    TodoChange {
        change: TodoChange,
        #[serde(default)]
        reason: String,
    },
    /// Something happens to the current task.
    Event { event: SimulationEvent },
    /// Play tasks one after another until `until` (local virtual time).
    Auto {
        until: DateTime<Utc>,
        #[serde(default)]
        variability: Variability,
    },
}

/// Responsibility: Name the kind of a line in a sandbox's log (the Focus app colors by it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogKind {
    Time,
    Pick,
    Approved,
    Refused,
    Event,
    Problem,
}

/// Responsibility: Hold one line of a sandbox's log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at: DateTime<Utc>,
    pub kind: LogKind,
    pub text: String,
}

/// Responsibility: Show one sandbox: its rules' name, virtual clock, current task, log and
/// summary.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxView {
    pub label: String,
    pub clock: DateTime<Utc>,
    pub task: Option<String>,
    pub task_until: Option<DateTime<Utc>>,
    pub log: Vec<LogEntry>,
    /// AI calls this sandbox made.
    pub ai_calls: usize,
    pub summary: Vec<String>,
}

/// Responsibility: Show a simulation: whether an action is running, and every sandbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SimulationView {
    pub running: bool,
    pub started_at: DateTime<Utc>,
    /// Changes of the to-do list are reviewed by the AI (strict mode).
    pub review: bool,
    pub sandboxes: Vec<SandboxView>,
}

/// Responsibility: Stand in for the focus check in a sandbox: it gives the AI settings and
/// watches nothing.
struct SandboxFocus {
    settings: AiSettings,
    watched: Mutex<Option<WatchedTask>>,
    resting: AtomicBool,
}

impl FocusLink for SandboxFocus {
    fn ai_settings(&self) -> AiSettings {
        self.settings.clone()
    }

    fn recent_activity(&self) -> String {
        String::new()
    }

    fn watched(&self) -> Option<WatchedTask> {
        self.watched
            .lock()
            .map(|watched| watched.clone())
            .unwrap_or(None)
    }

    fn watch(&self, task: Option<WatchedTask>) -> Result<(), String> {
        if let Ok(mut watched) = self.watched.lock() {
            *watched = task;
        }
        Ok(())
    }

    fn rest(&self, resting: bool) -> Result<(), String> {
        self.resting.store(resting, Ordering::SeqCst);
        Ok(())
    }

    fn resting(&self) -> bool {
        self.resting.load(Ordering::SeqCst)
    }
}

/// Responsibility: Count a sandbox's AI calls while passing them on.
struct CountingAdvisor {
    inner: Arc<dyn Advisor>,
    calls: Arc<AtomicUsize>,
}

impl CountingAdvisor {
    fn count(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

impl Advisor for CountingAdvisor {
    fn pick(&self, request: &PickRequest) -> Result<Pick, JudgeError> {
        self.count();
        self.inner.pick(request)
    }

    fn review(&self, request: &ReviewRequest) -> Result<Review, JudgeError> {
        self.count();
        self.inner.review(request)
    }

    fn review_todo_change(&self, request: &TodoChangeRequest) -> Result<TodoReview, JudgeError> {
        self.count();
        self.inner.review_todo_change(request)
    }

    fn plan_week(&self, request: &WeekPlanRequest) -> Result<WeekPlanAnswer, JudgeError> {
        self.count();
        self.inner.plan_week(request)
    }

    fn review_appointment(
        &self,
        request: &AppointmentRequest,
    ) -> Result<AppointmentReview, JudgeError> {
        self.count();
        self.inner.review_appointment(request)
    }
}

/// Responsibility: Give an automatic run its "luck": the same sequence in every sandbox, so
/// both rule sets face the same events.
struct Luck(u64);

impl Luck {
    fn next(&mut self) -> u64 {
        // xorshift64
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn event(&mut self, variability: Variability) -> SimulationEvent {
        let roll = self.next() % 100;
        let (on_time, step) = match variability {
            Variability::None => return SimulationEvent::OnTime,
            Variability::Some => (76, 6),
            Variability::Often => (52, 12),
        };
        match roll {
            roll if roll < on_time => SimulationEvent::OnTime,
            roll if roll < on_time + step => SimulationEvent::Early,
            roll if roll < on_time + 2 * step => SimulationEvent::Late,
            roll if roll < on_time + 3 * step => SimulationEvent::Skipped,
            _ => SimulationEvent::Stuck,
        }
    }
}

/// Responsibility: Hold one sandbox: its planner, virtual clock and log.
struct Sandbox {
    label: String,
    planner: TaskPlanner,
    start: DateTime<Utc>,
    clock: DateTime<Utc>,
    log: Vec<LogEntry>,
    calls: Arc<AtomicUsize>,
    luck: Luck,
    /// What the Focus app sees, updated at every log line, so a long run shows its progress
    /// while the sandbox is busy.
    shown: Arc<Mutex<SandboxView>>,
}

/// Responsibility: Hold a running simulation: its sandboxes, and whether an action runs or
/// was asked to stop.
pub struct Simulation {
    sandboxes: Vec<Mutex<Sandbox>>,
    /// Each sandbox's [`Sandbox::shown`].
    shown: Vec<Arc<Mutex<SandboxView>>>,
    running: AtomicBool,
    stop: AtomicBool,
    review: bool,
    started_at: DateTime<Utc>,
}

impl TaskPlanner {
    /// A simulation over copies of this planner's files, at `setup.at`. `advisor` asks the AI
    /// (labeled for the statistics); `review`: to-do changes go to the AI (strict mode).
    pub fn simulation(
        &self,
        setup: SimulationSetup,
        advisor: AdvisorProvider,
        review: bool,
    ) -> Simulation {
        let config = self.config.get();
        let in_force = RuleSet::of(&config);
        let mut rule_sets = Vec::new();
        if let Some(draft) = setup.draft.filter(|draft| *draft != in_force) {
            rule_sets.push(("고치는 중인 규칙".to_string(), draft));
        }
        rule_sets.push(("지금 규칙".to_string(), in_force));
        let settings = self.focus.ai_settings();
        let situation: String = setup
            .situation
            .trim()
            .chars()
            .take(MAX_NOTES_CHARS)
            .collect();
        let sandboxes = rule_sets
            .into_iter()
            .map(|(label, rules)| {
                let mut sandbox_config = config.clone();
                rules.apply_to(&mut sandbox_config);
                let mut state: PlannerState = self.state.get();
                state.assignment = None;
                state.cheat = None;
                state.situation = situation.clone();
                let calls = Arc::new(AtomicUsize::new(0));
                let counted = Arc::clone(&calls);
                let advisor = Arc::clone(&advisor);
                let provider: AdvisorProvider = Arc::new(move |settings: &AiSettings| {
                    Ok(Arc::new(CountingAdvisor {
                        inner: advisor(settings)?,
                        calls: Arc::clone(&counted),
                    }) as Arc<dyn Advisor>)
                });
                let planner = TaskPlanner::new(
                    JsonStore::in_memory(sandbox_config),
                    JsonStore::in_memory(state),
                    Arc::new(SandboxFocus {
                        settings: settings.clone(),
                        watched: Mutex::new(None),
                        resting: AtomicBool::new(false),
                    }),
                    provider,
                );
                let mut sandbox = Sandbox {
                    label,
                    planner,
                    start: setup.at,
                    clock: setup.at,
                    log: Vec::new(),
                    calls,
                    luck: Luck(setup.at.timestamp().unsigned_abs() | 1),
                    shown: Arc::new(Mutex::new(SandboxView::default())),
                };
                sandbox.note(
                    LogKind::Time,
                    if situation.is_empty() {
                        "시뮬레이션을 시작했습니다.".to_string()
                    } else {
                        format!("시뮬레이션을 시작했습니다. 상황: {situation}")
                    },
                );
                sandbox
            })
            .collect::<Vec<Sandbox>>();
        Simulation {
            shown: sandboxes
                .iter()
                .map(|sandbox| Arc::clone(&sandbox.shown))
                .collect(),
            sandboxes: sandboxes.into_iter().map(Mutex::new).collect(),
            running: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            review,
            started_at: setup.at,
        }
    }
}

impl Simulation {
    /// An action runs now.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Say at once that an action runs (before its thread starts).
    pub fn mark_running(&self) {
        self.running.store(true, Ordering::SeqCst);
    }

    /// Ask an automatic run to stop after its current step.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Play `action` in every sandbox at once (each on its own thread), until it is done or
    /// `stop_when` says to stop (the AI budget is used up). Returns at the end.
    pub fn run(&self, action: &SimulationAction, stop_when: &(dyn Fn() -> bool + Sync)) {
        self.running.store(true, Ordering::SeqCst);
        self.stop.store(false, Ordering::SeqCst);
        let stopped = || self.stop.load(Ordering::SeqCst) || stop_when();
        std::thread::scope(|scope| {
            for sandbox in &self.sandboxes {
                let stopped = &stopped;
                scope.spawn(move || {
                    if let Ok(mut sandbox) = sandbox.lock() {
                        sandbox.act(action, self.review, stopped);
                        sandbox.publish();
                    }
                });
            }
        });
        self.running.store(false, Ordering::SeqCst);
    }

    /// What the Focus app shows (while an action runs, as far as it got).
    pub fn view(&self) -> SimulationView {
        let sandboxes = self
            .shown
            .iter()
            .map(|shown| shown.lock().map(|view| view.clone()).unwrap_or_default())
            .collect();
        SimulationView {
            running: self.running(),
            started_at: self.started_at,
            review: self.review,
            sandboxes,
        }
    }
}

/// `time` in local time, "HH:MM".
fn clock_text(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M").to_string()
}

/// How the request reads in a log line.
fn request_label(request: &ChangeKind) -> String {
    match request {
        ChangeKind::Done => "끝냈다".into(),
        ChangeKind::NotFinished => "다 못 했다".into(),
        ChangeKind::Repick => "다른 일 받기".into(),
        ChangeKind::End => "AI 검사 멈추기".into(),
        ChangeKind::Extend { minutes } => format!("{minutes}분 연장하기"),
        ChangeKind::AlreadyDone => "이미 처리했어요".into(),
        ChangeKind::Changed => "일이 변경되었어요".into(),
        other => format!("{other:?}"),
    }
}

/// How a to-do change reads in a log line.
fn todo_change_label(change: &TodoChange) -> String {
    match change {
        TodoChange::Add { todo } => format!("할 일 추가: {}", todo.title),
        TodoChange::AddAppointment { appointment } => format!(
            "약속 추가: {} ({}–{})",
            appointment.title,
            clock_text(appointment.start),
            clock_text(appointment.end)
        ),
        TodoChange::Remove { todo_id } => format!("할 일 지우기: {todo_id}"),
        TodoChange::Edit { todo_id, .. } => format!("할 일 고치기: {todo_id}"),
        other => format!("{other:?}"),
    }
}

impl Sandbox {
    fn note(&mut self, kind: LogKind, text: impl Into<String>) {
        self.log.push(LogEntry {
            at: self.clock,
            kind,
            text: text.into(),
        });
        self.publish();
    }

    /// Show the sandbox as it is now.
    fn publish(&self) {
        let view = self.view();
        if let Ok(mut shown) = self.shown.lock() {
            *shown = view;
        }
    }

    fn current(&self) -> Option<Assignment> {
        self.planner.state.get().assignment
    }

    /// How an assignment reads in a log line.
    fn describe(assignment: &Assignment) -> String {
        let until = assignment
            .until
            .map(|until| {
                format!(
                    " — {}분, {}까지",
                    (until - assignment.at).num_minutes(),
                    clock_text(until)
                )
            })
            .unwrap_or_default();
        if assignment.appointment_id.is_some() {
            format!("약속: {}{until}", assignment.task)
        } else if assignment.why.trim().is_empty() {
            format!("{}{until}", assignment.task)
        } else {
            format!("{}{until}. 이유: {}", assignment.task, assignment.why)
        }
    }

    /// Apply the list writes an approved change made, to this sandbox's copy of the list.
    fn apply_writes(&self, writes: &[TodoListWrite]) {
        if writes.is_empty() {
            return;
        }
        let _ = self.planner.config_store().update(|config| {
            let Ok(mut value) = serde_json::to_value(&*config) else {
                return;
            };
            setting_edits::apply(&mut value, &crate::list_write_edits(writes));
            if let Ok(changed) = serde_json::from_value(value) {
                *config = changed;
            }
        });
    }

    fn act(&mut self, action: &SimulationAction, review: bool, stopped: &dyn Fn() -> bool) {
        match action {
            SimulationAction::Assign => self.assign(),
            SimulationAction::Advance { minutes } => {
                self.advance(*minutes);
                self.note(LogKind::Time, format!("{minutes}분이 지났습니다."));
            }
            SimulationAction::Request { request, reason } => self.request(request, reason),
            SimulationAction::TodoChange { change, reason } => {
                self.todo_change(change, reason, review)
            }
            SimulationAction::Event { event } => self.event(*event),
            SimulationAction::Auto { until, variability } => {
                self.auto(*until, *variability, stopped)
            }
        }
    }

    fn assign(&mut self) {
        let before = self.current();
        match self.planner.assign(self.clock) {
            Ok(assignment) => {
                if before.as_ref().map(|current| current.at) == Some(assignment.at) {
                    self.note(
                        LogKind::Problem,
                        format!("이미 할 일이 있습니다: {}", assignment.task),
                    );
                } else {
                    self.note(LogKind::Pick, Self::describe(&assignment));
                }
            }
            Err(error) => self.note(LogKind::Problem, format!("할 일 받기: {error}")),
        }
    }

    /// Move the virtual clock on by `minutes`, in steps, so appointments start and end and a
    /// task's time runs out on the way (each noted).
    fn advance(&mut self, minutes: i64) {
        let target = self.clock + Duration::minutes(minutes.max(0));
        while self.clock < target {
            let before = self.current();
            let next = (self.clock + Duration::minutes(STEP_MINUTES)).min(target);
            if let Some(until) = before.as_ref().and_then(|current| current.until) {
                if self.clock < until
                    && until <= next
                    && before
                        .as_ref()
                        .is_some_and(|current| current.appointment_id.is_none())
                {
                    self.clock = until;
                    let task = before
                        .as_ref()
                        .map(|current| current.task.clone())
                        .unwrap_or_default();
                    self.note(LogKind::Time, format!("시간이 다 되었습니다: {task}"));
                }
            }
            self.clock = next;
            if let Err(error) = self.planner.resume(self.clock) {
                self.note(LogKind::Problem, error.to_string());
            }
            let after = self.current();
            if before.as_ref().map(|current| current.at) != after.as_ref().map(|current| current.at)
            {
                match &after {
                    Some(assignment) => {
                        let text = Self::describe(assignment);
                        self.note(LogKind::Pick, text);
                    }
                    None => {
                        if let Some(ended) = &before {
                            self.note(LogKind::Time, format!("끝났습니다: {}", ended.task));
                        }
                    }
                }
            }
        }
    }

    fn request(&mut self, request: &ChangeKind, reason: &str) {
        let before = self.current();
        let label = request_label(request);
        match self
            .planner
            .request_change(self.clock, request.clone(), reason, Evidence::default())
        {
            Ok(record) => {
                if record.approved {
                    self.apply_writes(&record.list_writes);
                }
                let mut text = format!("{label}: \"{reason}\" → {}", record.reply);
                for line in &record.list_summary {
                    text.push_str(&format!("\n  · {line}"));
                }
                self.note(
                    if record.approved {
                        LogKind::Approved
                    } else {
                        LogKind::Refused
                    },
                    text,
                );
                let after = self.current();
                if let Some(assignment) = after
                    .filter(|after| before.as_ref().map(|current| current.at) != Some(after.at))
                {
                    self.note(LogKind::Pick, Self::describe(&assignment));
                }
            }
            Err(error) => self.note(LogKind::Problem, format!("{label}: {error}")),
        }
    }

    fn todo_change(&mut self, change: &TodoChange, reason: &str, review: bool) {
        let label = todo_change_label(change);
        let result = if review {
            self.planner
                .review_todo_change(self.clock, change.clone(), reason, VIA)
        } else {
            self.planner
                .todo_change_without_review(self.clock, change.clone(), reason, VIA)
        };
        match result {
            Ok(record) => {
                if record.approved {
                    let writes = self.planner.writes_for(&record);
                    self.apply_writes(&writes);
                }
                let reply = if record.reply.trim().is_empty() {
                    "적용했습니다.".to_string()
                } else {
                    record.reply.clone()
                };
                self.note(
                    if record.approved {
                        LogKind::Approved
                    } else {
                        LogKind::Refused
                    },
                    format!("{label} → {reply}"),
                );
            }
            Err(error) => self.note(LogKind::Problem, format!("{label}: {error}")),
        }
    }

    /// Play `event` on the current task: time passes, then a request.
    fn event(&mut self, event: SimulationEvent) {
        let Some(current) = self.current() else {
            self.note(
                LogKind::Problem,
                "지금 할 일이 없습니다. 먼저 할 일을 받으세요.",
            );
            return;
        };
        if current.appointment_id.is_some() {
            // Appointments end by themselves.
            let minutes = current
                .until
                .map_or(30, |until| (until - self.clock).num_minutes().max(0) + 1);
            self.note(LogKind::Event, format!("약속을 마칩니다: {}", current.task));
            self.advance(minutes);
            return;
        }
        let remaining = current
            .until
            .map_or(60, |until| (until - self.clock).num_minutes().max(0));
        match event {
            SimulationEvent::Early => {
                self.advance((remaining * 6 / 10).max(STEP_MINUTES));
                self.note(LogKind::Event, "예상보다 일찍 끝냈습니다.");
                self.request(&ChangeKind::Done, "예상보다 일찍 다 끝냈습니다.");
            }
            SimulationEvent::OnTime => {
                self.advance(remaining);
                self.note(LogKind::Event, "예상대로 끝냈습니다.");
                self.request(&ChangeKind::Done, "다 끝냈습니다.");
            }
            SimulationEvent::Late => {
                self.advance(remaining);
                self.note(LogKind::Event, "생각보다 오래 걸립니다.");
                self.request(
                    &ChangeKind::Extend {
                        minutes: LATE_EXTRA_MINUTES,
                    },
                    &format!(
                        "생각보다 오래 걸려서 {LATE_EXTRA_MINUTES}분이 더 필요합니다. 거의 다 했습니다."
                    ),
                );
            }
            SimulationEvent::Skipped => {
                self.advance(remaining);
                self.note(LogKind::Event, "일을 하지 않았습니다.");
                self.request(
                    &ChangeKind::NotFinished,
                    "하기 싫어서 거의 하지 않았습니다.",
                );
            }
            SimulationEvent::Stuck => {
                self.advance(remaining.min(15));
                self.note(LogKind::Event, "감을 못 잡았습니다.");
                self.request(
                    &ChangeKind::Repick,
                    "어떻게 시작해야 할지 감이 안 잡혀서 다른 일을 하고 싶습니다.",
                );
            }
        }
    }

    /// Play tasks one after another until `until`: take a task when there is none, then let
    /// the luck pick what happens to it.
    fn auto(&mut self, until: DateTime<Utc>, variability: Variability, stopped: &dyn Fn() -> bool) {
        self.note(
            LogKind::Time,
            format!("{}까지 자동으로 진행합니다.", clock_text(until)),
        );
        let mut steps = 0;
        let ended = loop {
            if self.clock >= until {
                break format!("{}에 도착했습니다.", clock_text(until));
            }
            if steps >= MAX_AUTO_STEPS {
                break format!(
                    "한 번에 {MAX_AUTO_STEPS}번까지 진행합니다. 이어서 하려면 \"자동으로 진행\"을 다시 누르세요."
                );
            }
            if stopped() {
                break "멈췄습니다.".to_string();
            }
            steps += 1;
            let clock_before = self.clock;
            match self.current() {
                None => {
                    self.assign();
                    if self.current().is_none() {
                        self.advance(30);
                    }
                }
                Some(current) if current.time_is_up(self.clock) => {
                    // A task whose time is up waits for its report: it was done.
                    self.request(&ChangeKind::Done, "다 끝냈습니다.");
                    if self.current().is_some_and(|after| after.at == current.at) {
                        self.request(&ChangeKind::NotFinished, "끝내지 못했습니다.");
                    }
                }
                Some(_) => {
                    let event = self.luck.event(variability);
                    self.event(event);
                }
            }
            if self.clock == clock_before && self.current().is_none() {
                self.advance(STEP_MINUTES);
            }
        };
        self.note(LogKind::Time, format!("자동 진행을 마쳤습니다: {ended}"));
    }

    fn view(&self) -> SandboxView {
        let current = self.current();
        SandboxView {
            label: self.label.clone(),
            clock: self.clock,
            task: current.as_ref().map(|current| current.task.clone()),
            task_until: current.as_ref().and_then(|current| current.until),
            log: self.log.clone(),
            ai_calls: self.calls.load(Ordering::SeqCst),
            summary: self.summary(),
        }
    }

    /// What the run came to: finished to-dos, approved and refused requests, time per task and
    /// time without a task.
    fn summary(&self) -> Vec<String> {
        let state = self.planner.state.get();
        let mut lines = Vec::new();
        let done: Vec<String> = state
            .done
            .iter()
            .filter(|done| done.at >= self.start)
            .map(|done| done.title.clone())
            .collect();
        lines.push(match done.len() {
            0 => "끝낸 일: 없음".to_string(),
            count if count <= 6 => format!("끝낸 일 {count}개: {}", done.join(", ")),
            count => format!(
                "끝낸 일 {count}개: {} 외 {}개",
                done[..6].join(", "),
                count - 6
            ),
        });
        let requests: Vec<_> = state
            .requests
            .iter()
            .filter(|request| request.at >= self.start)
            .collect();
        let approved = requests.iter().filter(|request| request.approved).count();
        lines.push(format!(
            "요청 {}개: 승인 {approved}개, 승인되지 않음 {}개",
            requests.len(),
            requests.len() - approved
        ));
        let mut minutes_by_task: BTreeMap<String, i64> = BTreeMap::new();
        let spans = state
            .assignment_log
            .iter()
            .cloned()
            .chain(current_span(&state, self.clock));
        let mut busy_minutes = 0;
        for span in spans {
            let minutes = span.minutes_within(self.start, self.clock);
            if minutes > 0 {
                busy_minutes += minutes;
                *minutes_by_task.entry(span.task.clone()).or_default() += minutes;
            }
        }
        let mut by_task: Vec<(String, i64)> = minutes_by_task.into_iter().collect();
        by_task.sort_by(|first, second| second.1.cmp(&first.1));
        if !by_task.is_empty() {
            let shown: Vec<String> = by_task
                .iter()
                .take(6)
                .map(|(task, minutes)| format!("{} {}분", short(task), minutes))
                .collect();
            lines.push(format!("쓴 시간: {}", shown.join(", ")));
        }
        let total = (self.clock - self.start).num_minutes().max(0);
        lines.push(format!(
            "지난 시간 {}분 가운데 할 일 없이 지난 시간 {}분",
            total,
            (total - busy_minutes).max(0)
        ));
        lines
    }
}

/// The running assignment as a span up to `now`.
fn current_span(state: &PlannerState, now: DateTime<Utc>) -> Option<crate::model::AssignmentSpan> {
    state
        .assignment
        .as_ref()
        .map(|assignment| assignment.span_until(now))
}

/// A task's text cut to 30 characters for the summary.
fn short(task: &str) -> String {
    let mut text: String = task.chars().take(30).collect();
    if task.chars().count() > 30 {
        text.push('…');
    }
    text
}

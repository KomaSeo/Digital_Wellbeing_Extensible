//! Responsibility: Test TaskPlanner end to end with a fake AI advisor.

use super::*;
use crate::model::series_id;
use ai_focus_rule::{platform, AiFocusConfig, AiFocusStore, FocusMonitor, JudgeError};
use chrono::Utc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Responsibility: Test double for Advisor: pick the first open to-do (or a bogus id), approve
/// per flag, count calls.
struct FakeAdvisor {
    approve: AtomicBool,
    /// Every to-do change review fails (no key, no network).
    fail: AtomicBool,
    bogus_pick: AtomicBool,
    picks: AtomicUsize,
    reviews: AtomicUsize,
    /// Life rules the last question carried.
    saw_rules: parking_lot::Mutex<String>,
    /// System instruction the last question carried.
    saw_instructions: parking_lot::Mutex<String>,
    /// The repick reason the last pick carried.
    saw_repick_reason: parking_lot::Mutex<Option<String>>,
    /// What the next done review says about the result: decisive, open_ended or not_applicable.
    outcome: parking_lot::Mutex<String>,
    /// Proof text and whether a screenshot came with the last review.
    saw_evidence: parking_lot::Mutex<(String, bool)>,
    /// Planned, elapsed and spent minutes the last review carried.
    saw_minutes: parking_lot::Mutex<(Option<i64>, i64, i64)>,
    /// What the AI says about whether a picked session or an appointment is away from the
    /// computer.
    judges_away: AtomicBool,
    /// The appointment rules and the upcoming appointments the last question carried.
    saw_appointment_rules: parking_lot::Mutex<String>,
    saw_appointments: parking_lot::Mutex<Vec<String>>,
    /// The appointment the last appointment review was asked to replace (an edit), and the
    /// other appointments it saw.
    saw_replaced: parking_lot::Mutex<Option<String>>,
    saw_other_appointments: parking_lot::Mutex<Vec<String>>,
    /// The current assignment as the last change review saw it.
    saw_current_away: AtomicBool,
    /// The list changes the next change review makes.
    list_changes: parking_lot::Mutex<Vec<ListChange>>,
}

impl Advisor for FakeAdvisor {
    fn pick(&self, request: &PickRequest) -> Result<Pick, JudgeError> {
        self.picks.fetch_add(1, Ordering::SeqCst);
        *self.saw_repick_reason.lock() = request.repick_reason.map(str::to_string);
        *self.saw_rules.lock() = request.life_rules.to_string();
        *self.saw_instructions.lock() = request.instructions.to_string();
        *self.saw_appointments.lock() = request
            .appointments
            .iter()
            .map(|appointment| appointment.title.clone())
            .collect();
        let todo_id = if self.bogus_pick.load(Ordering::SeqCst) {
            "nope".into()
        } else {
            request.open[0].id.clone()
        };
        Ok(Pick {
            task: format!("work on {todo_id}"),
            todo_id,
            why: "deadline".into(),
            minutes: 45,
            away_from_computer: self.judges_away.load(Ordering::SeqCst),
            open: vec![
                "https://mail.google.com".into(),
                "not a url".into(),
                "http://plain.example".into(),
            ],
        })
    }

    fn review_appointment(
        &self,
        request: &AppointmentRequest,
    ) -> Result<AppointmentReview, JudgeError> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        *self.saw_appointment_rules.lock() = request.appointment_rules.to_string();
        *self.saw_replaced.lock() = request
            .replaces
            .map(|appointment| appointment.title.clone());
        *self.saw_other_appointments.lock() = request
            .appointments
            .iter()
            .map(|appointment| appointment.title.clone())
            .collect();
        *self.saw_instructions.lock() = request.instructions.to_string();
        if self.fail.load(Ordering::SeqCst) {
            return Err(JudgeError::NoApiKey);
        }
        let approved = self.approve.load(Ordering::SeqCst);
        Ok(AppointmentReview {
            approved,
            reply: if approved { "ok" } else { "no" }.into(),
            away_from_computer: self.judges_away.load(Ordering::SeqCst),
        })
    }

    fn plan_week(&self, request: &WeekPlanRequest) -> Result<WeekPlanAnswer, JudgeError> {
        *self.saw_instructions.lock() = request.instructions.to_string();
        let today = request.days_left[0].to_string();
        let todo = |todo_id: &str, minutes: i64| WeekPlanAnswerTodo {
            todo_id: todo_id.into(),
            minutes,
        };
        Ok(WeekPlanAnswer {
            days: vec![
                WeekPlanAnswerDay {
                    date: today.clone(),
                    work_minutes: 3000,
                    free_minutes: 600,
                    todos: vec![todo("a", 120), todo("not on the list", 60)],
                    note: "busy".into(),
                },
                // Twice the same day, a day outside this week, and nonsense: dropped.
                WeekPlanAnswerDay {
                    date: today,
                    work_minutes: 60,
                    ..Default::default()
                },
                WeekPlanAnswerDay {
                    date: "2099-01-01".into(),
                    work_minutes: 60,
                    ..Default::default()
                },
                WeekPlanAnswerDay {
                    date: "someday".into(),
                    ..Default::default()
                },
            ],
            summary: "a full week".into(),
            warnings: vec!["  ".into(), "a is at risk".into()],
        })
    }

    fn review_todo_change(&self, request: &TodoChangeRequest) -> Result<TodoReview, JudgeError> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        *self.saw_rules.lock() = request.life_rules.to_string();
        if self.fail.load(Ordering::SeqCst) {
            return Err(JudgeError::NoApiKey);
        }
        let approved = self.approve.load(Ordering::SeqCst);
        Ok(TodoReview {
            approved,
            reply: if approved { "ok" } else { "no" }.into(),
            // What the AI fills in; the user's own fields must win.
            todo: ReviewedTodo {
                title: "ignored".into(),
                notes: "filled by the AI".into(),
                constraints: "AI: needs a computer".into(),
                deadline: "2030-01-01".into(),
                priority: 2,
                estimate_minutes: 30,
                away_from_computer: self.judges_away.load(Ordering::SeqCst),
            },
        })
    }

    fn review(&self, request: &ReviewRequest) -> Result<Review, JudgeError> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        *self.saw_rules.lock() = request.life_rules.to_string();
        *self.saw_instructions.lock() = request.instructions.to_string();
        *self.saw_evidence.lock() = (request.evidence.to_string(), request.screenshot.is_some());
        self.saw_current_away.store(
            request
                .current
                .is_some_and(|assignment| assignment.away_from_computer),
            Ordering::SeqCst,
        );
        *self.saw_minutes.lock() = (
            request.planned_minutes,
            request.elapsed_minutes,
            request.spent_on_todo_minutes,
        );
        let approved = self.approve.load(Ordering::SeqCst);
        Ok(Review {
            approved,
            reply: if approved { "ok" } else { "no" }.into(),
            minutes: 30,
            outcome: self.outcome.lock().clone(),
            list_changes: self.list_changes.lock().clone(),
        })
    }
}

/// Responsibility: Bundle a planner under test with its focus monitor and fake advisor.
struct Harness {
    planner: TaskPlanner,
    focus: Arc<FocusMonitor>,
    advisor: Arc<FakeAdvisor>,
}

fn todo(id: &str) -> TodoItem {
    TodoItem {
        id: id.into(),
        ..TodoItem::new(format!("title {id}"))
    }
}

fn harness(enabled: bool, ids: &[&str]) -> Harness {
    harness_with_state(enabled, ids, JsonStore::in_memory(PlannerState::default()))
}

fn harness_with_state(enabled: bool, ids: &[&str], state: JsonStore<PlannerState>) -> Harness {
    let focus = Arc::new(FocusMonitor::new(
        AiFocusStore::in_memory(AiFocusConfig::default()),
        platform::native(),
        Arc::new(|_: &AiFocusConfig| Err(JudgeError::NoApiKey)),
    ));
    let advisor = Arc::new(FakeAdvisor {
        approve: AtomicBool::new(true),
        fail: AtomicBool::new(false),
        bogus_pick: AtomicBool::new(false),
        picks: AtomicUsize::new(0),
        reviews: AtomicUsize::new(0),
        saw_rules: parking_lot::Mutex::new(String::new()),
        saw_instructions: parking_lot::Mutex::new(String::new()),
        saw_repick_reason: parking_lot::Mutex::new(None),
        outcome: parking_lot::Mutex::new("not_applicable".into()),
        saw_evidence: parking_lot::Mutex::new((String::new(), false)),
        saw_minutes: parking_lot::Mutex::new((None, 0, 0)),
        judges_away: AtomicBool::new(false),
        saw_appointment_rules: parking_lot::Mutex::new(String::new()),
        saw_appointments: parking_lot::Mutex::new(Vec::new()),
        saw_replaced: parking_lot::Mutex::new(None),
        saw_other_appointments: parking_lot::Mutex::new(Vec::new()),
        saw_current_away: AtomicBool::new(false),
        list_changes: parking_lot::Mutex::new(Vec::new()),
    });
    let provided = Arc::clone(&advisor);
    let planner = TaskPlanner::new(
        JsonStore::in_memory(PlannerConfig {
            enabled,
            todos: ids.iter().map(|id| todo(id)).collect(),
            ..Default::default()
        }),
        state,
        Arc::clone(&focus),
        Arc::new(move |_| Ok(Arc::clone(&provided) as Arc<dyn Advisor>)),
    );
    Harness {
        planner,
        focus,
        advisor,
    }
}

#[test]
fn the_focus_follows_an_assignment_another_device_made() {
    // Two devices whose planner_state.json sync keeps the same (here: one file).
    let folder = std::env::temp_dir().join(format!("dw_planner_follow_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&folder);
    let path = folder.join("planner_state.json");
    let laptop = harness_with_state(
        true,
        &["a", "b"],
        JsonStore::open(path.clone(), true).unwrap(),
    );
    let desktop = harness_with_state(
        true,
        &["a", "b"],
        JsonStore::open(path.clone(), true).unwrap(),
    );

    laptop.planner.assign(Utc::now()).unwrap();
    assert!(!desktop.focus.store().get().session_active);
    desktop.planner.resume(Utc::now()).unwrap();
    let focus = desktop.focus.store().get();
    assert!(focus.session_active);
    assert_eq!(focus.task, "work on a");
    assert_eq!(focus.task_scope, "a");
    assert_eq!(desktop.advisor.picks.load(Ordering::SeqCst), 0);

    // "사용 중지" on one device stops the other too.
    laptop.planner.stop_use(Utc::now()).unwrap();
    desktop.planner.resume(Utc::now()).unwrap();
    assert!(desktop.planner.stopped());
    assert!(!desktop.focus.store().get().session_active);
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn always_on_and_inactive_only_when_the_list_is_empty() {
    // The old on/off switch is ignored: the AI always decides.
    assert!(harness(false, &["a"]).planner.is_active());
    assert!(!harness(true, &[]).planner.is_active());
    let fixture = harness(true, &[]);
    assert!(matches!(
        fixture.planner.assign(Utc::now()),
        Err(PlannerError::NotActive)
    ));
}

#[test]
fn assign_starts_focus_session_and_resumes_without_asking_again() {
    let fixture = harness(true, &["a", "b"]);
    let assignment = fixture.planner.assign(Utc::now()).unwrap();
    assert_eq!(assignment.todo_id.as_deref(), Some("a"));
    // Only https websites are kept for the Focus app to open.
    assert_eq!(assignment.open, ["https://mail.google.com"]);
    let focus = fixture.focus.store().get();
    assert!(focus.session_active);
    assert_eq!(focus.task, "work on a");

    // e.g. after logging on again
    fixture
        .focus
        .store()
        .update(|config| config.session_active = false)
        .unwrap();
    fixture.planner.assign(Utc::now()).unwrap();
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 1);
    assert!(fixture.focus.store().get().session_active);
}

#[test]
fn life_rules_reach_every_question() {
    let fixture = harness(true, &["a", "b"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.life_rules = "No games before 18:00.".into())
        .unwrap();
    fixture.planner.assign(Utc::now()).unwrap();
    assert_eq!(*fixture.advisor.saw_rules.lock(), "No games before 18:00.");
    fixture.advisor.saw_rules.lock().clear();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::End,
            "dinner time now",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(*fixture.advisor.saw_rules.lock(), "No games before 18:00.");
    assert!(crate::advisor::rules_text("").is_empty());
}

#[test]
fn bogus_pick_is_refused() {
    let fixture = harness(true, &["a"]);
    fixture.advisor.bogus_pick.store(true, Ordering::SeqCst);
    assert!(matches!(
        fixture.planner.assign(Utc::now()),
        Err(PlannerError::BadPick(_))
    ));
    assert!(!fixture.focus.store().get().session_active);
}

#[test]
fn short_reason_is_refused_without_asking_the_ai() {
    let fixture = harness(true, &["a"]);
    fixture.planner.assign(Utc::now()).unwrap();
    let result =
        fixture
            .planner
            .request_change(Utc::now(), ChangeKind::End, " 응 ", Evidence::default());
    assert!(matches!(result, Err(PlannerError::ReasonTooShort)));
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 0);
}

#[test]
fn rejected_request_is_logged_and_changes_nothing() {
    let fixture = harness(true, &["a", "b"]);
    fixture.planner.assign(Utc::now()).unwrap();
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let record = fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchFree {
                task: "유튜브".into(),
            },
            "그냥 지루해서",
            Evidence::default(),
        )
        .unwrap();
    assert!(!record.approved);
    assert_eq!(fixture.focus.store().get().task, "work on a");
    let status = fixture.planner.status();
    assert_eq!(status.recent_requests.len(), 1);
    assert_eq!(status.recent_requests[0].reason, "그냥 지루해서");
}

#[test]
fn approved_done_waits_for_the_next_request_and_routines_stay() {
    let fixture = harness(true, &["a", "b"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.todos[1].repeat = true)
        .unwrap();
    fixture.planner.assign(Utc::now()).unwrap();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Done,
            "3장 초안 제출함",
            Evidence::default(),
        )
        .unwrap();
    // Nothing is assigned until the user asks; no pick happened.
    assert!(fixture.planner.status().assignment.is_none());
    assert!(!fixture.focus.store().get().session_active);
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.planner.status().open_todos.len(), 1);

    // The routine "b" is done but stays on the list.
    fixture.planner.assign(Utc::now()).unwrap();
    assert_eq!(fixture.focus.store().get().task, "work on b");
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Done,
            "운동 마침",
            Evidence::default(),
        )
        .unwrap();
    let status = fixture.planner.status();
    assert!(status.assignment.is_none());
    assert_eq!(status.open_todos.len(), 1);
    assert!(status.open_todos[0].repeat);
    assert!(status.recent_done.iter().any(|done| done.todo_id == "b"));
    // Its time counts again from the finish.
    assert_eq!(status.minutes_this_round.get("b"), Some(&0));
}

#[test]
fn approved_switches_set_the_task() {
    let fixture = harness(true, &["a", "b"]);
    fixture.planner.assign(Utc::now()).unwrap();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchTodo {
                todo_id: "b".into(),
            },
            "a는 교수님 답장 대기 중",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(fixture.focus.store().get().task, "title b");

    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchFree {
                task: "은행 업무".into(),
            },
            "오늘 마감인 서류 제출",
            Evidence::default(),
        )
        .unwrap();
    let status = fixture.planner.status();
    assert_eq!(status.assignment.unwrap().todo_id, None);
    assert_eq!(fixture.focus.store().get().task, "은행 업무");
}

#[test]
fn switch_to_unknown_todo_is_refused() {
    let fixture = harness(true, &["a"]);
    fixture.planner.assign(Utc::now()).unwrap();
    let result = fixture.planner.request_change(
        Utc::now(),
        ChangeKind::SwitchTodo {
            todo_id: "zzz".into(),
        },
        "this one instead",
        Evidence::default(),
    );
    assert!(matches!(result, Err(PlannerError::UnknownTodo(_))));
}

#[test]
fn change_kind_json_shape() {
    let json_value = serde_json::to_value(ChangeKind::SwitchTodo {
        todo_id: "a".into(),
    })
    .unwrap();
    assert_eq!(
        json_value,
        serde_json::json!({ "kind": "switch_todo", "todo_id": "a" })
    );
    let done: ChangeKind = serde_json::from_value(serde_json::json!({ "kind": "done" })).unwrap();
    assert_eq!(done, ChangeKind::Done);
}

fn proposal(title: &str) -> ProposalInput {
    ProposalInput {
        title: title.into(),
        priority: Some(9),
        ..Default::default()
    }
}

/// What the agent does with an approved change: write it to `planner.json`.
fn apply_writes(fixture: &Harness, writes: Vec<TodoListWrite>) {
    fixture
        .planner
        .config_store()
        .update(|config| {
            for write in &writes {
                match write {
                    TodoListWrite::Put(todo) => {
                        config.todos.retain(|existing| existing.id != todo.id);
                        config.todos.push(todo.clone());
                    }
                    TodoListWrite::Merge {
                        todo_id,
                        changed_fields,
                        base,
                    } => {
                        let position = config
                            .todos
                            .iter()
                            .position(|existing| &existing.id == todo_id);
                        let current = match (position, base) {
                            (Some(index), _) => config.todos.remove(index),
                            (None, Some(base_item)) => base_item.clone(),
                            (None, None) => continue,
                        };
                        let mut item = serde_json::to_value(current).unwrap();
                        item.as_object_mut().unwrap().extend(changed_fields.clone());
                        config.todos.push(serde_json::from_value(item).unwrap());
                    }
                    TodoListWrite::Remove(todo_id) => {
                        config.todos.retain(|existing| &existing.id != todo_id)
                    }
                    TodoListWrite::Dismiss(todo_id) => {
                        config.dismissed.insert(todo_id.clone());
                    }
                    TodoListWrite::PutAppointment(appointment) => {
                        config
                            .appointments
                            .retain(|existing| existing.id != appointment.id);
                        config.appointments.push(appointment.clone());
                    }
                    TodoListWrite::RemoveAppointment(appointment_id) => config
                        .appointments
                        .retain(|existing| &existing.id != appointment_id),
                }
            }
        })
        .unwrap();
}

fn add(title: &str) -> TodoChange {
    TodoChange::Add {
        todo: proposal(title),
    }
}

#[test]
fn an_approved_addition_is_completed_by_the_ai_and_joins_the_list() {
    let fixture = harness(true, &[]);
    assert!(!fixture.planner.is_active());
    fixture
        .planner
        .config_store()
        .update(|config| config.life_rules = "No games on weekdays.".into())
        .unwrap();
    let record = fixture
        .planner
        .review_todo_change(Utc::now(), add("  Buy   groceries "), "", "claude desktop")
        .unwrap();
    assert!(record.approved);
    assert_eq!(record.via, "claude desktop");
    assert_eq!(*fixture.advisor.saw_rules.lock(), "No games on weekdays.");
    let after = record.after.clone().unwrap();
    assert_eq!(after.title, "Buy groceries");
    assert_eq!(after.priority, 5, "the user's priority, clamped to 1..=5");
    assert_eq!(after.notes, "filled by the AI");
    assert_eq!(after.constraints, "AI: needs a computer");
    assert_eq!(after.deadline.unwrap().to_string(), "2030-01-01");
    assert_eq!(after.estimate_minutes, Some(30));
    assert!(after.proposed);

    // The agent applies it to planner.json.
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    let status = fixture.planner.status();
    assert!(status.active);
    assert_eq!(status.open_todos[0].title, "Buy groceries");
    assert_eq!(status.recent_todo_changes.len(), 1);

    // What the user filled in is kept.
    let mine = TodoChange::Add {
        todo: ProposalInput {
            title: "Call the bank".into(),
            notes: "about the card".into(),
            ..Default::default()
        },
    };
    let record = fixture
        .planner
        .review_todo_change(Utc::now(), mine, "", "to-do tab")
        .unwrap();
    assert_eq!(record.after.unwrap().notes, "about the card");
}

#[test]
fn a_rejected_or_unreviewed_change_is_logged_only() {
    let fixture = harness(true, &["a"]);
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let record = fixture
        .planner
        .review_todo_change(Utc::now(), add("Play games"), "", "claude desktop")
        .unwrap();
    assert!(!record.approved && !record.ai_failed);
    fixture.advisor.fail.store(true, Ordering::SeqCst);
    let record = fixture
        .planner
        .review_todo_change(Utc::now(), add("Read a novel"), "", "to-do tab")
        .unwrap();
    assert!(!record.approved && record.ai_failed);
    assert!(record.reply.contains("could not review"));
    // Without the AI the user's own fields still make a to-do the user may schedule.
    assert_eq!(record.after.unwrap().title, "Read a novel");
    let status = fixture.planner.status();
    assert_eq!(status.open_todos.len(), 1);
    assert_eq!(status.recent_todo_changes.len(), 2);
    assert!(fixture.planner.todo_change(&record.id).is_some());
}

#[test]
fn bad_duplicate_and_reasonless_changes_are_refused_without_asking_the_ai() {
    let fixture = harness(true, &["a"]);
    let refuse = |change: TodoChange, reason: &str| {
        fixture
            .planner
            .review_todo_change(Utc::now(), change, reason, "x")
            .unwrap_err()
    };
    assert!(matches!(refuse(add(" "), ""), PlannerError::BadProposal));
    assert!(matches!(
        refuse(add("TITLE A"), ""),
        PlannerError::Duplicate(_)
    ));
    let remove_a = TodoChange::Remove {
        todo_id: "a".into(),
    };
    assert!(matches!(
        refuse(remove_a, "no"),
        PlannerError::ReasonTooShort
    ));
    let remove_unknown = TodoChange::Remove {
        todo_id: "zzz".into(),
    };
    assert!(matches!(
        refuse(remove_unknown, "finished elsewhere"),
        PlannerError::UnknownTodo(_)
    ));
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 0);
}

#[test]
fn in_the_setup_stage_edits_and_removals_need_neither_the_ai_nor_a_reason() {
    // An AI that says no to everything: it must not be asked.
    let fixture = harness(true, &["a", "b"]);
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let edit = TodoChange::Edit {
        todo_id: "a".into(),
        todo: ProposalInput {
            priority: Some(1),
            ..Default::default()
        },
    };
    let record = fixture
        .planner
        .todo_change_without_review(Utc::now(), edit, "", "to-do tab")
        .unwrap();
    assert!(record.approved && !record.ai_failed);
    assert_eq!(record.after.unwrap().priority, 1);
    let remove = TodoChange::Remove {
        todo_id: "b".into(),
    };
    let record = fixture
        .planner
        .todo_change_without_review(Utc::now(), remove, "", "to-do tab")
        .unwrap();
    assert!(record.approved);
    assert_eq!(
        fixture.planner.writes_for(&record),
        [TodoListWrite::Remove("b".into())]
    );
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 0);
    // An unknown to-do is still refused; an addition is taken as the user wrote it.
    let remove_unknown = TodoChange::Remove {
        todo_id: "zzz".into(),
    };
    assert!(matches!(
        fixture
            .planner
            .todo_change_without_review(Utc::now(), remove_unknown, "", "x")
            .unwrap_err(),
        PlannerError::UnknownTodo(_)
    ));
    let record = fixture
        .planner
        .todo_change_without_review(Utc::now(), add("Play games"), "", "to-do tab")
        .unwrap();
    assert!(record.approved);
    assert_eq!(record.after.unwrap().title, "Play games");
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 0);
    // An appointment still goes to the AI.
    let appointment = TodoChange::AddAppointment {
        appointment: AppointmentInput {
            title: "Dentist".into(),
            notes: String::new(),
            start: Utc::now() + chrono::Duration::hours(2),
            end: Utc::now() + chrono::Duration::hours(3),
            away_from_computer: None,
            repeat_weekdays: Vec::new(),
        },
    };
    let _ = fixture
        .planner
        .todo_change_without_review(Utc::now(), appointment, "", "to-do tab");
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 1);
}

#[test]
fn outside_strict_mode_a_done_report_needs_neither_the_ai_nor_a_reason() {
    let fixture = harness(true, &["a", "b"]);
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let start = Utc::now();
    fixture.planner.assign(start).unwrap();
    let reviews_before = fixture.advisor.reviews.load(Ordering::SeqCst);
    let record = fixture
        .planner
        .report_done_without_review(
            start + chrono::Duration::minutes(5),
            "",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    assert_eq!(
        fixture.advisor.reviews.load(Ordering::SeqCst),
        reviews_before
    );
    assert!(fixture.planner.status().assignment.is_none());
    // Nothing assigned: nothing to report.
    assert!(matches!(
        fixture
            .planner
            .report_done_without_review(Utc::now(), "", Evidence::default()),
        Err(PlannerError::NothingAssigned)
    ));
}

#[test]
fn edits_and_removals_write_the_list_or_hide_an_older_proposed_todo() {
    let fixture = harness(true, &["a", "b"]);
    let edit = TodoChange::Edit {
        todo_id: "a".into(),
        todo: ProposalInput {
            priority: Some(1),
            ..Default::default()
        },
    };
    let record = fixture
        .planner
        .review_todo_change(
            Utc::now(),
            edit,
            "the advisor moved it to next month",
            "to-do tab",
        )
        .unwrap();
    assert_eq!(record.before.as_ref().unwrap().priority, 3);
    let after = record.after.clone().unwrap();
    assert_eq!((after.title.as_str(), after.priority), ("title a", 1));
    let mut changed_fields = serde_json::Map::new();
    changed_fields.insert("priority".into(), serde_json::json!(1));
    assert_eq!(
        fixture.planner.writes_for(&record),
        [TodoListWrite::Merge {
            todo_id: "a".into(),
            changed_fields,
            base: None,
        }]
    );
    // Applied later (as a scheduled change would be): an edit made meanwhile stays.
    fixture
        .planner
        .config_store()
        .update(|config| config.todos[0].notes = "changed meanwhile".into())
        .unwrap();
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    let edited = fixture
        .planner
        .config_store()
        .get()
        .todos
        .into_iter()
        .find(|todo| todo.id == "a")
        .unwrap();
    assert_eq!(
        (
            edited.priority,
            edited.notes.as_str(),
            edited.title.as_str()
        ),
        (1, "changed meanwhile", "title a")
    );

    let remove = TodoChange::Remove {
        todo_id: "b".into(),
    };
    let record = fixture
        .planner
        .review_todo_change(
            Utc::now(),
            remove,
            "done at the office already",
            "to-do tab",
        )
        .unwrap();
    assert_eq!(
        fixture.planner.writes_for(&record),
        [TodoListWrite::Remove("b".into())]
    );
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    let open: Vec<_> = fixture
        .planner
        .status()
        .open_todos
        .into_iter()
        .map(|todo| (todo.id, todo.priority))
        .collect();
    assert_eq!(open, [("a".to_string(), 1)]);

    // A to-do an older version kept outside planner.json is hidden instead.
    let older = todo("old");
    fixture
        .planner
        .state_store_for_tests()
        .update(|state| state.proposed.push(older.clone()))
        .unwrap();
    let remove_old = TodoChange::Remove {
        todo_id: "old".into(),
    };
    let record = fixture
        .planner
        .review_todo_change(Utc::now(), remove_old, "not needed any more", "to-do tab")
        .unwrap();
    assert_eq!(
        fixture.planner.writes_for(&record),
        [
            TodoListWrite::Dismiss("old".into()),
            TodoListWrite::Remove("old".into())
        ]
    );
}

#[test]
fn the_todo_change_question_carries_the_rules_and_the_item() {
    let change = add("Watch a movie");
    let request = TodoChangeRequest {
        instructions: crate::advisor::PROPOSAL_PROMPT,
        now_local: "2026-10-30 Friday 14:05",
        life_rules: "No movies before 18:00.",
        change: &change,
        target: None,
        reason: "",
        open: &[],
        recent_changes: &[],
        history: "",
    };
    let content = crate::advisor::question(
        request.life_rules,
        request.open,
        crate::advisor::todo_change_text(&request),
    );
    let text: String = content
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    assert!(text.contains("The owner's life rules (binding):\nNo movies before 18:00."));
    assert!(text.contains("\"title\": \"Watch a movie\""));
    assert!(text.contains("empty fields are for you to fill in"));
}

#[test]
fn a_question_starts_with_the_cached_rules_and_list_and_ends_with_what_changes() {
    let mut polish = todo("polish:a");
    polish.title = "Improve: title a".into();
    let open = vec![todo("a"), todo("b"), polish];
    let first = crate::advisor::question("rules", &open, "Now: 14:05".into());
    let second = crate::advisor::question("rules", &open, "Now: 14:40".into());
    // The cached start is the same for both calls and marked for an hour.
    assert_eq!(first[0], second[0]);
    assert_eq!(first[0]["cache_control"]["ttl"], "1h");
    let start = first[0]["text"].as_str().unwrap();
    assert!(
        start.contains("rules") && start.contains("\n[a] "),
        "{start}"
    );
    // The time and the "polish:" to-dos (their time left changes) come after it.
    assert!(!start.contains("polish:a") && !start.contains("14:05"));
    assert!(first[1].get("cache_control").is_none());
    // The to-do change review sends the same question without the cache mark.
    let unmarked = crate::advisor::uncached(first.clone());
    assert_eq!(unmarked[0]["text"], first[0]["text"]);
    assert!(unmarked
        .iter()
        .all(|block| block.get("cache_control").is_none()));
    let pick = crate::advisor::pick_text(&PickRequest {
        instructions: crate::advisor::PICK_PROMPT,
        now_local: "14:05",
        life_rules: "rules",
        open: &open,
        appointments: &[],
        recently_done: &[],
        previous: None,
        history: "",
        repick_reason: None,
        situation: "카페, 노트북만 있음",
    });
    assert!(pick.contains("카페, 노트북만 있음"), "{pick}");
    assert!(
        pick.contains("[polish:a] ") && !pick.contains("[b] "),
        "{pick}"
    );
}

#[test]
fn a_to_do_is_one_line_with_only_the_fields_that_are_set() {
    let mut full = todo("t1");
    full.title = "논문 3장 초안 쓰기".into();
    full.notes = "Google Docs\n관련 연구  절부터".into();
    full.constraints = "컴퓨터 필요\n12월 1일 이후".into();
    full.deadline = chrono::NaiveDate::from_ymd_opt(2026, 11, 20);
    full.priority = 5;
    full.estimate_minutes = Some(120);
    full.repeat = true;
    let mut bare = todo("t2");
    bare.title = "장보기".into();
    bare.notes = String::new();
    bare.deadline = None;
    bare.estimate_minutes = None;
    bare.repeat = false;
    let content = crate::advisor::question("", &[full, bare], "Now".into());
    let text = content[0]["text"].as_str().unwrap();
    assert!(
        text.contains(
            "[t1] 논문 3장 초안 쓰기 | due 2026-11-20 | priority 5 | estimate 120 min | routine | \
             constraints: 컴퓨터 필요 / 12월 1일 이후\n[t2] 장보기 | priority 3"
        ),
        "{text}"
    );
    // The notes (how to do the work) are not in the list.
    assert!(!text.contains("Google Docs"), "{text}");
    assert!(!text.contains("\"proposed\""), "{text}");
}

#[test]
fn when_the_time_is_up_the_task_stays_until_the_user_reports() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now();
    let assignment = fixture.planner.assign(start).unwrap();
    assert_eq!(
        assignment.until,
        Some(start + chrono::Duration::minutes(45))
    );
    let before_end = start + chrono::Duration::minutes(44);
    let at_end = start + chrono::Duration::minutes(45);
    let report = |now, kind: ChangeKind| {
        fixture
            .planner
            .request_change(now, kind, "보고합니다 진행 상황", Evidence::default())
    };

    // "Not finished" only once the time is up.
    assert!(matches!(
        report(before_end, ChangeKind::NotFinished),
        Err(PlannerError::NotTimeYet)
    ));
    // Once it is up, nothing but a report: no new task, no repick.
    assert!(matches!(
        fixture.planner.assign(at_end),
        Err(PlannerError::ReportFirst)
    ));
    assert!(matches!(
        report(at_end, ChangeKind::Repick),
        Err(PlannerError::ReportFirst)
    ));
    assert!(matches!(
        report(
            at_end,
            ChangeKind::SetWhenTimeIsUp {
                action: WhenTimeIsUp::NotifyOnly
            }
        ),
        Err(PlannerError::NoLongerUsed)
    ));
    assert_eq!(
        fixture
            .planner
            .status()
            .assignment
            .unwrap()
            .todo_id
            .as_deref(),
        Some("a")
    );

    // "Not finished": no AI review, the to-do stays, nothing is assigned.
    let record = report(at_end, ChangeKind::NotFinished).unwrap();
    assert!(record.approved);
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 0);
    let status = fixture.planner.status();
    assert!(status.assignment.is_none());
    assert_eq!(status.open_todos.len(), 2);
    assert!(!fixture.focus.store().get().session_active);
    assert!(matches!(
        report(at_end, ChangeKind::Done),
        Err(PlannerError::NothingAssigned)
    ));

    // The next task comes when the user asks for it.
    fixture
        .planner
        .assign(at_end + chrono::Duration::minutes(5))
        .unwrap();
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 2);
}

#[test]
fn an_approved_switch_runs_for_the_time_the_review_gave_it() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now();
    fixture.planner.assign(start).unwrap();
    fixture
        .planner
        .request_change(
            start,
            ChangeKind::SwitchTodo {
                todo_id: "b".into(),
            },
            "waiting for the advisor's reply on a",
            Evidence::default(),
        )
        .unwrap();
    let assignment = fixture.planner.status().assignment.unwrap();
    assert_eq!(assignment.todo_id.as_deref(), Some("b"));
    assert_eq!(
        assignment.until,
        Some(start + chrono::Duration::minutes(30))
    );
}

#[test]
fn the_owners_instructions_replace_the_built_in_ones() {
    let fixture = harness(true, &["a", "b"]);
    fixture.planner.assign(Utc::now()).unwrap();
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        crate::advisor::PICK_PROMPT
    );
    fixture
        .planner
        .config_store()
        .update(|config| {
            config.instructions.pick = "Always pick the shortest to-do.".into();
            config.instructions.change_review = "Approve only with a doctor's note.".into();
        })
        .unwrap();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::End,
            "dinner time now",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        "Approve only with a doctor's note."
    );
    // A blank text falls back to the built-in instruction.
    let blank = PlannerInstructions {
        pick: "   ".into(),
        ..Default::default()
    };
    assert_eq!(blank.pick_or_default(), crate::advisor::PICK_PROMPT);
}

#[test]
fn time_spent_per_todo_is_counted_from_the_assignments() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now() - chrono::Duration::minutes(50);
    fixture.planner.assign(start).unwrap();
    // 30 minutes on a, then a switch to b.
    fixture
        .planner
        .request_change(
            start + chrono::Duration::minutes(30),
            ChangeKind::SwitchTodo {
                todo_id: "b".into(),
            },
            "blocked on data for a",
            Evidence::default(),
        )
        .unwrap();
    let status = fixture.planner.status();
    assert_eq!(status.minutes_total.get("a"), Some(&30));
    // b runs from then until now (about 20 minutes).
    let on_b = *status.minutes_total.get("b").unwrap();
    assert!((19..=21).contains(&on_b), "{on_b}");
}

#[test]
fn notes_are_saved_for_open_todos_and_reach_the_done_review() {
    let fixture = harness(true, &["a"]);
    fixture.planner.assign(Utc::now()).unwrap();
    let notes = TaskNotes {
        text: "outline done".into(),
        steps: vec![
            TaskStep {
                text: "write intro".into(),
                done: true,
            },
            TaskStep {
                text: "  ".into(),
                done: false,
            },
        ],
    };
    fixture.planner.put_task_notes("a", notes).unwrap();
    assert!(fixture
        .planner
        .put_task_notes("zzz", TaskNotes::default())
        .is_err());
    let saved = &fixture.planner.status().task_notes["a"];
    assert_eq!(saved.steps.len(), 1, "empty steps are dropped");
    let state = fixture.planner.state_store_for_tests().get();
    let text = crate::advisor::review_text(&ReviewRequest {
        instructions: crate::advisor::REVIEW_PROMPT,
        now_local: "now",
        life_rules: "",
        current: state.assignment.as_ref(),
        request: &ChangeKind::Done,
        target_title: None,
        reason: "finished",
        open: &[],
        recent_requests: &[],
        history: "",
        notes: state.task_notes.get("a"),
        evidence: "",
        screenshot: None,
        planned_minutes: None,
        elapsed_minutes: 0,
        spent_on_todo_minutes: 0,
        estimate_minutes: None,
    });
    assert!(
        text.contains("outline done") && text.contains("- [x] write intro"),
        "{text}"
    );
}

#[test]
fn open_ended_work_finished_early_keeps_the_time_left_for_improving_it() {
    let fixture = harness(true, &["a", "b"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.todos[0].estimate_minutes = Some(240))
        .unwrap();
    let start = Utc::now() - chrono::Duration::minutes(50);
    fixture.planner.assign(start).unwrap();
    *fixture.advisor.outcome.lock() = "open_ended".into();
    // 30 minutes on a report the AI judged needs 240.
    let finished_at = start + chrono::Duration::minutes(30);
    let record = fixture
        .planner
        .request_change(
            finished_at,
            ChangeKind::Done,
            "the report is written",
            Evidence {
                text: "sent the draft to my advisor".into(),
                screenshot_jpeg: Some(vec![1, 2, 3]),
            },
        )
        .unwrap();
    assert!(record.approved);
    assert_eq!(record.outcome, "open_ended");
    assert_eq!(record.evidence, "sent the draft to my advisor");
    assert!(record.screenshot_attached);
    assert_eq!(
        *fixture.advisor.saw_evidence.lock(),
        ("sent the draft to my advisor".to_string(), true)
    );
    let (planned, elapsed, spent) = *fixture.advisor.saw_minutes.lock();
    assert_eq!((planned, elapsed, spent), (Some(45), 30, 30));
    let polish = &fixture.planner.state_store_for_tests().get().polish["a"];
    assert_eq!(polish.minutes_left, 210);
    let open = fixture.planner.open_todos();
    let improve = open
        .iter()
        .find(|todo| todo.id == "polish:a")
        .expect("an Improve to-do is open");
    assert_eq!(improve.title, "Improve: title a");
    assert_eq!(improve.estimate_minutes, Some(210));

    // 25 minutes improving it come off the time left (within the switch's 30 minutes).
    *fixture.advisor.outcome.lock() = "not_applicable".into();
    let improving_from = finished_at + chrono::Duration::minutes(5);
    fixture
        .planner
        .request_change(
            improving_from,
            ChangeKind::SwitchTodo {
                todo_id: "polish:a".into(),
            },
            "time to improve the report",
            Evidence::default(),
        )
        .unwrap();
    fixture
        .planner
        .request_change(
            improving_from + chrono::Duration::minutes(25),
            ChangeKind::SwitchTodo {
                todo_id: "b".into(),
            },
            "b is due soon after all",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(
        fixture.planner.state_store_for_tests().get().polish["a"].minutes_left,
        185
    );

    // Finishing the improvement removes it.
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchTodo {
                todo_id: "polish:a".into(),
            },
            "back to the report",
            Evidence::default(),
        )
        .unwrap();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Done,
            "the report is polished",
            Evidence::default(),
        )
        .unwrap();
    assert!(fixture
        .planner
        .state_store_for_tests()
        .get()
        .polish
        .is_empty());
    assert!(!fixture
        .planner
        .open_todos()
        .iter()
        .any(|todo| todo.id.starts_with("polish:")));
}

#[test]
fn decisive_or_nearly_full_work_leaves_no_improve_todo() {
    for (outcome, estimate) in [("decisive", 240), ("open_ended", 35)] {
        let fixture = harness(true, &["a", "b"]);
        fixture
            .planner
            .config_store()
            .update(|config| config.todos[0].estimate_minutes = Some(estimate))
            .unwrap();
        let start = Utc::now() - chrono::Duration::minutes(30);
        fixture.planner.assign(start).unwrap();
        *fixture.advisor.outcome.lock() = outcome.into();
        fixture
            .planner
            .request_change(
                start + chrono::Duration::minutes(30),
                ChangeKind::Done,
                "mail sent to the client",
                Evidence::default(),
            )
            .unwrap();
        assert!(
            fixture
                .planner
                .state_store_for_tests()
                .get()
                .polish
                .is_empty(),
            "{outcome} {estimate}"
        );
    }
}

#[test]
fn improve_todos_cannot_be_edited_or_removed() {
    let fixture = harness(true, &["a", "b"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.todos[0].estimate_minutes = Some(240))
        .unwrap();
    fixture.planner.assign(Utc::now()).unwrap();
    *fixture.advisor.outcome.lock() = "open_ended".into();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Done,
            "the report is written",
            Evidence::default(),
        )
        .unwrap();
    let reviews_before = fixture.advisor.reviews.load(Ordering::SeqCst);
    let refused = fixture.planner.review_todo_change(
        Utc::now(),
        TodoChange::Remove {
            todo_id: "polish:a".into(),
        },
        "no need to improve it",
        "e2e",
    );
    assert!(matches!(refused, Err(PlannerError::MadeByTheApp(_))));
    assert_eq!(
        fixture.advisor.reviews.load(Ordering::SeqCst),
        reviews_before,
        "the AI is not asked"
    );
}

#[test]
fn without_an_estimate_only_this_assignment_counts_for_improving() {
    let fixture = harness(true, &["a", "b"]);
    // An earlier 40 minutes on a, then a new 30-minute assignment finished after 10 minutes.
    let start = Utc::now() - chrono::Duration::minutes(70);
    fixture.planner.assign(start).unwrap();
    fixture
        .planner
        .request_change(
            start + chrono::Duration::minutes(40),
            ChangeKind::SwitchTodo {
                todo_id: "a".into(),
            },
            "start a fresh session on a",
            Evidence::default(),
        )
        .unwrap();
    *fixture.advisor.outcome.lock() = "open_ended".into();
    fixture
        .planner
        .request_change(
            start + chrono::Duration::minutes(50),
            ChangeKind::Done,
            "the draft is usable",
            Evidence::default(),
        )
        .unwrap();
    // The approved switch runs for the review's 30 minutes; 10 of them were used.
    assert_eq!(
        fixture.planner.state_store_for_tests().get().polish["a"].minutes_left,
        20
    );
}

#[test]
fn improve_todos_are_dropped_after_two_weeks() {
    let fixture = harness(true, &["b"]);
    let finished = todo("a");
    fixture
        .planner
        .state_store_for_tests()
        .update(|state| {
            for (todo_id, days_ago) in [("a", 15), ("c", 13)] {
                state.polish.insert(
                    todo_id.into(),
                    PolishItem {
                        todo: TodoItem {
                            id: todo_id.into(),
                            ..finished.clone()
                        },
                        minutes_left: 60,
                        since: Utc::now() - chrono::Duration::days(days_ago),
                    },
                );
            }
        })
        .unwrap();
    let improving: Vec<_> = fixture
        .planner
        .open_todos()
        .into_iter()
        .map(|todo| todo.id)
        .filter(|todo_id| todo_id.starts_with("polish:"))
        .collect();
    assert_eq!(improving, ["polish:c"]);
    // The next change of assignment also removes it from the state.
    fixture.planner.assign(Utc::now()).unwrap();
    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchTodo {
                todo_id: "polish:c".into(),
            },
            "time to improve c",
            Evidence::default(),
        )
        .unwrap();
    let kept: Vec<_> = fixture
        .planner
        .state_store_for_tests()
        .get()
        .polish
        .into_keys()
        .collect();
    assert_eq!(kept, ["c"]);
}

#[test]
fn repick_lets_the_ai_choose_what_comes_next() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now() - chrono::Duration::minutes(20);
    fixture.planner.assign(start).unwrap();
    assert_eq!(*fixture.advisor.saw_repick_reason.lock(), None);
    let picks = fixture.advisor.picks.load(Ordering::SeqCst);
    let record = fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Repick,
            "waiting for the data from the lab",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    // The AI was asked again, with the reason; the user named no task.
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), picks + 1);
    assert_eq!(
        fixture.advisor.saw_repick_reason.lock().as_deref(),
        Some("waiting for the data from the lab")
    );
    let assignment = fixture.planner.status().assignment.unwrap();
    assert_eq!(assignment.source, AssignmentSource::Planner);
    // A rejected repick changes nothing and asks the AI for no pick.
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let picks = fixture.advisor.picks.load(Ordering::SeqCst);
    let record = fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Repick,
            "bored of it",
            Evidence::default(),
        )
        .unwrap();
    assert!(!record.approved);
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), picks);
    assert_eq!(
        fixture.planner.status().assignment.unwrap().at,
        assignment.at
    );
}

#[test]
fn the_week_plan_keeps_the_seven_days_and_listed_todos_only() {
    let fixture = harness(true, &["a", "b"]);
    let plan = fixture.planner.plan_week(Utc::now()).unwrap();
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        crate::advisor::WEEK_PLAN_PROMPT
    );
    assert_eq!(plan.days.len(), 1, "{plan:?}");
    let today = &plan.days[0];
    assert_eq!(today.date, Some(chrono::Local::now().date_naive()));
    // A day has 24 hours: the work is cut to that, and no free time is left.
    assert_eq!((today.work_minutes, today.free_minutes), (1440, 0));
    let planned: Vec<_> = today
        .todos
        .iter()
        .map(|todo| todo.todo_id.as_str())
        .collect();
    assert_eq!(planned, ["a"]);
    assert_eq!(plan.work_minutes_total, 1440);
    assert_eq!(plan.warnings, ["a is at risk"]);
    assert_eq!(
        fixture.planner.status().week_plan.unwrap().summary,
        "a full week"
    );
    // The question names each day with its weekday.
    let text = crate::advisor::week_plan_text(&WeekPlanRequest {
        instructions: "",
        now_local: "now",
        life_rules: "",
        days_left: &[chrono::NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()],
        open: &[(todo("a"), 30)],
        worked_this_week: &[],
        recently_done: &[],
        appointments: &[],
    });
    assert!(text.contains("2026-11-06 Friday"), "{text}");
    assert!(text.contains("| spent 30 min"), "{text}");
}

fn appointment_change(
    title: &str,
    start: chrono::DateTime<Utc>,
    minutes: i64,
    away: Option<bool>,
) -> TodoChange {
    TodoChange::AddAppointment {
        appointment: AppointmentInput {
            title: title.into(),
            notes: String::new(),
            start,
            end: start + chrono::Duration::minutes(minutes),
            away_from_computer: away,
            repeat_weekdays: Vec::new(),
        },
    }
}

/// Add an appointment through the review, as the agent does, and return its record.
fn add_appointment(
    fixture: &Harness,
    now: chrono::DateTime<Utc>,
    change: TodoChange,
) -> Result<TodoChangeRecord, PlannerError> {
    let record = fixture
        .planner
        .review_todo_change(now, change, "", "to-do tab")?;
    if record.approved {
        apply_writes(fixture, fixture.planner.writes_for(&record));
    }
    Ok(record)
}

#[test]
fn nothing_is_done_away_from_the_computer_any_more() {
    // Whatever the AI or an older to-do says, a task never locks the computer.
    let fixture = harness(true, &["sleep"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.todos[0].away_from_computer = Some(true))
        .unwrap();
    fixture.advisor.judges_away.store(true, Ordering::SeqCst);
    let now = Utc::now();
    let assignment = fixture.planner.assign(now).unwrap();
    assert!(!assignment.away_from_computer);
    fixture
        .planner
        .request_change(
            now,
            ChangeKind::Repick,
            "I want to play",
            Evidence::default(),
        )
        .unwrap();
    assert!(!fixture.advisor.saw_current_away.load(Ordering::SeqCst));
    // A new to-do is not marked either, whatever the AI or the user says.
    let record = fixture
        .planner
        .review_todo_change(now, add("go running"), "", "to-do tab")
        .unwrap();
    assert_eq!(record.after.unwrap().away_from_computer, None);
    let record = fixture
        .planner
        .review_todo_change(
            now + chrono::Duration::seconds(1),
            TodoChange::Add {
                todo: ProposalInput {
                    title: "online class".into(),
                    away_from_computer: Some(true),
                    ..Default::default()
                },
            },
            "",
            "to-do tab",
        )
        .unwrap();
    assert_eq!(record.after.unwrap().away_from_computer, None);
}

#[test]
fn appointments_are_reviewed_by_the_owners_rules_and_checked() {
    let fixture = harness(true, &["a"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.appointment_rules = "No appointments for games.".into())
        .unwrap();
    let now = Utc::now();
    let start = now + chrono::Duration::hours(2);
    fixture.advisor.judges_away.store(true, Ordering::SeqCst);
    let record = add_appointment(
        &fixture,
        now,
        appointment_change("dentist", start, 60, None),
    )
    .unwrap();
    assert!(record.approved);
    assert_eq!(
        *fixture.advisor.saw_appointment_rules.lock(),
        "No appointments for games."
    );
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        crate::advisor::APPOINTMENT_PROMPT
    );
    let accepted = fixture.planner.appointments(now);
    assert_eq!(accepted.len(), 1);
    // Never away from the computer, whatever the AI thinks: appointments do not lock.
    assert!(!accepted[0].away_from_computer);
    // Overlapping, past, too long, untitled: refused without asking the AI.
    let reviews = fixture.advisor.reviews.load(Ordering::SeqCst);
    let later = now + chrono::Duration::seconds(1);
    assert!(matches!(
        add_appointment(
            &fixture,
            later,
            appointment_change("call", start + chrono::Duration::minutes(30), 60, None)
        ),
        Err(PlannerError::AppointmentOverlaps(_))
    ));
    for refused in [
        appointment_change("old", now - chrono::Duration::hours(3), 60, None),
        appointment_change("all week", start + chrono::Duration::hours(2), 3000, None),
        appointment_change(" ", start + chrono::Duration::hours(2), 60, None),
        appointment_change("backwards", start + chrono::Duration::hours(2), -5, None),
    ] {
        assert!(matches!(
            add_appointment(&fixture, later, refused),
            Err(PlannerError::BadAppointment)
        ));
    }
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), reviews);
    // Rejected: nothing is added.
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let record = add_appointment(
        &fixture,
        later,
        appointment_change(
            "gaming night",
            start + chrono::Duration::hours(3),
            60,
            Some(false),
        ),
    )
    .unwrap();
    assert!(!record.approved);
    assert_eq!(fixture.planner.appointments(now).len(), 1);
    // Removing needs no AI.
    let reviews = fixture.advisor.reviews.load(Ordering::SeqCst);
    let record = fixture
        .planner
        .review_todo_change(
            later + chrono::Duration::seconds(1),
            TodoChange::RemoveAppointment {
                appointment_id: accepted[0].id.clone(),
            },
            "",
            "to-do tab",
        )
        .unwrap();
    assert!(record.approved);
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    assert!(fixture.planner.appointments(now).is_empty());
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), reviews);
}

#[test]
fn an_appointment_is_fixed_at_its_time_and_ends_by_itself() {
    let fixture = harness(true, &["a", "b"]);
    let now = Utc::now();
    let start = now + chrono::Duration::minutes(20);
    add_appointment(
        &fixture,
        now,
        appointment_change("meeting", start, 60, Some(true)),
    )
    .unwrap();
    // A task picked before it is cut off at its start, and the AI knows of it.
    let assignment = fixture.planner.assign(now).unwrap();
    assert_eq!(assignment.until, Some(start));
    assert_eq!(*fixture.advisor.saw_appointments.lock(), ["meeting"]);
    // At its start it replaces the task, without a report and without asking the user.
    fixture
        .planner
        .resume(start + chrono::Duration::seconds(5))
        .unwrap();
    let current = fixture.planner.status().assignment.unwrap();
    assert!(current.appointment_id.is_some());
    // It locks nothing; the window checks judge what has to do with it.
    assert!(!current.away_from_computer);
    let checked_task = fixture.focus.store().get().task;
    assert!(
        checked_task.starts_with("meeting") && checked_task.contains("약속과 관계된 일"),
        "{checked_task}"
    );
    // "할 일 받기" during it gives the appointment, not a to-do.
    let picks = fixture.advisor.picks.load(Ordering::SeqCst);
    let again = fixture
        .planner
        .assign(start + chrono::Duration::minutes(10))
        .unwrap();
    assert!(again.appointment_id.is_some());
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), picks);
    // Past its planned end it goes on for the grace time (meetings run late) ...
    fixture
        .planner
        .resume(start + chrono::Duration::minutes(61))
        .unwrap();
    assert!(fixture
        .planner
        .status()
        .assignment
        .is_some_and(|assignment| assignment.appointment_id.is_some()));
    // ... then it ends by itself: the user asks for the next task, and may ask to extend it.
    let after_grace = start + chrono::Duration::minutes(60 + APPOINTMENT_GRACE_MINUTES + 1);
    fixture.planner.resume(after_grace).unwrap();
    assert!(fixture.planner.status().assignment.is_none());
    assert!(fixture.planner.waiting_for_task());
    let offer = fixture.planner.extension_offer(after_grace).unwrap();
    assert_eq!(offer.appointment.title, "meeting");
}

#[test]
fn a_repeating_appointment_takes_place_on_its_weekdays_and_one_day_can_be_skipped() {
    let fixture = harness(true, &["a", "b"]);
    let now = Utc::now();
    let start = now + chrono::Duration::minutes(20);
    let every_day = match appointment_change("class", start, 60, None) {
        TodoChange::AddAppointment { mut appointment } => {
            appointment.repeat_weekdays = vec![6, 0, 1, 2, 3, 4, 5, 5];
            TodoChange::AddAppointment { appointment }
        }
        _ => unreachable!(),
    };
    let record = add_appointment(&fixture, now, every_day).unwrap();
    let series = record.appointment.unwrap();
    // Kept once on the list, with its days in order.
    assert_eq!(series.repeat_weekdays, [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(fixture.planner.config_store().get().appointments.len(), 1);
    // Listed once per day for the coming week, at the same time.
    let occurrences = fixture.planner.appointments(now);
    assert!(occurrences.len() >= 7, "{}", occurrences.len());
    for (day, occurrence) in occurrences.iter().enumerate() {
        assert_eq!(series_id(&occurrence.id), series.id);
        assert_eq!(occurrence.start, start + chrono::Duration::days(day as i64));
    }
    // Another appointment at its time on a later day overlaps it.
    let overlapping = add_appointment(
        &fixture,
        now,
        appointment_change("lunch", start + chrono::Duration::days(3), 30, None),
    );
    assert!(
        matches!(overlapping, Err(PlannerError::AppointmentOverlaps(title)) if title == "class")
    );
    // It runs today, and again tomorrow after today's has ended.
    fixture
        .planner
        .resume(start + chrono::Duration::seconds(5))
        .unwrap();
    let today = fixture
        .planner
        .status()
        .assignment
        .unwrap()
        .appointment_id
        .unwrap();
    assert_eq!(today, occurrences[0].id);
    let tomorrow_start = start + chrono::Duration::days(1);
    fixture
        .planner
        .resume(tomorrow_start - chrono::Duration::hours(2))
        .unwrap();
    assert!(fixture.planner.status().assignment.is_none());
    fixture
        .planner
        .resume(tomorrow_start + chrono::Duration::seconds(5))
        .unwrap();
    let tomorrow = fixture
        .planner
        .status()
        .assignment
        .unwrap()
        .appointment_id
        .unwrap();
    assert_eq!(tomorrow, occurrences[1].id);
    // Skipping the day after tomorrow takes only that day off; the list stays as it is.
    let skipped = occurrences[2].id.clone();
    let record = fixture
        .planner
        .review_todo_change(
            now,
            TodoChange::RemoveAppointment {
                appointment_id: skipped.clone(),
            },
            "",
            "to-do tab",
        )
        .unwrap();
    assert!(record.approved);
    assert!(fixture.planner.writes_for(&record).is_empty());
    let left: Vec<String> = fixture
        .planner
        .appointments(now)
        .into_iter()
        .map(|appointment| appointment.id)
        .collect();
    assert!(!left.contains(&skipped));
    assert!(left.contains(&occurrences[3].id));
    assert_eq!(fixture.planner.config_store().get().appointments.len(), 1);
    // Editing a day of it edits the appointment on the list.
    let record = add_appointment(
        &fixture,
        now,
        edit_appointment(&occurrences[3].id, "evening class", start, 60),
    )
    .unwrap();
    match &record.change {
        TodoChange::EditAppointment { appointment_id, .. } => {
            assert_eq!(appointment_id, &series.id)
        }
        _ => unreachable!(),
    }
    // Removing it by its own id removes every day.
    add_appointment(
        &fixture,
        now,
        TodoChange::RemoveAppointment {
            appointment_id: series.id.clone(),
        },
    )
    .unwrap();
    assert!(fixture.planner.config_store().get().appointments.is_empty());
}

fn edit_appointment(
    appointment_id: &str,
    title: &str,
    start: chrono::DateTime<Utc>,
    minutes: i64,
) -> TodoChange {
    match appointment_change(title, start, minutes, None) {
        TodoChange::AddAppointment { appointment } => TodoChange::EditAppointment {
            appointment_id: appointment_id.into(),
            appointment,
        },
        _ => unreachable!(),
    }
}

#[test]
fn an_appointment_is_edited_by_the_ai_in_strict_mode_and_at_once_outside_it() {
    let fixture = harness(true, &["a"]);
    let now = Utc::now();
    let start = now + chrono::Duration::minutes(20);
    add_appointment(
        &fixture,
        now,
        appointment_change("meeting", start, 60, None),
    )
    .unwrap();
    add_appointment(
        &fixture,
        now + chrono::Duration::seconds(1),
        appointment_change("class", start + chrono::Duration::hours(3), 60, None),
    )
    .unwrap();
    let meeting_id = fixture.planner.appointments(now)[0].id.clone();
    // Strict mode: the AI decides, seeing the appointment the edit replaces and the others
    // (not the replaced one). Moving it over its own old time is no overlap.
    let reviews = fixture.advisor.reviews.load(Ordering::SeqCst);
    let moved = start + chrono::Duration::minutes(30);
    let record = add_appointment(
        &fixture,
        now + chrono::Duration::seconds(2),
        edit_appointment(&meeting_id, "meeting moved", moved, 60),
    )
    .unwrap();
    assert!(record.approved);
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), reviews + 1);
    assert_eq!(
        fixture.advisor.saw_replaced.lock().as_deref(),
        Some("meeting")
    );
    assert_eq!(*fixture.advisor.saw_other_appointments.lock(), ["class"]);
    assert_eq!(record.appointment_before.as_ref().unwrap().title, "meeting");
    let appointments = fixture.planner.appointments(now);
    assert_eq!(appointments.len(), 2);
    assert_eq!(appointments[0].id, meeting_id);
    assert_eq!(appointments[0].title, "meeting moved");
    assert_eq!(appointments[0].start, moved);
    // Rejected: it stays as it was.
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let record = add_appointment(
        &fixture,
        now + chrono::Duration::seconds(3),
        edit_appointment(&meeting_id, "gaming", moved, 90),
    )
    .unwrap();
    assert!(!record.approved);
    assert_eq!(fixture.planner.appointments(now)[0].title, "meeting moved");
    // Another appointment's time is never taken, and an unknown one cannot be edited.
    assert!(matches!(
        add_appointment(
            &fixture,
            now + chrono::Duration::seconds(4),
            edit_appointment(&meeting_id, "meeting", start + chrono::Duration::hours(3), 30),
        ),
        Err(PlannerError::AppointmentOverlaps(title)) if title == "class"
    ));
    assert!(matches!(
        add_appointment(
            &fixture,
            now + chrono::Duration::seconds(5),
            edit_appointment("nope", "meeting", start, 30),
        ),
        Err(PlannerError::UnknownAppointment(_))
    ));
    // Outside strict mode: applied at once, without the AI.
    let reviews = fixture.advisor.reviews.load(Ordering::SeqCst);
    let record = fixture
        .planner
        .todo_change_without_review(
            now + chrono::Duration::seconds(6),
            edit_appointment(&meeting_id, "meeting", start, 60),
            "",
            "to-do tab",
        )
        .unwrap();
    assert!(record.approved);
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), reviews);
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    assert_eq!(fixture.planner.appointments(now)[0].start, start);
    // An appointment that runs now runs on its new times once edited.
    let during = start + chrono::Duration::minutes(10);
    fixture.planner.resume(during).unwrap();
    assert!(fixture
        .planner
        .status()
        .assignment
        .is_some_and(
            |assignment| assignment.appointment_id.as_deref() == Some(meeting_id.as_str())
        ));
    let record = fixture
        .planner
        .todo_change_without_review(
            during,
            edit_appointment(&meeting_id, "meeting", start, 120),
            "",
            "to-do tab",
        )
        .unwrap();
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    fixture
        .planner
        .appointment_edited(during, &meeting_id)
        .unwrap();
    let running = fixture.planner.status().assignment.unwrap();
    assert_eq!(running.appointment_id.as_deref(), Some(meeting_id.as_str()));
    assert_eq!(
        running.until,
        Some(start + chrono::Duration::minutes(120 + APPOINTMENT_GRACE_MINUTES))
    );
    // Moved to later: it ends now and starts at its new time.
    let record = fixture
        .planner
        .todo_change_without_review(
            during,
            edit_appointment(
                &meeting_id,
                "meeting",
                start + chrono::Duration::minutes(60),
                60,
            ),
            "",
            "to-do tab",
        )
        .unwrap();
    apply_writes(&fixture, fixture.planner.writes_for(&record));
    fixture
        .planner
        .appointment_edited(during, &meeting_id)
        .unwrap();
    assert!(fixture
        .planner
        .status()
        .assignment
        .is_none_or(|assignment| assignment.appointment_id.is_none()));
}

#[test]
fn an_appointment_is_extended_only_with_an_approved_reason() {
    let fixture = harness(true, &["a"]);
    let now = Utc::now();
    let start = now + chrono::Duration::minutes(5);
    add_appointment(
        &fixture,
        now,
        appointment_change("meeting", start, 60, Some(true)),
    )
    .unwrap();
    let ended = start + chrono::Duration::minutes(60 + APPOINTMENT_GRACE_MINUTES);
    let after = ended + chrono::Duration::minutes(2);
    fixture
        .planner
        .resume(start + chrono::Duration::minutes(1))
        .unwrap();
    fixture.planner.resume(after).unwrap();
    let appointment_id = fixture
        .planner
        .extension_offer(after)
        .unwrap()
        .appointment
        .id;
    let extend = |minutes| ChangeKind::ExtendAppointment {
        appointment_id: appointment_id.clone(),
        minutes,
    };
    // Checked before the AI: a reason, a sensible length.
    assert!(matches!(
        fixture.planner.check_change(after, &extend(30), "hm"),
        Err(PlannerError::ReasonTooShort)
    ));
    assert!(matches!(
        fixture
            .planner
            .check_change(after, &extend(1000), "the meeting still runs"),
        Err(PlannerError::BadExtension)
    ));
    // Rejected: it stays ended.
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let record = fixture
        .planner
        .request_change(after, extend(30), "I want more time", Evidence::default())
        .unwrap();
    assert!(!record.approved);
    assert!(fixture.planner.status().assignment.is_none());
    // Approved: it runs again, until the extension ends (the review gave 30 minutes).
    fixture.advisor.approve.store(true, Ordering::SeqCst);
    fixture
        .planner
        .request_change(
            after,
            extend(45),
            "the meeting is still going on",
            Evidence::default(),
        )
        .unwrap();
    let assignment = fixture.planner.status().assignment.unwrap();
    assert!(assignment.appointment_id.is_some());
    assert_eq!(
        assignment.until,
        Some(after + chrono::Duration::minutes(30))
    );
    // Too late to extend: half an hour after it ended there is no offer.
    fixture
        .planner
        .resume(after + chrono::Duration::minutes(31 + APPOINTMENT_GRACE_MINUTES))
        .unwrap();
    assert!(matches!(
        fixture.planner.check_change(
            after + chrono::Duration::minutes(31 + APPOINTMENT_GRACE_MINUTES),
            &extend(30),
            "the meeting still runs"
        ),
        Err(PlannerError::BadExtension)
    ));
}

#[test]
fn an_appointment_left_early_does_not_start_again() {
    let fixture = harness(true, &["a"]);
    let now = Utc::now();
    let start = now + chrono::Duration::minutes(5);
    add_appointment(
        &fixture,
        now,
        appointment_change("lecture", start, 90, Some(false)),
    )
    .unwrap();
    let during = start + chrono::Duration::minutes(10);
    fixture.planner.resume(during).unwrap();
    assert!(fixture
        .planner
        .status()
        .assignment
        .unwrap()
        .appointment_id
        .is_some());
    fixture
        .planner
        .request_change(
            during,
            ChangeKind::Done,
            "the lecture was cancelled",
            Evidence::default(),
        )
        .unwrap();
    fixture
        .planner
        .resume(during + chrono::Duration::seconds(20))
        .unwrap();
    assert!(fixture.planner.status().assignment.is_none());
    let next = fixture
        .planner
        .assign(during + chrono::Duration::minutes(1))
        .unwrap();
    assert!(next.appointment_id.is_none());
}

#[test]
fn stopping_ends_the_task_and_holds_everything_until_resumed() {
    let fixture = harness(true, &["a"]);
    let now = Utc::now();
    fixture.planner.assign(now).unwrap();
    // An appointment soon after.
    add_appointment(
        &fixture,
        now,
        appointment_change("call", now + chrono::Duration::minutes(5), 30, Some(false)),
    )
    .unwrap();
    fixture.planner.stop_use(now).unwrap();
    assert!(fixture.planner.stopped());
    assert!(fixture.planner.status().assignment.is_none());
    assert!(!fixture.focus.store().get().session_active);
    assert_eq!(fixture.planner.open_todos().len(), 1, "the to-do stays");
    // Nothing starts and nothing calls the AI while stopped.
    fixture
        .planner
        .resume(now + chrono::Duration::minutes(10))
        .unwrap();
    assert!(fixture.planner.status().assignment.is_none());
    assert!(matches!(
        fixture
            .planner
            .review_todo_change(now, add("x"), "", "to-do tab"),
        Err(PlannerError::Stopped)
    ));
    assert!(matches!(
        fixture.planner.plan_week(now),
        Err(PlannerError::Stopped)
    ));
    // "다시 사용하기": the appointment that runs now starts again with the agent's loop.
    fixture.planner.resume_use().unwrap();
    assert!(!fixture.planner.stopped());
    fixture
        .planner
        .resume(now + chrono::Duration::minutes(10))
        .unwrap();
    assert!(fixture
        .planner
        .status()
        .assignment
        .is_some_and(|assignment| assignment.appointment_id.is_some()));
}

#[test]
fn reports_about_the_to_do_itself_use_the_completion_instruction() {
    let fixture = harness(true, &["a", "b", "c"]);
    let mut now = Utc::now();
    fixture.planner.assign(now).unwrap();
    // "끝냈다" and the other reports about the to-do: the completion instruction.
    for request in [
        ChangeKind::Done,
        ChangeKind::AlreadyDone,
        ChangeKind::Changed,
    ] {
        now += chrono::Duration::minutes(1);
        let _ = fixture.planner.request_change(
            now,
            request.clone(),
            "the report's reason",
            Evidence::default(),
        );
        assert_eq!(
            *fixture.advisor.saw_instructions.lock(),
            crate::advisor::COMPLETION_PROMPT,
            "{request:?}"
        );
        if fixture.planner.status().assignment.is_none() {
            fixture.planner.assign(now).unwrap();
        }
    }
    // Stopping (like getting another task): the change review instruction.
    now += chrono::Duration::minutes(1);
    if fixture.planner.status().assignment.is_none() {
        fixture.planner.assign(now).unwrap();
    }
    let _ = fixture.planner.request_change(
        now,
        ChangeKind::End,
        "dinner time now",
        Evidence::default(),
    );
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        crate::advisor::REVIEW_PROMPT
    );
    // An owner's change review written before the two were apart still judges the reports,
    // until the owner writes a completion instruction.
    let mut instructions = PlannerInstructions {
        change_review: "The owner's own review.".into(),
        ..Default::default()
    };
    assert_eq!(
        instructions.completion_review_or_default(),
        "The owner's own review."
    );
    instructions.completion_review = "The owner's completion review.".into();
    assert_eq!(
        instructions.completion_review_or_default(),
        "The owner's completion review."
    );
    assert_eq!(
        PlannerInstructions::default().completion_review_or_default(),
        crate::advisor::COMPLETION_PROMPT
    );
}

#[test]
fn extending_a_task_uses_its_own_instruction_and_moves_the_end() {
    let fixture = harness(true, &["a"]);
    let now = Utc::now();
    let assignment = fixture.planner.assign(now).unwrap();
    let until = assignment.until.unwrap();
    // Before the end: the end moves by what the review gave (the fake gives 30).
    fixture
        .planner
        .request_change(
            now,
            ChangeKind::Extend { minutes: 45 },
            "almost done with the draft",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        crate::advisor::EXTENSION_PROMPT
    );
    let extended = fixture.planner.status().assignment.unwrap().until.unwrap();
    assert_eq!(extended, until + chrono::Duration::minutes(30));
    // After the time is up, more time is accepted instead of a report, counted from now.
    let late = extended + chrono::Duration::minutes(10);
    fixture
        .planner
        .request_change(
            late,
            ChangeKind::Extend { minutes: 20 },
            "one more section to write",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(
        fixture.planner.status().assignment.unwrap().until,
        Some(late + chrono::Duration::minutes(20))
    );
    // Out of range: refused before the AI.
    assert!(matches!(
        fixture
            .planner
            .check_change(late, &ChangeKind::Extend { minutes: 1 }, "just a bit"),
        Err(PlannerError::BadTaskExtension)
    ));
}

#[test]
fn an_approved_done_can_add_a_follow_up_and_set_steps() {
    let fixture = harness(true, &["mail", "b"]);
    let now = Utc::now();
    fixture.planner.assign(now).unwrap();
    *fixture.advisor.list_changes.lock() = vec![
        ListChange {
            action: "add".into(),
            title: "Reply to the mail".into(),
            constraints: "Not before 2030-01-03".into(),
            ..Default::default()
        },
        ListChange {
            action: "set_notes".into(),
            todo_id: "b".into(),
            working_notes: "started".into(),
            steps: vec!["outline".into(), "draft".into()],
            ..Default::default()
        },
        // Dropped: names no open to-do, or repeats a title.
        ListChange {
            action: "remove".into(),
            todo_id: "nope".into(),
            ..Default::default()
        },
        ListChange {
            action: "add".into(),
            title: "title b".into(),
            ..Default::default()
        },
    ];
    let record = fixture
        .planner
        .request_change(
            now,
            ChangeKind::Done,
            "read the mail, reply in two days",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    assert_eq!(
        record.list_summary,
        ["추가: Reply to the mail", "메모와 단계: title b"]
    );
    assert_eq!(record.list_writes.len(), 1);
    apply_writes(&fixture, record.list_writes.clone());
    assert!(fixture
        .planner
        .open_todos()
        .iter()
        .any(|todo| todo.title == "Reply to the mail" && todo.constraints.contains("2030-01-03")));
    assert_eq!(
        fixture.planner.status().task_notes["b"].steps.len(),
        2,
        "the steps were set"
    );
    // A rejected request changes nothing in the list.
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    fixture.planner.assign(now).unwrap();
    let rejected = fixture
        .planner
        .request_change(now, ChangeKind::Repick, "bored of it", Evidence::default())
        .unwrap();
    assert!(rejected.list_writes.is_empty());
}

#[test]
fn the_user_pauses_a_task_and_its_time_stands_still_until_they_go_on() {
    let fixture = harness(true, &["a"]);
    let start = Utc::now();
    let assignment = fixture.planner.assign(start).unwrap();
    let planned_end = assignment.until.unwrap();
    // Paused four minutes in: even past the planned end, the time is not up, and the agent's
    // rounds do not end the pause.
    let paused = fixture
        .planner
        .pause(start + chrono::Duration::minutes(4))
        .unwrap();
    assert_eq!(
        paused.paused_since,
        Some(start + chrono::Duration::minutes(4))
    );
    // Paused: the agent covers the screen (machine::lockdown).
    assert!(fixture.planner.paused());
    fixture
        .planner
        .resume(planned_end + chrono::Duration::minutes(5))
        .unwrap();
    let still = fixture.planner.status().assignment.unwrap();
    assert!(still.paused_since.is_some());
    assert!(!still.time_is_up(planned_end + chrono::Duration::minutes(5)));
    // Pausing again changes nothing.
    fixture
        .planner
        .pause(start + chrono::Duration::minutes(30))
        .unwrap();
    assert_eq!(
        fixture.planner.status().assignment.unwrap().paused_since,
        Some(start + chrono::Duration::minutes(4))
    );
    // Going on at 64 minutes: the 60 minutes paused are logged and added to the end.
    let back = start + chrono::Duration::minutes(64);
    let running = fixture.planner.unpause(back).unwrap();
    assert!(!fixture.planner.paused());
    assert_eq!(running.paused_since, None);
    assert_eq!(
        running.until,
        Some(planned_end + chrono::Duration::minutes(60))
    );
    let later = back + chrono::Duration::minutes(10);
    let kinds: Vec<(TimetableKind, i64)> = fixture
        .planner
        .timetable(start - chrono::Duration::hours(1), later, later)
        .iter()
        .map(|entry| (entry.kind, (entry.end - entry.start).num_minutes()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (TimetableKind::Task, 4),
            (TimetableKind::Unknown, 60),
            (TimetableKind::Task, 10)
        ]
    );
    // A task whose time is up is reported first.
    let up = running.until.unwrap() + chrono::Duration::minutes(1);
    assert!(matches!(
        fixture.planner.pause(up),
        Err(PlannerError::ReportFirst)
    ));
}

#[test]
fn an_appointment_cannot_be_paused() {
    let fixture = harness(true, &["a"]);
    let now = Utc::now();
    add_appointment(
        &fixture,
        now,
        appointment_change("meeting", now - chrono::Duration::minutes(1), 60, None),
    )
    .unwrap();
    fixture.planner.resume(now).unwrap();
    assert!(fixture
        .planner
        .status()
        .assignment
        .is_some_and(|assignment| assignment.appointment_id.is_some()));
    assert!(matches!(
        fixture.planner.pause(now),
        Err(PlannerError::AppointmentCannotPause)
    ));
}

#[test]
fn a_to_do_done_before_counts_as_done_without_this_assignments_time() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now() - chrono::Duration::minutes(40);
    fixture.planner.assign(start).unwrap();
    let assigned = fixture
        .planner
        .status()
        .assignment
        .unwrap()
        .todo_id
        .unwrap();
    let record = fixture
        .planner
        .report_without_review(
            start + chrono::Duration::minutes(30),
            ChangeKind::AlreadyDone,
            "",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    let status = fixture.planner.status();
    assert!(status.assignment.is_none());
    assert!(status
        .recent_done
        .iter()
        .any(|done_record| done_record.todo_id == assigned));
    // The 30 minutes of this assignment are not counted on the to-do, and no improvement
    // time is kept.
    assert_eq!(status.minutes_total.get(&assigned).copied().unwrap_or(0), 0);
    assert!(fixture
        .planner
        .state_store_for_tests()
        .get()
        .polish
        .is_empty());
}

#[test]
fn a_changed_to_do_is_edited_by_the_ai_and_the_task_ends() {
    let fixture = harness(true, &["a", "b"]);
    let now = Utc::now();
    fixture.planner.assign(now).unwrap();
    let assigned = fixture
        .planner
        .status()
        .assignment
        .unwrap()
        .todo_id
        .unwrap();
    *fixture.advisor.list_changes.lock() = vec![ListChange {
        action: "edit".into(),
        todo_id: assigned.clone(),
        deadline: "2030-01-10".into(),
        ..Default::default()
    }];
    let record = fixture
        .planner
        .request_change(
            now + chrono::Duration::minutes(5),
            ChangeKind::Changed,
            "the advisor moved the deadline to next week",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    assert_eq!(record.list_writes.len(), 1, "{:?}", record.list_writes);
    assert!(fixture.planner.status().assignment.is_none());
    // A reason is needed (strict mode), as for other requests.
    fixture.planner.assign(now).unwrap();
    assert!(matches!(
        fixture
            .planner
            .request_change(now, ChangeKind::Changed, "no", Evidence::default()),
        Err(PlannerError::ReasonTooShort)
    ));
}

#[test]
fn constraints_are_the_users_or_the_reviews_and_follow_new_notes() {
    let fixture = harness(true, &["a"]);
    // The user's own constraints win over the review's.
    let mine = TodoChange::Add {
        todo: ProposalInput {
            title: "Repaint the fence".into(),
            constraints: "at home, dry weather".into(),
            ..Default::default()
        },
    };
    let record = fixture
        .planner
        .review_todo_change(Utc::now(), mine, "", "to-do tab")
        .unwrap();
    assert_eq!(record.after.unwrap().constraints, "at home, dry weather");

    // An edit of the notes takes the review's constraints for the edited to-do.
    let new_notes = TodoChange::Edit {
        todo_id: "a".into(),
        todo: ProposalInput {
            notes: "now needs the office printer".into(),
            ..Default::default()
        },
    };
    let record = fixture
        .planner
        .review_todo_change(
            Utc::now(),
            new_notes,
            "the plan changed at work",
            "to-do tab",
        )
        .unwrap();
    assert_eq!(
        record.after.as_ref().unwrap().constraints,
        "AI: needs a computer"
    );
    let writes = fixture.planner.writes_for(&record);
    let TodoListWrite::Merge { changed_fields, .. } = &writes[0] else {
        panic!("{writes:?}");
    };
    assert_eq!(changed_fields["constraints"], "AI: needs a computer");
    assert_eq!(changed_fields["notes"], "now needs the office printer");

    // An edit of the priority only leaves the constraints alone.
    let priority = TodoChange::Edit {
        todo_id: "a".into(),
        todo: ProposalInput {
            priority: Some(4),
            ..Default::default()
        },
    };
    let record = fixture
        .planner
        .review_todo_change(
            Utc::now(),
            priority,
            "the deadline moved closer",
            "to-do tab",
        )
        .unwrap();
    let writes = fixture.planner.writes_for(&record);
    let TodoListWrite::Merge { changed_fields, .. } = &writes[0] else {
        panic!("{writes:?}");
    };
    assert!(
        !changed_fields.contains_key("constraints"),
        "{changed_fields:?}"
    );
}

#[test]
fn a_question_about_one_to_do_shows_its_notes_the_list_does_not() {
    let mut current = todo("a");
    current.notes = "write the design memo, then commit".into();
    current.constraints = "computer".into();
    let mut target = todo("b");
    target.notes = "call the clinic before 6".into();
    let other = {
        let mut other = todo("c");
        other.notes = "secret details of c".into();
        other
    };
    let open = [current, target, other];
    let assignment = Assignment {
        todo_id: Some("a".into()),
        task: "title a".into(),
        why: String::new(),
        source: AssignmentSource::Planner,
        at: Utc::now(),
        until: None,
        when_time_is_up: WhenTimeIsUp::Reassign,
        away_from_computer: false,
        appointment_id: None,
        open: Vec::new(),
        paused_since: None,
        idle: Vec::new(),
    };
    let request = ChangeKind::SwitchTodo {
        todo_id: "b".into(),
    };
    let text = crate::advisor::review_text(&ReviewRequest {
        instructions: "",
        now_local: "now",
        life_rules: "",
        current: Some(&assignment),
        request: &request,
        target_title: Some("title b"),
        reason: "the clinic called",
        open: &open,
        recent_requests: &[],
        history: "",
        notes: None,
        evidence: "",
        screenshot: None,
        planned_minutes: None,
        elapsed_minutes: 0,
        spent_on_todo_minutes: 0,
        estimate_minutes: None,
    });
    assert!(
        text.contains("The current to-do's notes: write the design memo, then commit"),
        "{text}"
    );
    assert!(
        text.contains("The current to-do's constraints: computer"),
        "{text}"
    );
    assert!(
        text.contains("The to-do they want to switch to's notes: call the clinic before 6"),
        "{text}"
    );
    assert!(!text.contains("secret details of c"), "{text}");
}

#[test]
fn an_older_follow_up_shows_its_date_from_the_notes() {
    let mut older = todo("f");
    older.notes = "reply politely. not before 2030-01-03, after the call".into();
    let content = crate::advisor::question("", &[older], "Now".into());
    let text = content[0]["text"].as_str().unwrap();
    assert!(
        text.contains("| constraints: Not before 2030-01-03"),
        "{text}"
    );
    assert!(!text.contains("reply politely"), "{text}");
}

#[test]
fn a_task_whose_time_is_up_waits_for_a_report() {
    let fixture = harness(true, &["a"]);
    let start = Utc::now();
    let assignment = fixture.planner.assign(start).unwrap();
    let until = assignment.until.unwrap();
    assert!(!fixture
        .planner
        .report_due(until - chrono::Duration::minutes(1)));
    // Time up: the agent covers the screen until the user reports (machine::lockdown).
    assert!(fixture
        .planner
        .report_due(until + chrono::Duration::minutes(1)));
    assert!(fixture
        .planner
        .report_due(until + chrono::Duration::hours(5)));
}

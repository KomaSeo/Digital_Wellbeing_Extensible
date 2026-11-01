//! Responsibility: Test TaskPlanner end to end with a fake AI advisor.

use super::*;
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
    /// The current assignment as the last change review saw it.
    saw_current_away: AtomicBool,
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
        })
    }

    fn review_appointment(
        &self,
        request: &AppointmentRequest,
    ) -> Result<AppointmentReview, JudgeError> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        *self.saw_appointment_rules.lock() = request.appointment_rules.to_string();
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
        saw_current_away: AtomicBool::new(false),
    });
    let provided = Arc::clone(&advisor);
    let planner = TaskPlanner::new(
        JsonStore::in_memory(PlannerConfig {
            enabled,
            todos: ids.iter().map(|id| todo(id)).collect(),
            ..Default::default()
        }),
        JsonStore::in_memory(PlannerState::default()),
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
    let text = crate::advisor::todo_change_text(&TodoChangeRequest {
        instructions: crate::advisor::PROPOSAL_PROMPT,
        now_local: "2026-10-30 Friday 14:05",
        life_rules: "No movies before 18:00.",
        change: &change,
        target: None,
        reason: "",
        open: &[],
        recent_changes: &[],
        history: "",
    });
    assert!(text.contains("The owner's life rules (binding):\nNo movies before 18:00."));
    assert!(text.contains("\"title\": \"Watch a movie\""));
    assert!(text.contains("empty fields are for you to fill in"));
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
    assert!(text.contains("\"minutes_spent\": 30"), "{text}");
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
fn away_from_computer_tasks_are_marked_and_their_changes_say_so() {
    let fixture = harness(true, &["sleep"]);
    // The to-do does not say: the AI's judgement at the pick counts.
    fixture.advisor.judges_away.store(true, Ordering::SeqCst);
    let now = Utc::now();
    fixture.planner.assign(now).unwrap();
    assert!(fixture.planner.away_from_computer_task().is_some());
    fixture
        .planner
        .request_change(
            now,
            ChangeKind::Repick,
            "I want to play",
            Evidence::default(),
        )
        .unwrap();
    assert!(fixture.advisor.saw_current_away.load(Ordering::SeqCst));
    // The to-do's own flag wins over the AI.
    let fixture = harness(true, &["mail"]);
    fixture
        .planner
        .config_store()
        .update(|config| config.todos[0].away_from_computer = Some(false))
        .unwrap();
    fixture.advisor.judges_away.store(true, Ordering::SeqCst);
    fixture.planner.assign(now).unwrap();
    assert!(fixture.planner.away_from_computer_task().is_none());
    // A new to-do gets the AI's judgement unless the user said.
    fixture.advisor.judges_away.store(true, Ordering::SeqCst);
    let record = fixture
        .planner
        .review_todo_change(now, add("go running"), "", "to-do tab")
        .unwrap();
    assert_eq!(record.after.unwrap().away_from_computer, Some(true));
    let record = fixture
        .planner
        .review_todo_change(
            now + chrono::Duration::seconds(1),
            TodoChange::Add {
                todo: ProposalInput {
                    title: "online class".into(),
                    away_from_computer: Some(false),
                    ..Default::default()
                },
            },
            "",
            "to-do tab",
        )
        .unwrap();
    assert_eq!(record.after.unwrap().away_from_computer, Some(false));
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
    assert!(accepted[0].away_from_computer);
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
    assert!(current.away_from_computer);
    assert!(fixture.planner.away_from_computer_task().is_some());
    assert_eq!(fixture.focus.store().get().task, "meeting");
    // "할 일 받기" during it gives the appointment, not a to-do.
    let picks = fixture.advisor.picks.load(Ordering::SeqCst);
    let again = fixture
        .planner
        .assign(start + chrono::Duration::minutes(10))
        .unwrap();
    assert!(again.appointment_id.is_some());
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), picks);
    // At its end it ends by itself: the user asks for the next task.
    fixture
        .planner
        .resume(start + chrono::Duration::minutes(61))
        .unwrap();
    assert!(fixture.planner.status().assignment.is_none());
    assert!(fixture.planner.waiting_for_task());
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

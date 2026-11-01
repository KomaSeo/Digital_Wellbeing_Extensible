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
    /// What the next done review says about the result: decisive, open_ended or not_applicable.
    outcome: parking_lot::Mutex<String>,
    /// Proof text and whether a screenshot came with the last review.
    saw_evidence: parking_lot::Mutex<(String, bool)>,
    /// Planned, elapsed and spent minutes the last review carried.
    saw_minutes: parking_lot::Mutex<(Option<i64>, i64, i64)>,
}

impl Advisor for FakeAdvisor {
    fn pick(&self, request: &PickRequest) -> Result<Pick, JudgeError> {
        self.picks.fetch_add(1, Ordering::SeqCst);
        *self.saw_rules.lock() = request.life_rules.to_string();
        *self.saw_instructions.lock() = request.instructions.to_string();
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
            },
        })
    }

    fn review(&self, request: &ReviewRequest) -> Result<Review, JudgeError> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        *self.saw_rules.lock() = request.life_rules.to_string();
        *self.saw_instructions.lock() = request.instructions.to_string();
        *self.saw_evidence.lock() = (request.evidence.to_string(), request.screenshot.is_some());
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
        outcome: parking_lot::Mutex::new("not_applicable".into()),
        saw_evidence: parking_lot::Mutex::new((String::new(), false)),
        saw_minutes: parking_lot::Mutex::new((None, 0, 0)),
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
fn inactive_when_disabled_or_empty() {
    assert!(!harness(false, &["a"]).planner.is_active());
    assert!(!harness(true, &[]).planner.is_active());
    let fixture = harness(false, &["a"]);
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
fn approved_done_moves_to_next_and_ends_after_the_last() {
    let fixture = harness(true, &["a", "b"]);
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
    assert_eq!(fixture.focus.store().get().task, "work on b");
    assert_eq!(fixture.planner.status().open_todos.len(), 1);

    fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Done,
            "4장도 끝냄",
            Evidence::default(),
        )
        .unwrap();
    assert!(!fixture.planner.is_active());
    assert!(fixture.planner.status().assignment.is_none());
    assert!(!fixture.focus.store().get().session_active);
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

#[test]
fn approved_done_with_a_failing_pick_ends_the_session() {
    let fixture = harness(true, &["a", "b"]);
    fixture.planner.assign(Utc::now()).unwrap();
    fixture.advisor.bogus_pick.store(true, Ordering::SeqCst);
    let record = fixture
        .planner
        .request_change(
            Utc::now(),
            ChangeKind::Done,
            "3장 초안 제출함",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    assert!(fixture.planner.status().assignment.is_none());
    assert!(!fixture.focus.store().get().session_active);
    // "a" is done; "b" is still open, so the planner stays active.
    assert!(fixture.planner.is_active());
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
fn an_assignment_runs_for_the_time_the_ai_gave_it_then_a_new_one_is_picked() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now();
    let assignment = fixture.planner.assign(start).unwrap();
    assert_eq!(
        assignment.until,
        Some(start + chrono::Duration::minutes(45))
    );
    assert_eq!(assignment.when_time_is_up, WhenTimeIsUp::Reassign);

    // Before the end: nothing happens and the AI is not asked.
    let before_end = start + chrono::Duration::minutes(44);
    assert!(fixture
        .planner
        .reassign_if_time_is_up(before_end)
        .unwrap()
        .is_none());
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 1);

    // At the end: the AI picks again (here the same to-do) with a new end.
    let at_end = start + chrono::Duration::minutes(45);
    let next = fixture
        .planner
        .reassign_if_time_is_up(at_end)
        .unwrap()
        .expect("a new assignment");
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 2);
    assert_eq!(next.until, Some(at_end + chrono::Duration::minutes(45)));

    // A restart after the time is up picks again instead of resuming.
    let later = at_end + chrono::Duration::minutes(50);
    fixture.planner.assign(later).unwrap();
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 3);
}

#[test]
fn notify_only_needs_the_ai_and_keeps_the_task_past_its_end() {
    let fixture = harness(true, &["a", "b"]);
    let start = Utc::now();
    fixture.planner.assign(start).unwrap();
    let notify_only = ChangeKind::SetWhenTimeIsUp {
        action: WhenTimeIsUp::NotifyOnly,
    };

    // Rejected: nothing changes, and the task is replaced when its time is up.
    fixture.advisor.approve.store(false, Ordering::SeqCst);
    let record = fixture
        .planner
        .request_change(
            start,
            notify_only.clone(),
            "almost done with the proof",
            Evidence::default(),
        )
        .unwrap();
    assert!(!record.approved);
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 1);
    let assignment = fixture.planner.status().assignment.unwrap();
    assert_eq!(assignment.when_time_is_up, WhenTimeIsUp::Reassign);

    // Approved: the task stays after its end, also across a restart.
    fixture.advisor.approve.store(true, Ordering::SeqCst);
    assert!(
        fixture
            .planner
            .request_change(
                start,
                notify_only,
                "almost done with the proof",
                Evidence::default()
            )
            .unwrap()
            .approved
    );
    let after_end = start + chrono::Duration::minutes(60);
    assert!(fixture
        .planner
        .reassign_if_time_is_up(after_end)
        .unwrap()
        .is_none());
    let resumed = fixture.planner.assign(after_end).unwrap();
    assert_eq!(resumed.when_time_is_up, WhenTimeIsUp::NotifyOnly);
    assert_eq!(fixture.advisor.picks.load(Ordering::SeqCst), 1);

    // Going back to "a new task when time is up" is stricter: no AI call.
    let record = fixture
        .planner
        .request_change(
            after_end,
            ChangeKind::SetWhenTimeIsUp {
                action: WhenTimeIsUp::Reassign,
            },
            "back to the plan",
            Evidence::default(),
        )
        .unwrap();
    assert!(record.approved);
    assert_eq!(fixture.advisor.reviews.load(Ordering::SeqCst), 2);
    assert!(fixture
        .planner
        .reassign_if_time_is_up(after_end)
        .unwrap()
        .is_some());
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
    let start = Utc::now() - chrono::Duration::minutes(90);
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

    // 60 minutes improving it come off the time left.
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
            improving_from + chrono::Duration::minutes(60),
            ChangeKind::SwitchTodo {
                todo_id: "b".into(),
            },
            "b is due soon after all",
            Evidence::default(),
        )
        .unwrap();
    assert_eq!(
        fixture.planner.state_store_for_tests().get().polish["a"].minutes_left,
        150
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
    // An earlier hour on a, then a new 45-minute assignment finished after 10 minutes.
    let start = Utc::now() - chrono::Duration::minutes(70);
    fixture.planner.assign(start).unwrap();
    fixture
        .planner
        .request_change(
            start + chrono::Duration::minutes(60),
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
            start + chrono::Duration::minutes(70),
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

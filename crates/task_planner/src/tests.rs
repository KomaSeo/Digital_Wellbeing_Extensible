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

    fn plan_day(&self, request: &PickRequest) -> Result<DayPlan, JudgeError> {
        *self.saw_instructions.lock() = request.instructions.to_string();
        Ok(DayPlan {
            at: None,
            items: request
                .open
                .iter()
                .map(|todo| DayPlanItem {
                    todo_id: todo.id.clone(),
                    minutes: 600,
                    why: "next".into(),
                })
                .chain(std::iter::once(DayPlanItem {
                    todo_id: "not on the list".into(),
                    minutes: 30,
                    why: "made up".into(),
                }))
                .collect(),
            summary: "a good day".into(),
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
        let approved = self.approve.load(Ordering::SeqCst);
        Ok(Review {
            approved,
            reply: if approved { "ok" } else { "no" }.into(),
            minutes: 30,
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
        .request_change(Utc::now(), ChangeKind::End, "dinner time now")
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
    let result = fixture
        .planner
        .request_change(Utc::now(), ChangeKind::End, " 응 ");
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
        .request_change(Utc::now(), ChangeKind::Done, "3장 초안 제출함")
        .unwrap();
    assert_eq!(fixture.focus.store().get().task, "work on b");
    assert_eq!(fixture.planner.status().open_todos.len(), 1);

    fixture
        .planner
        .request_change(Utc::now(), ChangeKind::Done, "4장도 끝냄")
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
        .request_change(Utc::now(), ChangeKind::Done, "3장 초안 제출함")
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
    assert_eq!(
        fixture.planner.writes_for(&record),
        [TodoListWrite::Put(after)]
    );
    apply_writes(&fixture, fixture.planner.writes_for(&record));

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
        [TodoListWrite::Dismiss("old".into())]
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
        .request_change(start, notify_only.clone(), "almost done with the proof")
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
            .request_change(start, notify_only, "almost done with the proof")
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
        .request_change(Utc::now(), ChangeKind::End, "dinner time now")
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
    });
    assert!(
        text.contains("outline done") && text.contains("- [x] write intro"),
        "{text}"
    );
}

#[test]
fn a_day_plan_keeps_only_open_todos_and_sensible_lengths() {
    let fixture = harness(true, &["a", "b"]);
    let plan = fixture.planner.plan_day(Utc::now()).unwrap();
    let ids: Vec<_> = plan
        .items
        .iter()
        .map(|item| item.todo_id.as_str())
        .collect();
    assert_eq!(ids, ["a", "b"]);
    assert!(plan.items.iter().all(|item| item.minutes <= 480));
    assert_eq!(
        *fixture.advisor.saw_instructions.lock(),
        crate::advisor::DAY_PLAN_PROMPT
    );
    assert_eq!(
        fixture.planner.status().day_plan.unwrap().summary,
        "a good day"
    );
}

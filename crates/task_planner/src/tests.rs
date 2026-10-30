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
    bogus_pick: AtomicBool,
    picks: AtomicUsize,
    reviews: AtomicUsize,
}

impl Advisor for FakeAdvisor {
    fn pick(&self, r: &PickRequest) -> Result<Pick, JudgeError> {
        self.picks.fetch_add(1, Ordering::SeqCst);
        let todo_id = if self.bogus_pick.load(Ordering::SeqCst) {
            "nope".into()
        } else {
            r.open[0].id.clone()
        };
        Ok(Pick {
            task: format!("work on {todo_id}"),
            todo_id,
            why: "deadline".into(),
        })
    }

    fn review(&self, _r: &ReviewRequest) -> Result<Review, JudgeError> {
        self.reviews.fetch_add(1, Ordering::SeqCst);
        let approved = self.approve.load(Ordering::SeqCst);
        Ok(Review {
            approved,
            reply: if approved { "ok" } else { "no" }.into(),
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
        bogus_pick: AtomicBool::new(false),
        picks: AtomicUsize::new(0),
        reviews: AtomicUsize::new(0),
    });
    let provided = Arc::clone(&advisor);
    let planner = TaskPlanner::new(
        JsonStore::in_memory(PlannerConfig {
            enabled,
            todos: ids.iter().map(|id| todo(id)).collect(),
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
    let h = harness(false, &["a"]);
    assert!(matches!(
        h.planner.assign(Utc::now()),
        Err(PlannerError::NotActive)
    ));
}

#[test]
fn assign_starts_focus_session_and_resumes_without_asking_again() {
    let h = harness(true, &["a", "b"]);
    let a = h.planner.assign(Utc::now()).unwrap();
    assert_eq!(a.todo_id.as_deref(), Some("a"));
    let focus = h.focus.store().get();
    assert!(focus.session_active);
    assert_eq!(focus.task, "work on a");

    // e.g. after logging on again
    h.focus
        .store()
        .update(|c| c.session_active = false)
        .unwrap();
    h.planner.assign(Utc::now()).unwrap();
    assert_eq!(h.advisor.picks.load(Ordering::SeqCst), 1);
    assert!(h.focus.store().get().session_active);
}

#[test]
fn bogus_pick_is_refused() {
    let h = harness(true, &["a"]);
    h.advisor.bogus_pick.store(true, Ordering::SeqCst);
    assert!(matches!(
        h.planner.assign(Utc::now()),
        Err(PlannerError::BadPick(_))
    ));
    assert!(!h.focus.store().get().session_active);
}

#[test]
fn short_reason_is_refused_without_asking_the_ai() {
    let h = harness(true, &["a"]);
    h.planner.assign(Utc::now()).unwrap();
    let r = h
        .planner
        .request_change(Utc::now(), ChangeKind::End, " 응 ");
    assert!(matches!(r, Err(PlannerError::ReasonTooShort)));
    assert_eq!(h.advisor.reviews.load(Ordering::SeqCst), 0);
}

#[test]
fn rejected_request_is_logged_and_changes_nothing() {
    let h = harness(true, &["a", "b"]);
    h.planner.assign(Utc::now()).unwrap();
    h.advisor.approve.store(false, Ordering::SeqCst);
    let record = h
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
    assert_eq!(h.focus.store().get().task, "work on a");
    let status = h.planner.status();
    assert_eq!(status.recent_requests.len(), 1);
    assert_eq!(status.recent_requests[0].reason, "그냥 지루해서");
}

#[test]
fn approved_done_moves_to_next_and_ends_after_the_last() {
    let h = harness(true, &["a", "b"]);
    h.planner.assign(Utc::now()).unwrap();
    h.planner
        .request_change(Utc::now(), ChangeKind::Done, "3장 초안 제출함")
        .unwrap();
    assert_eq!(h.focus.store().get().task, "work on b");
    assert_eq!(h.planner.status().open_todos.len(), 1);

    h.planner
        .request_change(Utc::now(), ChangeKind::Done, "4장도 끝냄")
        .unwrap();
    assert!(!h.planner.is_active());
    assert!(h.planner.status().assignment.is_none());
    assert!(!h.focus.store().get().session_active);
}

#[test]
fn approved_switches_set_the_task() {
    let h = harness(true, &["a", "b"]);
    h.planner.assign(Utc::now()).unwrap();
    h.planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchTodo {
                todo_id: "b".into(),
            },
            "a는 교수님 답장 대기 중",
        )
        .unwrap();
    assert_eq!(h.focus.store().get().task, "title b");

    h.planner
        .request_change(
            Utc::now(),
            ChangeKind::SwitchFree {
                task: "은행 업무".into(),
            },
            "오늘 마감인 서류 제출",
        )
        .unwrap();
    let status = h.planner.status();
    assert_eq!(status.assignment.unwrap().todo_id, None);
    assert_eq!(h.focus.store().get().task, "은행 업무");
}

#[test]
fn switch_to_unknown_todo_is_refused() {
    let h = harness(true, &["a"]);
    h.planner.assign(Utc::now()).unwrap();
    let r = h.planner.request_change(
        Utc::now(),
        ChangeKind::SwitchTodo {
            todo_id: "zzz".into(),
        },
        "this one instead",
    );
    assert!(matches!(r, Err(PlannerError::UnknownTodo(_))));
}

#[test]
fn change_kind_json_shape() {
    let v = serde_json::to_value(ChangeKind::SwitchTodo {
        todo_id: "a".into(),
    })
    .unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "kind": "switch_todo", "todo_id": "a" })
    );
    let done: ChangeKind = serde_json::from_value(serde_json::json!({ "kind": "done" })).unwrap();
    assert_eq!(done, ChangeKind::Done);
}

#[test]
fn approved_done_with_a_failing_pick_ends_the_session() {
    let h = harness(true, &["a", "b"]);
    h.planner.assign(Utc::now()).unwrap();
    h.advisor.bogus_pick.store(true, Ordering::SeqCst);
    let record = h
        .planner
        .request_change(Utc::now(), ChangeKind::Done, "3장 초안 제출함")
        .unwrap();
    assert!(record.approved);
    assert!(h.planner.status().assignment.is_none());
    assert!(!h.focus.store().get().session_active);
    // "a" is done; "b" is still open, so the planner stays active.
    assert!(h.planner.is_active());
}

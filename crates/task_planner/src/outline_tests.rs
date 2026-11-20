//! Tests of [`crate::outline`]: groups, waiting, deadlines and loops.

use crate::model::TodoItem;
use crate::outline::Outline;
use chrono::NaiveDate;

fn todo(id: &str, parent: Option<&str>, after: &[&str]) -> TodoItem {
    let mut todo = TodoItem::new(id);
    todo.id = id.to_string();
    todo.title = format!("title {id}");
    todo.parent = parent.map(str::to_string);
    todo.after = after.iter().map(|waited| waited.to_string()).collect();
    todo
}

fn group(id: &str, parent: Option<&str>, after: &[&str]) -> TodoItem {
    let mut group = todo(id, parent, after);
    group.group = true;
    group
}

fn ids(todos: &[&TodoItem]) -> Vec<String> {
    todos.iter().map(|todo| todo.id.clone()).collect()
}

fn date(text: &str) -> NaiveDate {
    text.parse().unwrap()
}

/// The owner's example: shopping holds "buy brushes", and "draw" (in hobbies) waits for it.
fn shopping_and_drawing() -> Vec<TodoItem> {
    vec![
        group("hobby", None, &[]),
        todo("draw", Some("hobby"), &["brushes"]),
        group("shopping", None, &[]),
        todo("brushes", Some("shopping"), &[]),
        todo("milk", Some("shopping"), &[]),
    ]
}

#[test]
fn a_to_do_waits_for_another_in_a_different_group() {
    let todos = shopping_and_drawing();
    let outline = Outline::new(&todos);
    assert!(!outline.is_ready("draw"));
    assert_eq!(ids(&outline.blockers("draw")), ["brushes"]);
    assert!(outline.is_ready("brushes") && outline.is_ready("milk"));
    // Groups are never assigned.
    assert!(!outline.is_ready("shopping"));
    assert_eq!(outline.path_text("draw"), "title hobby");
    assert_eq!(ids(&outline.waiting_for("brushes")), ["draw"]);

    // Once "buy brushes" is finished (it leaves the open list), drawing can start.
    let finished: Vec<TodoItem> = todos
        .into_iter()
        .filter(|todo| todo.id != "brushes")
        .collect();
    assert!(Outline::new(&finished).is_ready("draw"));
}

#[test]
fn a_group_that_waits_holds_up_everything_inside_it() {
    let todos = vec![
        todo("research", None, &[]),
        group("thesis", None, &["research"]),
        group("chapter", Some("thesis"), &[]),
        todo("intro", Some("chapter"), &[]),
    ];
    let outline = Outline::new(&todos);
    assert_eq!(ids(&outline.blockers("intro")), ["research"]);
    assert_eq!(outline.path_text("intro"), "title thesis › title chapter");
}

#[test]
fn waiting_for_a_group_means_waiting_for_all_of_it() {
    let mut todos = shopping_and_drawing();
    todos[1].after = vec!["shopping".into()];
    let outline = Outline::new(&todos);
    assert_eq!(ids(&outline.blockers("draw")), ["shopping"]);
    // With only routines left inside, the group no longer holds anything up.
    let mut routines = todos.clone();
    routines[3].repeat = true;
    routines[4].repeat = true;
    assert!(Outline::new(&routines).is_ready("draw"));
    // An empty group holds nothing up either.
    let empty: Vec<TodoItem> = todos
        .into_iter()
        .filter(|todo| todo.parent.as_deref() != Some("shopping"))
        .collect();
    assert!(Outline::new(&empty).is_ready("draw"));
}

#[test]
fn routines_and_missing_to_dos_hold_nothing_up() {
    let mut todos = vec![
        todo("walk", None, &[]),
        todo("read", None, &["walk", "gone"]),
    ];
    todos[0].repeat = true;
    assert!(Outline::new(&todos).is_ready("read"));
}

#[test]
fn rows_follow_the_groups_and_the_ranks() {
    let mut todos = vec![
        todo("b", None, &[]),
        group("g", None, &[]),
        todo("g2", Some("g"), &[]),
        todo("g1", Some("g"), &[]),
        todo("a", None, &[]),
        // Its group is missing: it shows at the top.
        todo("lost", Some("nowhere"), &[]),
    ];
    todos[0].rank = 2.0;
    todos[1].rank = 1.0;
    todos[2].rank = 2.0;
    todos[3].rank = 1.0;
    todos[4].rank = 0.5;
    todos[5].rank = 3.0;
    let outline = Outline::new(&todos);
    let rows: Vec<(String, usize)> = outline
        .rows()
        .into_iter()
        .map(|row| (row.todo.id.clone(), row.depth))
        .collect();
    assert_eq!(
        rows,
        [
            ("a".to_string(), 0),
            ("g".to_string(), 0),
            ("g1".to_string(), 1),
            ("g2".to_string(), 1),
            ("b".to_string(), 0),
            ("lost".to_string(), 0),
        ]
    );
    assert_eq!(ids(&outline.children(Some("g"))), ["g1", "g2"]);
    assert_eq!(ids(&outline.leaves_under("g")), ["g1", "g2"]);
}

#[test]
fn a_parent_that_is_not_a_group_or_makes_a_loop_is_ignored() {
    let todos = vec![
        todo("plain", None, &[]),
        todo("child", Some("plain"), &[]),
        group("x", Some("y"), &[]),
        group("y", Some("x"), &[]),
        todo("inside", Some("x"), &[]),
    ];
    let outline = Outline::new(&todos);
    assert!(outline.parent("child").is_none());
    // Every to-do still shows exactly once.
    let mut shown: Vec<String> = outline
        .rows()
        .into_iter()
        .map(|row| row.todo.id.clone())
        .collect();
    shown.sort();
    assert_eq!(shown, ["child", "inside", "plain", "x", "y"]);
}

#[test]
fn the_deadline_comes_from_the_group_or_from_what_waits() {
    let mut todos = shopping_and_drawing();
    todos[0].deadline = Some(date("2026-12-01"));
    todos[1].deadline = Some(date("2026-11-25"));
    let outline = Outline::new(&todos);
    // Drawing is due on the 25th: its own date comes before its group's.
    let draw = outline.due("draw").unwrap();
    assert_eq!(
        (draw.date, draw.from.map(|from| from.id.as_str())),
        (date("2026-11-25"), None)
    );
    // The brushes must be bought by then, since drawing waits for them.
    let brushes = outline.due("brushes").unwrap();
    assert_eq!(
        (brushes.date, brushes.from.map(|from| from.id.as_str())),
        (date("2026-11-25"), Some("draw"))
    );
    assert!(outline.due("milk").is_none());
    // A group's deadline reaches the to-dos inside it.
    todos[2].deadline = Some(date("2026-11-22"));
    let outline = Outline::new(&todos);
    let milk = outline.due("milk").unwrap();
    assert_eq!(
        (milk.date, milk.from.map(|from| from.id.as_str())),
        (date("2026-11-22"), Some("shopping"))
    );
}

#[test]
fn to_dos_that_wait_for_each_other_are_found() {
    assert!(Outline::new(&shopping_and_drawing())
        .waiting_loop()
        .is_none());
    let mut circle = shopping_and_drawing();
    circle[3].after = vec!["draw".into()];
    let found = Outline::new(&circle).waiting_loop().unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    // A to-do waiting for the group it is in waits for itself.
    let inside = vec![group("g", None, &[]), todo("a", Some("g"), &["g"])];
    assert_eq!(Outline::new(&inside).waiting_loop().unwrap(), ["title a"]);
    // So does a group waiting for something inside it.
    let group_inside = vec![group("g", None, &["a"]), todo("a", Some("g"), &[])];
    assert!(Outline::new(&group_inside).waiting_loop().is_some());
    // A routine never holds anything up, so it closes no loop.
    let mut routine = shopping_and_drawing();
    routine[3].after = vec!["draw".into()];
    routine[3].repeat = true;
    assert!(Outline::new(&routine).waiting_loop().is_none());
}

#[test]
fn a_group_cannot_go_inside_itself() {
    let todos = vec![
        group("outer", None, &[]),
        group("inner", Some("outer"), &[]),
        todo("leaf", Some("inner"), &[]),
    ];
    let outline = Outline::new(&todos);
    assert!(outline.is_inside("inner", "outer"));
    assert!(outline.is_inside("outer", "outer"));
    assert!(!outline.is_inside("outer", "inner"));
}

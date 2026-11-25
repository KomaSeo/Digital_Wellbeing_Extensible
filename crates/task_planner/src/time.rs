//! Responsibility: Work out how long to-dos still need and how their estimates hold: the time
//! left from the steps' minutes or the estimate, the time over the estimate, a group's sums,
//! whether a deadline can be met at the recent pace, and how long finished to-dos took
//! against their estimates.
//!
//! The owner asked for it while the time a task was given was "너무 부정확해": a group is
//! measured by the sum of its to-dos ("그룹인 경우 하위 task의 합계로 할 일을 측정해야겠지"), and
//! going over an estimate needs handling ("예상된 시간을 넘어갔을 때 대처도 추가를 해야할 거
//! 같아"). Pure functions; the Focus app and the AI's questions both use them.

use crate::model::{DoneRecord, EstimateMiss, MissCause, TaskNotes, TodoItem};
use crate::outline::Outline;
use chrono::NaiveDate;
use std::collections::BTreeMap;

/// An estimate this long or longer, with no steps, is a guess worth noting.
pub const LONG_ESTIMATE_MINUTES: u32 = 120;

/// Responsibility: Say how one to-do stands against its estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TodoTime {
    pub spent: i64,
    pub estimate: Option<u32>,
    /// Minutes still needed: the sum of the steps left when every one of them has minutes,
    /// otherwise the estimate minus the time spent (0 once it is spent); `None` with neither.
    pub remaining: Option<i64>,
    /// [`Self::remaining`] comes from the steps.
    pub from_steps: bool,
    /// Minutes spent beyond the estimate while the to-do is still open.
    pub over_by: Option<i64>,
    /// Steps not done yet, and how many of them have no minutes.
    pub steps_left: usize,
    pub steps_without_minutes: usize,
}

/// How `todo` stands, with `spent` minutes on it (for a routine, on this round) and its notes.
pub fn todo_time(todo: &TodoItem, spent: i64, notes: Option<&TaskNotes>) -> TodoTime {
    let steps_left: Vec<_> = notes
        .map(|notes| notes.steps.iter().filter(|step| !step.done).collect())
        .unwrap_or_default();
    let steps_without_minutes = steps_left
        .iter()
        .filter(|step| step.minutes.is_none())
        .count();
    let estimate = todo.estimate_minutes;
    let over_by = estimate
        .map(i64::from)
        .filter(|estimate| spent > *estimate)
        .map(|estimate| spent - estimate);
    let from_steps = !steps_left.is_empty() && steps_without_minutes == 0;
    let remaining = if from_steps {
        Some(
            steps_left
                .iter()
                .map(|step| i64::from(step.minutes.unwrap_or(0)))
                .sum(),
        )
    } else {
        estimate.map(|estimate| (i64::from(estimate) - spent).max(0))
    };
    TodoTime {
        spent,
        estimate,
        remaining,
        from_steps,
        over_by,
        steps_left: steps_left.len(),
        steps_without_minutes,
    }
}

/// Responsibility: Name why a to-do's estimate may not hold, from what is recorded about it
/// (no one marks an estimate uncertain by hand: the owner, "일을 안해봤는데 어떻게 알아").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateNote {
    /// The AI filled it in when the to-do was added.
    ByTheAi,
    /// It is long and the to-do has no steps.
    NoSteps,
    /// It changed this many times.
    Changed(u32),
}

impl EstimateNote {
    pub fn korean(self) -> String {
        match self {
            EstimateNote::ByTheAi => "AI가 정한 예상".into(),
            EstimateNote::NoSteps => "단계 없음".into(),
            EstimateNote::Changed(times) => format!("예상 {times}번 고침"),
        }
    }

    pub fn english(self) -> String {
        match self {
            EstimateNote::ByTheAi => "estimate set by the AI".into(),
            EstimateNote::NoSteps => "no steps".into(),
            EstimateNote::Changed(times) => format!("estimate changed {times} times"),
        }
    }
}

/// What is recorded against `todo`'s estimate; nothing when it has none or is a group.
pub fn estimate_notes(todo: &TodoItem, time: &TodoTime) -> Vec<EstimateNote> {
    let Some(estimate) = todo.estimate_minutes.filter(|_| !todo.group) else {
        return Vec::new();
    };
    let mut notes = Vec::new();
    if todo.estimate_by_ai {
        notes.push(EstimateNote::ByTheAi);
    }
    if estimate >= LONG_ESTIMATE_MINUTES && time.steps_left == 0 {
        notes.push(EstimateNote::NoSteps);
    }
    if todo.estimate_changes >= 2 {
        notes.push(EstimateNote::Changed(todo.estimate_changes));
    }
    notes
}

/// Responsibility: Add up the to-dos inside a group (at any depth), routines left out: they
/// are never finished, so their time would only grow. The time spent counts the groups' own
/// time too: a to-do split into a group keeps what was spent on it before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GroupTime {
    pub todos: usize,
    pub routines: usize,
    /// The sum of the time the to-dos still need, where it is known.
    pub remaining: i64,
    pub spent: i64,
    /// To-dos whose remaining time is unknown (no estimate and no steps with minutes).
    pub without_estimate: usize,
    /// To-dos over their estimate.
    pub over: usize,
}

/// The sums of `group_id`, with the minutes spent and the notes of each to-do.
pub fn group_time(
    outline: &Outline,
    group_id: &str,
    spent: &BTreeMap<String, i64>,
    notes: &BTreeMap<String, TaskNotes>,
) -> GroupTime {
    let mut sums = GroupTime::default();
    for todo in outline.leaves_under(group_id) {
        if todo.repeat {
            sums.routines += 1;
            continue;
        }
        let time = todo_time(
            todo,
            spent.get(&todo.id).copied().unwrap_or(0),
            notes.get(&todo.id),
        );
        sums.todos += 1;
        sums.spent += time.spent;
        match time.remaining {
            Some(remaining) => sums.remaining += remaining,
            None => sums.without_estimate += 1,
        }
        if time.over_by.is_some() {
            sums.over += 1;
        }
    }
    // A to-do split into a group keeps the time spent on it before the split.
    sums.spent += outline
        .groups_under(group_id)
        .iter()
        .map(|group| spent.get(&group.id).copied().unwrap_or(0))
        .sum::<i64>();
    sums
}

/// The minutes in `minutes` (by to-do id) of everything inside `group_id`: its to-dos, and
/// the groups' own time (a to-do split into a group keeps what was spent on it before).
pub fn minutes_under(outline: &Outline, group_id: &str, minutes: &BTreeMap<String, i64>) -> i64 {
    outline
        .leaves_under(group_id)
        .into_iter()
        .chain(outline.groups_under(group_id))
        .map(|todo| minutes.get(&todo.id).copied().unwrap_or(0))
        .sum()
}

/// Responsibility: Say that the work left before a deadline does not fit the time the
/// recent pace gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineRisk {
    pub remaining: i64,
    /// Days from today to the deadline, both counted (0: past it).
    pub days: i64,
    /// Minutes a day: on this work in the last seven days, or, with none, on all work.
    pub pace: i64,
    /// The pace is that of this work itself.
    pub own_pace: bool,
    pub short_by: i64,
}

/// `remaining` minutes due on `due`, against the minutes spent in the last seven days on this
/// work (`own_week`) and on all work (`all_week`). `None` when it fits, or when there was no
/// work at all to judge a pace by.
pub fn deadline_risk(
    remaining: i64,
    due: NaiveDate,
    today: NaiveDate,
    own_week: i64,
    all_week: i64,
) -> Option<DeadlineRisk> {
    if remaining <= 0 {
        return None;
    }
    let (pace, own_pace) = if own_week > 0 {
        (own_week / 7, true)
    } else if all_week > 0 {
        (all_week / 7, false)
    } else {
        return None;
    };
    let days = ((due - today).num_days() + 1).max(0);
    let short_by = remaining - pace * days;
    (short_by > 0).then_some(DeadlineRisk {
        remaining,
        days,
        pace,
        own_pace,
        short_by,
    })
}

/// Responsibility: Compare how long finished to-dos took with their first estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct EstimateAccuracy {
    pub finished: usize,
    pub estimated: i64,
    pub actual: i64,
}

impl EstimateAccuracy {
    /// The actual time over the estimate, e.g. 1.4.
    pub fn ratio(&self) -> f64 {
        if self.estimated == 0 {
            1.0
        } else {
            self.actual as f64 / self.estimated as f64
        }
    }
}

/// Finished to-dos needed before a ratio means anything.
pub const ACCURACY_MINIMUM: usize = 3;

/// How finished to-dos (routines left out) held to their first estimates: all of them, and
/// per group they were in. A ratio needs [`ACCURACY_MINIMUM`] to-dos.
pub fn estimate_accuracy(
    done: &[DoneRecord],
) -> (Option<EstimateAccuracy>, BTreeMap<String, EstimateAccuracy>) {
    let mut all = EstimateAccuracy::default();
    let mut by_group: BTreeMap<String, EstimateAccuracy> = BTreeMap::new();
    for record in done.iter().filter(|record| !record.routine) {
        let (Some(estimate), Some(spent)) = (
            record.first_estimate_minutes.or(record.estimate_minutes),
            record.spent_minutes.filter(|spent| *spent > 0),
        ) else {
            continue;
        };
        for sums in std::iter::once(&mut all).chain(
            record
                .parent
                .as_ref()
                .map(|parent| by_group.entry(parent.clone()).or_default()),
        ) {
            sums.finished += 1;
            sums.estimated += i64::from(estimate);
            sums.actual += spent;
        }
    }
    by_group.retain(|_, sums| sums.finished >= ACCURACY_MINIMUM);
    ((all.finished >= ACCURACY_MINIMUM).then_some(all), by_group)
}

/// How often each cause was given for going over an estimate.
pub fn miss_counts(misses: &[EstimateMiss]) -> [(MissCause, usize); 3] {
    [MissCause::NotSplit, MissCause::NotKnown, MissCause::Other].map(|cause| {
        (
            cause,
            misses.iter().filter(|miss| miss.cause == cause).count(),
        )
    })
}

/// Responsibility: Carry what is known about time for the AI's pick: the minutes spent on
/// each open to-do (this round, for a routine), their notes and steps, the minutes of the last
/// seven days, and how estimates held lately.
#[derive(Debug, Clone, Default)]
pub struct TimeFacts {
    pub spent: BTreeMap<String, i64>,
    pub notes: BTreeMap<String, TaskNotes>,
    pub last_week: BTreeMap<String, i64>,
    pub today: Option<NaiveDate>,
    pub accuracy: Option<EstimateAccuracy>,
    pub group_accuracy: BTreeMap<String, EstimateAccuracy>,
    pub misses: Vec<EstimateMiss>,
}

/// The part of the pick question about time: the to-dos with time spent, steps or notes on
/// their estimates; the groups with a deadline and what is left in them; how estimates held.
/// Empty when there is nothing to say. It changes as the user works, so it goes after the
/// cached list.
pub fn time_section(open: &[TodoItem], facts: &TimeFacts) -> String {
    let outline = Outline::new(open);
    let mut todo_lines = Vec::new();
    for todo in open.iter().filter(|todo| !todo.group) {
        let notes = facts.notes.get(&todo.id);
        let time = todo_time(todo, facts.spent.get(&todo.id).copied().unwrap_or(0), notes);
        let mut parts = Vec::new();
        if time.spent > 0 {
            parts.push(format!("spent {} min", time.spent));
        }
        if let (Some(over), Some(estimate)) = (time.over_by, time.estimate) {
            parts.push(format!(
                "over its estimate of {estimate} min by {over} min, not finished"
            ));
        }
        if let Some(remaining) = time.remaining.filter(|_| time.from_steps || time.spent > 0) {
            if time.from_steps {
                parts.push(format!("still needed {remaining} min (from its steps)"));
            } else if time.over_by.is_none() {
                parts.push(format!("still needed {remaining} min"));
            }
        }
        let estimate_notes: Vec<String> = estimate_notes(todo, &time)
            .into_iter()
            .map(EstimateNote::english)
            .collect();
        if !estimate_notes.is_empty() {
            parts.push(estimate_notes.join(", "));
        }
        if let Some(accuracy) = todo
            .parent
            .as_ref()
            .and_then(|parent| facts.group_accuracy.get(parent))
            .filter(|accuracy| (accuracy.ratio() - 1.0).abs() >= 0.2)
        {
            parts.push(format!(
                "to-dos in its group took {:.1} times their estimates",
                accuracy.ratio()
            ));
        }
        if let Some(notes) = notes.filter(|notes| !notes.steps.is_empty()) {
            let steps: Vec<String> = notes
                .steps
                .iter()
                .map(|step| {
                    let mark = if step.done { "[x]" } else { "[ ]" };
                    match step.minutes {
                        Some(minutes) => format!("{mark} {} ({minutes})", step.text),
                        None => format!("{mark} {}", step.text),
                    }
                })
                .collect();
            parts.push(format!("steps: {}", steps.join(", ")));
        }
        if !parts.is_empty() {
            todo_lines.push(format!(
                "- [{}] {}: {}",
                todo.id,
                todo.title,
                parts.join(" | ")
            ));
        }
    }
    let all_week: i64 = facts.last_week.values().sum();
    let mut group_lines = Vec::new();
    for group in open.iter().filter(|todo| todo.group) {
        let sums = group_time(&outline, &group.id, &facts.spent, &facts.notes);
        if sums.remaining <= 0 && sums.over == 0 {
            continue;
        }
        let mut line = format!(
            "- \"{}\": {} min of work left in it",
            group.title, sums.remaining
        );
        if sums.without_estimate > 0 {
            line.push_str(&format!(
                " ({} to-dos without an estimate not counted)",
                sums.without_estimate
            ));
        }
        if let (Some(due), Some(today)) = (group.deadline, facts.today) {
            line.push_str(&format!(", due {due}"));
            let own_week = minutes_under(&outline, &group.id, &facts.last_week);
            if let Some(risk) = deadline_risk(sums.remaining, due, today, own_week, all_week) {
                line.push_str(&format!(
                    "; at the pace of the last seven days ({} min a day {}) about {} min fit \
                     before it: {} min short",
                    risk.pace,
                    if risk.own_pace {
                        "on it"
                    } else {
                        "on all work"
                    },
                    risk.pace * risk.days,
                    risk.short_by
                ));
            }
        }
        group_lines.push(line);
    }
    let mut history = Vec::new();
    if let Some(accuracy) = facts.accuracy {
        history.push(format!(
            "Finished to-dos lately took {:.1} times their first estimates ({} to-dos).",
            accuracy.ratio(),
            accuracy.finished
        ));
    }
    let counts = miss_counts(&facts.misses);
    if counts.iter().any(|(_, count)| *count > 0) {
        let named: Vec<String> = counts
            .iter()
            .map(|(cause, count)| {
                let name = match cause {
                    MissCause::NotSplit => "not split into small enough steps",
                    MissCause::NotKnown => "did not know the work well",
                    MissCause::Other => "other",
                };
                format!("{name} {count}")
            })
            .collect();
        history.push(format!(
            "Reasons the user gave for going over an estimate: {}.",
            named.join(", ")
        ));
    }
    if todo_lines.is_empty() && group_lines.is_empty() && history.is_empty() {
        return String::new();
    }
    let mut text = String::from(
        "Time (minutes spent so far; what is still needed comes from the steps' minutes when \
         every step left has them, otherwise from the estimate). Use it to set `minutes`: plan \
         the session around the next unfinished steps and name them in `task`; a to-do over its \
         estimate is not nearly done, so judge what is left from its steps; when a group is \
         short of time before its deadline, give it the time first.\n",
    );
    if !todo_lines.is_empty() {
        text.push_str(&format!("{}\n", todo_lines.join("\n")));
    }
    if !group_lines.is_empty() {
        text.push_str(&format!("Groups:\n{}\n", group_lines.join("\n")));
    }
    if !history.is_empty() {
        text.push_str(&format!("{}\n", history.join(" ")));
    }
    text
}

/// "2시간 15분", "45분", "3시간".
pub fn minutes_korean(minutes: i64) -> String {
    let minutes = minutes.max(0);
    match (minutes / 60, minutes % 60) {
        (0, rest) => format!("{rest}분"),
        (hours, 0) => format!("{hours}시간"),
        (hours, rest) => format!("{hours}시간 {rest}분"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskStep;
    use chrono::{TimeZone, Utc};

    fn todo(id: &str, estimate: Option<u32>) -> TodoItem {
        let mut todo = TodoItem::new(id);
        todo.id = id.to_string();
        todo.estimate_minutes = estimate;
        todo
    }

    fn steps(list: &[(bool, Option<u32>)]) -> TaskNotes {
        TaskNotes {
            text: String::new(),
            steps: list
                .iter()
                .map(|(done, minutes)| TaskStep {
                    text: "step".into(),
                    done: *done,
                    minutes: *minutes,
                })
                .collect(),
        }
    }

    #[test]
    fn the_steps_left_tell_the_time_left_when_they_all_have_minutes() {
        let design = todo("design", Some(180));
        // Over the estimate, with every step left timed: the steps say what is left.
        let notes = steps(&[(true, Some(30)), (false, Some(30)), (false, Some(45))]);
        let time = todo_time(&design, 230, Some(&notes));
        assert_eq!(time.remaining, Some(75));
        assert!(time.from_steps);
        assert_eq!(time.over_by, Some(50));
        // One step left without minutes: back to the estimate, which is spent.
        let notes = steps(&[(false, Some(30)), (false, None)]);
        let time = todo_time(&design, 230, Some(&notes));
        assert_eq!((time.remaining, time.from_steps), (Some(0), false));
        assert_eq!(time.steps_without_minutes, 1);
        // No steps: the estimate minus the time spent.
        assert_eq!(todo_time(&design, 60, None).remaining, Some(120));
        assert_eq!(todo_time(&todo("x", None), 60, None).remaining, None);
    }

    #[test]
    fn notes_say_why_an_estimate_may_not_hold() {
        let mut guess = todo("guess", Some(600));
        guess.estimate_by_ai = true;
        guess.estimate_changes = 2;
        let time = todo_time(&guess, 0, None);
        assert_eq!(
            estimate_notes(&guess, &time),
            [
                EstimateNote::ByTheAi,
                EstimateNote::NoSteps,
                EstimateNote::Changed(2)
            ]
        );
        let short = todo("short", Some(30));
        assert!(estimate_notes(&short, &todo_time(&short, 0, None)).is_empty());
    }

    #[test]
    fn a_group_adds_up_its_to_dos_but_not_its_routines() {
        let mut group = todo("g", None);
        group.group = true;
        let mut inner = todo("inner", None);
        inner.group = true;
        inner.parent = Some("g".into());
        let mut first = todo("a", Some(120));
        first.parent = Some("g".into());
        let mut second = todo("b", Some(60));
        second.parent = Some("inner".into());
        let mut unknown = todo("c", None);
        unknown.parent = Some("g".into());
        let mut routine = todo("r", Some(40));
        routine.parent = Some("g".into());
        routine.repeat = true;
        let todos = vec![group, inner, first, second, unknown, routine];
        let outline = Outline::new(&todos);
        let spent = BTreeMap::from([("a".to_string(), 30), ("b".to_string(), 90)]);
        let sums = group_time(&outline, "g", &spent, &BTreeMap::new());
        assert_eq!(
            sums,
            GroupTime {
                todos: 3,
                routines: 1,
                remaining: 90,
                spent: 120,
                without_estimate: 1,
                over: 1,
            }
        );
    }

    #[test]
    fn a_deadline_is_at_risk_when_the_recent_pace_falls_short() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        let due = NaiveDate::from_ymd_opt(2026, 11, 30).unwrap();
        // 6 days at 25 minutes a day: 150 minutes for 840 left.
        let risk = deadline_risk(840, due, today, 175, 900).unwrap();
        assert_eq!(
            (risk.days, risk.pace, risk.own_pace, risk.short_by),
            (6, 25, true, 690)
        );
        // No work on it this week: the pace of all work, 300 a day, is enough.
        assert!(deadline_risk(840, due, today, 0, 2100).is_none());
        // No work at all: nothing to judge by.
        assert!(deadline_risk(840, due, today, 0, 0).is_none());
        // Past the deadline: every minute left is short.
        let late = deadline_risk(60, today, due, 70, 70).unwrap();
        assert_eq!((late.days, late.short_by), (0, 60));
    }

    #[test]
    fn the_pick_question_gets_the_time_spent_steps_and_groups_at_risk() {
        let mut group = todo("chi", None);
        group.group = true;
        group.title = "CHI 논문".into();
        group.deadline = NaiveDate::from_ymd_opt(2026, 11, 30);
        let mut design = todo("design", Some(180));
        design.title = "실험 설계".into();
        design.parent = Some("chi".into());
        let mut draft = todo("draft", Some(600));
        draft.title = "초고 쓰기".into();
        draft.parent = Some("chi".into());
        draft.estimate_by_ai = true;
        let quiet = todo("quiet", Some(30));
        let open = vec![group, design, draft, quiet];
        let facts = TimeFacts {
            spent: BTreeMap::from([("design".to_string(), 230)]),
            notes: BTreeMap::from([(
                "design".to_string(),
                steps(&[(true, Some(30)), (false, Some(45))]),
            )]),
            last_week: BTreeMap::from([("design".to_string(), 175)]),
            today: NaiveDate::from_ymd_opt(2026, 11, 25),
            misses: vec![EstimateMiss {
                at: Utc.with_ymd_and_hms(2026, 11, 24, 0, 0, 0).unwrap(),
                todo_id: "design".into(),
                title: "실험 설계".into(),
                estimate_minutes: 180,
                spent_minutes: 200,
                cause: MissCause::NotSplit,
            }],
            ..Default::default()
        };
        let text = time_section(&open, &facts);
        assert!(
            text.contains(
                "- [design] 실험 설계: spent 230 min | over its estimate of 180 min by 50 min, \
                 not finished | still needed 45 min (from its steps) | steps: [x] step (30), [ ] \
                 step (45)"
            ),
            "{text}"
        );
        assert!(
            text.contains("- [draft] 초고 쓰기: estimate set by the AI, no steps"),
            "{text}"
        );
        // Nothing to say about a to-do with no time, steps or notes.
        assert!(!text.contains("[quiet]"), "{text}");
        // 645 minutes left; 6 days at 25 a day fit 150.
        assert!(
            text.contains(
                "- \"CHI 논문\": 645 min of work left in it, due 2026-11-30; at the pace of the \
                 last seven days (25 min a day on it) about 150 min fit before it: 495 min short"
            ),
            "{text}"
        );
        assert!(
            text.contains("not split into small enough steps 1"),
            "{text}"
        );
        assert!(time_section(&[todo("x", None)], &TimeFacts::default()).is_empty());
    }

    #[test]
    fn finished_to_dos_show_how_estimates_held() {
        let record = |parent: &str, estimate: u32, spent: i64| DoneRecord {
            todo_id: "x".into(),
            title: "x".into(),
            at: Utc.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).unwrap(),
            first_estimate_minutes: Some(estimate),
            spent_minutes: Some(spent),
            parent: Some(parent.into()),
            ..Default::default()
        };
        let done = vec![
            record("chi", 60, 100),
            record("chi", 60, 90),
            record("chi", 60, 98),
            record("home", 30, 30),
            DoneRecord {
                routine: true,
                ..record("home", 30, 300)
            },
        ];
        let (all, by_group) = estimate_accuracy(&done);
        let all = all.unwrap();
        assert_eq!((all.finished, all.estimated, all.actual), (4, 210, 318));
        assert!((by_group["chi"].ratio() - 1.6).abs() < 1e-9);
        // Too few to judge "home" by.
        assert!(!by_group.contains_key("home"));
    }
}

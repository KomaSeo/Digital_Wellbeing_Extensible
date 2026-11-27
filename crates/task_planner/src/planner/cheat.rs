//! Responsibility: Run cheat time: start and end it, keep it to the owner's budget or times of
//! day, and show what is left (the owner: "엄격모드에서 치팅시간을 설정할 수 있도록 해줄래?").
//!
//! During cheat time the current task is paused, the focus check and the blocks the AI made
//! rest ([`crate::FocusLink::rest`]), and the screen is not covered even in strict mode
//! (`machine::lockdown`); the always-blocked programs and sites stay blocked. It needs no AI:
//! the budget, or the times of day, are the limit, and they change only in the settings.

use super::{PlannerError, TaskPlanner};
use crate::model::{
    CheatConfig, CheatMode, CheatSession, CheatSpan, CheatView, PlannerState, TimetableEntry,
    TimetableKind,
};
use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Utc};
use tracing::info;

/// How long ended cheat times are kept (enough for a weekly budget and the timetable).
const CHEAT_LOG_DAYS: i64 = 35;

impl TaskPlanner {
    /// The user starts cheat time ("치팅 시작"): with a daily or weekly budget only, for what
    /// is left of it. A cheat time already running is returned as it is.
    pub fn start_cheat(&self, now: DateTime<Utc>) -> Result<CheatSession, PlannerError> {
        let _busy = self.busy.lock();
        let state = self.state.get();
        if let Some(running) = state.cheat {
            return Ok(running);
        }
        let config = self.config.get().cheat;
        let minutes = match config.mode {
            CheatMode::Off => return Err(PlannerError::CheatOff),
            CheatMode::Times => return Err(PlannerError::CheatByTimes),
            CheatMode::Daily | CheatMode::Weekly => {
                minutes_left(&config, &state, now).unwrap_or_default()
            }
        };
        if minutes < 1 {
            return Err(PlannerError::NoCheatLeft);
        }
        if appointment_running(&state) {
            return Err(PlannerError::CheatDuringAppointment);
        }
        self.begin_cheat(now, now + Duration::minutes(minutes), false)
    }

    /// The user ends cheat time early ("치팅 끝내기"). A time of day ended early does not start
    /// again before its end.
    pub fn end_cheat(&self, now: DateTime<Utc>) -> Result<(), PlannerError> {
        let _busy = self.busy.lock();
        self.finish_cheat(now, true)
    }

    /// Cheat time runs now.
    pub fn cheating(&self) -> bool {
        self.state.get().cheat.is_some()
    }

    /// Start cheat time until `until`: the running task (not an appointment, nor one whose
    /// time is up) is paused, and the focus check rests.
    pub(super) fn begin_cheat(
        &self,
        now: DateTime<Utc>,
        until: DateTime<Utc>,
        by_times: bool,
    ) -> Result<CheatSession, PlannerError> {
        let pause = self
            .state
            .get()
            .assignment
            .as_ref()
            .is_some_and(|assignment| {
                assignment.appointment_id.is_none()
                    && assignment.paused_since.is_none()
                    && !assignment.time_is_up(now)
            });
        let session = CheatSession {
            started: now,
            until,
            paused_task: pause,
            by_times,
        };
        self.state.update(|planner_state| {
            if pause {
                if let Some(assignment) = planner_state.assignment.as_mut() {
                    assignment.paused_since = Some(now);
                }
            }
            planner_state.cheat = Some(session.clone());
        })?;
        info!("cheat time started, until {until}");
        self.focus.rest(true).map_err(super::focus_error)?;
        if let Some(assignment) = self.state.get().assignment {
            // Paused: the focus check stops watching it.
            self.start_focus(&assignment)?;
        }
        Ok(session)
    }

    /// End the cheat time that runs (at `now`, or at its planned end if that came first): it
    /// is logged, the focus check works again, and the task it paused goes on.
    pub(super) fn finish_cheat(
        &self,
        now: DateTime<Utc>,
        by_user: bool,
    ) -> Result<(), PlannerError> {
        let Some(session) = self.state.get().cheat else {
            return Ok(());
        };
        let end = now.min(session.until).max(session.started);
        self.state.update(|planner_state| {
            planner_state.cheat = None;
            planner_state.cheat_log.push_back(CheatSpan {
                start: session.started,
                end,
            });
            while planner_state
                .cheat_log
                .front()
                .is_some_and(|span| now - span.end > Duration::days(CHEAT_LOG_DAYS))
            {
                planner_state.cheat_log.pop_front();
            }
            if by_user && session.by_times {
                planner_state.cheat_declined_until = Some(session.until);
            }
        })?;
        info!("cheat time ended");
        self.focus.rest(false).map_err(super::focus_error)?;
        let paused_by_it = session.paused_task
            && self
                .state
                .get()
                .assignment
                .is_some_and(|assignment| assignment.paused_since.is_some());
        if paused_by_it {
            self.go_on(end)?;
        }
        Ok(())
    }

    /// The planner loop's part: end cheat time when it is over (or turned off), start the
    /// cheat time of a time of day, and keep the focus check resting exactly while cheat time
    /// runs (after a restart too).
    pub(super) fn keep_cheat(&self, now: DateTime<Utc>) -> Result<(), PlannerError> {
        let config = self.config.get().cheat;
        let state = self.state.get();
        match &state.cheat {
            Some(session) => {
                let over = now >= session.until
                    || config.mode == CheatMode::Off
                    || (session.by_times && config.mode != CheatMode::Times);
                if over {
                    self.finish_cheat(now, false)?;
                }
            }
            None if config.mode == CheatMode::Times
                && !appointment_running(&state)
                && state
                    .cheat_declined_until
                    .is_none_or(|declined| now >= declined) =>
            {
                if let Some(end) = time_of_day_end(&config.times, now) {
                    self.begin_cheat(now, end, true)?;
                }
            }
            None => {}
        }
        let cheating = self.state.get().cheat.is_some();
        if self.focus.resting() != cheating {
            self.focus.rest(cheating).map_err(super::focus_error)?;
        }
        Ok(())
    }

    /// What the Focus app shows about cheat time.
    pub(super) fn cheat_view(&self, state: &PlannerState, now: DateTime<Utc>) -> CheatView {
        let config = self.config.get().cheat;
        CheatView {
            mode: config.mode,
            running: state.cheat.clone(),
            minutes_left: minutes_left(&config, state, now),
            budget_minutes: match config.mode {
                CheatMode::Daily => config.daily_minutes,
                CheatMode::Weekly => config.weekly_minutes,
                _ => 0,
            },
            times: config.times,
        }
    }
}

/// An appointment runs (cheat time does not start during one).
fn appointment_running(state: &PlannerState) -> bool {
    state
        .assignment
        .as_ref()
        .is_some_and(|assignment| assignment.appointment_id.is_some())
}

/// What is left of the day's (or the week's, Monday to Sunday, local time) budget at `now`,
/// the running cheat time counted up to `now`; `None` without a budget.
pub(crate) fn minutes_left(
    config: &CheatConfig,
    state: &PlannerState,
    now: DateTime<Utc>,
) -> Option<i64> {
    let today = now.with_timezone(&Local).date_naive();
    let (from, budget) = match config.mode {
        CheatMode::Daily => (today, config.daily_minutes),
        CheatMode::Weekly => (
            today - Duration::days(i64::from(today.weekday().num_days_from_monday())),
            config.weekly_minutes,
        ),
        CheatMode::Off | CheatMode::Times => return None,
    };
    let from = local_midnight(from);
    let spans = state
        .cheat_log
        .iter()
        .cloned()
        .chain(state.cheat.as_ref().map(|running| CheatSpan {
            start: running.started,
            end: now.min(running.until),
        }));
    let used_seconds: i64 = spans
        .map(|span| {
            (span.end.min(now) - span.start.max(from))
                .num_seconds()
                .max(0)
        })
        .sum();
    // A started minute counts as used.
    let used_minutes = (used_seconds + 59) / 60;
    Some((i64::from(budget) - used_minutes).max(0))
}

/// The start of `day` in local time, as UTC.
fn local_midnight(day: NaiveDate) -> DateTime<Utc> {
    Local
        .from_local_datetime(&day.and_time(NaiveTime::MIN))
        .earliest()
        .map_or_else(
            || day.and_time(NaiveTime::MIN).and_utc(),
            |moment| moment.with_timezone(&Utc),
        )
}

/// A time of day "HH:MM-HH:MM" (the end later the same day); `None` when malformed.
pub fn parse_time_of_day(text: &str) -> Option<(NaiveTime, NaiveTime)> {
    let (start, end) = text.trim().split_once('-')?;
    let start = NaiveTime::parse_from_str(start.trim(), "%H:%M").ok()?;
    let end = NaiveTime::parse_from_str(end.trim(), "%H:%M").ok()?;
    (end > start).then_some((start, end))
}

/// The end of the time of day `now` falls in (local time), if any.
fn time_of_day_end(times: &[String], now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let local_now = now.with_timezone(&Local);
    let clock = local_now.time();
    times
        .iter()
        .filter_map(|text| parse_time_of_day(text))
        .filter(|(start, end)| *start <= clock && clock < *end)
        .map(|(_, end)| end)
        .max()
        .and_then(|end| {
            Local
                .from_local_datetime(&local_now.date_naive().and_time(end))
                .earliest()
                .map(|moment| moment.with_timezone(&Utc))
        })
}

/// Cheat time takes its stretches out of `entries` and shows as its own kind (a task it
/// paused would show as "unknown" there).
pub(super) fn with_cheat_times(
    mut entries: Vec<TimetableEntry>,
    state: &PlannerState,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<TimetableEntry> {
    let spans = state
        .cheat_log
        .iter()
        .cloned()
        .chain(state.cheat.as_ref().map(|running| CheatSpan {
            start: running.started,
            end: now.min(running.until),
        }));
    for span in spans {
        let start = span.start.max(from);
        let end = span.end.min(to);
        if end <= start {
            continue;
        }
        let mut carved = Vec::new();
        for entry in entries {
            if entry.end <= start || entry.start >= end {
                carved.push(entry);
                continue;
            }
            if entry.start < start {
                carved.push(TimetableEntry {
                    end: start,
                    ..entry.clone()
                });
            }
            if entry.end > end {
                carved.push(TimetableEntry {
                    start: end,
                    ..entry.clone()
                });
            }
        }
        carved.push(TimetableEntry {
            start,
            end,
            kind: TimetableKind::Cheat,
            task: "치팅 시간".into(),
            todo_id: None,
        });
        entries = carved;
    }
    entries.sort_by_key(|entry| entry.start);
    entries
}

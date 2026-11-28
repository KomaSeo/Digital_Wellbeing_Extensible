//! Responsibility: Make the to-do list a phone shows (the sync server's phone page) from the
//! planner's two files alone, with the planner's own rules for which to-dos are still open and
//! which appointments are still to come.

use crate::focus_link::{FocusLink, WatchedTask};
use crate::model::{PlannerConfig, PlannerState};
use crate::planner::TaskPlanner;
use ai_client::{AiSettings, JudgeError};
use chrono::{DateTime, Utc};
use json_store::JsonStore;
use serde_json::{json, Value};
use std::sync::Arc;

/// Responsibility: Stand in for the focus check where none runs (the server).
struct NoFocus;

impl FocusLink for NoFocus {
    fn ai_settings(&self) -> AiSettings {
        AiSettings::default()
    }

    fn recent_activity(&self) -> String {
        String::new()
    }

    fn watched(&self) -> Option<WatchedTask> {
        None
    }

    fn watch(&self, _task: Option<WatchedTask>) -> Result<(), String> {
        Ok(())
    }

    fn rest(&self, _resting: bool) -> Result<(), String> {
        Ok(())
    }

    fn resting(&self) -> bool {
        false
    }
}

/// The open to-dos and groups, and the appointments still to come, from `planner.json` and
/// `planner_state.json` as JSON: `{ "todos": […], "appointments": […] }`. Only what the phone
/// page shows goes in.
pub fn phone_list(config: &Value, state: &Value, now: DateTime<Utc>) -> Value {
    let config: PlannerConfig = serde_json::from_value(config.clone()).unwrap_or_default();
    let state: PlannerState = serde_json::from_value(state.clone()).unwrap_or_default();
    let planner = TaskPlanner::new(
        JsonStore::in_memory(config),
        JsonStore::in_memory(state),
        Arc::new(NoFocus),
        Arc::new(|_settings: &AiSettings| Err(JudgeError::NoApiKey)),
    );
    let todos: Vec<Value> = planner
        .open_todos()
        .into_iter()
        .map(|todo| {
            json!({
                "id": todo.id,
                "title": todo.title,
                "notes": todo.notes,
                "deadline": todo.deadline,
                "priority": todo.priority,
                "estimate_minutes": todo.estimate_minutes,
                "repeat": todo.repeat,
                "group": todo.group,
                "parent": todo.parent,
                "rank": todo.rank,
            })
        })
        .collect();
    let appointments: Vec<Value> = planner
        .appointments(now)
        .into_iter()
        .map(|appointment| {
            json!({
                "id": appointment.id,
                "title": appointment.title,
                "start": appointment.start,
                "end": appointment.end,
            })
        })
        .collect();
    json!({ "todos": todos, "appointments": appointments })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phone_sees_open_to_dos_only_and_appointments_to_come() {
        let now = Utc::now();
        let config = json!({
            "todos": [
                { "id": "milk", "title": "Buy milk", "priority": 2 },
                { "id": "done", "title": "Already done", "priority": 3 },
            ],
            "appointments": [
                { "id": "past", "title": "Yesterday", "start": now - chrono::Duration::days(1),
                  "end": now - chrono::Duration::days(1) + chrono::Duration::hours(1) },
                { "id": "dentist", "title": "Dentist", "start": now + chrono::Duration::hours(3),
                  "end": now + chrono::Duration::hours(4) },
            ],
        });
        let state = json!({ "done": [{ "todo_id": "done", "title": "Already done", "at": now }] });
        let list = phone_list(&config, &state, now);
        let titles: Vec<&str> = list["todos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|todo| todo["title"].as_str().unwrap())
            .collect();
        assert_eq!(titles, ["Buy milk"]);
        assert_eq!(list["todos"][0]["priority"], 2);
        let appointments: Vec<&str> = list["appointments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|appointment| appointment["title"].as_str().unwrap())
            .collect();
        assert_eq!(appointments, ["Dentist"]);
    }
}

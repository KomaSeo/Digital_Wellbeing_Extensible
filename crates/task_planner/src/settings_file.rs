//! Responsibility: Describe `planner.json` to the settings code: its name, how to read it,
//! and which edits make the rules stricter.

use crate::{PlannerConfig, TodoListWrite};
use serde_json::Value;
use setting_edits::{
    larger_is_stricter, removing_is_stricter, round_trip, smaller_is_stricter, true_is_stricter,
    Direction, Edit, SettingsFile,
};

/// The task planner's settings file (to-dos, appointments, life rules, instructions).
pub const SETTINGS_FILE: SettingsFile = SettingsFile {
    name: "planner",
    file_name: "planner.json",
    normalize: round_trip::<PlannerConfig>,
    direction,
};

fn direction(path: &[&str], edit: &Edit, in_force: &Value) -> Direction {
    match path {
        ["enabled"] => true_is_stricter(edit),
        // More answers, and more time between the questions, make a looser edit harder.
        ["confirmations", _] => larger_is_stricter(edit, in_force),
        // Less cheat time is stricter (applied at once); more, or another way of giving it,
        // is scheduled.
        ["cheat", "mode"] => match edit {
            Edit::Set { value, .. } if value.as_str() == Some("off") => Direction::Stricter,
            _ => Direction::Looser,
        },
        ["cheat", "daily_minutes" | "weekly_minutes"] => smaller_is_stricter(edit, in_force),
        ["cheat", "times"] => removing_is_stricter(edit),
        _ => Direction::Looser,
    }
}

/// The edits of `planner.json` that make `writes` (the to-do list and appointments a change
/// writes), for the settings files or a simulation's copy.
pub fn list_write_edits(writes: &[TodoListWrite]) -> Vec<Edit> {
    writes
        .iter()
        .map(|write| match write {
            TodoListWrite::Put(todo) => Edit::PutListItem {
                path: vec!["todos".into()],
                id: todo.id.clone(),
                item: Some(serde_json::to_value(todo).unwrap_or(serde_json::Value::Null)),
            },
            TodoListWrite::Merge {
                todo_id,
                changed_fields,
                base,
            } => Edit::MergeListItem {
                path: vec!["todos".into()],
                id: todo_id.clone(),
                changed_fields: changed_fields.clone(),
                base: base
                    .as_ref()
                    .map(|todo| serde_json::to_value(todo).unwrap_or(serde_json::Value::Null)),
            },
            TodoListWrite::Remove(todo_id) => Edit::PutListItem {
                path: vec!["todos".into()],
                id: todo_id.clone(),
                item: None,
            },
            TodoListWrite::Dismiss(todo_id) => Edit::AddItems {
                path: vec!["dismissed".into()],
                items: vec![serde_json::Value::String(todo_id.clone())],
            },
            TodoListWrite::PutAppointment(appointment) => Edit::PutListItem {
                path: vec!["appointments".into()],
                id: appointment.id.clone(),
                item: Some(serde_json::to_value(appointment).unwrap_or(serde_json::Value::Null)),
            },
            TodoListWrite::RemoveAppointment(appointment_id) => Edit::PutListItem {
                path: vec!["appointments".into()],
                id: appointment_id.clone(),
                item: None,
            },
        })
        .collect()
}

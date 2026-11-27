//! Responsibility: Describe `planner.json` to the settings code: its name, how to read it,
//! and which edits make the rules stricter.

use crate::PlannerConfig;
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

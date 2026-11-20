//! Responsibility: Describe `planner.json` to the settings code: its name, how to read it,
//! and which edits make the rules stricter.

use crate::PlannerConfig;
use serde_json::Value;
use setting_edits::{
    larger_is_stricter, round_trip, true_is_stricter, Direction, Edit, SettingsFile,
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
        _ => Direction::Looser,
    }
}

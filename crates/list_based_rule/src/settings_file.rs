//! Responsibility: Describe `policy.json` to the settings code: its name, how to read it, and
//! which edits make the rules stricter.

use crate::ListConfig;
use serde_json::Value;
use setting_edits::{adding_is_stricter, round_trip, Direction, Edit, SettingsFile};

/// The always-blocked programs' settings file.
pub const SETTINGS_FILE: SettingsFile = SettingsFile {
    name: "policy",
    file_name: "policy.json",
    normalize: round_trip::<ListConfig>,
    direction,
};

fn direction(path: &[&str], edit: &Edit, _in_force: &Value) -> Direction {
    match path {
        // More programs always blocked.
        ["blacklist"] => adding_is_stricter(edit),
        _ => Direction::Looser,
    }
}

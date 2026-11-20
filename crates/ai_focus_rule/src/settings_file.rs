//! Responsibility: Describe `ai_focus.json` to the settings code: its name, how to read it,
//! and which edits make the rules stricter.

use crate::AiFocusConfig;
use serde_json::Value;
use setting_edits::{
    adding_is_stricter, false_is_stricter, lookup, new_number, old_number, removing_is_stricter,
    round_trip, smaller_is_stricter, true_is_stricter, Direction, Edit, SettingsFile,
};

/// The AI focus check's settings file.
pub const SETTINGS_FILE: SettingsFile = SettingsFile {
    name: "ai_focus",
    file_name: "ai_focus.json",
    normalize: round_trip::<AiFocusConfig>,
    direction,
};

fn direction(path: &[&str], edit: &Edit, in_force: &Value) -> Direction {
    use Direction::{Looser, Neutral, Stricter};
    match path {
        ["strikes_to_block"] | ["max_grant_minutes"] | ["check_interval_secs"] => {
            smaller_is_stricter(edit, in_force)
        }
        ["recheck_minutes"] => {
            // 0 means "as often as unsure windows" (seconds): the strictest choice.
            let seconds = |minutes: Option<f64>| {
                minutes.map(|minutes| {
                    if minutes == 0.0 {
                        lookup(in_force, &["check_interval_secs".to_string()])
                            .as_f64()
                            .unwrap_or(60.0)
                    } else {
                        minutes * 60.0
                    }
                })
            };
            match (
                seconds(old_number(edit, in_force)),
                seconds(new_number(edit)),
            ) {
                (Some(old_value), Some(new_value)) if new_value <= old_value => Stricter,
                _ => Looser,
            }
        }
        ["send_screenshot"] => true_is_stricter(edit),
        ["allow_permanent_grants"] => false_is_stricter(edit),
        ["daily_budget_usd"] => {
            // 0 means no limit: the strictest, since focus checks never stop.
            let limit = |value: Option<f64>| match value {
                Some(amount) if amount <= 0.0 => f64::INFINITY,
                Some(amount) => amount,
                None => f64::INFINITY,
            };
            let new_value = new_number(edit);
            match new_value {
                Some(_) if limit(new_value) >= limit(old_number(edit, in_force)) => Stricter,
                _ => Looser,
            }
        }
        ["always_allow"] | ["private_apps"] => removing_is_stricter(edit),
        // Only measures: the extra questions decide nothing.
        ["comparison_percent"] => Neutral,
        // Takes effect at once both ways (the owner: "프로그램 업데이트 리스트를 추가는 엄격모드에서
        // 금지 하지 않도록 해줄래? 어차피 사용은 AI가 감시하니까"): an installed program is
        // still judged by the window checks.
        ["updatable_programs"] => Neutral,
        ["revoked_grants"] | ["always_blocked_sites"] => adding_is_stricter(edit),
        // An id no longer blocked is dropped from the list: that lifts nothing.
        ["unblocked"] => removing_is_stricter(edit),
        _ => Looser,
    }
}

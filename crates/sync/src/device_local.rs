//! Responsibility: Name the parts of shared documents that stay on each device, and take them
//! out of (or put them back into) a document.
//!
//! The owner wants one Anthropic API key per device, so:
//! - `ai_focus.json`: the `api_key` value;
//! - `settings_reservations.json`: scheduled updates that change the API key (they are applied
//!   on the device that scheduled them).

use serde_json::Value;

/// Does `value` hold an edit whose `path` starts with `api_key`? (Any edit format.)
fn touches_api_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.get("path")
                .and_then(Value::as_array)
                .and_then(|path| path.first())
                .and_then(Value::as_str)
                == Some("api_key")
                || map.values().any(touches_api_key)
        }
        Value::Array(items) => items.iter().any(touches_api_key),
        _ => false,
    }
}

fn is_api_key_reservation(reservation: &Value) -> bool {
    reservation.get("name").and_then(Value::as_str) == Some("ai_focus")
        && reservation.get("edits").is_some_and(touches_api_key)
}

/// `document` without this device's own parts (what is sent and stored on the server).
pub fn shared_part(document_name: &str, document: &Value) -> Value {
    let mut shared = document.clone();
    match document_name {
        "ai_focus.json" => {
            if let Some(map) = shared.as_object_mut() {
                map.remove("api_key");
            }
        }
        "settings_reservations.json" => {
            if let Some(scheduled) = shared.get_mut("scheduled").and_then(Value::as_array_mut) {
                scheduled.retain(|reservation| !is_api_key_reservation(reservation));
            }
        }
        _ => {}
    }
    shared
}

/// `shared` with this device's own parts taken from `local` (its file as it is now).
pub fn with_local_part(document_name: &str, shared: &Value, local: Option<&Value>) -> Value {
    let mut whole = shared.clone();
    let Some(local) = local else {
        return whole;
    };
    match document_name {
        "ai_focus.json" => {
            if let (Some(map), Some(key)) = (whole.as_object_mut(), local.get("api_key")) {
                map.insert("api_key".into(), key.clone());
            }
        }
        "settings_reservations.json" => {
            let own: Vec<Value> = local
                .get("scheduled")
                .and_then(Value::as_array)
                .map(|scheduled| {
                    scheduled
                        .iter()
                        .filter(|reservation| is_api_key_reservation(reservation))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if !own.is_empty() {
                if let Some(map) = whole.as_object_mut() {
                    let scheduled = map
                        .entry("scheduled")
                        .or_insert_with(|| Value::Array(Vec::new()));
                    if let Some(scheduled) = scheduled.as_array_mut() {
                        scheduled.extend(own);
                    }
                }
            }
        }
        _ => {}
    }
    whole
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_api_key_and_its_scheduled_changes_stay_on_the_device() {
        let settings = json!({ "api_key": "sk-laptop", "model": "m" });
        assert_eq!(
            shared_part("ai_focus.json", &settings),
            json!({ "model": "m" })
        );
        let from_server = json!({ "model": "other" });
        assert_eq!(
            with_local_part("ai_focus.json", &from_server, Some(&settings)),
            json!({ "model": "other", "api_key": "sk-laptop" })
        );

        let key_change = json!({ "id": "r1", "name": "ai_focus",
            "edits": [{ "set": { "path": ["api_key"], "value": "sk-new" } }] });
        let rules_change = json!({ "id": "r2", "name": "planner",
            "edits": [{ "set": { "path": ["life_rules"], "value": "x" } }] });
        let reservations = json!({ "scheduled": [key_change, rules_change], "history": [] });
        let shared = shared_part("settings_reservations.json", &reservations);
        assert_eq!(shared["scheduled"], json!([rules_change]));
        let back = with_local_part("settings_reservations.json", &shared, Some(&reservations));
        assert_eq!(back["scheduled"], json!([rules_change, key_change]));
        // Other documents are shared whole.
        assert_eq!(shared_part("planner.json", &settings), settings);
    }
}

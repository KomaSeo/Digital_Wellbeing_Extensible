//! Responsibility: Merge two versions of a JSON document that both started from the same base
//! (this device's and the server's), keeping both sides' changes where they do not touch the
//! same value.
//!
//! - Objects merge key by key.
//! - Lists whose items all carry an `"id"` merge item by item; other lists keep the server's
//!   items minus what this device removed, plus what this device added (logs and name lists).
//! - Where both sides changed the same value differently, the server's wins.
//! - `atomic` keys (top level) are never merged inside: one side's whole value wins (an
//!   assignment must not mix one device's task with another's end time).

use serde_json::{Map, Value};

/// The merged document: `base` is the version both sides started from.
pub fn merge(base: &Value, local: &Value, remote: &Value, atomic: &[&str]) -> Value {
    if local == base || local == remote {
        return remote.clone();
    }
    if remote == base {
        return local.clone();
    }
    match (base, local, remote) {
        (Value::Object(base), Value::Object(local), Value::Object(remote)) => {
            Value::Object(merge_objects(base, local, remote, atomic))
        }
        _ => merge_values(base, local, remote),
    }
}

fn merge_objects(
    base: &Map<String, Value>,
    local: &Map<String, Value>,
    remote: &Map<String, Value>,
    atomic: &[&str],
) -> Map<String, Value> {
    let mut keys: Vec<&String> = remote.keys().collect();
    keys.extend(local.keys().filter(|key| !remote.contains_key(*key)));
    let mut merged = Map::new();
    for key in keys {
        let value = if atomic.contains(&key.as_str()) {
            merge_atomic(base.get(key), local.get(key), remote.get(key))
        } else {
            merge_optional(base.get(key), local.get(key), remote.get(key))
        };
        if let Some(value) = value {
            merged.insert(key.clone(), value);
        }
    }
    merged
}

fn merge_atomic(
    base: Option<&Value>,
    local: Option<&Value>,
    remote: Option<&Value>,
) -> Option<Value> {
    if local == base {
        remote.cloned()
    } else if remote == base {
        local.cloned()
    } else {
        remote.cloned()
    }
}

/// `None` = the key is absent on that side.
fn merge_optional(
    base: Option<&Value>,
    local: Option<&Value>,
    remote: Option<&Value>,
) -> Option<Value> {
    if local == base || local == remote {
        return remote.cloned();
    }
    if remote == base {
        return local.cloned();
    }
    match (base, local, remote) {
        (Some(base), Some(local), Some(remote)) => Some(merge_values(base, local, remote)),
        // Added on both sides with different values: merge them as if from empty.
        (None, Some(local), Some(remote)) => Some(merge_values(&Value::Null, local, remote)),
        // One side removed it, the other changed it: the server's side wins.
        _ => remote.cloned(),
    }
}

fn merge_values(base: &Value, local: &Value, remote: &Value) -> Value {
    if local == base || local == remote {
        return remote.clone();
    }
    if remote == base {
        return local.clone();
    }
    match (local, remote) {
        (Value::Object(local_map), Value::Object(remote_map)) => {
            let empty = Map::new();
            let base_map = base.as_object().unwrap_or(&empty);
            Value::Object(merge_objects(base_map, local_map, remote_map, &[]))
        }
        (Value::Array(local_items), Value::Array(remote_items)) => {
            let empty = Vec::new();
            let base_items = base.as_array().unwrap_or(&empty);
            Value::Array(merge_arrays(base_items, local_items, remote_items))
        }
        // Both changed the same value: the server's wins.
        _ => remote.clone(),
    }
}

fn id_of(item: &Value) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

fn merge_arrays(base: &[Value], local: &[Value], remote: &[Value]) -> Vec<Value> {
    let all_have_ids = base
        .iter()
        .chain(local)
        .chain(remote)
        .all(|item| id_of(item).is_some());
    if all_have_ids && !(base.is_empty() && local.is_empty() && remote.is_empty()) {
        let find = |items: &'_ [Value], id: &str| -> Option<Value> {
            items.iter().find(|item| id_of(item) == Some(id)).cloned()
        };
        let mut ids: Vec<String> = remote.iter().filter_map(id_of).map(String::from).collect();
        for item in local {
            let id = id_of(item).unwrap_or_default();
            if !ids.iter().any(|known| known == id) {
                ids.push(id.to_string());
            }
        }
        return ids
            .iter()
            .filter_map(|id| {
                merge_optional(
                    find(base, id).as_ref(),
                    find(local, id).as_ref(),
                    find(remote, id).as_ref(),
                )
            })
            .collect();
    }
    // The server's items without those this device removed, then what this device added.
    let mut merged: Vec<Value> = remote
        .iter()
        .filter(|item| !(base.contains(item) && !local.contains(item)))
        .cloned()
        .collect();
    for item in local {
        if !base.contains(item) && !merged.contains(item) {
            merged.push(item.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn changes_on_both_sides_are_kept() {
        let base = json!({ "life_rules": "a", "strikes": 2, "blacklist": ["steam.exe"] });
        let local =
            json!({ "life_rules": "b", "strikes": 2, "blacklist": ["steam.exe", "lol.exe"] });
        let remote = json!({ "life_rules": "a", "strikes": 1, "blacklist": [] });
        assert_eq!(
            merge(&base, &local, &remote, &[]),
            json!({ "life_rules": "b", "strikes": 1, "blacklist": ["lol.exe"] })
        );
    }

    #[test]
    fn the_server_wins_a_real_conflict() {
        let base = json!({ "life_rules": "a" });
        let local = json!({ "life_rules": "local" });
        let remote = json!({ "life_rules": "remote" });
        assert_eq!(merge(&base, &local, &remote, &[]), remote);
    }

    #[test]
    fn items_with_ids_merge_one_by_one() {
        let base = json!({ "todos": [{ "id": "a", "title": "A" }, { "id": "b", "title": "B" }] });
        let local = json!({ "todos": [{ "id": "a", "title": "A2" }, { "id": "b", "title": "B" }, { "id": "c", "title": "C" }] });
        let remote = json!({ "todos": [{ "id": "a", "title": "A" }, { "id": "d", "title": "D" }] });
        assert_eq!(
            merge(&base, &local, &remote, &[]),
            json!({ "todos": [{ "id": "a", "title": "A2" }, { "id": "d", "title": "D" }, { "id": "c", "title": "C" }] })
        );
    }

    #[test]
    fn logs_keep_entries_from_both_sides() {
        let base = json!({ "done": [{ "todo_id": "a" }] });
        let local = json!({ "done": [{ "todo_id": "a" }, { "todo_id": "b" }] });
        let remote = json!({ "done": [{ "todo_id": "a" }, { "todo_id": "c" }] });
        assert_eq!(
            merge(&base, &local, &remote, &[]),
            json!({ "done": [{ "todo_id": "a" }, { "todo_id": "c" }, { "todo_id": "b" }] })
        );
    }

    #[test]
    fn atomic_values_are_never_mixed() {
        let base = json!({ "assignment": { "task": "a", "until": 1 } });
        let local = json!({ "assignment": { "task": "a", "until": 2 } });
        let remote = json!({ "assignment": { "task": "b", "until": 1 } });
        assert_eq!(
            merge(&base, &local, &remote, &["assignment"]),
            json!({ "assignment": { "task": "b", "until": 1 } })
        );
        // Without the rule the two would be mixed.
        assert_eq!(
            merge(&base, &local, &remote, &[]),
            json!({ "assignment": { "task": "b", "until": 2 } })
        );
    }
}

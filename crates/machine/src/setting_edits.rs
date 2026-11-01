//! Responsibility: Describe a change to a settings file as separate edits, apply edits to a
//! file, and tell for each edit whether it makes the rules stricter or looser.
//!
//! Once the settings are protected, a stricter edit takes effect at once and a looser one is
//! scheduled (see `crate::settings`). Edits are field by field, so a scheduled edit applies
//! to whatever the file holds when it takes effect, and edits made meanwhile are kept.
//!
//! Which way an edit goes (`direction`) is decided per setting below. An edit whose direction
//! cannot be told (an API key, a model, the life rules, the AI's instructions, a browser list)
//! counts as looser: it is never applied at once.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Responsibility: Hold one change to a settings file.
///
/// `path` names object keys from the file's root, e.g. `["instructions", "pick"]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Edit {
    /// Put `value` at `path` (`null` removes the key).
    Set { path: Vec<String>, value: Value },
    /// Add these strings to the set at `path` (a JSON array used as a set).
    AddItems {
        path: Vec<String>,
        items: Vec<Value>,
    },
    /// Remove these strings from the set at `path`.
    RemoveItems {
        path: Vec<String>,
        items: Vec<Value>,
    },
    /// In the list of objects at `path`, put `item` in place of the element whose `id` is
    /// `id` (appending it when there is none), or remove that element when `item` is `None`.
    PutListItem {
        path: Vec<String>,
        id: String,
        item: Option<Value>,
    },
    /// In the list of objects at `path`, set `changed_fields` on the element whose `id` is
    /// `id`, keeping its other fields. When there is none, append `base` with the fields set,
    /// or change nothing when there is no `base`.
    MergeListItem {
        path: Vec<String>,
        id: String,
        changed_fields: Map<String, Value>,
        #[serde(default)]
        base: Option<Value>,
    },
}

impl Edit {
    pub fn path(&self) -> &[String] {
        match self {
            Edit::Set { path, .. }
            | Edit::AddItems { path, .. }
            | Edit::RemoveItems { path, .. }
            | Edit::PutListItem { path, .. }
            | Edit::MergeListItem { path, .. } => path,
        }
    }

    /// e.g. `always_allow: + steam.exe`, for the Settings tab and the logs.
    pub fn describe(&self) -> String {
        let path = self.path().join(".");
        let shown = |items: &[Value]| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map_or_else(|| item.to_string(), str::to_string)
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        match self {
            Edit::Set { value, .. } => format!("{path} = {}", short(value)),
            Edit::AddItems { items, .. } => format!("{path}: + {}", shown(items)),
            Edit::RemoveItems { items, .. } => format!("{path}: − {}", shown(items)),
            Edit::PutListItem { id, item: None, .. } => format!("{path}: remove {id}"),
            Edit::PutListItem {
                id,
                item: Some(item),
                ..
            } => format!("{path}: {id} = {}", short(item)),
            Edit::MergeListItem {
                id, changed_fields, ..
            } => format!(
                "{path}: {id}: {}",
                changed_fields
                    .iter()
                    .map(|(key, value)| format!("{key} = {}", short(value)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

fn short(value: &Value) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    if text.chars().count() > 300 {
        format!("{}…", text.chars().take(300).collect::<String>())
    } else {
        text
    }
}

/// Which way an edit moves the rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Blocks more, checks more often or more closely, allows less: applied at once.
    Stricter,
    /// Anything else: scheduled and confirmed.
    Looser,
    /// Changes nothing the rules depend on (a timestamp): applied at once.
    Neutral,
}

/// The edits that turn `before` into `after`. Arrays of strings are compared as sets, arrays
/// of objects with an `id` as keyed lists, everything else value by value.
pub fn diff(before: &Value, after: &Value) -> Vec<Edit> {
    let mut edits = Vec::new();
    diff_at(&mut Vec::new(), before, after, &mut edits);
    edits
}

fn diff_at(path: &mut Vec<String>, before: &Value, after: &Value, edits: &mut Vec<Edit>) {
    if before == after {
        return;
    }
    let no_fields = Map::new();
    let as_object = |value: &Value| match value {
        Value::Object(fields) => Some(fields.clone()),
        Value::Null => Some(no_fields.clone()),
        _ => None,
    };
    if before.is_object() || after.is_object() {
        if let (Some(before_fields), Some(after_fields)) = (as_object(before), as_object(after)) {
            let mut keys: Vec<&String> = before_fields.keys().chain(after_fields.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                path.push(key.clone());
                diff_at(
                    path,
                    before_fields.get(key).unwrap_or(&Value::Null),
                    after_fields.get(key).unwrap_or(&Value::Null),
                    edits,
                );
                path.pop();
            }
            return;
        }
    }
    let empty = Vec::new();
    let as_array = |value: &'_ Value| match value {
        Value::Array(items) => Some(items.clone()),
        Value::Null => Some(empty.clone()),
        _ => None,
    };
    if let (Some(before_items), Some(after_items)) = (as_array(before), as_array(after)) {
        let all: Vec<&Value> = before_items.iter().chain(after_items.iter()).collect();
        if !all.is_empty() && all.iter().all(|item| item.is_string()) {
            let added: Vec<Value> = after_items
                .iter()
                .filter(|item| !before_items.contains(item))
                .cloned()
                .collect();
            let removed: Vec<Value> = before_items
                .iter()
                .filter(|item| !after_items.contains(item))
                .cloned()
                .collect();
            if !added.is_empty() {
                edits.push(Edit::AddItems {
                    path: path.clone(),
                    items: added,
                });
            }
            if !removed.is_empty() {
                edits.push(Edit::RemoveItems {
                    path: path.clone(),
                    items: removed,
                });
            }
            return;
        }
        let id_of = |item: &Value| item.get("id").and_then(Value::as_str).map(str::to_string);
        if !all.is_empty() && all.iter().all(|item| id_of(item).is_some()) {
            for item in &after_items {
                let id = id_of(item).unwrap_or_default();
                let previous = before_items
                    .iter()
                    .find(|before_item| id_of(before_item).as_deref() == Some(id.as_str()));
                if previous != Some(item) {
                    edits.push(Edit::PutListItem {
                        path: path.clone(),
                        id,
                        item: Some(item.clone()),
                    });
                }
            }
            for item in &before_items {
                let id = id_of(item).unwrap_or_default();
                if !after_items
                    .iter()
                    .any(|after_item| id_of(after_item).as_deref() == Some(id.as_str()))
                {
                    edits.push(Edit::PutListItem {
                        path: path.clone(),
                        id,
                        item: None,
                    });
                }
            }
            return;
        }
    }
    edits.push(Edit::Set {
        path: path.clone(),
        value: after.clone(),
    });
}

/// Apply `edits` to `file` (an object; `null` becomes one).
pub fn apply(file: &mut Value, edits: &[Edit]) {
    for edit in edits {
        if !file.is_object() {
            *file = Value::Object(Map::new());
        }
        let path = edit.path();
        let Some((last, parents)) = path.split_last() else {
            continue;
        };
        let mut target = &mut *file;
        for key in parents {
            let fields = target.as_object_mut().expect("objects along the path");
            let child = fields
                .entry(key.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            if !child.is_object() {
                *child = Value::Object(Map::new());
            }
            target = child;
        }
        let fields = target.as_object_mut().expect("objects along the path");
        match edit {
            Edit::Set { value, .. } => {
                if value.is_null() {
                    fields.remove(last);
                } else {
                    fields.insert(last.clone(), value.clone());
                }
            }
            Edit::AddItems { items, .. } => {
                let list = array_at(fields, last);
                for item in items {
                    if !list.contains(item) {
                        list.push(item.clone());
                    }
                }
            }
            Edit::RemoveItems { items, .. } => {
                array_at(fields, last).retain(|item| !items.contains(item));
            }
            Edit::PutListItem { id, item, .. } => {
                let list = array_at(fields, last);
                let position = list
                    .iter()
                    .position(|element| element.get("id").and_then(Value::as_str) == Some(id));
                match (position, item) {
                    (Some(index), Some(new_item)) => list[index] = new_item.clone(),
                    (None, Some(new_item)) => list.push(new_item.clone()),
                    (Some(index), None) => {
                        list.remove(index);
                    }
                    (None, None) => {}
                }
            }
            Edit::MergeListItem {
                id,
                changed_fields,
                base,
                ..
            } => {
                let list = array_at(fields, last);
                let position = list
                    .iter()
                    .position(|element| element.get("id").and_then(Value::as_str) == Some(id));
                let element = match (position, base) {
                    (Some(index), _) => Some(&mut list[index]),
                    (None, Some(base_item)) => {
                        list.push(base_item.clone());
                        list.last_mut()
                    }
                    (None, None) => None,
                };
                if let Some(Value::Object(element_fields)) = element {
                    for (key, value) in changed_fields {
                        element_fields.insert(key.clone(), value.clone());
                    }
                }
            }
        }
    }
}

fn array_at<'a>(fields: &'a mut Map<String, Value>, key: &str) -> &'a mut Vec<Value> {
    let slot = fields
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !slot.is_array() {
        *slot = Value::Array(Vec::new());
    }
    slot.as_array_mut().expect("just made an array")
}

/// Which way `edit` of the settings file `name` moves the rules, given the file in force.
pub fn direction(name: &str, edit: &Edit, in_force: &Value) -> Direction {
    use Direction::{Looser, Neutral, Stricter};
    let path: Vec<&str> = edit.path().iter().map(String::as_str).collect();
    let old = lookup(in_force, edit.path());
    let number = |value: &Value| value.as_f64();
    let new_number = match edit {
        Edit::Set { value, .. } => number(value),
        _ => None,
    };
    let old_number = number(&old);
    let smaller_is_stricter = || match (old_number, new_number) {
        (Some(old_value), Some(new_value)) if new_value <= old_value => Stricter,
        _ => Looser,
    };
    let true_is_stricter = || match edit {
        Edit::Set { value, .. } if value.as_bool() == Some(true) => Stricter,
        _ => Looser,
    };
    let removing_is_stricter = || match edit {
        Edit::RemoveItems { .. } => Stricter,
        _ => Looser,
    };
    let adding_is_stricter = || match edit {
        Edit::AddItems { .. } => Stricter,
        _ => Looser,
    };
    match (name, path.as_slice()) {
        (_, ["updated_at"]) => Neutral,
        ("policy", ["blacklist"]) => adding_is_stricter(),
        ("ai_focus", ["strikes_to_block"] | ["max_grant_minutes"]) => smaller_is_stricter(),
        ("ai_focus", ["check_interval_secs"]) => smaller_is_stricter(),
        ("ai_focus", ["recheck_minutes"]) => {
            // 0 means "as often as unsure windows" (seconds): the strictest choice.
            let seconds = |minutes: Option<f64>| {
                minutes.map(|minutes| {
                    if minutes == 0.0 {
                        number(&lookup(in_force, &["check_interval_secs".to_string()]))
                            .unwrap_or(60.0)
                    } else {
                        minutes * 60.0
                    }
                })
            };
            match (seconds(old_number), seconds(new_number)) {
                (Some(old_value), Some(new_value)) if new_value <= old_value => Stricter,
                _ => Looser,
            }
        }
        ("ai_focus", ["send_screenshot"]) => true_is_stricter(),
        ("ai_focus", ["allow_permanent_grants"]) => match edit {
            Edit::Set { value, .. } if value.as_bool() == Some(false) => Stricter,
            _ => Looser,
        },
        ("ai_focus", ["daily_budget_usd"]) => {
            // 0 means no limit: the strictest, since focus checks never stop.
            let limit = |value: Option<f64>| match value {
                Some(amount) if amount <= 0.0 => f64::INFINITY,
                Some(amount) => amount,
                None => f64::INFINITY,
            };
            match new_number {
                Some(_) if limit(new_number) >= limit(old_number) => Stricter,
                _ => Looser,
            }
        }
        ("ai_focus", ["always_allow"] | ["private_apps"]) => removing_is_stricter(),
        // Fewer programs the service may update as administrator.
        ("ai_focus", ["updatable_programs"]) => removing_is_stricter(),
        ("ai_focus", ["revoked_grants"] | ["always_blocked_sites"]) => adding_is_stricter(),
        // An id no longer blocked is dropped from the list: that lifts nothing.
        ("ai_focus", ["unblocked"]) => removing_is_stricter(),
        ("planner", ["enabled"]) => true_is_stricter(),
        _ => Looser,
    }
}

fn lookup(file: &Value, path: &[String]) -> Value {
    let mut current = file;
    for key in path {
        match current.get(key) {
            Some(child) => current = child,
            None => return Value::Null,
        }
    }
    current.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_then_apply_reproduces_the_new_file() {
        let before = json!({
            "always_allow": ["a.exe", "b.exe"],
            "strikes_to_block": 2,
            "todos": [{ "id": "t1", "title": "one" }, { "id": "t2", "title": "two" }],
            "instructions": { "pick": "" },
        });
        let after = json!({
            "always_allow": ["b.exe", "c.exe"],
            "strikes_to_block": 1,
            "todos": [{ "id": "t2", "title": "two!" }, { "id": "t3", "title": "three" }],
            "instructions": { "pick": "mine" },
            "model": "m",
        });
        let edits = diff(&before, &after);
        let mut applied = before.clone();
        apply(&mut applied, &edits);
        // Sets and keyed lists keep their own order; compare as sets.
        assert_eq!(applied["strikes_to_block"], 1);
        assert_eq!(applied["instructions"]["pick"], "mine");
        assert_eq!(applied["model"], "m");
        let mut allow: Vec<_> = applied["always_allow"].as_array().unwrap().clone();
        allow.sort_by_key(|item| item.to_string());
        assert_eq!(allow, [json!("b.exe"), json!("c.exe")]);
        let titles: Vec<_> = applied["todos"]
            .as_array()
            .unwrap()
            .iter()
            .map(|todo| todo["title"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(titles, ["two!", "three"]);
        assert!(diff(&after, &after).is_empty());
    }

    #[test]
    fn stricter_and_looser_edits_are_told_apart() {
        let in_force = json!({
            "always_allow": ["a.exe"], "strikes_to_block": 2, "send_screenshot": false,
            "daily_budget_usd": 1.0, "recheck_minutes": 15, "check_interval_secs": 60,
            "revoked_grants": [], "unblocked": [], "model": "m",
        });
        let direction_of = |after: Value| {
            let edits = diff(&in_force, &after);
            assert_eq!(edits.len(), 1, "{edits:?}");
            direction("ai_focus", &edits[0], &in_force)
        };
        let with = |key: &str, value: Value| {
            let mut changed = in_force.clone();
            changed[key] = value;
            changed
        };
        use Direction::{Looser, Stricter};
        assert_eq!(direction_of(with("always_allow", json!([]))), Stricter);
        assert_eq!(
            direction_of(with("always_allow", json!(["a.exe", "b.exe"]))),
            Looser
        );
        assert_eq!(direction_of(with("strikes_to_block", json!(1))), Stricter);
        assert_eq!(direction_of(with("strikes_to_block", json!(3))), Looser);
        assert_eq!(direction_of(with("send_screenshot", json!(true))), Stricter);
        assert_eq!(direction_of(with("daily_budget_usd", json!(2.0))), Stricter);
        assert_eq!(direction_of(with("daily_budget_usd", json!(0.0))), Stricter);
        assert_eq!(direction_of(with("daily_budget_usd", json!(0.5))), Looser);
        assert_eq!(direction_of(with("recheck_minutes", json!(0))), Stricter);
        assert_eq!(direction_of(with("recheck_minutes", json!(30))), Looser);
        assert_eq!(
            direction_of(with("revoked_grants", json!(["g1"]))),
            Stricter
        );
        assert_eq!(direction_of(with("unblocked", json!(["b1"]))), Looser);
        assert_eq!(direction_of(with("model", json!("other"))), Looser);

        let policy = json!({ "blacklist": ["game.exe"] });
        let blacklist = |after: Value| direction("policy", &diff(&policy, &after)[0], &policy);
        assert_eq!(
            blacklist(json!({ "blacklist": ["game.exe", "x.exe"] })),
            Stricter
        );
        assert_eq!(blacklist(json!({ "blacklist": [] })), Looser);
        let planner = json!({ "enabled": false, "life_rules": "a" });
        let planner_direction =
            |after: Value| direction("planner", &diff(&planner, &after)[0], &planner);
        assert_eq!(
            planner_direction(json!({ "enabled": true, "life_rules": "a" })),
            Stricter
        );
        assert_eq!(
            planner_direction(json!({ "enabled": false, "life_rules": "b" })),
            Looser
        );
    }

    #[test]
    fn a_merge_sets_only_its_fields_and_adds_the_base_when_missing() {
        let mut file = json!({ "todos": [{ "id": "t1", "title": "one", "notes": "kept" }] });
        let mut changed_fields = Map::new();
        changed_fields.insert("title".into(), json!("one!"));
        apply(
            &mut file,
            &[
                Edit::MergeListItem {
                    path: vec!["todos".into()],
                    id: "t1".into(),
                    changed_fields: changed_fields.clone(),
                    base: None,
                },
                Edit::MergeListItem {
                    path: vec!["todos".into()],
                    id: "gone".into(),
                    changed_fields: changed_fields.clone(),
                    base: None,
                },
                Edit::MergeListItem {
                    path: vec!["todos".into()],
                    id: "old".into(),
                    changed_fields,
                    base: Some(json!({ "id": "old", "title": "x", "notes": "n" })),
                },
            ],
        );
        assert_eq!(
            file["todos"],
            json!([
                { "id": "t1", "title": "one!", "notes": "kept" },
                { "id": "old", "title": "one!", "notes": "n" }
            ])
        );
    }
}

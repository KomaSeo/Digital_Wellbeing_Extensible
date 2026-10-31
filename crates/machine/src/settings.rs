//! Responsibility: Let users edit the settings through their agent at any time, apply an edit
//! at once while the settings are unlocked and only when the lock ends while they are locked,
//! and let anyone lock them for a while.
//!
//! The settings (`policy.json`, `ai_focus.json`, `planner.json`) sit in the data folder, which
//! only SYSTEM and Administrators can write; the user works in a standard account. The program
//! does not tell accounts apart: what decides is the lock alone. The agent (SYSTEM) reads them
//! for any local user who asks ([`snapshot`]) and saves what the user sends ([`put`]); the
//! Focus app's Settings tab is built on this.
//!
//! - **unlocked** (no lock, or the lock has run out): a saved file takes effect at once;
//! - **locked** ([`SettingsLock`], `settings_lock.json`): a saved file is checked and kept as a
//!   pending change (`settings_pending.json`); the settings in force stay as they are until
//!   the lock ends, and then every agent's reload loop applies the pending changes
//!   ([`apply_pending`]). So a change made in a weak moment gives nothing now: it waits out
//!   the lock, and it can be discarded before then ([`discard_pending`]).
//!
//! Anyone may lock or extend a lock ([`extend_lock`]); nothing in the program shortens one.
//! (Windows cannot stop an administrator account from deleting the file or the service; the
//! user does not work in one.)
//!
//! Standard users cannot change the system clock, so the lock cannot be outwaited by setting
//! the clock forward.

use crate::data_dir;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The admin settings a standard user may edit while unlocked (name -> file in the data
/// folder). `machine.json` (which accounts are managed) is not among them.
pub const EDITABLE: [(&str, &str); 3] = [
    ("policy", "policy.json"),
    ("ai_focus", "ai_focus.json"),
    ("planner", "planner.json"),
];

/// Longest lock one request may add: a typo should not lock the settings for decades.
pub const MAX_LOCK_HOURS: u32 = 24 * 366;

pub fn lock_path() -> PathBuf {
    data_dir().join("settings_lock.json")
}

const PENDING_FILE: &str = "settings_pending.json";

/// Responsibility: Record until when the admin settings are locked for standard users.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsLock {
    pub until: Option<DateTime<Utc>>,
    /// When the lock was last set or extended.
    pub set_at: Option<DateTime<Utc>>,
}

impl SettingsLock {
    pub fn locked(&self, now: DateTime<Utc>) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// `until` while locked, `None` once it has run out.
    pub fn locked_until(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.until.filter(|_| self.locked(now))
    }
}

/// Missing file = unlocked. Only SYSTEM and Administrators can write the file, so an
/// unreadable one comes from them (or a power cut); it reads as unlocked too.
pub fn read_lock(path: &Path) -> SettingsLock {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Lock for `hours` from now, or keep the current lock if it already lasts longer.
pub fn extend_lock(path: &Path, now: DateTime<Utc>, hours: u32) -> io::Result<SettingsLock> {
    if hours == 0 || hours > MAX_LOCK_HOURS {
        return Err(io::Error::other(format!(
            "lock for 1 to {MAX_LOCK_HOURS} hours"
        )));
    }
    let current = read_lock(path);
    let wanted = now + Duration::hours(hours as i64);
    let lock = SettingsLock {
        until: Some(
            current
                .locked_until(now)
                .map_or(wanted, |current_until| current_until.max(wanted)),
        ),
        set_at: Some(now),
    };
    write_atomic(
        path,
        &serde_json::to_vec_pretty(&lock).map_err(io::Error::other)?,
    )?;
    Ok(lock)
}

/// Responsibility: Hold one saved edit that waits for the lock to end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingFile {
    pub content: Value,
    pub saved_at: DateTime<Utc>,
}

/// Responsibility: Say what became of a saved edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PutOutcome {
    /// Unlocked: the file in force was replaced.
    Applied,
    /// Locked: kept as a pending change, applied when the lock ends.
    Pending { applies_at: DateTime<Utc> },
}

/// Responsibility: Carry the admin settings and what the admin panels show next to them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsSnapshot {
    /// Editable files in force by name (see [`EDITABLE`]); a missing file is `null`.
    pub files: BTreeMap<String, Value>,
    /// Saved edits waiting for the lock to end, by the same names.
    pub pending: BTreeMap<String, PendingFile>,
    /// When the pending edits take effect; `None` while unlocked.
    pub locked_until: Option<DateTime<Utc>>,
    /// Read-only views: `planner_state.json` and every user's grants and blocks files.
    pub planner_state: Value,
    pub grants: BTreeMap<String, Value>,
    pub blocks: BTreeMap<String, Value>,
    /// Every user's token usage file (see `ai_focus_rule::usage`).
    pub usage: BTreeMap<String, Value>,
}

/// Read everything the admin panels need from `data_directory`, locked or not.
pub fn snapshot(data_directory: &Path, now: DateTime<Utc>) -> Result<SettingsSnapshot, String> {
    let files = EDITABLE
        .iter()
        .map(|(name, file)| (name.to_string(), read_json(&data_directory.join(file))))
        .collect();
    Ok(SettingsSnapshot {
        files,
        pending: read_pending(data_directory),
        locked_until: locked_until(data_directory, now),
        planner_state: read_json(&data_directory.join("planner_state.json")),
        grants: read_folder(&data_directory.join("grants")),
        blocks: read_folder(&data_directory.join("blocks")),
        usage: read_folder(&data_directory.join("usage")),
    })
}

/// Save `content` as the editable file `name`: in force at once while unlocked, as a pending
/// change while locked. Refused when `content` does not parse as that file's settings.
pub fn put(
    data_directory: &Path,
    now: DateTime<Utc>,
    name: &str,
    content: &Value,
) -> Result<PutOutcome, String> {
    let file = file_of(name)?;
    validate(name, content)?;
    let mut pending = read_pending(data_directory);
    if let Some(until) = locked_until(data_directory, now) {
        pending.insert(
            name.to_string(),
            PendingFile {
                content: content.clone(),
                saved_at: now,
            },
        );
        write_pending(data_directory, &pending)?;
        return Ok(PutOutcome::Pending { applies_at: until });
    }
    write_setting(data_directory, file, content)?;
    // A pending edit is older than this one; applying it later would undo this.
    if pending.remove(name).is_some() {
        write_pending(data_directory, &pending)?;
    }
    Ok(PutOutcome::Applied)
}

/// Drop the pending edit of `name`, or every pending edit (`None`). Allowed while locked:
/// dropping an edit that never took effect gains nothing.
pub fn discard_pending(data_directory: &Path, name: Option<&str>) -> Result<(), String> {
    let mut pending = read_pending(data_directory);
    match name {
        Some(name) => {
            file_of(name)?;
            pending.remove(name);
        }
        None => pending.clear(),
    }
    write_pending(data_directory, &pending)
}

/// Once the lock has ended, put every pending edit in force and return their names. Called
/// every few seconds by each agent; two agents applying the same edit write the same bytes.
pub fn apply_pending(data_directory: &Path, now: DateTime<Utc>) -> Vec<String> {
    let pending = read_pending(data_directory);
    if pending.is_empty() || locked_until(data_directory, now).is_some() {
        return Vec::new();
    }
    let mut applied = Vec::new();
    for (name, edit) in &pending {
        let written = file_of(name).and_then(|file| {
            validate(name, &edit.content)?;
            write_setting(data_directory, file, &edit.content)
        });
        match written {
            Ok(()) => applied.push(name.clone()),
            Err(error) => tracing::warn!("pending settings '{name}' dropped: {error}"),
        }
    }
    // Remove what was handled, unless it was saved again meanwhile.
    let mut left = read_pending(data_directory);
    left.retain(|name, edit| pending.get(name) != Some(edit));
    if let Err(error) = write_pending(data_directory, &left) {
        tracing::warn!("{error}");
    }
    applied
}

fn file_of(name: &str) -> Result<&'static str, String> {
    EDITABLE
        .iter()
        .find(|(editable_name, _)| *editable_name == name)
        .map(|(_, file)| *file)
        .ok_or_else(|| format!("'{name}' is not an editable setting"))
}

fn write_setting(data_directory: &Path, file: &str, content: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(content).map_err(|error| error.to_string())?;
    write_atomic(&data_directory.join(file), &bytes)
        .map_err(|error| format!("could not save {file}: {error}"))
}

fn locked_until(data_directory: &Path, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    read_lock(&data_directory.join("settings_lock.json")).locked_until(now)
}

fn read_pending(data_directory: &Path) -> BTreeMap<String, PendingFile> {
    fs::read_to_string(data_directory.join(PENDING_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_pending(
    data_directory: &Path,
    pending: &BTreeMap<String, PendingFile>,
) -> Result<(), String> {
    let path = data_directory.join(PENDING_FILE);
    if pending.is_empty() {
        return match fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                Err(format!("could not update {PENDING_FILE}: {error}"))
            }
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(pending).map_err(|error| error.to_string())?;
    write_atomic(&path, &bytes).map_err(|error| format!("could not save {PENDING_FILE}: {error}"))
}

/// The content must load as the program that reads the file expects it.
fn validate(name: &str, content: &Value) -> Result<(), String> {
    let result = match name {
        "policy" => {
            serde_json::from_value::<list_based_rule::ListConfig>(content.clone()).map(drop)
        }
        "ai_focus" => {
            serde_json::from_value::<ai_focus_rule::AiFocusConfig>(content.clone()).map(drop)
        }
        "planner" => {
            serde_json::from_value::<task_planner::PlannerConfig>(content.clone()).map(drop)
        }
        _ => return Err(format!("'{name}' is not an editable setting")),
    };
    result.map_err(|error| format!("{name}: {error}"))
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

/// Every `<name>.json` in `data_directory` by file stem; unreadable files are left out.
fn read_folder(data_directory: &Path) -> BTreeMap<String, Value> {
    let Ok(entries) = fs::read_dir(data_directory) else {
        return BTreeMap::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let value = read_json(&path);
            let name = path.file_stem()?.to_string_lossy().to_string();
            (!value.is_null()).then_some((name, value))
        })
        .collect()
}

/// Temp file + rename, so a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let data_directory =
            std::env::temp_dir().join(format!("dw_settings_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&data_directory);
        fs::create_dir_all(&data_directory).unwrap();
        data_directory
    }

    #[test]
    fn unlocked_settings_can_be_read_and_written() {
        let data_directory = temp_dir("rw");
        let now = Utc::now();
        put(
            &data_directory,
            now,
            "planner",
            &json!({ "enabled": true, "todos": [] }),
        )
        .unwrap();
        fs::create_dir_all(data_directory.join("blocks")).unwrap();
        fs::write(
            data_directory.join("blocks").join("work.json"),
            r#"{"processes":{}}"#,
        )
        .unwrap();
        let settings_snapshot = snapshot(&data_directory, now).unwrap();
        assert_eq!(settings_snapshot.files["planner"]["enabled"], true);
        assert!(settings_snapshot.files["policy"].is_null());
        assert!(settings_snapshot.blocks.contains_key("work"));
        assert!(settings_snapshot.pending.is_empty() && settings_snapshot.locked_until.is_none());
        // Content that would not load is refused, and so are other files.
        assert!(put(
            &data_directory,
            now,
            "planner",
            &json!({ "enabled": "yes" })
        )
        .is_err());
        assert!(put(&data_directory, now, "machine", &json!({})).is_err());
    }

    #[test]
    fn a_lock_can_only_grow() {
        let data_directory = temp_dir("lock");
        let path = data_directory.join("settings_lock.json");
        let now = Utc::now();
        let lock = extend_lock(&path, now, 48).unwrap();
        assert!(lock.locked(now));
        // A shorter request keeps the longer lock.
        let lock = extend_lock(&path, now, 1).unwrap();
        assert_eq!(lock.until, Some(now + Duration::hours(48)));
        assert!(extend_lock(&path, now, 0).is_err());
        assert!(extend_lock(&path, now, MAX_LOCK_HOURS + 1).is_err());
        assert!(!read_lock(&path).locked(now + Duration::hours(49)));
    }

    #[test]
    fn edits_saved_while_locked_wait_for_the_lock_to_end() {
        let data_directory = temp_dir("pending");
        let now = Utc::now();
        put(
            &data_directory,
            now,
            "planner",
            &json!({ "life_rules": "no games" }),
        )
        .unwrap();
        let until = extend_lock(&data_directory.join("settings_lock.json"), now, 24)
            .unwrap()
            .until
            .unwrap();

        // Locked: readable, and an edit is kept aside, not put in force.
        let outcome = put(
            &data_directory,
            now,
            "planner",
            &json!({ "life_rules": "games ok" }),
        )
        .unwrap();
        assert_eq!(outcome, PutOutcome::Pending { applies_at: until });
        let settings_snapshot = snapshot(&data_directory, now).unwrap();
        assert_eq!(settings_snapshot.files["planner"]["life_rules"], "no games");
        assert_eq!(
            settings_snapshot.pending["planner"].content["life_rules"],
            "games ok"
        );
        assert_eq!(settings_snapshot.locked_until, Some(until));
        // A bad edit is refused at once, locked or not.
        assert!(put(
            &data_directory,
            now,
            "planner",
            &json!({ "enabled": "yes" })
        )
        .is_err());
        // Nothing is applied before the lock ends.
        assert!(apply_pending(&data_directory, now + Duration::hours(23)).is_empty());
        assert_eq!(
            snapshot(&data_directory, now).unwrap().files["planner"]["life_rules"],
            "no games"
        );

        // Once it has ended, the edit takes effect and is no longer pending.
        let later = until + Duration::seconds(1);
        assert_eq!(
            apply_pending(&data_directory, later),
            vec!["planner".to_string()]
        );
        let settings_snapshot = snapshot(&data_directory, later).unwrap();
        assert_eq!(settings_snapshot.files["planner"]["life_rules"], "games ok");
        assert!(settings_snapshot.pending.is_empty());
        assert!(!data_directory.join(PENDING_FILE).exists());
    }

    #[test]
    fn pending_edits_can_be_discarded_and_are_replaced_by_newer_ones() {
        let data_directory = temp_dir("discard");
        let now = Utc::now();
        extend_lock(&data_directory.join("settings_lock.json"), now, 1).unwrap();
        put(
            &data_directory,
            now,
            "planner",
            &json!({ "life_rules": "a" }),
        )
        .unwrap();
        put(
            &data_directory,
            now,
            "planner",
            &json!({ "life_rules": "b" }),
        )
        .unwrap();
        put(&data_directory, now, "policy", &json!({})).unwrap();
        let settings_snapshot = snapshot(&data_directory, now).unwrap();
        assert_eq!(
            settings_snapshot.pending["planner"].content["life_rules"],
            "b"
        );
        discard_pending(&data_directory, Some("planner")).unwrap();
        let names: Vec<String> = snapshot(&data_directory, now)
            .unwrap()
            .pending
            .into_keys()
            .collect();
        assert_eq!(names, ["policy"]);
        assert!(discard_pending(&data_directory, Some("machine")).is_err());
        discard_pending(&data_directory, None).unwrap();
        assert!(snapshot(&data_directory, now).unwrap().pending.is_empty());

        // Unlocked, a save takes effect at once and drops an older pending edit.
        let later = now + Duration::hours(2);
        let old = json!({ "planner": { "content": { "life_rules": "old" }, "saved_at": now } });
        fs::write(data_directory.join(PENDING_FILE), old.to_string()).unwrap();
        let outcome = put(
            &data_directory,
            later,
            "planner",
            &json!({ "life_rules": "new" }),
        )
        .unwrap();
        assert_eq!(outcome, PutOutcome::Applied);
        assert!(apply_pending(&data_directory, later).is_empty());
        assert_eq!(
            snapshot(&data_directory, later).unwrap().files["planner"]["life_rules"],
            "new"
        );
    }
}

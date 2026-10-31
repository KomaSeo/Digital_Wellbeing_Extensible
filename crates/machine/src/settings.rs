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
        self.until.is_some_and(|u| now < u)
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
        until: Some(current.locked_until(now).map_or(wanted, |u| u.max(wanted))),
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

/// Read everything the admin panels need from `dir`, locked or not.
pub fn snapshot(dir: &Path, now: DateTime<Utc>) -> Result<SettingsSnapshot, String> {
    let files = EDITABLE
        .iter()
        .map(|(name, file)| (name.to_string(), read_json(&dir.join(file))))
        .collect();
    Ok(SettingsSnapshot {
        files,
        pending: read_pending(dir),
        locked_until: locked_until(dir, now),
        planner_state: read_json(&dir.join("planner_state.json")),
        grants: read_folder(&dir.join("grants")),
        blocks: read_folder(&dir.join("blocks")),
        usage: read_folder(&dir.join("usage")),
    })
}

/// Save `content` as the editable file `name`: in force at once while unlocked, as a pending
/// change while locked. Refused when `content` does not parse as that file's settings.
pub fn put(
    dir: &Path,
    now: DateTime<Utc>,
    name: &str,
    content: &Value,
) -> Result<PutOutcome, String> {
    let file = file_of(name)?;
    validate(name, content)?;
    let mut pending = read_pending(dir);
    if let Some(until) = locked_until(dir, now) {
        pending.insert(
            name.to_string(),
            PendingFile {
                content: content.clone(),
                saved_at: now,
            },
        );
        write_pending(dir, &pending)?;
        return Ok(PutOutcome::Pending { applies_at: until });
    }
    write_setting(dir, file, content)?;
    // A pending edit is older than this one; applying it later would undo this.
    if pending.remove(name).is_some() {
        write_pending(dir, &pending)?;
    }
    Ok(PutOutcome::Applied)
}

/// Drop the pending edit of `name`, or every pending edit (`None`). Allowed while locked:
/// dropping an edit that never took effect gains nothing.
pub fn discard_pending(dir: &Path, name: Option<&str>) -> Result<(), String> {
    let mut pending = read_pending(dir);
    match name {
        Some(name) => {
            file_of(name)?;
            pending.remove(name);
        }
        None => pending.clear(),
    }
    write_pending(dir, &pending)
}

/// Once the lock has ended, put every pending edit in force and return their names. Called
/// every few seconds by each agent; two agents applying the same edit write the same bytes.
pub fn apply_pending(dir: &Path, now: DateTime<Utc>) -> Vec<String> {
    let pending = read_pending(dir);
    if pending.is_empty() || locked_until(dir, now).is_some() {
        return Vec::new();
    }
    let mut applied = Vec::new();
    for (name, edit) in &pending {
        let written = file_of(name).and_then(|file| {
            validate(name, &edit.content)?;
            write_setting(dir, file, &edit.content)
        });
        match written {
            Ok(()) => applied.push(name.clone()),
            Err(err) => tracing::warn!("pending settings '{name}' dropped: {err}"),
        }
    }
    // Remove what was handled, unless it was saved again meanwhile.
    let mut left = read_pending(dir);
    left.retain(|name, edit| pending.get(name) != Some(edit));
    if let Err(err) = write_pending(dir, &left) {
        tracing::warn!("{err}");
    }
    applied
}

fn file_of(name: &str) -> Result<&'static str, String> {
    EDITABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, file)| *file)
        .ok_or_else(|| format!("'{name}' is not an editable setting"))
}

fn write_setting(dir: &Path, file: &str, content: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(content).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(file), &bytes).map_err(|e| format!("could not save {file}: {e}"))
}

fn locked_until(dir: &Path, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    read_lock(&dir.join("settings_lock.json")).locked_until(now)
}

fn read_pending(dir: &Path) -> BTreeMap<String, PendingFile> {
    fs::read_to_string(dir.join(PENDING_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_pending(dir: &Path, pending: &BTreeMap<String, PendingFile>) -> Result<(), String> {
    let path = dir.join(PENDING_FILE);
    if pending.is_empty() {
        return match fs::remove_file(&path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => {
                Err(format!("could not update {PENDING_FILE}: {err}"))
            }
            _ => Ok(()),
        };
    }
    let bytes = serde_json::to_vec_pretty(pending).map_err(|e| e.to_string())?;
    write_atomic(&path, &bytes).map_err(|e| format!("could not save {PENDING_FILE}: {e}"))
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
    result.map_err(|e| format!("{name}: {e}"))
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

/// Every `<name>.json` in `dir` by file stem; unreadable files are left out.
fn read_folder(dir: &Path) -> BTreeMap<String, Value> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let value = read_json(&p);
            let name = p.file_stem()?.to_string_lossy().to_string();
            (!value.is_null()).then_some((name, value))
        })
        .collect()
}

/// Temp file + rename, so a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dw_settings_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unlocked_settings_can_be_read_and_written() {
        let dir = temp_dir("rw");
        let now = Utc::now();
        put(
            &dir,
            now,
            "planner",
            &json!({ "enabled": true, "todos": [] }),
        )
        .unwrap();
        fs::create_dir_all(dir.join("blocks")).unwrap();
        fs::write(dir.join("blocks").join("work.json"), r#"{"processes":{}}"#).unwrap();
        let snap = snapshot(&dir, now).unwrap();
        assert_eq!(snap.files["planner"]["enabled"], true);
        assert!(snap.files["policy"].is_null());
        assert!(snap.blocks.contains_key("work"));
        assert!(snap.pending.is_empty() && snap.locked_until.is_none());
        // Content that would not load is refused, and so are other files.
        assert!(put(&dir, now, "planner", &json!({ "enabled": "yes" })).is_err());
        assert!(put(&dir, now, "machine", &json!({})).is_err());
    }

    #[test]
    fn a_lock_can_only_grow() {
        let dir = temp_dir("lock");
        let path = dir.join("settings_lock.json");
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
        let dir = temp_dir("pending");
        let now = Utc::now();
        put(&dir, now, "planner", &json!({ "life_rules": "no games" })).unwrap();
        let until = extend_lock(&dir.join("settings_lock.json"), now, 24)
            .unwrap()
            .until
            .unwrap();

        // Locked: readable, and an edit is kept aside, not put in force.
        let outcome = put(&dir, now, "planner", &json!({ "life_rules": "games ok" })).unwrap();
        assert_eq!(outcome, PutOutcome::Pending { applies_at: until });
        let snap = snapshot(&dir, now).unwrap();
        assert_eq!(snap.files["planner"]["life_rules"], "no games");
        assert_eq!(snap.pending["planner"].content["life_rules"], "games ok");
        assert_eq!(snap.locked_until, Some(until));
        // A bad edit is refused at once, locked or not.
        assert!(put(&dir, now, "planner", &json!({ "enabled": "yes" })).is_err());
        // Nothing is applied before the lock ends.
        assert!(apply_pending(&dir, now + Duration::hours(23)).is_empty());
        assert_eq!(
            snapshot(&dir, now).unwrap().files["planner"]["life_rules"],
            "no games"
        );

        // Once it has ended, the edit takes effect and is no longer pending.
        let later = until + Duration::seconds(1);
        assert_eq!(apply_pending(&dir, later), vec!["planner".to_string()]);
        let snap = snapshot(&dir, later).unwrap();
        assert_eq!(snap.files["planner"]["life_rules"], "games ok");
        assert!(snap.pending.is_empty());
        assert!(!dir.join(PENDING_FILE).exists());
    }

    #[test]
    fn pending_edits_can_be_discarded_and_are_replaced_by_newer_ones() {
        let dir = temp_dir("discard");
        let now = Utc::now();
        extend_lock(&dir.join("settings_lock.json"), now, 1).unwrap();
        put(&dir, now, "planner", &json!({ "life_rules": "a" })).unwrap();
        put(&dir, now, "planner", &json!({ "life_rules": "b" })).unwrap();
        put(&dir, now, "policy", &json!({})).unwrap();
        let snap = snapshot(&dir, now).unwrap();
        assert_eq!(snap.pending["planner"].content["life_rules"], "b");
        discard_pending(&dir, Some("planner")).unwrap();
        let names: Vec<String> = snapshot(&dir, now).unwrap().pending.into_keys().collect();
        assert_eq!(names, ["policy"]);
        assert!(discard_pending(&dir, Some("machine")).is_err());
        discard_pending(&dir, None).unwrap();
        assert!(snapshot(&dir, now).unwrap().pending.is_empty());

        // Unlocked, a save takes effect at once and drops an older pending edit.
        let later = now + Duration::hours(2);
        let old = json!({ "planner": { "content": { "life_rules": "old" }, "saved_at": now } });
        fs::write(dir.join(PENDING_FILE), old.to_string()).unwrap();
        let outcome = put(&dir, later, "planner", &json!({ "life_rules": "new" })).unwrap();
        assert_eq!(outcome, PutOutcome::Applied);
        assert!(apply_pending(&dir, later).is_empty());
        assert_eq!(
            snapshot(&dir, later).unwrap().files["planner"]["life_rules"],
            "new"
        );
    }
}

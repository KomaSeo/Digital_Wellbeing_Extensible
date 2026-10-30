//! Responsibility: Let users edit the settings through their agent while the settings are
//! unlocked, and let anyone lock them for a while.
//!
//! The settings (`policy.json`, `ai_focus.json`, `planner.json`) sit in the data folder, which
//! only SYSTEM and Administrators can write; the user works in a standard account. The program
//! does not tell accounts apart: what decides is the lock alone.
//!
//! - **unlocked** (no lock, or the lock has run out): the agent (SYSTEM) reads and writes these
//!   files for any local user who asks ([`snapshot`], [`put`]); the Focus app's Settings tab
//!   is built on this;
//! - **locked** ([`SettingsLock`], `settings_lock.json`): the agent refuses. Anyone may lock or
//!   extend a lock ([`extend_lock`]); nothing in the program shortens one. (Windows cannot stop
//!   an administrator account from deleting the file or the service; the user does not work
//!   in one.)
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

/// Responsibility: Carry the admin settings and what the admin panels show next to them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsSnapshot {
    /// Editable files by name (see [`EDITABLE`]); a missing file is `null`.
    pub files: BTreeMap<String, Value>,
    /// Read-only views: `planner_state.json` and every user's grants and blocks files.
    pub planner_state: Value,
    pub grants: BTreeMap<String, Value>,
    pub blocks: BTreeMap<String, Value>,
    /// Every user's token usage file (see `ai_focus_rule::usage`).
    pub usage: BTreeMap<String, Value>,
}

/// Read everything the admin panels need from `dir`; refused while locked.
pub fn snapshot(dir: &Path, now: DateTime<Utc>) -> Result<SettingsSnapshot, String> {
    refuse_if_locked(dir, now)?;
    let files = EDITABLE
        .iter()
        .map(|(name, file)| (name.to_string(), read_json(&dir.join(file))))
        .collect();
    Ok(SettingsSnapshot {
        files,
        planner_state: read_json(&dir.join("planner_state.json")),
        grants: read_folder(&dir.join("grants")),
        blocks: read_folder(&dir.join("blocks")),
        usage: read_folder(&dir.join("usage")),
    })
}

/// Replace the editable file `name` with `content`; refused while locked or when `content`
/// does not parse as that file's settings.
pub fn put(dir: &Path, now: DateTime<Utc>, name: &str, content: &Value) -> Result<(), String> {
    refuse_if_locked(dir, now)?;
    let file = EDITABLE
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, file)| *file)
        .ok_or_else(|| format!("'{name}' is not an editable setting"))?;
    validate(name, content)?;
    let bytes = serde_json::to_vec_pretty(content).map_err(|e| e.to_string())?;
    write_atomic(&dir.join(file), &bytes).map_err(|e| format!("could not save {file}: {e}"))
}

fn refuse_if_locked(dir: &Path, now: DateTime<Utc>) -> Result<(), String> {
    match read_lock(&dir.join("settings_lock.json")).locked_until(now) {
        Some(until) => Err(format!("settings are locked until {}", until.to_rfc3339())),
        None => Ok(()),
    }
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
        // Content that would not load is refused, and so are other files.
        assert!(put(&dir, now, "planner", &json!({ "enabled": "yes" })).is_err());
        assert!(put(&dir, now, "machine", &json!({})).is_err());
    }

    #[test]
    fn a_lock_refuses_both_and_can_only_grow() {
        let dir = temp_dir("lock");
        let path = dir.join("settings_lock.json");
        let now = Utc::now();
        let lock = extend_lock(&path, now, 48).unwrap();
        assert!(lock.locked(now));
        assert!(snapshot(&dir, now).unwrap_err().contains("locked until"));
        assert!(put(&dir, now, "planner", &json!({})).is_err());
        // A shorter request keeps the longer lock.
        let lock = extend_lock(&path, now, 1).unwrap();
        assert_eq!(lock.until, Some(now + Duration::hours(48)));
        assert!(extend_lock(&path, now, 0).is_err());
        assert!(extend_lock(&path, now, MAX_LOCK_HOURS + 1).is_err());
        // After it runs out, editing works again.
        let later = now + Duration::hours(49);
        assert!(!read_lock(&path).locked(later));
        assert!(put(&dir, later, "planner", &json!({})).is_ok());
    }
}

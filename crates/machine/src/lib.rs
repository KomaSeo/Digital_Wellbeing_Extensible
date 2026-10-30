//! Responsibility: Provide what every part of a machine install shares: the data paths and the
//! managed-users setting.
//!
//! Pieces shared by a machine-wide install (service + per-session agent +
//! Focus app + admin UI).
//!
//! Layout of a machine install:
//!
//! ```text
//! C:\Program Files\DigitalWellbeing\     binaries (admin-only write)
//! C:\ProgramData\DigitalWellbeing\       config + logs (SYSTEM/Administrators only)
//!     policy.json      list-based rule
//!     ai_focus.json    AI focus settings
//!     machine.json     which Windows accounts are managed
//!     planner.json     to-do list for the task planner (admin)
//!     planner_state.json  assignment + change-request log (written by agents)
//!     grants\<user>.json  access grants + their log (written by that user's agent)
//!     logs\
//! ```

pub mod session;
pub mod user_api;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// `%ProgramData%\DigitalWellbeing`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("DigitalWellbeing")
}

pub fn policy_path() -> PathBuf {
    data_dir().join("policy.json")
}

pub fn ai_focus_path() -> PathBuf {
    data_dir().join("ai_focus.json")
}

pub fn planner_path() -> PathBuf {
    data_dir().join("planner.json")
}

/// Shared by every managed session's agent (one person, several sessions is rare).
pub fn planner_state_path() -> PathBuf {
    data_dir().join("planner_state.json")
}

/// One grants file per user (`grants\<user>.json`), written by that user's agent.
pub fn grants_dir() -> PathBuf {
    data_dir().join("grants")
}

/// Grants file of `user`. Grants belong to a person, not to one logon, and
/// agents of different users must not overwrite each other's file.
pub fn grants_path_for(user: &str) -> PathBuf {
    let safe: String = bare_user(user)
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    grants_dir().join(format!("{safe}.json"))
}

/// `PC\name` or `DOMAIN\name` -> `name` (sessions report bare names).
pub fn bare_user(user: &str) -> &str {
    user.rsplit('\\').next().unwrap_or(user).trim()
}

pub fn machine_config_path() -> PathBuf {
    data_dir().join("machine.json")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Responsibility: Hold which Windows accounts the service manages, loaded from machine.json.
///
/// Settings only the service reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    /// Windows user names (case-insensitive) whose sessions get an agent.
    /// Empty = every interactive session, including administrators'.
    pub managed_users: Vec<String>,
}

impl MachineConfig {
    /// Missing or unreadable file = defaults (manage everyone).
    pub fn load() -> Self {
        fs::read_to_string(machine_config_path())
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        fs::create_dir_all(data_dir())?;
        fs::write(
            machine_config_path(),
            serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    pub fn manages(&self, user: &str) -> bool {
        self.managed_users.is_empty()
            || self
                .managed_users
                .iter()
                .any(|u| bare_user(u).eq_ignore_ascii_case(bare_user(user)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_manages_everyone() {
        let c = MachineConfig::default();
        assert!(c.manages("anyone"));
        let c = MachineConfig {
            managed_users: vec!["Work".into()],
        };
        assert!(c.manages("work"));
        assert!(!c.manages("Admin"));
        // `--manage PC\Work` must still match the bare name the session reports.
        let c = MachineConfig {
            managed_users: vec!["DESKTOP-1\\Work".into()],
        };
        assert!(c.manages("work"));
        assert!(
            grants_path_for("PC\\Work").ends_with("grants/work.json")
                || grants_path_for("PC\\Work").ends_with("grants\\work.json")
        );
    }
}

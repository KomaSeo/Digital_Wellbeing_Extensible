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
//!     grants.json      access grants + their log (written by agents)
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

/// Access grants and their log; written by agents.
pub fn grants_path() -> PathBuf {
    data_dir().join("grants.json")
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
                .any(|u| u.eq_ignore_ascii_case(user))
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
    }
}

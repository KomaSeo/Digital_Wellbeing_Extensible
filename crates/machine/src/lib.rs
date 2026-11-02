//! Responsibility: Provide what every part of a machine install shares: the data paths and the
//! managed-users setting.
//!
//! Pieces shared by a machine-wide install (service + per-session agent +
//! Focus app, whose Settings tab edits the settings through the agent).
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
//!     blocks\<user>.json  blocks in force (written by that user's agent)
//!     usage\<user>.json   Claude token usage and estimated cost (that user's agent)
//!     settings_lock.json  until when standard users may not edit settings (see `settings`)
//!     logs\
//! ```

/// This is the test edition (built with the `test-edition` feature): the settings protection
/// can be turned off again. The main edition has no way to do that.
pub const TEST_EDITION: bool = cfg!(feature = "test-edition");

pub mod assistant;
pub mod assistant_tools;
pub mod lockdown;
pub mod model_catalog;
pub mod session;
pub mod setting_edits;
pub mod settings;
pub mod system_care;
pub mod user_api;

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// The shared data folder: `%ProgramData%\DigitalWellbeing` on Windows,
/// `/var/lib/digital-wellbeing` elsewhere. `DW_DATA_DIR` overrides it (tests, and
/// running the agent by hand on Linux); on Windows only an administrator can set the
/// environment of the SYSTEM service, so a user cannot move it.
pub fn data_dir() -> PathBuf {
    if let Some(override_directory) =
        std::env::var_os("DW_DATA_DIR").filter(|value| !value.is_empty())
    {
        return PathBuf::from(override_directory);
    }
    if cfg!(windows) {
        let base = std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
        base.join("DigitalWellbeing")
    } else {
        PathBuf::from("/var/lib/digital-wellbeing")
    }
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
        .map(|character| {
            if character.is_alphanumeric() || "-_.".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect();
    grants_dir().join(format!("{safe}.json"))
}

/// One blocks file per user (`blocks\<user>.json`), written by that user's agent.
pub fn blocks_dir() -> PathBuf {
    data_dir().join("blocks")
}

/// One token-usage file per user (`usage\<user>.json`), written by that user's agent.
pub fn usage_dir() -> PathBuf {
    data_dir().join("usage")
}

/// Token usage of `user`'s Claude calls (see `ai_focus_rule::usage`).
pub fn usage_path_for(user: &str) -> PathBuf {
    let file = grants_path_for(user);
    usage_dir().join(file.file_name().unwrap_or_default())
}

/// Blocks in force for `user` (resumed when the agent restarts).
pub fn blocks_path_for(user: &str) -> PathBuf {
    let file = grants_path_for(user);
    blocks_dir().join(file.file_name().unwrap_or_default())
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

    /// Atomic (temp file + rename): a half-written file would read as
    /// "manage everyone" for a moment.
    pub fn save(&self) -> std::io::Result<()> {
        fs::create_dir_all(data_dir())?;
        let path = machine_config_path();
        let temp_path = path.with_extension("json.tmp");
        fs::write(
            &temp_path,
            serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )?;
        fs::rename(&temp_path, &path)
    }

    pub fn manages(&self, user: &str) -> bool {
        self.managed_users.is_empty()
            || self
                .managed_users
                .iter()
                .any(|managed| bare_user(managed).eq_ignore_ascii_case(bare_user(user)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_manages_everyone() {
        let config = MachineConfig::default();
        assert!(config.manages("anyone"));
        let config = MachineConfig {
            managed_users: vec!["Work".into()],
        };
        assert!(config.manages("work"));
        assert!(!config.manages("Admin"));
        // `--manage PC\Work` must still match the bare name the session reports.
        let config = MachineConfig {
            managed_users: vec!["DESKTOP-1\\Work".into()],
        };
        assert!(config.manages("work"));
        assert!(
            grants_path_for("PC\\Work").ends_with("grants/work.json")
                || grants_path_for("PC\\Work").ends_with("grants\\work.json")
        );
    }
}

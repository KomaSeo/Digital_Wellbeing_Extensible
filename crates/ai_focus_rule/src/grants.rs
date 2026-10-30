//! Access grants: the user asks to use an app or website that the focus check
//! blocks, and says what it is for. If the AI approves, the block is lifted.
//!
//! A grant does not end observation. The monitor keeps judging a granted app
//! or site against the stated purpose; repeated off-purpose use revokes the
//! grant and blocks again (see `monitor.rs`).

use crate::config::AiFocusConfig;
use chrono::{DateTime, Utc};
use enforcer::normalize_process_name;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const LOG_CAPACITY: usize = 50;

/// What a grant covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantTarget {
    /// A program by process name (`spotify` and `Spotify.exe` are the same).
    App { process: String },
    /// Browser tabs whose title contains this text, ignoring case.
    Site { keyword: String },
}

impl GrantTarget {
    /// Process names as `name.exe`, keywords trimmed.
    pub fn normalized(&self) -> Self {
        match self {
            Self::App { process } => Self::App {
                process: normalize_process_name(process),
            },
            Self::Site { keyword } => Self::Site {
                keyword: keyword.trim().to_string(),
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::App { process } => process.trim().is_empty(),
            Self::Site { keyword } => keyword.trim().is_empty(),
        }
    }

    /// Does this window fall under the grant?
    pub fn matches(&self, config: &AiFocusConfig, process_name: &str, title: &str) -> bool {
        match self {
            Self::App { process } => normalize_process_name(process_name) == *process,
            Self::Site { keyword } => {
                config.is_browser(process_name) && contains_ignore_case(title, keyword)
            }
        }
    }

    /// Does a block-list key (`steam.exe`, or `chrome.exe|tab title`) fall under the grant?
    pub fn matches_block_key(&self, key: &str) -> bool {
        match self {
            Self::App { process } => key == process,
            Self::Site { keyword } => key
                .split_once('|')
                .is_some_and(|(_, title)| contains_ignore_case(title, keyword)),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::App { process } => process.clone(),
            Self::Site { keyword } => format!("sites with \"{keyword}\" in the tab title"),
        }
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// How long the user asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantDuration {
    Minutes { minutes: u32 },
    /// Until revoked (by the admin, or by the monitor for misuse).
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub target: GrantTarget,
    /// What the user will use it for; the monitor judges use against this.
    pub purpose: String,
    /// `None` = permanent.
    pub until: Option<DateTime<Utc>>,
    pub granted_at: DateTime<Utc>,
    /// Task at the time of the grant, for the admin's information.
    pub task: String,
}

impl Grant {
    pub fn is_active(&self, now: DateTime<Utc>, config: &AiFocusConfig) -> bool {
        !config.revoked_grants.contains(&self.id) && self.until.is_none_or(|u| now < u)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GrantEvent {
    /// The user asked; `approved` is the AI's decision.
    Requested {
        duration: GrantDuration,
        approved: bool,
        /// Minutes actually granted; `None` when permanent or rejected.
        granted_minutes: Option<u32>,
    },
    /// The monitor took the grant back after repeated off-purpose use.
    RevokedForMisuse,
    /// The admin revoked it.
    RevokedByAdmin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantLogEntry {
    pub at: DateTime<Utc>,
    pub target: GrantTarget,
    pub purpose: String,
    pub event: GrantEvent,
    /// The AI's answer, or why the grant ended.
    pub message: String,
}

/// Contents of `grants.json`. Only the process running the monitor writes it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GrantState {
    pub grants: Vec<Grant>,
    /// Newest last, capped.
    pub log: VecDeque<GrantLogEntry>,
}

impl GrantState {
    pub fn push_log(&mut self, entry: GrantLogEntry) {
        if self.log.len() >= LOG_CAPACITY {
            self.log.pop_front();
        }
        self.log.push_back(entry);
    }

    /// The active grant covering this window, if any.
    pub fn covering(
        &self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        process_name: &str,
        title: &str,
    ) -> Option<&Grant> {
        self.grants
            .iter()
            .find(|g| g.is_active(now, config) && g.target.matches(config, process_name, title))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn config() -> AiFocusConfig {
        AiFocusConfig::default()
    }

    #[test]
    fn app_target_matches_by_normalized_name() {
        let t = GrantTarget::App {
            process: "Spotify".into(),
        }
        .normalized();
        assert!(t.matches(&config(), "spotify.exe", "whatever"));
        assert!(t.matches_block_key("spotify.exe"));
        assert!(!t.matches(&config(), "steam.exe", "Spotify"));
    }

    #[test]
    fn site_target_matches_browser_titles_only() {
        let t = GrantTarget::Site {
            keyword: "YouTube".into(),
        };
        assert!(t.matches(&config(), "chrome.exe", "Rust 강의 - youtube"));
        assert!(!t.matches(&config(), "notepad.exe", "youtube notes"));
        assert!(t.matches_block_key("chrome.exe|Rust 강의 - YouTube"));
        assert!(!t.matches_block_key("steam.exe"));
    }

    #[test]
    fn expired_and_revoked_grants_are_inactive() {
        let now = Utc::now();
        let grant = Grant {
            id: "g1".into(),
            target: GrantTarget::App {
                process: "spotify.exe".into(),
            },
            purpose: "집중용 음악".into(),
            until: Some(now + Duration::minutes(10)),
            granted_at: now,
            task: "thesis".into(),
        };
        let mut c = config();
        assert!(grant.is_active(now, &c));
        assert!(!grant.is_active(now + Duration::minutes(11), &c));
        c.revoked_grants.insert("g1".into());
        assert!(!grant.is_active(now, &c));

        let state = GrantState {
            grants: vec![grant],
            log: VecDeque::new(),
        };
        assert!(state.covering(now, &config(), "spotify.exe", "").is_some());
        assert!(state.covering(now, &c, "spotify.exe", "").is_none());
    }

    #[test]
    fn json_shapes() {
        let v = serde_json::to_value(GrantTarget::Site {
            keyword: "docs.rs".into(),
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({ "kind": "site", "keyword": "docs.rs" }));
        let d: GrantDuration =
            serde_json::from_value(serde_json::json!({ "kind": "minutes", "minutes": 30 })).unwrap();
        assert_eq!(d, GrantDuration::Minutes { minutes: 30 });
    }
}

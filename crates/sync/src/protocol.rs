//! Responsibility: Define what a device and the server say to each other in one sync round
//! (the plaintext inside an [`crate::envelope::Envelope`]).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The longest a request may take to arrive, and the furthest the two clocks may differ.
pub const MAX_CLOCK_SKEW_SECONDS: i64 = 600;

/// Responsibility: Carry one document a device sends, with the server revision it started
/// from (0: the server did not have it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Push {
    pub name: String,
    pub base_revision: u64,
    pub content: Value,
}

/// Responsibility: Carry one sync round from a device: what it changed, and which revisions
/// it already has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    pub sent_at: DateTime<Utc>,
    /// Grows with every request of this device, so an old request cannot be sent again.
    pub counter: u64,
    pub pushes: Vec<Push>,
    /// Document name -> the revision this device has.
    pub known: BTreeMap<String, u64>,
}

/// Responsibility: Carry one document the server holds, at a revision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Document {
    pub name: String,
    pub revision: u64,
    pub content: Value,
    /// The device that wrote this revision.
    #[serde(default)]
    pub written_by: String,
}

/// Responsibility: Name a push the server refused, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rejected {
    pub name: String,
    pub reason: String,
}

/// Responsibility: Carry the server's answer: every document newer than the device knows
/// (including the ones it just accepted), and the pushes it refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncResponse {
    pub server_time: DateTime<Utc>,
    pub documents: Vec<Document>,
    pub rejected: Vec<Rejected>,
}

/// Responsibility: Carry what a device needs to join: the server's address is given apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairingCode {
    pub device_id: String,
    pub key: String,
}

impl PairingCode {
    /// One line to copy: `<device id>:<key>`.
    pub fn to_line(&self) -> String {
        format!("{}:{}", self.device_id, self.key)
    }

    pub fn from_line(line: &str) -> Result<Self, String> {
        let (device_id, key) = line
            .trim()
            .split_once(':')
            .ok_or("the pairing code looks like <device>:<key>")?;
        if device_id.is_empty() || key.is_empty() {
            return Err("the pairing code looks like <device>:<key>".into());
        }
        Ok(Self {
            device_id: device_id.to_string(),
            key: key.to_string(),
        })
    }
}

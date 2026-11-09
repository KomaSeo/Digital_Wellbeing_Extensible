//! Responsibility: One device's side of sync: which local files are shared, and one sync round
//! that sends this device's changes and takes in the other devices'.
//!
//! Files (all in the device's data folder, written only by the service):
//! - `sync.json`: the server's address, this device's id and key, and the account key when it
//!   logged in ([`ClientConfig`]).
//! - `sync_state.json`: the request counter and the server revision of every document
//!   ([`SyncState`]).
//! - `sync_base/<name>.json`: each document as the server last had it, the base of a merge.
//!
//! Which local file is which document:
//! - `policy.json`, `ai_focus.json`, `planner.json`, `planner_state.json`,
//!   `settings_protection.json`, `settings_reservations.json`: the same name.
//! - `grants/<user>.json`, `blocks/<user>.json`: the same name.
//! - `usage/<user>.json` (this device's): `usage/<device>/<user>.json`. Another device's usage
//!   is kept as `usage/<device>~<user>.json`, so everything that adds up `usage\` (statistics,
//!   the daily budget) counts every device. Those copies are never sent back.

use crate::device_local::{shared_part, with_local_part};
use crate::envelope::{self, Envelope};
use crate::merge::merge;
use crate::protocol::{Push, Rejected, SyncRequest, SyncResponse};
use crate::server::{
    allowed_name, protected, protection_may_end, read_json, write_json, DEVICE_FOLDER,
    SERVER_DOCUMENTS, SHARED_DOCUMENTS, SHARED_FOLDERS,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_FILE: &str = "sync.json";
pub const STATE_FILE: &str = "sync_state.json";
pub const BASE_FOLDER: &str = "sync_base";
/// Rounds in one sync (a conflict costs one more round).
const MAX_ROUNDS: usize = 4;

/// Responsibility: Say where this device syncs to and with which identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientConfig {
    /// `http://<host>:<port>`.
    pub server_url: String,
    pub device_id: String,
    pub key: String,
    /// The account this device logged in with (none when it was paired with a code): with
    /// it, the device logs in again by itself when the server no longer knows it (the server
    /// was set up again).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<crate::account::AccountCredentials>,
}

impl ClientConfig {
    pub fn load(data_directory: &Path) -> Option<Self> {
        read_json(&data_directory.join(CONFIG_FILE))
    }

    pub fn save(&self, data_directory: &Path) -> Result<(), String> {
        write_json(&data_directory.join(CONFIG_FILE), self)
    }
}

/// Responsibility: Remember what this device last had from the server, between rounds and
/// restarts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncState {
    /// The counter of the last request sent (the server refuses any lower one).
    pub counter: u64,
    /// Document name -> the server revision this device's base copy is.
    pub revisions: BTreeMap<String, u64>,
    pub last_success: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// Pushes the server refused in the last sync (not conflicts), with the reason.
    pub refused: Vec<Rejected>,
    /// Other devices that must be updated before they sync again: (device, its version).
    pub outdated_devices: Vec<(String, String)>,
    /// Every paired device as the server last told: (id, name).
    pub devices: Vec<(String, String)>,
    /// Every paired device with its version and last sync, as the server last told.
    pub device_details: Vec<crate::protocol::DeviceSummary>,
    /// Devices this PC asked to take off the list that the server kept: (id, why).
    pub not_forgotten: Vec<(String, String)>,
}

/// Other devices the user asked to take off the server's list (`{"devices": [id, ...]}`),
/// written by the agent and sent with the next sync, which then deletes the file.
pub const FORGET_FILE: &str = "sync_forget.json";

/// Responsibility: Hold the devices waiting to be taken off the list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ForgetRequest {
    pub devices: Vec<String>,
}

impl ForgetRequest {
    pub fn load(data_directory: &Path) -> Self {
        read_json(&data_directory.join(FORGET_FILE)).unwrap_or_default()
    }

    /// Add `device_id` to the waiting list.
    pub fn add(data_directory: &Path, device_id: &str) -> Result<(), String> {
        let mut request = Self::load(data_directory);
        if !request.devices.iter().any(|waiting| waiting == device_id) {
            request.devices.push(device_id.to_string());
        }
        write_json(&data_directory.join(FORGET_FILE), &request)
    }
}

impl SyncState {
    pub fn load(data_directory: &Path) -> Self {
        read_json(&data_directory.join(STATE_FILE)).unwrap_or_default()
    }

    fn save(&self, data_directory: &Path) -> Result<(), String> {
        write_json(&data_directory.join(STATE_FILE), self)
    }
}

/// Responsibility: Say what one sync did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncReport {
    /// Documents this device sent and the server took.
    pub pushed: Vec<String>,
    /// Documents written here from the server (as they were, or merged with this device's).
    pub pulled: Vec<String>,
    /// Documents where both sides had changed, merged.
    pub merged: Vec<String>,
    pub refused: Vec<Rejected>,
    pub outdated_devices: Vec<(String, String)>,
    pub devices: Vec<(String, String)>,
    pub device_details: Vec<crate::protocol::DeviceSummary>,
    pub not_forgotten: Vec<(String, String)>,
}

/// Top-level values of a document that are never merged inside (one side's whole value wins).
pub fn atomic_keys(document_name: &str) -> &'static [&'static str] {
    match document_name {
        // One device's task with the other's end time would be a task nobody chose.
        "planner_state.json" => &["assignment", "stopped_at", "week_plan"],
        "settings_protection.json" => &["protected_since"],
        _ => &[],
    }
}

/// The local file of `document_name`, or `None` for a name this device does not keep.
pub fn local_path_for(
    data_directory: &Path,
    device_id: &str,
    document_name: &str,
) -> Option<PathBuf> {
    if !allowed_name(document_name, None) {
        return None;
    }
    let parts: Vec<&str> = document_name.split('/').collect();
    match parts.as_slice() {
        [name] => Some(data_directory.join(name)),
        [folder, file] => Some(data_directory.join(folder).join(file)),
        [folder, owner, file] if *folder == DEVICE_FOLDER => {
            let local_name = if *owner == device_id {
                file.to_string()
            } else {
                format!("{owner}~{file}")
            };
            Some(data_directory.join(folder).join(local_name))
        }
        _ => None,
    }
}

/// Is `document_name` another device's file (taken in, never sent)?
fn belongs_to_another_device(device_id: &str, document_name: &str) -> bool {
    let parts: Vec<&str> = document_name.split('/').collect();
    matches!(parts.as_slice(), [folder, owner, _] if *folder == DEVICE_FOLDER && *owner != device_id)
}

/// The documents this device has and may send.
pub fn local_documents(data_directory: &Path, device_id: &str) -> Vec<String> {
    let mut names: Vec<String> = SHARED_DOCUMENTS
        .iter()
        .filter(|name| data_directory.join(name).is_file())
        .map(|name| name.to_string())
        .collect();
    let json_files = |folder: &str| -> Vec<String> {
        let Ok(entries) = fs::read_dir(data_directory.join(folder)) else {
            return Vec::new();
        };
        let mut files: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            // `<name>.json` only (a name may hold dots, e.g. `john.doe.json`): temporary
            // (`….tmp`) and corrupt (`….json.corrupt`) files end otherwise; other devices'
            // files (`<device>~<user>.json`) are taken in, never sent.
            .filter(|file| file.ends_with(".json") && !file.contains('~'))
            .collect();
        files.sort();
        files
    };
    for folder in SHARED_FOLDERS {
        names.extend(
            json_files(folder)
                .into_iter()
                .map(|file| format!("{folder}/{file}")),
        );
    }
    names.extend(
        json_files(DEVICE_FOLDER)
            .into_iter()
            .map(|file| format!("{DEVICE_FOLDER}/{device_id}/{file}")),
    );
    names.retain(|name| allowed_name(name, Some(device_id)));
    names
}

fn read_value(path: &Path) -> Option<Value> {
    read_json(path)
}

/// Responsibility: Run sync rounds for one device's data folder.
pub struct SyncClient {
    data_directory: PathBuf,
    config: ClientConfig,
}

/// Held while one sync runs on a data folder (`sync.lock`), so the service's loop and a
/// `sync-now` never run at once. A lock older than [`STALE_LOCK`] is left from a crash.
struct SyncLock(PathBuf);

const STALE_LOCK: std::time::Duration = std::time::Duration::from_secs(300);

impl SyncLock {
    fn take(data_directory: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_directory).map_err(|error| format!("sync lock: {error}"))?;
        let path = data_directory.join("sync.lock");
        for _ in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > STALE_LOCK);
                    if !stale {
                        return Err("another sync of this PC is running".into());
                    }
                    let _ = fs::remove_file(&path);
                }
                Err(error) => return Err(format!("sync lock: {error}")),
            }
        }
        Err("another sync of this PC is running".into())
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl SyncClient {
    pub fn new(data_directory: PathBuf, config: ClientConfig) -> Self {
        Self {
            data_directory,
            config,
        }
    }

    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    fn base_path(&self, document_name: &str) -> PathBuf {
        self.data_directory
            .join(BASE_FOLDER)
            .join(document_name.replace('/', "~"))
    }

    fn local_path(&self, document_name: &str) -> Option<PathBuf> {
        local_path_for(&self.data_directory, &self.config.device_id, document_name)
    }

    /// Documents whose local file differs from the base copy (or has none yet).
    fn changed_documents(&self) -> Vec<(String, Value)> {
        local_documents(&self.data_directory, &self.config.device_id)
            .into_iter()
            .filter_map(|name| {
                let local = shared_part(&name, &read_value(&self.local_path(&name)?)?);
                let base = read_value(&self.base_path(&name));
                (base.as_ref() != Some(&local)).then_some((name, local))
            })
            .collect()
    }

    /// Write `content` as the local file of `document_name` unless the file already says the
    /// same. Re-reads just before writing, so a change the agent saved meanwhile is merged in
    /// rather than lost.
    fn write_local(
        &self,
        document_name: &str,
        base: Option<&Value>,
        remote: &Value,
    ) -> Result<bool, String> {
        let Some(path) = self.local_path(document_name) else {
            return Ok(false);
        };
        let current_whole = read_value(&path);
        let current = current_whole
            .as_ref()
            .map(|whole| shared_part(document_name, whole));
        let next_shared = match (&current, base) {
            (Some(local), Some(base)) if local != base => {
                merge(base, local, remote, atomic_keys(document_name))
            }
            _ => remote.clone(),
        };
        // A protected device stays protected unless its protection may end now (the same
        // rule as the server's): its protected version is sent back at the next round.
        if document_name == "settings_protection.json" {
            if let Some(local) = current.as_ref().filter(|local| protected(local)) {
                if !protected(&next_shared) && !protection_may_end(local, Utc::now(), None) {
                    return Ok(false);
                }
            }
        }
        // This device's own parts (its API key) stay as they are.
        let next = with_local_part(document_name, &next_shared, current_whole.as_ref());
        if current_whole.as_ref() == Some(&next) {
            return Ok(false);
        }
        write_json(&path, &next)?;
        Ok(true)
    }

    /// One sync: send what changed here, take in what changed elsewhere, merge conflicts and
    /// send again, at most [`MAX_ROUNDS`] rounds. `transport` carries one sealed request to the
    /// server and returns its sealed answer.
    pub fn sync_once(
        &self,
        transport: &mut dyn FnMut(&Envelope) -> Result<Envelope, String>,
    ) -> Result<SyncReport, String> {
        let _lock = SyncLock::take(&self.data_directory)?;
        let mut state = SyncState::load(&self.data_directory);
        state.last_attempt = Some(Utc::now());
        let result = self.rounds(&mut state, transport);
        match &result {
            Ok(report) => {
                state.last_success = state.last_attempt;
                state.last_error = None;
                state.refused = report.refused.clone();
                state.outdated_devices = report.outdated_devices.clone();
                state.devices = report.devices.clone();
                state.device_details = report.device_details.clone();
                state.not_forgotten = report.not_forgotten.clone();
                // The server has had the request (and said which ones it kept).
                let _ = fs::remove_file(self.data_directory.join(FORGET_FILE));
            }
            Err(error) => state.last_error = Some(error.clone()),
        }
        state.save(&self.data_directory)?;
        result
    }

    fn rounds(
        &self,
        state: &mut SyncState,
        transport: &mut dyn FnMut(&Envelope) -> Result<Envelope, String>,
    ) -> Result<SyncReport, String> {
        let mut report = SyncReport::default();
        // A device that never synced takes the server's state first, and only then sends
        // what the server does not have yet (the first device fills an empty server).
        let mut joining = state.revisions.is_empty();
        let mut refused_names: BTreeSet<String> = BTreeSet::new();
        // Sent with the first request only.
        let mut forget_devices = ForgetRequest::load(&self.data_directory).devices;
        for _ in 0..MAX_ROUNDS {
            let pushes: Vec<Push> = if joining {
                Vec::new()
            } else {
                self.changed_documents()
                    .into_iter()
                    .filter(|(name, _)| !refused_names.contains(name))
                    .map(|(name, content)| Push {
                        base_revision: state.revisions.get(&name).copied().unwrap_or(0),
                        name,
                        content,
                    })
                    .collect()
            };
            state.counter += 1;
            // Saved before sending, so a crash never sends the same counter twice.
            state.save(&self.data_directory)?;
            let request = SyncRequest {
                sent_at: Utc::now(),
                counter: state.counter,
                pushes: pushes.clone(),
                known: state.revisions.clone(),
                app_version: crate::protocol::app_version().to_string(),
                machine_id: crate::machine_identity::get(),
                forget_devices: std::mem::take(&mut forget_devices),
            };
            let plaintext = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
            let sealed = envelope::seal(&self.config.device_id, &self.config.key, &plaintext)?;
            let answer = transport(&sealed)?;
            if answer.device_id != self.config.device_id {
                return Err("the server answered for another device".into());
            }
            let response: SyncResponse =
                serde_json::from_slice(&envelope::open(&answer, &self.config.key)?)
                    .map_err(|error| format!("bad answer: {error}"))?;
            // Only the answer to this very request counts: an old answer sent again would
            // bring old documents back.
            if response.request_counter != request.counter
                || (Utc::now() - response.server_time).num_seconds().abs()
                    > crate::protocol::MAX_CLOCK_SKEW_SECONDS
            {
                return Err("the answer does not belong to this request".into());
            }

            report.outdated_devices = response.outdated_devices.clone();
            report.devices = response.devices.clone();
            report.device_details = response.device_details.clone();
            report
                .not_forgotten
                .extend(response.not_forgotten.iter().cloned());
            let mut conflicts = 0;
            for refusal in &response.rejected {
                if refusal.reason == "conflict" {
                    conflicts += 1;
                } else {
                    refused_names.insert(refusal.name.clone());
                    if !report.refused.contains(refusal) {
                        report.refused.push(refusal.clone());
                    }
                }
            }
            let rejected =
                |name: &str| response.rejected.iter().any(|refusal| refusal.name == name);
            for push in &pushes {
                if !rejected(&push.name) && !report.pushed.contains(&push.name) {
                    report.pushed.push(push.name.clone());
                }
            }
            for document in &response.documents {
                let pushed_here = pushes.iter().find(|push| push.name == document.name);
                if pushed_here.is_some_and(|push| push.content == document.content) {
                    // The server took this device's version: it is the new base.
                    write_json(&self.base_path(&document.name), &document.content)?;
                    state
                        .revisions
                        .insert(document.name.clone(), document.revision);
                    continue;
                }
                // Another device's file, or a push the server refused for a rule: the server's
                // version is taken as it is.
                if belongs_to_another_device(&self.config.device_id, &document.name)
                    || SERVER_DOCUMENTS.contains(&document.name.as_str())
                    || refused_names.contains(&document.name)
                {
                    if self.write_local(&document.name, None, &document.content)? {
                        report.pulled.push(document.name.clone());
                    }
                    state
                        .revisions
                        .insert(document.name.clone(), document.revision);
                    continue;
                }
                let base = if joining {
                    // Joining: the server's version replaces this device's.
                    None
                } else {
                    Some(read_value(&self.base_path(&document.name)).unwrap_or(Value::Null))
                };
                let local_before = self
                    .local_path(&document.name)
                    .and_then(|path| read_value(&path))
                    .map(|whole| shared_part(&document.name, &whole));
                let had_local_changes = match (&local_before, &base) {
                    (Some(local), Some(base)) => local != base && local != &document.content,
                    _ => false,
                };
                if self.write_local(&document.name, base.as_ref(), &document.content)? {
                    report.pulled.push(document.name.clone());
                }
                if had_local_changes && !report.merged.contains(&document.name) {
                    report.merged.push(document.name.clone());
                }
                write_json(&self.base_path(&document.name), &document.content)?;
                state
                    .revisions
                    .insert(document.name.clone(), document.revision);
            }
            state.save(&self.data_directory)?;
            let was_joining = joining;
            joining = false;
            // Done once nothing is left to send (a conflict leaves the merged version to send).
            let left = self
                .changed_documents()
                .into_iter()
                .filter(|(name, _)| !refused_names.contains(name))
                .count();
            if left == 0 || (!was_joining && conflicts == 0) {
                break;
            }
        }
        report.pulled.sort();
        report.pulled.dedup();
        Ok(report)
    }
}

/// Send one envelope to `server_url` (`POST /sync`) and return the answer.
pub fn http_transport(server_url: &str) -> impl FnMut(&Envelope) -> Result<Envelope, String> {
    let url = format!("{}/sync", server_url.trim_end_matches('/'));
    let agent = web_client::agent_builder()
        .timeout(std::time::Duration::from_secs(60))
        .build();
    move |sealed: &Envelope| match agent.post(&url).send_json(sealed) {
        Ok(response) => response
            .into_json::<Envelope>()
            .map_err(|error| format!("bad answer from the server: {error}")),
        Err(ureq::Error::Status(code, response)) => Err(format!(
            "the server refused ({code}): {}",
            response.into_string().unwrap_or_default()
        )),
        Err(error) => Err(format!("could not reach the server: {error}")),
    }
}

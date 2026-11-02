//! Responsibility: Hold the documents every device shares and answer sync rounds, so that only
//! paired devices can read or change them, and only within the rules.
//!
//! Rules the server keeps, whatever a request says:
//! - A request must open with the key of a paired device, be recent, and carry a counter
//!   higher than any earlier request of that device (no replays).
//! - Only known document names are stored; a device's usage file (`usage/<device>/…`) is
//!   written only by that device.
//! - A push must start from the server's current revision; otherwise it is refused as a
//!   conflict, and the device merges and tries again.
//! - Once the settings are protected (`settings_protection.json` has `protected_since`), no
//!   push can turn the protection off (except a test-edition server).
//!
//! Storage: `<folder>/devices.json` and `<folder>/documents/<name with / as ~>.json`.

use crate::envelope::{self, Envelope};
use crate::protocol::{
    Document, PairingCode, Rejected, SyncRequest, SyncResponse, MAX_CLOCK_SKEW_SECONDS,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Documents every device shares.
pub const SHARED_DOCUMENTS: [&str; 6] = [
    "policy.json",
    "ai_focus.json",
    "planner.json",
    "settings_protection.json",
    "settings_reservations.json",
    "planner_state.json",
];
/// Folders whose files every device shares (one file per Windows user).
pub const SHARED_FOLDERS: [&str; 2] = ["grants", "blocks"];
/// The folder of per-device files (`usage/<device>/<user>.json`), summed for the statistics.
pub const DEVICE_FOLDER: &str = "usage";
/// The largest request the server reads.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Responsibility: Describe one paired device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub key: String,
    pub added_at: DateTime<Utc>,
    #[serde(default)]
    pub last_counter: u64,
    #[serde(default)]
    pub last_seen: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Devices {
    devices: Vec<Device>,
}

/// Responsibility: Keep one stored document with its revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored {
    revision: u64,
    written_by: String,
    written_at: DateTime<Utc>,
    content: Value,
}

/// Is `name` a document the server keeps? `usage/<device>/<file>` only for `device_id`.
pub fn allowed_name(name: &str, device_id: Option<&str>) -> bool {
    let safe_part = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && part
                .chars()
                .all(|character| character.is_alphanumeric() || "-_.@ ".contains(character))
    };
    if SHARED_DOCUMENTS.contains(&name) {
        return true;
    }
    let parts: Vec<&str> = name.split('/').collect();
    if !name.ends_with(".json") || !parts.iter().all(|part| safe_part(part)) {
        return false;
    }
    match parts.as_slice() {
        [folder, _] => SHARED_FOLDERS.contains(folder),
        [folder, owner, _] => {
            *folder == DEVICE_FOLDER && device_id.is_none_or(|device| device == *owner)
        }
        _ => false,
    }
}

fn file_name(name: &str) -> String {
    name.replace('/', "~")
}

/// Responsibility: Own the server's folder and answer sync rounds one at a time.
pub struct SyncServer {
    folder: PathBuf,
    /// One request at a time: rounds are short, and this keeps revisions simple.
    busy: Mutex<()>,
    /// A test-edition server lets a device turn the protection off (the test edition can).
    allow_unprotect: bool,
}

impl SyncServer {
    pub fn new(folder: PathBuf, allow_unprotect: bool) -> Self {
        Self {
            folder,
            busy: Mutex::new(()),
            allow_unprotect,
        }
    }

    fn devices_path(&self) -> PathBuf {
        self.folder.join("devices.json")
    }

    fn documents_folder(&self) -> PathBuf {
        self.folder.join("documents")
    }

    fn load_devices(&self) -> Devices {
        read_json(&self.devices_path()).unwrap_or_default()
    }

    fn save_devices(&self, devices: &Devices) -> Result<(), String> {
        write_json(&self.devices_path(), devices)
    }

    /// Pair a new device: its id comes from `name`, its key is new. Show the code once.
    pub fn add_device(&self, name: &str) -> Result<PairingCode, String> {
        let _busy = self.busy.lock();
        let mut devices = self.load_devices();
        let base: String = name
            .trim()
            .to_lowercase()
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        let base = if base.trim_matches('-').is_empty() {
            "device".to_string()
        } else {
            base.trim_matches('-').to_string()
        };
        let mut id = base.clone();
        let mut number = 2;
        while devices.devices.iter().any(|device| device.id == id) {
            id = format!("{base}-{number}");
            number += 1;
        }
        let key = envelope::new_key()?;
        devices.devices.push(Device {
            id: id.clone(),
            name: name.trim().to_string(),
            key: key.clone(),
            added_at: Utc::now(),
            last_counter: 0,
            last_seen: None,
        });
        self.save_devices(&devices)?;
        Ok(PairingCode { device_id: id, key })
    }

    pub fn remove_device(&self, id: &str) -> Result<(), String> {
        let _busy = self.busy.lock();
        let mut devices = self.load_devices();
        let before = devices.devices.len();
        devices.devices.retain(|device| device.id != id);
        if devices.devices.len() == before {
            return Err(format!("no device '{id}'"));
        }
        self.save_devices(&devices)
    }

    /// The paired devices, without their keys.
    pub fn devices(&self) -> Vec<Device> {
        self.load_devices()
            .devices
            .into_iter()
            .map(|device| Device {
                key: String::new(),
                ..device
            })
            .collect()
    }

    fn load_document(&self, name: &str) -> Option<Stored> {
        read_json(&self.documents_folder().join(file_name(name)))
    }

    fn all_documents(&self) -> Vec<(String, Stored)> {
        let Ok(entries) = fs::read_dir(self.documents_folder()) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file = entry.file_name().to_string_lossy().to_string();
                let name = file.replace('~', "/");
                if !allowed_name(&name, None) {
                    return None;
                }
                Some((name, read_json::<Stored>(&entry.path())?))
            })
            .collect()
    }

    /// Answer one sealed request with a sealed response.
    pub fn handle(&self, envelope: &Envelope) -> Result<Envelope, String> {
        let _busy = self.busy.lock();
        let mut devices = self.load_devices();
        let Some(device) = devices
            .devices
            .iter_mut()
            .find(|device| device.id == envelope.device_id)
        else {
            return Err("unknown device".into());
        };
        let plaintext = envelope::open(envelope, &device.key)?;
        let request: SyncRequest =
            serde_json::from_slice(&plaintext).map_err(|error| format!("bad request: {error}"))?;
        let now = Utc::now();
        if (now - request.sent_at).num_seconds().abs() > MAX_CLOCK_SKEW_SECONDS {
            return Err("the request is too old (or the clocks differ by more than 10 minutes)".into());
        }
        if request.counter <= device.last_counter {
            return Err("the request was already used".into());
        }
        device.last_counter = request.counter;
        device.last_seen = Some(now);
        let device_id = device.id.clone();
        let key = device.key.clone();
        self.save_devices(&devices)?;

        let mut rejected = Vec::new();
        for push in &request.pushes {
            match self.accept(&device_id, push, now) {
                Ok(()) => {}
                Err(reason) => rejected.push(Rejected {
                    name: push.name.clone(),
                    reason,
                }),
            }
        }
        // Every document newer than the device has, and the current version of every
        // refused one (so the device can merge, or take the server's).
        let documents = self
            .all_documents()
            .into_iter()
            .filter(|(name, stored)| {
                request.known.get(name).is_none_or(|known| stored.revision > *known)
                    || rejected.iter().any(|refused| &refused.name == name)
            })
            .map(|(name, stored)| Document {
                name,
                revision: stored.revision,
                content: stored.content,
                written_by: stored.written_by,
            })
            .collect();
        let response = SyncResponse {
            server_time: now,
            documents,
            rejected,
        };
        let bytes = serde_json::to_vec(&response).map_err(|error| error.to_string())?;
        envelope::seal(&device_id, &key, &bytes)
    }

    fn accept(
        &self,
        device_id: &str,
        push: &crate::protocol::Push,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        if !allowed_name(&push.name, Some(device_id)) {
            return Err("not a document this device may write".into());
        }
        let current = self.load_document(&push.name);
        let revision = current.as_ref().map_or(0, |stored| stored.revision);
        if push.base_revision != revision {
            return Err("conflict".into());
        }
        if push.name == "settings_protection.json" && !self.allow_unprotect {
            let protected = |content: &Value| {
                content
                    .get("protected_since")
                    .is_some_and(|since| !since.is_null())
            };
            if current.as_ref().is_some_and(|stored| protected(&stored.content))
                && !protected(&push.content)
            {
                return Err("the protection cannot be turned off".into());
            }
        }
        let stored = Stored {
            revision: revision + 1,
            written_by: device_id.to_string(),
            written_at: now,
            content: push.content.clone(),
        };
        write_json(&self.documents_folder().join(file_name(&push.name)), &stored)
    }

    /// Serve HTTP on `port` forever (`POST /sync` with an envelope as JSON).
    pub fn serve(self: std::sync::Arc<Self>, port: u16) -> std::io::Result<()> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        tracing::info!("sync server listening on port {port}");
        for stream in listener.incoming().flatten() {
            let server = std::sync::Arc::clone(&self);
            std::thread::spawn(move || {
                if let Err(error) = server.answer(stream) {
                    tracing::debug!("sync request: {error}");
                }
            });
        }
        Ok(())
    }

    fn answer(&self, stream: TcpStream) -> Result<(), String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(stream);
        let mut request_line = String::new();
        reader
            .read_line(&mut request_line)
            .map_err(|error| error.to_string())?;
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).map_err(|error| error.to_string())? == 0
                || line == "\r\n"
            {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        let (status, body) = if !request_line.starts_with("POST /sync ") {
            ("200 OK", "digital wellbeing sync server".to_string())
        } else if length > MAX_BODY_BYTES {
            ("413 Payload Too Large", "too large".to_string())
        } else {
            let mut body = vec![0; length];
            reader
                .read_exact(&mut body)
                .map_err(|error| error.to_string())?;
            match serde_json::from_slice::<Envelope>(&body)
                .map_err(|error| error.to_string())
                .and_then(|envelope| self.handle(&envelope))
                .and_then(|sealed| serde_json::to_string(&sealed).map_err(|error| error.to_string()))
            {
                Ok(json) => ("200 OK", json),
                Err(error) => ("403 Forbidden", error),
            }
        };
        let mut stream = reader.into_inner();
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .map_err(|error| error.to_string())
    }
}

pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Temp file + rename.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.sync.tmp");
    fs::write(&temporary, text).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_documents_and_own_usage_are_allowed() {
        assert!(allowed_name("planner.json", Some("laptop")));
        assert!(allowed_name("blocks/work.json", Some("laptop")));
        assert!(allowed_name("usage/laptop/work.json", Some("laptop")));
        assert!(!allowed_name("usage/desktop/work.json", Some("laptop")));
        assert!(allowed_name("usage/desktop/work.json", None));
        assert!(!allowed_name("machine.json", Some("laptop")));
        assert!(!allowed_name("blocks/../ai_focus.json", Some("laptop")));
        assert!(!allowed_name("grants/a/b/c.json", Some("laptop")));
    }
}

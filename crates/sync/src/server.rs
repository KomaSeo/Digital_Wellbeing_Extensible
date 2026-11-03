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
//! - Once the settings are protected (`settings_protection.json` has `protected_since`), a
//!   push turns the protection off only the way it allows ([`protection_may_end`]).
//!
//! Storage: `<folder>/devices.json` and `<folder>/documents/<name with / as ~>.json`.

use crate::envelope::{self, Envelope};
use crate::protocol::{
    version_order, Document, PairingCode, Rejected, SyncRequest, SyncResponse,
    MAX_CLOCK_SKEW_SECONDS,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// Documents only the server writes; devices take them as they are.
pub const SERVER_DOCUMENTS: [&str; 1] = [crate::users::USERS_DOCUMENT];
/// Folders whose files every device shares (one file per login or Windows user).
pub const SHARED_FOLDERS: [&str; 2] = ["grants", "blocks"];
/// The folder of per-device files (`usage/<device>/<user>.json`), summed for the statistics.
pub const DEVICE_FOLDER: &str = "usage";
/// The largest request the server reads.
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// The largest request line and headers together.
const MAX_HEAD_BYTES: u64 = 16 * 1024;
/// Requests answered at the same time: one owner with about ten devices, each syncing every
/// 20 seconds in a fraction of a second, needs few; more are refused at once.
const MAX_CONNECTIONS: usize = 16;

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
    /// The program version the device last synced with.
    #[serde(default)]
    pub version: String,
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

/// Is the protection on in this `settings_protection.json` content?
pub fn protected(content: &Value) -> bool {
    content
        .get("protected_since")
        .is_some_and(|since| !since.is_null())
}

/// May this protection (as `settings_protection.json` holds it) end now, ended by the device
/// `by_device` (`None`: not known, as on a device taking in what the server took)? Never a
/// permanent one (nor one saved before there were options); a time one once its time is over;
/// a password one, or an administrator one, when a device ended it (that device's service
/// checked the password or the administrator rights, which neither the server nor another
/// device can) — an administrator one tied to a device only when that device ended it.
pub fn protection_may_end(protection: &Value, now: DateTime<Utc>, by_device: Option<&str>) -> bool {
    match protection.pointer("/mode/kind").and_then(Value::as_str) {
        Some("until") => protection
            .pointer("/mode/until")
            .and_then(|until| serde_json::from_value::<DateTime<Utc>>(until.clone()).ok())
            .is_some_and(|until| now >= until),
        Some("administrator") => {
            match (
                protection.pointer("/mode/device").and_then(Value::as_str),
                by_device,
            ) {
                (Some(required), Some(device)) => required == device,
                _ => true,
            }
        }
        Some("password") => true,
        _ => false,
    }
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
    if SERVER_DOCUMENTS.contains(&name) {
        return device_id.is_none();
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
}

impl SyncServer {
    pub fn new(folder: PathBuf) -> Self {
        Self {
            folder,
            busy: Mutex::new(()),
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
            version: String::new(),
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

    /// Add a login or give it a new password (`users.json`, sent to every device).
    pub fn set_user(&self, name: &str, password: &str) -> Result<(), String> {
        self.change_users(|users| users.set(name, password))
    }

    pub fn remove_user(&self, name: &str) -> Result<(), String> {
        self.change_users(|users| {
            if users.remove(name) {
                Ok(())
            } else {
                Err(format!("no login '{name}'"))
            }
        })
    }

    pub fn users(&self) -> Vec<String> {
        self.load_document(crate::users::USERS_DOCUMENT)
            .and_then(|stored| serde_json::from_value::<crate::users::Users>(stored.content).ok())
            .unwrap_or_default()
            .names()
    }

    fn change_users(
        &self,
        change: impl FnOnce(&mut crate::users::Users) -> Result<(), String>,
    ) -> Result<(), String> {
        let _busy = self.busy.lock();
        let name = crate::users::USERS_DOCUMENT;
        let current = self.load_document(name);
        let mut users: crate::users::Users = current
            .as_ref()
            .and_then(|stored| serde_json::from_value(stored.content.clone()).ok())
            .unwrap_or_default();
        change(&mut users)?;
        let stored = Stored {
            revision: current.map_or(0, |stored| stored.revision) + 1,
            written_by: "server".into(),
            written_at: Utc::now(),
            content: serde_json::to_value(&users).map_err(|error| error.to_string())?,
        };
        write_json(&self.documents_folder().join(file_name(name)), &stored)
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
            return Err(
                "the request is too old (or the clocks differ by more than 10 minutes)".into(),
            );
        }
        if request.counter <= device.last_counter {
            return Err("the request was already used".into());
        }
        device.last_counter = request.counter;
        device.last_seen = Some(now);
        device.version = request.app_version.clone();
        let device_id = device.id.clone();
        let key = device.key.clone();
        self.save_devices(&devices)?;

        // Every device runs the newest version any of them runs: an older one would drop
        // the settings it does not know when it writes a file.
        let newest = devices
            .devices
            .iter()
            .map(|device| device.version.as_str())
            .max_by_key(|version| version_order(version))
            .unwrap_or_default()
            .to_string();
        if version_order(&request.app_version) < version_order(&newest) {
            return Err(format!(
                "이 PC의 프로그램(버전 {})이 다른 PC(버전 {newest})보다 오래되어 동기화를 멈췄습니다. \
                 이 PC를 {newest}로 업데이트하면 다시 동기화합니다",
                if request.app_version.is_empty() { "알 수 없음" } else { &request.app_version }
            ));
        }
        let outdated_devices: Vec<(String, String)> = devices
            .devices
            .iter()
            .filter(|device| {
                device.last_seen.is_some()
                    && version_order(&device.version) < version_order(&newest)
            })
            .map(|device| (device.id.clone(), device.version.clone()))
            .collect();

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
                request
                    .known
                    .get(name)
                    .is_none_or(|known| stored.revision > *known)
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
            request_counter: request.counter,
            server_time: now,
            documents,
            rejected,
            outdated_devices,
            devices: devices
                .devices
                .iter()
                .map(|device| (device.id.clone(), device.name.clone()))
                .collect(),
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
        if push.name == "settings_protection.json" {
            if let Some(stored) = current.as_ref().filter(|stored| protected(&stored.content)) {
                if !protected(&push.content)
                    && !protection_may_end(&stored.content, now, Some(device_id))
                {
                    return Err("the protection cannot be turned off".into());
                }
                // How a protection ends is chosen once, when protecting.
                if protected(&push.content)
                    && push.content.get("mode") != stored.content.get("mode")
                {
                    return Err("the protection cannot be changed".into());
                }
            }
        }
        let stored = Stored {
            revision: revision + 1,
            written_by: device_id.to_string(),
            written_at: now,
            // Never kept here, whatever a device sends: each device has its own API key.
            content: crate::device_local::shared_part(&push.name, &push.content),
        };
        write_json(
            &self.documents_folder().join(file_name(&push.name)),
            &stored,
        )
    }

    /// The port this server was set up with (`server.json`), for the pairing hint.
    pub fn port(&self) -> Option<u16> {
        read_json::<Value>(&self.folder.join("server.json"))?
            .get("port")?
            .as_u64()
            .and_then(|port| u16::try_from(port).ok())
    }

    /// Serve HTTP on `port` forever (`POST /sync` with an envelope as JSON). At most
    /// [`MAX_CONNECTIONS`] requests are answered at once; more are closed at once, so a
    /// flood of connections cannot use up the server's memory or threads.
    pub fn serve(self: std::sync::Arc<Self>, port: u16) -> std::io::Result<()> {
        let listener = TcpListener::bind(("0.0.0.0", port))?;
        if let Err(error) = write_json(&self.folder.join("server.json"), &json!({ "port": port })) {
            tracing::warn!("could not note the port: {error}");
        }
        tracing::info!("sync server listening on port {port}");
        let open_connections = std::sync::Arc::new(AtomicUsize::new(0));
        for stream in listener.incoming().flatten() {
            if open_connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                open_connections.fetch_sub(1, Ordering::SeqCst);
                drop(stream);
                continue;
            }
            let server = std::sync::Arc::clone(&self);
            let open_connections = std::sync::Arc::clone(&open_connections);
            std::thread::spawn(move || {
                if let Err(error) = server.answer(stream) {
                    tracing::debug!("sync request: {error}");
                }
                open_connections.fetch_sub(1, Ordering::SeqCst);
            });
        }
        Ok(())
    }

    fn answer(&self, stream: TcpStream) -> Result<(), String> {
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .map_err(|error| error.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(30)))
            .map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(stream);
        // The request line and headers together are read through a limit, so a line without
        // an end cannot grow without bound.
        let mut head = (&mut reader).take(MAX_HEAD_BYTES);
        let mut request_line = String::new();
        head.read_line(&mut request_line)
            .map_err(|error| error.to_string())?;
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if head
                .read_line(&mut line)
                .map_err(|error| error.to_string())?
                == 0
                || line == "\r\n"
            {
                break;
            }
            if !line.ends_with('\n') {
                return Err("the request head is too long".into());
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
                .and_then(|sealed| {
                    serde_json::to_string(&sealed).map_err(|error| error.to_string())
                }) {
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

/// Temp file (unique per process and write) + flush to disk + rename, so a crash or power
/// loss leaves the old file or the new one, never an empty one.
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let text = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!(
        "json.sync.{}.{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    written.map_err(|error| {
        let _ = fs::remove_file(&temporary);
        error.to_string()
    })
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
        // Only the server writes the logins; every device reads them.
        assert!(!allowed_name("users.json", Some("laptop")));
        assert!(allowed_name("users.json", None));
    }
}

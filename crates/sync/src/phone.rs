//! Responsibility: Let a phone see the to-do list and send to-do changes through the sync
//! server, without the account's password (the owner: "핸드폰에서 할 일 만 관리할 수 있게 만들
//! 수 있나.").
//!
//! - A PC makes a one-time code ([`new_code`]) into `phone_access.json`, a document the PCs
//!   share. The phone gives the code to the server once ([`crate::server::SyncServer::phone_pair`])
//!   and gets a token that reaches nothing but the phone page's requests.
//! - The server keeps the phones and their requests in `phones.json` ([`Phones`]): only the
//!   server writes it, and every PC gets it. A PC whose agent runs says so in its sync round
//!   (`phone_ready`); the server gives it the requests nobody handles ([`assign`]). That PC's
//!   service hands each one to its agent's `todo_change`, the same AI review as the Focus
//!   app's, and sends the outcome back in its next round (`phone_outcomes`, [`record_outcomes`]).
//! - The server keeps the token hashes and the used codes in its own `phone_tokens.json`.
//! - A PC takes a phone off by naming it in `phone_access.json` (`removed`).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// The document the PCs share: the codes they made and the phones they took off.
pub const ACCESS_DOCUMENT: &str = "phone_access.json";
/// The document only the server writes: the phones and their requests.
pub const PHONES_DOCUMENT: &str = "phones.json";
/// This PC's outcomes not yet sent (never shared).
pub const OUTCOMES_FILE: &str = "phone_outcomes.json";
/// The server's own file: token hashes and used codes.
pub(crate) const TOKENS_FILE: &str = "phone_tokens.json";

/// How long a code is good for.
pub const CODE_MINUTES: i64 = 10;
/// A request given to a PC that sent no outcome for this long goes to another PC (that PC
/// was turned off while its agent asked the AI).
const REASSIGN_AFTER_MINUTES: i64 = 30;
/// A PC said it can take requests within this many seconds: the phone says a PC is on (the
/// server remembers it in memory, so `phones.json` does not change every round).
pub const READY_SECONDS: i64 = 90;
/// Requests kept (the oldest finished ones go first).
const MAX_REQUESTS_KEPT: usize = 200;
/// Requests one phone may have waiting at once.
pub(crate) const MAX_WAITING_PER_PHONE: usize = 30;
/// The largest change one request may carry.
pub(crate) const MAX_CHANGE_BYTES: usize = 16 * 1024;
/// The longest reason one request may carry.
pub(crate) const MAX_REASON_CHARS: usize = 2000;
/// What a phone may ask for: to-dos added, edited and removed, and appointments added.
pub const ALLOWED_KINDS: [&str; 4] = ["add", "edit", "remove", "add_appointment"];
/// Letters of a code: no 0/O, 1/I/L, so it is read off a screen without doubt.
const CODE_LETTERS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
const CODE_LENGTH: usize = 8;

/// Responsibility: Hold the codes the PCs made and the phones they took off
/// (`phone_access.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhoneAccess {
    pub codes: Vec<PhoneCode>,
    /// Phone ids taken off: their token no longer works.
    pub removed: Vec<String>,
}

/// Responsibility: Hold one code a PC made: its hash only, so the shared file does not hold
/// the code itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneCode {
    /// Lists of items with ids merge item by item when two PCs change the file.
    pub id: String,
    pub hash: String,
    pub made_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Responsibility: Hold the phones and their requests (`phones.json`, written by the server).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Phones {
    pub phones: Vec<Phone>,
    pub requests: Vec<PhoneRequest>,
}

/// Responsibility: Describe one connected phone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phone {
    pub id: String,
    pub name: String,
    pub paired_at: DateTime<Utc>,
}

/// Responsibility: Hold one change a phone asked for, and what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneRequest {
    pub id: String,
    pub phone_id: String,
    pub at: DateTime<Utc>,
    /// A `task_planner::TodoChange` as JSON (one of [`ALLOWED_KINDS`]).
    pub change: Value,
    #[serde(default)]
    pub reason: String,
    /// The PC handling it, and since when.
    #[serde(default)]
    pub assigned_to: Option<String>,
    #[serde(default)]
    pub assigned_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub outcome: Option<PhoneOutcome>,
}

/// Responsibility: Carry what a PC's agent decided about one request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneOutcome {
    pub request_id: String,
    pub at: DateTime<Utc>,
    pub approved: bool,
    /// The AI's answer, or why the change could not be made.
    pub reply: String,
}

/// Responsibility: Hold what only the server knows: each phone's token hash and the codes
/// already used.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Tokens {
    pub(crate) phones: Vec<PhoneToken>,
    pub(crate) used_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PhoneToken {
    pub(crate) phone_id: String,
    pub(crate) hash: String,
}

/// The SHA-256 of `text`, in hex.
pub(crate) fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A code as typed, without spaces or dashes, in capitals.
pub(crate) fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase()
}

/// `count` random bytes.
pub(crate) fn random_bytes(count: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; count];
    getrandom::getrandom(&mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// A random id or token: `count` bytes in hex.
pub(crate) fn random_hex(count: usize) -> Result<String, String> {
    Ok(random_bytes(count)?
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// A new code to show, like `K7QM-2XRA`, and its entry for `phone_access.json`.
pub fn make_code(now: DateTime<Utc>) -> Result<(String, PhoneCode), String> {
    let letters: String = random_bytes(CODE_LENGTH)?
        .iter()
        .map(|byte| CODE_LETTERS[usize::from(*byte) % CODE_LETTERS.len()] as char)
        .collect();
    let shown = format!("{}-{}", &letters[..4], &letters[4..]);
    Ok((
        shown,
        PhoneCode {
            id: random_hex(8)?,
            hash: hash(&letters),
            made_at: now,
            expires_at: now + Duration::minutes(CODE_MINUTES),
        },
    ))
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    crate::server::read_json(path).unwrap_or_default()
}

/// Make a code on this PC: it goes to the server with the next sync (within 20 seconds).
/// Codes past their time leave the file.
pub fn new_code(data_directory: &Path, now: DateTime<Utc>) -> Result<String, String> {
    let path = data_directory.join(ACCESS_DOCUMENT);
    let mut access: PhoneAccess = read_json(&path);
    access.codes.retain(|code| code.expires_at > now);
    let (shown, code) = make_code(now)?;
    access.codes.push(code);
    crate::server::write_json(&path, &access)?;
    Ok(shown)
}

/// Take a phone off from this PC: its token stops working once the server has the file.
pub fn remove_phone(data_directory: &Path, phone_id: &str) -> Result<(), String> {
    let path = data_directory.join(ACCESS_DOCUMENT);
    let mut access: PhoneAccess = read_json(&path);
    if !access.removed.iter().any(|removed| removed == phone_id) {
        access.removed.push(phone_id.to_string());
    }
    crate::server::write_json(&path, &access)
}

/// The phones and their requests as this PC last got them from the server.
pub fn phones(data_directory: &Path) -> Phones {
    read_json(&data_directory.join(PHONES_DOCUMENT))
}

/// This PC's outcomes not yet seen in `phones.json`.
pub fn pending_outcomes(data_directory: &Path) -> Vec<PhoneOutcome> {
    read_json(&data_directory.join(OUTCOMES_FILE))
}

/// Keep `outcome` until the server has it.
pub fn save_outcome(data_directory: &Path, outcome: PhoneOutcome) -> Result<(), String> {
    let mut outcomes = pending_outcomes(data_directory);
    outcomes.retain(|kept| kept.request_id != outcome.request_id);
    outcomes.push(outcome);
    crate::server::write_json(&data_directory.join(OUTCOMES_FILE), &outcomes)
}

/// Drop the outcomes the server has (or whose request it no longer keeps).
pub fn prune_outcomes(data_directory: &Path) -> Result<(), String> {
    let phones = phones(data_directory);
    let mut outcomes = pending_outcomes(data_directory);
    let before = outcomes.len();
    outcomes.retain(|outcome| {
        phones
            .requests
            .iter()
            .any(|request| request.id == outcome.request_id && request.outcome.is_none())
    });
    if outcomes.len() == before {
        return Ok(());
    }
    crate::server::write_json(&data_directory.join(OUTCOMES_FILE), &outcomes)
}

/// The requests the server gave this PC that it has not decided yet.
pub fn requests_for(data_directory: &Path, device_id: &str) -> Vec<PhoneRequest> {
    let decided: Vec<String> = pending_outcomes(data_directory)
        .into_iter()
        .map(|outcome| outcome.request_id)
        .collect();
    phones(data_directory)
        .requests
        .into_iter()
        .filter(|request| {
            request.outcome.is_none()
                && request.assigned_to.as_deref() == Some(device_id)
                && !decided.contains(&request.id)
        })
        .collect()
}

/// Give `device_id` (a PC whose agent runs) every request nobody handles, or that its PC left
/// for longer than [`REASSIGN_AFTER_MINUTES`]. True when something changed.
pub fn assign(phones: &mut Phones, device_id: &str, now: DateTime<Utc>) -> bool {
    let mut changed = false;
    for request in phones
        .requests
        .iter_mut()
        .filter(|request| request.outcome.is_none())
    {
        let left = request.assigned_at.is_none_or(|assigned_at| {
            now - assigned_at > Duration::minutes(REASSIGN_AFTER_MINUTES)
        });
        if request.assigned_to.as_deref() != Some(device_id) && left {
            request.assigned_to = Some(device_id.to_string());
            request.assigned_at = Some(now);
            changed = true;
        }
    }
    changed
}

/// Put in the outcomes `device_id` sent for requests given to it. True when something changed.
pub fn record_outcomes(
    phones: &mut Phones,
    device_id: &str,
    outcomes: &[PhoneOutcome],
    now: DateTime<Utc>,
) -> bool {
    let mut changed = false;
    for outcome in outcomes {
        if let Some(request) = phones.requests.iter_mut().find(|request| {
            request.id == outcome.request_id
                && request.outcome.is_none()
                && request.assigned_to.as_deref() == Some(device_id)
        }) {
            request.outcome = Some(PhoneOutcome {
                at: now,
                ..outcome.clone()
            });
            changed = true;
        }
    }
    changed
}

/// Take off the phones `removed` names, with their waiting requests. True when something
/// changed.
pub fn drop_removed(phones: &mut Phones, removed: &[String]) -> bool {
    let before = (phones.phones.len(), phones.requests.len());
    phones.phones.retain(|phone| !removed.contains(&phone.id));
    phones
        .requests
        .retain(|request| request.outcome.is_some() || !removed.contains(&request.phone_id));
    before != (phones.phones.len(), phones.requests.len())
}

/// Keep at most [`MAX_REQUESTS_KEPT`] requests: the oldest finished ones go first.
pub(crate) fn trim_requests(phones: &mut Phones) {
    while phones.requests.len() > MAX_REQUESTS_KEPT {
        match phones
            .requests
            .iter()
            .position(|request| request.outcome.is_some())
        {
            Some(index) => {
                phones.requests.remove(index);
            }
            None => break,
        }
    }
}

/// Check a change a phone sent: one of [`ALLOWED_KINDS`], not too large. The agent checks
/// everything else when it handles it.
pub(crate) fn check_change(change: &Value) -> Result<(), String> {
    let kind = change["kind"].as_str().unwrap_or_default();
    if !ALLOWED_KINDS.contains(&kind) {
        return Err(format!("휴대폰에서는 이 변경을 보낼 수 없습니다: {kind}"));
    }
    let size = serde_json::to_vec(change)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_CHANGE_BYTES {
        return Err("변경이 너무 깁니다".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(id: &str) -> PhoneRequest {
        PhoneRequest {
            id: id.into(),
            phone_id: "phone".into(),
            at: Utc::now(),
            change: json!({ "kind": "add", "todo": { "title": "milk" } }),
            reason: String::new(),
            assigned_to: None,
            assigned_at: None,
            outcome: None,
        }
    }

    #[test]
    fn a_code_is_eight_clear_letters_and_only_its_hash_is_kept() {
        let now = Utc::now();
        let (shown, code) = make_code(now).unwrap();
        assert_eq!(shown.len(), 9);
        assert_eq!(&shown[4..5], "-");
        assert!(normalize_code(&shown)
            .bytes()
            .all(|letter| CODE_LETTERS.contains(&letter)));
        assert_eq!(code.hash, hash(&normalize_code(&shown.to_lowercase())));
        assert_eq!(code.expires_at, now + Duration::minutes(CODE_MINUTES));
    }

    #[test]
    fn a_request_goes_to_one_pc_and_moves_on_only_when_that_pc_left_it() {
        let now = Utc::now();
        let mut phones = Phones {
            requests: vec![request("first")],
            ..Default::default()
        };
        assign(&mut phones, "desktop", now);
        assign(&mut phones, "laptop", now + Duration::minutes(1));
        assert_eq!(phones.requests[0].assigned_to.as_deref(), Some("desktop"));
        // An outcome from a PC it was not given counts for nothing.
        let outcome = PhoneOutcome {
            request_id: "first".into(),
            at: now,
            approved: true,
            reply: "ok".into(),
        };
        assert!(!record_outcomes(
            &mut phones,
            "laptop",
            &[outcome.clone()],
            now
        ));
        // The desktop was turned off: after half an hour the laptop takes it.
        assign(&mut phones, "laptop", now + Duration::minutes(31));
        assert_eq!(phones.requests[0].assigned_to.as_deref(), Some("laptop"));
        assert!(record_outcomes(&mut phones, "laptop", &[outcome], now));
        assert!(phones.requests[0].outcome.as_ref().unwrap().approved);
    }

    #[test]
    fn a_removed_phone_leaves_with_its_waiting_requests() {
        let mut phones = Phones {
            phones: vec![Phone {
                id: "phone".into(),
                name: "휴대폰".into(),
                paired_at: Utc::now(),
            }],
            requests: vec![request("waiting")],
        };
        assert!(drop_removed(&mut phones, &["phone".to_string()]));
        assert!(phones.phones.is_empty() && phones.requests.is_empty());
    }

    #[test]
    fn a_phone_sends_only_to_do_changes() {
        assert!(check_change(&json!({ "kind": "add", "todo": { "title": "milk" } })).is_ok());
        assert!(check_change(&json!({ "kind": "add_appointment" })).is_ok());
        assert!(check_change(&json!({ "kind": "split" })).is_err());
        assert!(check_change(&json!({ "kind": "move" })).is_err());
        let long = "x".repeat(MAX_CHANGE_BYTES);
        assert!(check_change(&json!({ "kind": "add", "todo": { "title": long } })).is_err());
    }
}

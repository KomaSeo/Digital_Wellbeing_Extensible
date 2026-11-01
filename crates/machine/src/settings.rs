//! Responsibility: Let users edit the settings through their agent; once the settings are
//! protected, keep every edit as a scheduled update that takes effect only after the user
//! confirmed it three times, two days apart; and turn the protection on (one way).
//!
//! The settings (`policy.json`, `ai_focus.json`, `planner.json` — the last one also holds the
//! planner's AI instructions and the life rules) sit in the data folder, which only SYSTEM and
//! Administrators can write; the user works in a standard account. The program does not tell
//! accounts apart. The agent (SYSTEM) reads them for any local user who asks ([`snapshot`])
//! and saves what the user sends ([`put`]); the Focus app's Settings tab and the Claude
//! Desktop connector are built on this.
//!
//! - **setup** (right after installing, `settings_protection.json` missing): a saved file
//!   takes effect at once, so the user can set everything up;
//! - **protected** ([`protect`], one way; nothing in the program turns it off): a saved file
//!   is split into edits ([`crate::setting_edits`]). Edits that make the rules stricter (block
//!   more, allow less, check more closely) take effect at once. The others are kept as a
//!   scheduled update ([`Reservation`], `settings_reservations.json`). Two days after it was
//!   scheduled the Focus app asks "is this update really right?"; each "yes" ([`answer`])
//!   brings the next question two days after the answer, and the third "yes" in a row puts
//!   it in force. One "no" rejects it. A question waits until it is answered. Saving the same
//!   file again with other looser edits replaces the scheduled update and starts over.
//! - A to-do change the AI rejected can be scheduled the same way ([`schedule`]), with
//!   questions two hours apart.
//!
//! So a change made in an irrational moment cannot take effect before the user has stood by
//! it on three different days. (Windows cannot stop an administrator account from deleting
//! these files or the service; the user does not work in one. Standard users cannot change
//! the system clock, so the waiting cannot be skipped by setting the clock forward.)

use crate::setting_edits::{self, Direction, Edit};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io;
use std::path::Path;

/// The admin settings a user may edit (name -> file in the data folder). `machine.json`
/// (which accounts are managed) is not among them.
pub const EDITABLE: [(&str, &str); 3] = [
    ("policy", "policy.json"),
    ("ai_focus", "ai_focus.json"),
    ("planner", "planner.json"),
];

/// "Yes" answers in a row that put a scheduled update in force.
pub const APPROVALS_NEEDED: usize = 3;

const PROTECTION_FILE: &str = "settings_protection.json";
const RESERVATIONS_FILE: &str = "settings_reservations.json";
/// Written by versions that had a settings lock: its presence means the settings were
/// protected; pending edits of that time become scheduled updates.
const OLD_LOCK_FILE: &str = "settings_lock.json";
const OLD_PENDING_FILE: &str = "settings_pending.json";
const HISTORY_CAPACITY: usize = 50;

/// Time between scheduling a settings update and its first question, and between an answer
/// and the next question.
pub const SETTINGS_QUESTION_INTERVAL_SECONDS: i64 = 2 * 24 * 3600;
/// The same for a to-do change the AI rejected and the user scheduled anyway.
pub const TODO_QUESTION_INTERVAL_SECONDS: i64 = 2 * 3600;

/// `DW_SETTINGS_CONFIRM_INTERVAL_SECONDS` replaces every interval, for tests; only an
/// administrator can set the service's environment.
fn question_interval(interval_seconds: i64) -> Duration {
    std::env::var("DW_SETTINGS_CONFIRM_INTERVAL_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse::<i64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::seconds)
        .unwrap_or_else(|| Duration::seconds(interval_seconds))
}

/// Held while a settings file or the scheduled updates are read, changed and written back,
/// so two requests at once (the Focus app and Claude Desktop) cannot lose each other's change.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn settings_interval() -> i64 {
    SETTINGS_QUESTION_INTERVAL_SECONDS
}

/// Responsibility: Record when the settings were protected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Protection {
    pub protected_since: Option<DateTime<Utc>>,
}

/// Responsibility: Say where a scheduled update came from, which decides how it is replaced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationOrigin {
    /// The looser edits of a saved settings file; saving the file again replaces it.
    #[default]
    Settings,
    /// One to-do change the AI rejected (or could not review) and the user scheduled anyway.
    TodoChange,
}

/// Responsibility: Hold one scheduled update of a settings file and the answers it got.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: String,
    /// One of the [`EDITABLE`] names.
    pub name: String,
    /// What changes, field by field; applied to the file as it is when the update takes
    /// effect.
    #[serde(default)]
    pub edits: Vec<Edit>,
    pub scheduled_at: DateTime<Utc>,
    /// Who scheduled it, as the caller said ("settings tab", "claude desktop").
    pub via: String,
    /// When the user answered "yes", oldest first.
    #[serde(default)]
    pub approvals: Vec<DateTime<Utc>>,
    #[serde(default)]
    pub origin: ReservationOrigin,
    /// Seconds between questions: two days for settings, two hours for a to-do change.
    #[serde(default = "settings_interval")]
    pub interval_seconds: i64,
    /// What the user wrote about it (a to-do change's request and reason).
    #[serde(default)]
    pub note: String,
    /// The whole new file, as versions before field-level edits kept it; turned into
    /// `edits` when read.
    #[serde(default, skip_serializing)]
    content: Option<Value>,
}

impl Reservation {
    /// When the next "is this update really right?" question is due.
    pub fn next_question_at(&self) -> DateTime<Utc> {
        self.approvals.last().copied().unwrap_or(self.scheduled_at)
            + question_interval(self.interval_seconds)
    }

    /// One line per edit, for the Settings tab.
    pub fn describe(&self) -> Vec<String> {
        self.edits.iter().map(Edit::describe).collect()
    }

    pub fn question_due(&self, now: DateTime<Utc>) -> bool {
        now >= self.next_question_at()
    }
}

/// Responsibility: Say how a scheduled update ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReservationOutcome {
    /// Confirmed three times: in force.
    Applied,
    /// The user answered "no".
    Rejected,
    /// The user withdrew it before it was answered three times.
    Withdrawn,
    /// The same file was saved again; the newer update replaced this one.
    Replaced,
}

/// Responsibility: Remember how a past scheduled update ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReservationRecord {
    pub id: String,
    pub name: String,
    pub via: String,
    pub scheduled_at: DateTime<Utc>,
    pub outcome: ReservationOutcome,
    pub at: DateTime<Utc>,
}

/// Responsibility: Hold the scheduled updates and how earlier ones ended.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reservations {
    pub scheduled: Vec<Reservation>,
    /// Newest last.
    pub history: VecDeque<ReservationRecord>,
}

/// Responsibility: Say what became of a saved file: which edits are in force now, and which
/// were scheduled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PutOutcome {
    /// Edits in force now: every edit during setup, the stricter ones once protected.
    pub applied: Vec<String>,
    /// The looser edits, kept as a scheduled update; `None` when there were none.
    pub scheduled: Option<ScheduledUpdate>,
}

/// Responsibility: Name a scheduled update and when it is first asked about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledUpdate {
    pub id: String,
    pub first_question_at: DateTime<Utc>,
    pub edits: Vec<String>,
}

/// Responsibility: Say what an answer did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AnswerOutcome {
    /// "Yes", and more questions follow; the next one is due then.
    Approved {
        approvals: usize,
        next_question_at: DateTime<Utc>,
    },
    /// The third "yes": the update is in force.
    Applied,
    /// "No": the update is dropped.
    Rejected,
}

/// Responsibility: Carry the admin settings and what the admin panels show next to them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsSnapshot {
    /// Editable files in force by name (see [`EDITABLE`]); a missing file is `null`.
    pub files: BTreeMap<String, Value>,
    /// Setup is over: edits are scheduled and need three confirmations.
    pub protected: bool,
    pub reservations: Reservations,
    /// Read-only views: `planner_state.json` and every user's grants and blocks files.
    pub planner_state: Value,
    pub grants: BTreeMap<String, Value>,
    pub blocks: BTreeMap<String, Value>,
    /// Every user's token usage file (see `ai_focus_rule::usage`).
    pub usage: BTreeMap<String, Value>,
}

/// Setup is over (or an older version had locked the settings).
pub fn is_protected(data_directory: &Path) -> bool {
    data_directory.join(PROTECTION_FILE).exists() || data_directory.join(OLD_LOCK_FILE).exists()
}

/// End the setup: from now on every edit is scheduled. Nothing in the program undoes this.
pub fn protect(data_directory: &Path, now: DateTime<Utc>) -> Result<Protection, String> {
    let _write = write_lock();
    let path = data_directory.join(PROTECTION_FILE);
    let current: Protection = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if current.protected_since.is_some() {
        return Ok(current);
    }
    let protection = Protection {
        protected_since: Some(now),
    };
    let bytes = serde_json::to_vec_pretty(&protection).map_err(|error| error.to_string())?;
    write_atomic(&path, &bytes)
        .map_err(|error| format!("could not save {PROTECTION_FILE}: {error}"))?;
    Ok(protection)
}

/// Read everything the admin panels need from `data_directory`.
pub fn snapshot(data_directory: &Path) -> Result<SettingsSnapshot, String> {
    let files = EDITABLE
        .iter()
        .map(|(name, file)| (name.to_string(), read_json(&data_directory.join(file))))
        .collect();
    Ok(SettingsSnapshot {
        files,
        protected: is_protected(data_directory),
        reservations: read_reservations(data_directory),
        planner_state: read_json(&data_directory.join("planner_state.json")),
        grants: read_folder(&data_directory.join("grants")),
        blocks: read_folder(&data_directory.join("blocks")),
        usage: read_folder(&data_directory.join("usage")),
    })
}

/// The scheduled updates whose question is due now.
pub fn questions_due(data_directory: &Path, now: DateTime<Utc>) -> usize {
    read_reservations(data_directory)
        .scheduled
        .iter()
        .filter(|reservation| reservation.question_due(now))
        .count()
}

/// Save `content` as the editable file `name`. During setup it takes effect at once. Once
/// protected, the edits that make the rules stricter take effect at once and the looser ones
/// become a scheduled update (replacing the earlier scheduled update of this file, whose
/// answers are lost, unless it holds exactly the same edits). Refused when `content` does not
/// parse as that file's settings. Edits of the settings in `fix_now` (paths of keys; the API
/// key while a broken AI connection locks every app, see [`crate::lockdown`]) take effect at
/// once like stricter ones.
pub fn put(
    data_directory: &Path,
    now: DateTime<Utc>,
    name: &str,
    content: &Value,
    via: &str,
    fix_now: &[Vec<String>],
) -> Result<PutOutcome, String> {
    let _write = write_lock();
    let file = file_of(name)?;
    validate(name, content)?;
    // Both sides as the program reads them (defaults filled in), so only real changes count.
    let in_force = normalize(name, &read_json(&data_directory.join(file)))?;
    let content = &normalize(name, content)?;
    let edits = setting_edits::diff(&in_force, content);
    if !is_protected(data_directory) {
        write_setting(data_directory, file, content)?;
        return Ok(PutOutcome {
            applied: edits.iter().map(Edit::describe).collect(),
            scheduled: None,
        });
    }
    let (stricter, looser): (Vec<Edit>, Vec<Edit>) = edits.into_iter().partition(|edit| {
        setting_edits::direction(name, edit, &in_force) != Direction::Looser
            || (name == "ai_focus"
                && fix_now
                    .iter()
                    .any(|path| edit.path().starts_with(path.as_slice())))
    });
    if !stricter.is_empty() {
        apply_edits(data_directory, name, &stricter)?;
    }
    let mut reservations = read_reservations(data_directory);
    let existing = reservations.scheduled.iter().position(|reservation| {
        reservation.name == name && reservation.origin == ReservationOrigin::Settings
    });
    let mut outcome = PutOutcome {
        applied: stricter.iter().map(Edit::describe).collect(),
        scheduled: None,
    };
    match existing {
        // Saved again with the same looser edits (e.g. only a stricter one was added):
        // the scheduled update and its answers stay.
        Some(index) if reservations.scheduled[index].edits == looser => {
            let reservation = &reservations.scheduled[index];
            outcome.scheduled = Some(ScheduledUpdate {
                id: reservation.id.clone(),
                first_question_at: reservation.next_question_at(),
                edits: reservation.describe(),
            });
            return Ok(outcome);
        }
        Some(index) => {
            let replaced = reservations.scheduled.remove(index);
            let outcome_of_old = if looser.is_empty() {
                ReservationOutcome::Withdrawn
            } else {
                ReservationOutcome::Replaced
            };
            record(&mut reservations, &replaced, outcome_of_old, now);
        }
        None => {}
    }
    if !looser.is_empty() {
        let reservation = new_reservation(
            now,
            name,
            looser,
            via,
            ReservationOrigin::Settings,
            SETTINGS_QUESTION_INTERVAL_SECONDS,
            "",
        );
        outcome.scheduled = Some(ScheduledUpdate {
            id: reservation.id.clone(),
            first_question_at: reservation.next_question_at(),
            edits: reservation.describe(),
        });
        reservations.scheduled.push(reservation);
    }
    write_reservations(data_directory, &reservations)?;
    Ok(outcome)
}

/// Put `edits` of the settings file `name` in force now, on top of the file as it is.
/// Refused when the result does not parse as that file's settings.
pub fn apply_now(data_directory: &Path, name: &str, edits: &[Edit]) -> Result<(), String> {
    let _write = write_lock();
    apply_edits(data_directory, name, edits)
}

/// [`apply_now`] for a caller that already holds the write lock.
fn apply_edits(data_directory: &Path, name: &str, edits: &[Edit]) -> Result<(), String> {
    let file = file_of(name)?;
    let path = data_directory.join(file);
    let current = read_json(&path);
    // An unreadable file must not be replaced by the defaults (it holds the API key).
    if current.is_null() && path.exists() {
        return Err(format!("{file}을(를) 읽지 못해서 바꾸지 않았습니다"));
    }
    let mut updated = normalize(name, &current)?;
    setting_edits::apply(&mut updated, edits);
    validate(name, &updated)?;
    write_setting(data_directory, file, &updated)
}

/// Schedule `edits` of `name` as their own update (a to-do change the AI did not approve),
/// asked about every `interval_seconds`. Returns the update.
pub fn schedule(
    data_directory: &Path,
    now: DateTime<Utc>,
    name: &str,
    edits: Vec<Edit>,
    via: &str,
    interval_seconds: i64,
    note: &str,
) -> Result<ScheduledUpdate, String> {
    let _write = write_lock();
    let file = file_of(name)?;
    let mut preview = read_json(&data_directory.join(file));
    setting_edits::apply(&mut preview, &edits);
    validate(name, &preview)?;
    let mut reservations = read_reservations(data_directory);
    let mut reservation = new_reservation(
        now,
        name,
        edits,
        via,
        ReservationOrigin::TodoChange,
        interval_seconds,
        note,
    );
    // Two in the same millisecond would share an id.
    while reservations
        .scheduled
        .iter()
        .any(|existing| existing.id == reservation.id)
    {
        reservation.id.push('x');
    }
    let scheduled = ScheduledUpdate {
        id: reservation.id.clone(),
        first_question_at: reservation.next_question_at(),
        edits: reservation.describe(),
    };
    reservations.scheduled.push(reservation);
    write_reservations(data_directory, &reservations)?;
    Ok(scheduled)
}

fn new_reservation(
    now: DateTime<Utc>,
    name: &str,
    edits: Vec<Edit>,
    via: &str,
    origin: ReservationOrigin,
    interval_seconds: i64,
    note: &str,
) -> Reservation {
    Reservation {
        id: format!("u{}", now.timestamp_millis()),
        name: name.to_string(),
        edits,
        scheduled_at: now,
        via: via.trim().chars().take(40).collect(),
        approvals: Vec::new(),
        origin,
        interval_seconds,
        note: note.trim().chars().take(500).collect(),
        content: None,
    }
}

/// The user's answer to "is this update really right?". Refused before the question is due.
pub fn answer(
    data_directory: &Path,
    now: DateTime<Utc>,
    id: &str,
    approve: bool,
) -> Result<AnswerOutcome, String> {
    let _write = write_lock();
    let mut reservations = read_reservations(data_directory);
    let index = reservations
        .scheduled
        .iter()
        .position(|reservation| reservation.id == id)
        .ok_or_else(|| format!("no scheduled update '{id}'"))?;
    let due_at = reservations.scheduled[index].next_question_at();
    if now < due_at {
        return Err(format!(
            "the next question about this update is due at {}",
            due_at.to_rfc3339()
        ));
    }
    if !approve {
        let rejected = reservations.scheduled.remove(index);
        record(
            &mut reservations,
            &rejected,
            ReservationOutcome::Rejected,
            now,
        );
        write_reservations(data_directory, &reservations)?;
        return Ok(AnswerOutcome::Rejected);
    }
    reservations.scheduled[index].approvals.push(now);
    if reservations.scheduled[index].approvals.len() < APPROVALS_NEEDED {
        let reservation = &reservations.scheduled[index];
        let outcome = AnswerOutcome::Approved {
            approvals: reservation.approvals.len(),
            next_question_at: reservation.next_question_at(),
        };
        write_reservations(data_directory, &reservations)?;
        return Ok(outcome);
    }
    let applied = reservations.scheduled.remove(index);
    // Applied to the file as it is now; checked again, since it may have changed meanwhile.
    apply_edits(data_directory, &applied.name, &applied.edits)?;
    record(
        &mut reservations,
        &applied,
        ReservationOutcome::Applied,
        now,
    );
    write_reservations(data_directory, &reservations)?;
    Ok(AnswerOutcome::Applied)
}

/// Drop a scheduled update before it takes effect. Withdrawing gains nothing, so it needs
/// no confirmation.
pub fn withdraw(data_directory: &Path, now: DateTime<Utc>, id: &str) -> Result<(), String> {
    let _write = write_lock();
    let mut reservations = read_reservations(data_directory);
    let index = reservations
        .scheduled
        .iter()
        .position(|reservation| reservation.id == id)
        .ok_or_else(|| format!("no scheduled update '{id}'"))?;
    let withdrawn = reservations.scheduled.remove(index);
    record(
        &mut reservations,
        &withdrawn,
        ReservationOutcome::Withdrawn,
        now,
    );
    write_reservations(data_directory, &reservations)
}

fn record(
    reservations: &mut Reservations,
    reservation: &Reservation,
    outcome: ReservationOutcome,
    now: DateTime<Utc>,
) {
    if reservations.history.len() >= HISTORY_CAPACITY {
        reservations.history.pop_front();
    }
    reservations.history.push_back(ReservationRecord {
        id: reservation.id.clone(),
        name: reservation.name.clone(),
        via: reservation.via.clone(),
        scheduled_at: reservation.scheduled_at,
        outcome,
        at: now,
    });
}

fn file_of(name: &str) -> Result<&'static str, String> {
    EDITABLE
        .iter()
        .find(|(editable_name, _)| *editable_name == name)
        .map(|(_, file)| *file)
        .ok_or_else(|| format!("'{name}' is not an editable setting"))
}

fn write_setting(data_directory: &Path, file: &str, content: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(content).map_err(|error| error.to_string())?;
    write_atomic(&data_directory.join(file), &bytes)
        .map_err(|error| format!("could not save {file}: {error}"))
}

/// The scheduled updates; edits an older version kept "until the lock ends" join them as
/// updates scheduled when they were saved.
fn read_reservations(data_directory: &Path) -> Reservations {
    let mut reservations: Reservations = fs::read_to_string(data_directory.join(RESERVATIONS_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let old_pending: BTreeMap<String, OldPendingFile> =
        fs::read_to_string(data_directory.join(OLD_PENDING_FILE))
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
    if !old_pending.is_empty() {
        for (name, pending) in old_pending {
            if reservations
                .scheduled
                .iter()
                .any(|reservation| reservation.name == name)
            {
                continue;
            }
            let mut reservation = new_reservation(
                pending.saved_at,
                &name,
                Vec::new(),
                "settings tab (older version)",
                ReservationOrigin::Settings,
                SETTINGS_QUESTION_INTERVAL_SECONDS,
                "",
            );
            reservation.id = format!("u{}{}", pending.saved_at.timestamp_millis(), name.len());
            reservation.content = Some(pending.content);
            reservations.scheduled.push(reservation);
        }
        if write_reservations(data_directory, &reservations).is_ok() {
            let _ = fs::remove_file(data_directory.join(OLD_PENDING_FILE));
        }
    }
    // Whole-file updates of earlier versions become the edits against the file in force.
    let mut converted = false;
    for reservation in &mut reservations.scheduled {
        if let Some(content) = reservation.content.take() {
            if let Ok(file) = file_of(&reservation.name) {
                reservation.edits =
                    setting_edits::diff(&read_json(&data_directory.join(file)), &content);
                converted = true;
            }
        }
    }
    if converted {
        let _ = write_reservations(data_directory, &reservations);
    }
    reservations
}

/// Responsibility: Read an edit an older version kept until its settings lock ended.
#[derive(Deserialize)]
struct OldPendingFile {
    content: Value,
    saved_at: DateTime<Utc>,
}

fn write_reservations(data_directory: &Path, reservations: &Reservations) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(reservations).map_err(|error| error.to_string())?;
    write_atomic(&data_directory.join(RESERVATIONS_FILE), &bytes)
        .map_err(|error| format!("could not save {RESERVATIONS_FILE}: {error}"))
}

/// `content` as the program reads it, written back: missing settings get their defaults.
/// A missing file (`null`) counts as all defaults.
pub fn normalize(name: &str, content: &Value) -> Result<Value, String> {
    fn round_trip<T: serde::de::DeserializeOwned + Serialize>(
        content: &Value,
    ) -> Result<Value, String> {
        let typed: T =
            serde_json::from_value(content.clone()).map_err(|error| error.to_string())?;
        serde_json::to_value(typed).map_err(|error| error.to_string())
    }
    let content = if content.is_null() {
        &Value::Object(Default::default())
    } else {
        content
    };
    match name {
        "policy" => round_trip::<list_based_rule::ListConfig>(content),
        "ai_focus" => round_trip::<ai_focus_rule::AiFocusConfig>(content),
        "planner" => round_trip::<task_planner::PlannerConfig>(content),
        _ => Err(format!("'{name}' is not an editable setting")),
    }
    .map_err(|error| format!("{name}: {error}"))
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
    result.map_err(|error| format!("{name}: {error}"))
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

/// Every `<name>.json` in `data_directory` by file stem; unreadable files are left out.
fn read_folder(data_directory: &Path) -> BTreeMap<String, Value> {
    let Ok(entries) = fs::read_dir(data_directory) else {
        return BTreeMap::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let value = read_json(&path);
            let name = path.file_stem()?.to_string_lossy().to_string();
            (!value.is_null()).then_some((name, value))
        })
        .collect()
}

/// Temp file + rename, so a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // Unique per write, so two writers never share a temporary file.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp_path = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let data_directory =
            std::env::temp_dir().join(format!("dw_settings_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&data_directory);
        fs::create_dir_all(&data_directory).unwrap();
        data_directory
    }

    fn in_force(data_directory: &Path, name: &str) -> Value {
        snapshot(data_directory).unwrap().files[name].clone()
    }

    /// Save `content`; return the id of the scheduled update it made.
    fn schedule_file(data_directory: &Path, at: DateTime<Utc>, content: Value) -> String {
        put(data_directory, at, "planner", &content, "settings tab", &[])
            .unwrap()
            .scheduled
            .expect("a scheduled update")
            .id
    }

    #[test]
    fn an_unreadable_file_is_never_replaced_by_the_defaults() {
        let data_directory = temp_dir("unreadable");
        fs::write(
            data_directory.join("ai_focus.json"),
            "{ \"api_key\": \"k\", broken",
        )
        .unwrap();
        let edit = Edit::Set {
            path: vec!["time_zone".into()],
            value: json!("Korea Standard Time"),
        };
        assert!(apply_now(&data_directory, "ai_focus", &[edit]).is_err());
        let left = fs::read_to_string(data_directory.join("ai_focus.json")).unwrap();
        assert!(left.contains("\"k\""), "{left}");
    }

    #[test]
    fn during_a_lockdown_a_new_key_takes_effect_at_once() {
        let data_directory = temp_dir("lockdown_key");
        let now = Utc::now();
        put(
            &data_directory,
            now,
            "ai_focus",
            &json!({ "api_key": "old" }),
            "",
            &[],
        )
        .unwrap();
        protect(&data_directory, now).unwrap();
        let new_key = json!({ "api_key": "new", "model": "other-model" });
        let fix_now = vec![vec!["api_key".to_string()]];
        let outcome = put(
            &data_directory,
            now,
            "ai_focus",
            &new_key,
            "settings tab",
            &fix_now,
        )
        .unwrap();
        assert_eq!(outcome.applied.len(), 1, "{outcome:?}");
        assert!(outcome.applied[0].starts_with("api_key"), "{outcome:?}");
        // The model is not part of the fix: still scheduled.
        assert!(outcome.scheduled.is_some());
        assert_eq!(in_force(&data_directory, "ai_focus")["api_key"], "new");
        // Without a lockdown the key is scheduled as usual.
        let outcome = put(
            &data_directory,
            now,
            "ai_focus",
            &json!({ "api_key": "third" }),
            "",
            &[],
        )
        .unwrap();
        assert!(outcome.applied.is_empty(), "{outcome:?}");
    }

    #[test]
    fn during_setup_edits_take_effect_at_once() {
        let data_directory = temp_dir("setup");
        let now = Utc::now();
        let outcome = put(
            &data_directory,
            now,
            "planner",
            &json!({ "enabled": true, "todos": [] }),
            "settings tab",
            &[],
        )
        .unwrap();
        assert!(outcome.scheduled.is_none());
        assert!(!outcome.applied.is_empty());
        fs::create_dir_all(data_directory.join("blocks")).unwrap();
        fs::write(
            data_directory.join("blocks").join("work.json"),
            r#"{"processes":{}}"#,
        )
        .unwrap();
        let settings_snapshot = snapshot(&data_directory).unwrap();
        assert_eq!(settings_snapshot.files["planner"]["enabled"], true);
        assert!(settings_snapshot.files["policy"].is_null());
        assert!(settings_snapshot.blocks.contains_key("work"));
        assert!(!settings_snapshot.protected);
        // Content that would not load is refused, and so are other files.
        assert!(put(
            &data_directory,
            now,
            "planner",
            &json!({ "enabled": "yes" }),
            "",
            &[]
        )
        .is_err());
        assert!(put(&data_directory, now, "machine", &json!({}), "", &[]).is_err());
    }

    #[test]
    fn once_protected_stricter_edits_apply_at_once_and_looser_ones_are_scheduled() {
        let data_directory = temp_dir("split");
        let start = Utc::now();
        let ai_focus = json!({ "always_allow": ["a.exe"], "strikes_to_block": 2, "model": "m" });
        put(&data_directory, start, "ai_focus", &ai_focus, "", &[]).unwrap();
        protect(&data_directory, start).unwrap();

        // Stricter (fewer strikes, a.exe no longer allowed) and looser (another model).
        let saved = json!({ "always_allow": [], "strikes_to_block": 1, "model": "cheap" });
        let outcome = put(
            &data_directory,
            start,
            "ai_focus",
            &saved,
            "settings tab",
            &[],
        )
        .unwrap();
        assert_eq!(outcome.applied.len(), 2, "{outcome:?}");
        let scheduled = outcome.scheduled.expect("the model change is scheduled");
        assert_eq!(scheduled.edits, ["model = cheap"]);
        let now_in_force = in_force(&data_directory, "ai_focus");
        assert_eq!(now_in_force["strikes_to_block"], 1);
        assert_eq!(now_in_force["always_allow"], json!([]));
        assert_eq!(now_in_force["model"], "m");

        // Saving again with the same looser edit and one more stricter one keeps the
        // scheduled update and its answers.
        answer(
            &data_directory,
            start + Duration::days(2),
            &scheduled.id,
            true,
        )
        .unwrap();
        let resaved = json!({ "always_allow": [], "strikes_to_block": 1, "model": "cheap", "send_screenshot": true });
        let outcome = put(
            &data_directory,
            start + Duration::days(2),
            "ai_focus",
            &resaved,
            "",
            &[],
        )
        .unwrap();
        assert_eq!(outcome.scheduled.unwrap().id, scheduled.id);
        let kept = &snapshot(&data_directory).unwrap().reservations.scheduled[0];
        assert_eq!(kept.approvals.len(), 1);
        assert_eq!(
            in_force(&data_directory, "ai_focus")["send_screenshot"],
            true
        );

        // The scheduled edit lands on the file as it is then, keeping the stricter edits.
        answer(
            &data_directory,
            start + Duration::days(4),
            &scheduled.id,
            true,
        )
        .unwrap();
        answer(
            &data_directory,
            start + Duration::days(6),
            &scheduled.id,
            true,
        )
        .unwrap();
        let final_file = in_force(&data_directory, "ai_focus");
        assert_eq!(final_file["model"], "cheap");
        assert_eq!(final_file["send_screenshot"], true);
        assert_eq!(final_file["strikes_to_block"], 1);
    }

    #[test]
    fn missing_defaults_in_the_file_in_force_are_not_changes() {
        let data_directory = temp_dir("defaults");
        let start = Utc::now();
        // A sparse file, as an older version or a hand edit leaves it.
        fs::write(
            data_directory.join("ai_focus.json"),
            json!({ "api_key": "k", "send_screenshot": false }).to_string(),
        )
        .unwrap();
        protect(&data_directory, start).unwrap();
        // The Settings tab sends the whole typed config, defaults included, with one change.
        let mut full: ai_focus_rule::AiFocusConfig =
            serde_json::from_value(json!({ "api_key": "k", "send_screenshot": false })).unwrap();
        full.send_screenshot = true;
        let outcome = put(
            &data_directory,
            start,
            "ai_focus",
            &serde_json::to_value(&full).unwrap(),
            "settings tab",
            &[],
        )
        .unwrap();
        assert_eq!(outcome.applied, ["send_screenshot = true"]);
        assert!(outcome.scheduled.is_none(), "{outcome:?}");
        assert_eq!(
            in_force(&data_directory, "ai_focus")["send_screenshot"],
            true
        );
    }

    #[test]
    fn a_protected_edit_takes_effect_after_three_yes_answers_two_days_apart() {
        let data_directory = temp_dir("three_yes");
        let start = Utc::now();
        put(
            &data_directory,
            start,
            "planner",
            &json!({ "life_rules": "no games" }),
            "",
            &[],
        )
        .unwrap();
        protect(&data_directory, start).unwrap();
        assert!(is_protected(&data_directory));
        let outcome = put(
            &data_directory,
            start,
            "planner",
            &json!({ "life_rules": "games ok" }),
            "claude desktop",
            &[],
        )
        .unwrap();
        let scheduled = outcome.scheduled.expect("life rules are scheduled");
        assert!(outcome.applied.is_empty());
        assert_eq!(scheduled.first_question_at, start + Duration::days(2));
        let id = scheduled.id;
        assert_eq!(
            in_force(&data_directory, "planner")["life_rules"],
            "no games"
        );

        // Not before the question is due.
        assert!(answer(&data_directory, start + Duration::days(1), &id, true).is_err());
        assert_eq!(questions_due(&data_directory, start + Duration::days(1)), 0);
        // The first question waits until it is answered, here a day late.
        let first_answer = start + Duration::days(3);
        assert_eq!(questions_due(&data_directory, first_answer), 1);
        assert_eq!(
            answer(&data_directory, first_answer, &id, true).unwrap(),
            AnswerOutcome::Approved {
                approvals: 1,
                next_question_at: first_answer + Duration::days(2),
            }
        );
        // The next question is two days after the answer, not after the schedule.
        assert!(answer(&data_directory, start + Duration::days(4), &id, true).is_err());
        let second_answer = first_answer + Duration::days(2);
        answer(&data_directory, second_answer, &id, true).unwrap();
        assert_eq!(
            in_force(&data_directory, "planner")["life_rules"],
            "no games"
        );
        assert_eq!(
            answer(
                &data_directory,
                second_answer + Duration::days(2),
                &id,
                true
            )
            .unwrap(),
            AnswerOutcome::Applied
        );
        assert_eq!(
            in_force(&data_directory, "planner")["life_rules"],
            "games ok"
        );
        let settings_snapshot = snapshot(&data_directory).unwrap();
        assert!(settings_snapshot.reservations.scheduled.is_empty());
        assert_eq!(
            settings_snapshot
                .reservations
                .history
                .back()
                .unwrap()
                .outcome,
            ReservationOutcome::Applied
        );
    }

    #[test]
    fn one_no_rejects_and_other_looser_edits_start_over() {
        let data_directory = temp_dir("no");
        let start = Utc::now();
        put(
            &data_directory,
            start,
            "planner",
            &json!({ "life_rules": "no games" }),
            "",
            &[],
        )
        .unwrap();
        protect(&data_directory, start).unwrap();
        assert_eq!(
            protect(&data_directory, start + Duration::days(9))
                .unwrap()
                .protected_since,
            Some(start)
        );
        let first = schedule_file(&data_directory, start, json!({ "life_rules": "a" }));
        answer(&data_directory, start + Duration::days(2), &first, true).unwrap();
        // Other looser edits replace the update; its answers are lost.
        let later = start + Duration::days(2) + Duration::seconds(1);
        let second = schedule_file(&data_directory, later, json!({ "life_rules": "b" }));
        let settings_snapshot = snapshot(&data_directory).unwrap();
        assert_eq!(settings_snapshot.reservations.scheduled.len(), 1);
        assert!(settings_snapshot.reservations.scheduled[0]
            .approvals
            .is_empty());
        assert!(answer(&data_directory, start + Duration::days(5), &first, true).is_err());

        let first_question = later + Duration::days(2);
        answer(&data_directory, first_question, &second, true).unwrap();
        assert_eq!(
            answer(
                &data_directory,
                first_question + Duration::days(2),
                &second,
                false
            )
            .unwrap(),
            AnswerOutcome::Rejected
        );
        assert_eq!(
            in_force(&data_directory, "planner")["life_rules"],
            "no games"
        );
        let outcomes: Vec<_> = snapshot(&data_directory)
            .unwrap()
            .reservations
            .history
            .iter()
            .map(|record| record.outcome)
            .collect();
        assert_eq!(
            outcomes,
            [ReservationOutcome::Replaced, ReservationOutcome::Rejected]
        );

        // Saving the file back without the looser edit withdraws the update; withdrawing
        // needs no answer either.
        let third = schedule_file(
            &data_directory,
            start + Duration::days(10),
            json!({ "life_rules": "c" }),
        );
        put(
            &data_directory,
            start + Duration::days(10),
            "planner",
            &json!({ "life_rules": "no games" }),
            "",
            &[],
        )
        .unwrap();
        assert!(snapshot(&data_directory)
            .unwrap()
            .reservations
            .scheduled
            .is_empty());
        assert!(withdraw(&data_directory, start, &third).is_err());
        let fourth = schedule_file(
            &data_directory,
            start + Duration::days(11),
            json!({ "life_rules": "d" }),
        );
        withdraw(&data_directory, start + Duration::days(11), &fourth).unwrap();
        assert!(snapshot(&data_directory)
            .unwrap()
            .reservations
            .scheduled
            .is_empty());
    }

    #[test]
    fn a_scheduled_todo_change_is_asked_about_every_two_hours() {
        let data_directory = temp_dir("todo_change");
        let start = Utc::now();
        put(
            &data_directory,
            start,
            "planner",
            &json!({ "todos": [] }),
            "",
            &[],
        )
        .unwrap();
        protect(&data_directory, start).unwrap();
        let todo = json!({ "id": "t1", "title": "Read a novel" });
        let edits = vec![Edit::PutListItem {
            path: vec!["todos".into()],
            id: "t1".into(),
            item: Some(todo.clone()),
        }];
        let scheduled = schedule(
            &data_directory,
            start,
            "planner",
            edits,
            "to-do tab",
            TODO_QUESTION_INTERVAL_SECONDS,
            "add: the AI said no",
        )
        .unwrap();
        assert_eq!(scheduled.first_question_at, start + Duration::hours(2));
        // A settings save of the same file does not replace it.
        schedule_file(
            &data_directory,
            start,
            json!({ "todos": [], "life_rules": "x" }),
        );
        assert_eq!(
            snapshot(&data_directory)
                .unwrap()
                .reservations
                .scheduled
                .len(),
            2
        );
        for hours in [2, 4, 6] {
            answer(
                &data_directory,
                start + Duration::hours(hours),
                &scheduled.id,
                true,
            )
            .unwrap();
        }
        assert_eq!(in_force(&data_directory, "planner")["todos"], json!([todo]));
    }

    #[test]
    fn an_older_lock_means_protected_and_its_pending_edits_become_scheduled() {
        let data_directory = temp_dir("migrate");
        let saved_at = Utc::now() - Duration::days(1);
        fs::write(
            data_directory.join("planner.json"),
            json!({ "life_rules": "old" }).to_string(),
        )
        .unwrap();
        fs::write(
            data_directory.join(OLD_LOCK_FILE),
            json!({ "until": saved_at }).to_string(),
        )
        .unwrap();
        fs::write(
            data_directory.join(OLD_PENDING_FILE),
            json!({ "planner": { "content": { "life_rules": "x" }, "saved_at": saved_at } })
                .to_string(),
        )
        .unwrap();
        let settings_snapshot = snapshot(&data_directory).unwrap();
        assert!(settings_snapshot.protected);
        let scheduled = &settings_snapshot.reservations.scheduled;
        assert_eq!(scheduled.len(), 1);
        assert_eq!(scheduled[0].scheduled_at, saved_at);
        assert_eq!(scheduled[0].describe(), ["life_rules = x"]);
        assert!(!data_directory.join(OLD_PENDING_FILE).exists());
        // Read again: the edits were saved.
        assert_eq!(
            snapshot(&data_directory).unwrap().reservations.scheduled[0].describe(),
            ["life_rules = x"]
        );
    }
}

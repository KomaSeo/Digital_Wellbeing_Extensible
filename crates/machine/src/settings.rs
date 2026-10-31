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
//!   is checked and kept as a scheduled update ([`Reservation`], `settings_reservations.json`).
//!   Two days after it was scheduled the Focus app asks "is this update really right?"; each
//!   "yes" ([`answer`]) brings the next question two days after the answer, and the third
//!   "yes" in a row puts the update in force. One "no" rejects it. A question waits until it
//!   is answered. Saving the same file again replaces the scheduled update and starts over.
//!
//! So a change made in an irrational moment cannot take effect before the user has stood by
//! it on three different days. (Windows cannot stop an administrator account from deleting
//! these files or the service; the user does not work in one. Standard users cannot change
//! the system clock, so the waiting cannot be skipped by setting the clock forward.)

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

/// Time between scheduling and the first question, and between an answer and the next
/// question: two days. `DW_SETTINGS_CONFIRM_INTERVAL_SECONDS` shortens it for tests; only an
/// administrator can set the service's environment.
pub fn question_interval() -> Duration {
    std::env::var("DW_SETTINGS_CONFIRM_INTERVAL_SECONDS")
        .ok()
        .and_then(|seconds| seconds.parse::<i64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::seconds)
        .unwrap_or_else(|| Duration::days(2))
}

/// Responsibility: Record when the settings were protected.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Protection {
    pub protected_since: Option<DateTime<Utc>>,
}

/// Responsibility: Hold one scheduled update of a settings file and the answers it got.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: String,
    /// One of the [`EDITABLE`] names.
    pub name: String,
    /// The whole new file.
    pub content: Value,
    pub scheduled_at: DateTime<Utc>,
    /// Who scheduled it, as the caller said ("settings tab", "claude desktop").
    pub via: String,
    /// When the user answered "yes", oldest first.
    #[serde(default)]
    pub approvals: Vec<DateTime<Utc>>,
}

impl Reservation {
    /// When the next "is this update really right?" question is due.
    pub fn next_question_at(&self) -> DateTime<Utc> {
        self.approvals.last().copied().unwrap_or(self.scheduled_at) + question_interval()
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

/// Responsibility: Say what became of a saved edit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum PutOutcome {
    /// Setup (not yet protected): the file in force was replaced.
    Applied,
    /// Protected: kept as a scheduled update; the first question is due then.
    Scheduled {
        id: String,
        first_question_at: DateTime<Utc>,
    },
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

/// Save `content` as the editable file `name`: in force at once during setup, as a scheduled
/// update once protected (replacing an earlier scheduled update of the same file, whose
/// answers are lost). Refused when `content` does not parse as that file's settings.
pub fn put(
    data_directory: &Path,
    now: DateTime<Utc>,
    name: &str,
    content: &Value,
    via: &str,
) -> Result<PutOutcome, String> {
    let file = file_of(name)?;
    validate(name, content)?;
    if !is_protected(data_directory) {
        write_setting(data_directory, file, content)?;
        return Ok(PutOutcome::Applied);
    }
    let mut reservations = read_reservations(data_directory);
    if let Some(index) = reservations
        .scheduled
        .iter()
        .position(|reservation| reservation.name == name)
    {
        let replaced = reservations.scheduled.remove(index);
        record(
            &mut reservations,
            &replaced,
            ReservationOutcome::Replaced,
            now,
        );
    }
    let reservation = Reservation {
        id: format!("u{}", now.timestamp_millis()),
        name: name.to_string(),
        content: content.clone(),
        scheduled_at: now,
        via: via.trim().chars().take(40).collect(),
        approvals: Vec::new(),
    };
    let outcome = PutOutcome::Scheduled {
        id: reservation.id.clone(),
        first_question_at: reservation.next_question_at(),
    };
    reservations.scheduled.push(reservation);
    write_reservations(data_directory, &reservations)?;
    Ok(outcome)
}

/// The user's answer to "is this update really right?". Refused before the question is due.
pub fn answer(
    data_directory: &Path,
    now: DateTime<Utc>,
    id: &str,
    approve: bool,
) -> Result<AnswerOutcome, String> {
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
    // Checked again: the program that reads the file may have changed since it was saved.
    validate(&applied.name, &applied.content)?;
    write_setting(data_directory, file_of(&applied.name)?, &applied.content)?;
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
            reservations.scheduled.push(Reservation {
                id: format!("u{}{}", pending.saved_at.timestamp_millis(), name.len()),
                name,
                content: pending.content,
                scheduled_at: pending.saved_at,
                via: "settings tab (older version)".into(),
                approvals: Vec::new(),
            });
        }
        if write_reservations(data_directory, &reservations).is_ok() {
            let _ = fs::remove_file(data_directory.join(OLD_PENDING_FILE));
        }
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
    let temp_path = path.with_extension(format!("tmp.{}", std::process::id()));
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

    fn life_rules(data_directory: &Path) -> Value {
        snapshot(data_directory).unwrap().files["planner"]["life_rules"].clone()
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
        )
        .unwrap();
        assert_eq!(outcome, PutOutcome::Applied);
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
            ""
        )
        .is_err());
        assert!(put(&data_directory, now, "machine", &json!({}), "").is_err());
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
        )
        .unwrap();
        let PutOutcome::Scheduled {
            id,
            first_question_at,
        } = outcome
        else {
            panic!("not scheduled: {outcome:?}");
        };
        assert_eq!(first_question_at, start + Duration::days(2));
        assert_eq!(life_rules(&data_directory), "no games");

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
        assert_eq!(life_rules(&data_directory), "no games");

        let third_answer = second_answer + Duration::days(2);
        assert_eq!(
            answer(&data_directory, third_answer, &id, true).unwrap(),
            AnswerOutcome::Applied
        );
        assert_eq!(life_rules(&data_directory), "games ok");
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
    fn one_no_rejects_and_saving_again_starts_over() {
        let data_directory = temp_dir("no");
        let start = Utc::now();
        put(
            &data_directory,
            start,
            "planner",
            &json!({ "life_rules": "no games" }),
            "",
        )
        .unwrap();
        protect(&data_directory, start).unwrap();
        // Protecting twice keeps the first time.
        assert_eq!(
            protect(&data_directory, start + Duration::days(9))
                .unwrap()
                .protected_since,
            Some(start)
        );

        let schedule = |at, rules: &str| match put(
            &data_directory,
            at,
            "planner",
            &json!({ "life_rules": rules }),
            "settings tab",
        )
        .unwrap()
        {
            PutOutcome::Scheduled { id, .. } => id,
            other => panic!("not scheduled: {other:?}"),
        };
        let first = schedule(start, "a");
        answer(&data_directory, start + Duration::days(2), &first, true).unwrap();
        // Saving the same file again replaces the update; its answers are lost.
        let second = schedule(start + Duration::days(2) + Duration::seconds(1), "b");
        let settings_snapshot = snapshot(&data_directory).unwrap();
        assert_eq!(settings_snapshot.reservations.scheduled.len(), 1);
        assert!(settings_snapshot.reservations.scheduled[0]
            .approvals
            .is_empty());
        assert!(answer(&data_directory, start + Duration::days(5), &first, true).is_err());

        let first_question = start + Duration::days(4) + Duration::seconds(1);
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
        assert_eq!(life_rules(&data_directory), "no games");
        let history = snapshot(&data_directory).unwrap().reservations.history;
        let outcomes: Vec<_> = history.iter().map(|record| record.outcome).collect();
        assert_eq!(
            outcomes,
            [ReservationOutcome::Replaced, ReservationOutcome::Rejected]
        );

        // Withdrawing needs no answer.
        let third = schedule(start + Duration::days(10), "c");
        withdraw(&data_directory, start + Duration::days(10), &third).unwrap();
        assert!(snapshot(&data_directory)
            .unwrap()
            .reservations
            .scheduled
            .is_empty());
        assert!(withdraw(&data_directory, start, "nope").is_err());
    }

    #[test]
    fn an_older_lock_means_protected_and_its_pending_edits_become_scheduled() {
        let data_directory = temp_dir("migrate");
        let saved_at = Utc::now() - Duration::days(1);
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
        assert!(!data_directory.join(OLD_PENDING_FILE).exists());
    }
}

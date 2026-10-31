//! Responsibility: Show the admin settings in the Focus app, send the user's edits to the
//! agent, ask the user about scheduled updates ("is this update really right?"), and turn
//! the protection of the settings on.
//!
//! This tab is where the settings of a machine install are edited, from any managed
//! (standard) account. It asks the agent for the settings (`settings_get`), writes them into
//! a private copy ("mirror") under `%LOCALAPPDATA%`, and opens the usual stores and panels on
//! that copy. A background thread sends every file the panels save back to the agent
//! (`settings_put`), which writes the real file as SYSTEM, and refreshes the read-only views
//! (planner state, grants, blocks).
//!
//! During setup an edit takes effect at once. Once the settings are protected
//! (`settings_protect`, one way) the agent keeps every edit as a scheduled update; this tab
//! lists them, asks about each one when its question is due (`settings_answer`: three "yes"
//! answers two days apart put it in force, one "no" rejects it), and can withdraw one
//! (`settings_withdraw`). The mirror shows a scheduled update's version, so editing goes on
//! from it. Rules: `machine::settings`; guide: `docs/multi_account.md` 6.4.

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::{native_monitor, AiFocusRuleHandle, AiFocusStore};
use chrono::{DateTime, Local, Utc};
use list_based_rule::{FileListPersistence, ListBasedRuleHandle, ListStore};
use machine::settings::{
    AnswerOutcome, PutOutcome, Reservation, SettingsSnapshot, APPROVALS_NEEDED, EDITABLE,
};
use machine::user_api::call_value;
use parking_lot::Mutex;
use rule_ui::{Panel, RuleUi};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use task_planner::{JsonStore, PlannerAdminUi, PlannerConfig, PlannerState};

const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);
/// How often the list of scheduled updates is fetched while this tab is shown.
const RESERVATIONS_REFRESH: Duration = Duration::from_secs(3);

/// Responsibility: Hold the Settings tab: the loaded panels, the sync thread, the scheduled
/// updates last fetched, and the protection form.
pub struct SettingsTab {
    session: u32,
    loaded: Option<Loaded>,
    error: String,
    confirm_protect: bool,
    /// The last `settings_get` answer, for the scheduled-updates list and its questions.
    latest: Option<SettingsSnapshot>,
    fetched_at: Option<Instant>,
    /// What the last answer did, shown under the list.
    answer_message: String,
}

/// Responsibility: Keep the panels opened on the mirror and the thread that syncs it.
struct Loaded {
    rule_uis: Vec<Arc<dyn RuleUi>>,
    panels: Vec<Arc<dyn Panel>>,
    selected: usize,
    sync: Arc<Sync>,
}

impl Drop for Loaded {
    fn drop(&mut self) {
        self.sync.stop.store(true, Ordering::SeqCst);
    }
}

/// Responsibility: State shared between the Settings tab and its sync thread.
struct Sync {
    stop: AtomicBool,
    /// Last bytes of each editable file the agent has accepted (or sent us).
    synced: Mutex<HashMap<&'static str, Option<Vec<u8>>>>,
    message: Mutex<String>,
}

impl SettingsTab {
    pub fn new(session: u32) -> Self {
        Self {
            session,
            loaded: None,
            error: String::new(),
            confirm_protect: false,
            latest: None,
            fetched_at: None,
            answer_message: String::new(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, protected: bool) {
        if protected {
            ui.label(format!(
                "The settings are protected. Every change you save is scheduled: two days \
                 later you are asked whether it is really right, and it takes effect only \
                 after {APPROVALS_NEEDED} \"yes\" answers in a row, two days apart. One \"no\" \
                 rejects it."
            ));
        } else {
            self.protect_ui(ui);
        }
        if self.reservations_ui(ui) {
            // A scheduled update was answered or withdrawn: reopen on what is now true.
            if self.loaded.is_some() {
                self.open();
            }
        }

        ui.separator();
        if self.loaded.is_none() {
            if ui.button("Open the settings").clicked() {
                self.open();
            }
            if !self.error.is_empty() {
                ui.colored_label(RED, &self.error);
            }
            return;
        }
        let Some(loaded) = &mut self.loaded else {
            return;
        };
        let message = loaded.sync.message.lock().clone();
        if !message.is_empty() {
            ui.small(message);
        }
        let names: Vec<String> = loaded
            .rule_uis
            .iter()
            .map(|rule_ui| rule_ui.name().to_string())
            .chain(loaded.panels.iter().map(|panel| panel.name().to_string()))
            .collect();
        ui.horizontal(|ui| {
            for (index, name) in names.iter().enumerate() {
                if ui
                    .selectable_label(loaded.selected == index, name)
                    .clicked()
                {
                    loaded.selected = index;
                }
            }
        });
        ui.separator();
        let rules = loaded.rule_uis.len();
        if loaded.selected < rules {
            loaded.rule_uis[loaded.selected].settings_ui(ui, &[]);
        } else if let Some(panel) = loaded.panels.get(loaded.selected - rules) {
            panel.ui(ui);
        }
    }

    fn open(&mut self) {
        self.loaded = None;
        match self.load() {
            Ok(loaded) => {
                self.loaded = Some(loaded);
                self.error.clear();
            }
            Err(error) => self.error = error,
        }
    }

    /// Setup: changes take effect at once until the user protects the settings.
    fn protect_ui(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            YELLOW,
            "Setup: changes take effect at once, and anyone using this PC can make them.",
        );
        ui.label(format!(
            "When everything is set, protect the settings. From then on every change is \
             scheduled and takes effect only after you confirm it {APPROVALS_NEEDED} times, \
             two days apart."
        ));
        ui.checkbox(
            &mut self.confirm_protect,
            "I understand that nothing in the program turns the protection off.",
        );
        if ui
            .add_enabled(
                self.confirm_protect,
                egui::Button::new("Protect the settings"),
            )
            .clicked()
        {
            match call_value(self.session, "settings_protect", json!({})) {
                Ok(_) => {
                    self.confirm_protect = false;
                    self.error.clear();
                }
                Err(error) => self.error = error,
            }
        }
    }

    /// The scheduled updates, with the question for each one that is due, and a Withdraw
    /// button. Returns whether one was answered or withdrawn.
    fn reservations_ui(&mut self, ui: &mut egui::Ui) -> bool {
        if self
            .fetched_at
            .is_none_or(|fetched_at| fetched_at.elapsed() >= RESERVATIONS_REFRESH)
        {
            self.fetched_at = Some(Instant::now());
            match call_value(self.session, "settings_get", json!({}))
                .and_then(|reply| serde_json::from_value(reply).map_err(|error| error.to_string()))
            {
                Ok(snapshot) => self.latest = Some(snapshot),
                Err(error) => self.error = error,
            }
        }
        let Some(snapshot) = self.latest.clone() else {
            return false;
        };
        let now = Utc::now();
        let mut changed = false;
        if !snapshot.reservations.scheduled.is_empty() {
            ui.separator();
            ui.heading("Scheduled updates");
        }
        for reservation in &snapshot.reservations.scheduled {
            let file = file_name(&reservation.name);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(file);
                    ui.label(format!(
                        "scheduled {} via {} — {}/{APPROVALS_NEEDED} confirmed",
                        local(reservation.scheduled_at),
                        reservation.via,
                        reservation.approvals.len()
                    ));
                });
                let in_force = snapshot
                    .files
                    .get(&reservation.name)
                    .cloned()
                    .unwrap_or(Value::Null);
                egui::CollapsingHeader::new("What changes")
                    // Read before answering.
                    .default_open(reservation.question_due(now))
                    .id_salt(("changes", &reservation.id))
                    .show(ui, |ui| changes_ui(ui, &in_force, &reservation.content));
                if reservation.question_due(now) {
                    changed |= self.question_ui(ui, reservation);
                } else {
                    ui.label(format!(
                        "Next question: {}",
                        local(reservation.next_question_at())
                    ));
                }
                if ui.small_button("Withdraw this update").clicked() {
                    match call_value(
                        self.session,
                        "settings_withdraw",
                        json!({ "id": reservation.id }),
                    ) {
                        Ok(_) => {
                            self.answer_message = format!("The update of {file} was withdrawn.");
                            changed = true;
                        }
                        Err(error) => self.error = error,
                    }
                }
            });
        }
        if !self.answer_message.is_empty() {
            ui.label(&self.answer_message);
        }
        if changed {
            self.fetched_at = None;
        }
        changed
    }

    /// "Is this update really right?" with Yes and No. Returns whether it was answered.
    fn question_ui(&mut self, ui: &mut egui::Ui, reservation: &Reservation) -> bool {
        let file = file_name(&reservation.name);
        ui.colored_label(
            RED,
            format!(
                "Is this update of {file} really right? (question {} of {APPROVALS_NEEDED})",
                reservation.approvals.len() + 1
            ),
        );
        ui.small("Read what changes above. Answer as you would on a calm day.");
        let mut answer = None;
        ui.horizontal(|ui| {
            if ui.button("Yes, I still want it").clicked() {
                answer = Some(true);
            }
            if ui.button("No, reject it").clicked() {
                answer = Some(false);
            }
        });
        let Some(approve) = answer else {
            return false;
        };
        let reply = call_value(
            self.session,
            "settings_answer",
            json!({ "id": reservation.id, "approve": approve }),
        )
        .and_then(|reply| {
            serde_json::from_value::<AnswerOutcome>(reply).map_err(|error| error.to_string())
        });
        match reply {
            Ok(AnswerOutcome::Approved {
                approvals,
                next_question_at,
            }) => {
                self.answer_message = format!(
                    "{file}: {approvals}/{APPROVALS_NEEDED} confirmed. Next question: {}.",
                    local(next_question_at)
                )
            }
            Ok(AnswerOutcome::Applied) => {
                self.answer_message =
                    format!("{file}: confirmed {APPROVALS_NEEDED} times; in force now.")
            }
            Ok(AnswerOutcome::Rejected) => {
                self.answer_message = format!("{file}: the update was rejected.")
            }
            Err(error) => self.error = error,
        }
        true
    }

    /// Ask the agent for the settings, write the mirror, open the panels on it.
    fn load(&self) -> Result<Loaded, String> {
        let snapshot: SettingsSnapshot =
            serde_json::from_value(call_value(self.session, "settings_get", json!({}))?)
                .map_err(|error| error.to_string())?;
        let mirror_directory = mirror_dir();
        let _ = fs::remove_dir_all(&mirror_directory);
        let mut synced = HashMap::new();
        for (name, file) in EDITABLE {
            // A scheduled update is what the user saved last: go on editing from it.
            let current = snapshot
                .reservations
                .scheduled
                .iter()
                .find(|reservation| reservation.name == name)
                .map(|reservation| &reservation.content)
                .or_else(|| snapshot.files.get(name));
            let bytes = match current.filter(|value| !value.is_null()) {
                Some(value) => {
                    let bytes =
                        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
                    write(&mirror_directory.join(file), &bytes)?;
                    Some(bytes)
                }
                None => None,
            };
            synced.insert(name, bytes);
        }
        write_read_only(&mirror_directory, &snapshot)?;

        let list_store = ListStore::with_persistence(Arc::new(FileListPersistence::new(
            mirror_directory.join("policy.json"),
        )))
        .map_err(|error| error.to_string())?;
        let ai_store = AiFocusStore::open(mirror_directory.join("ai_focus.json"))
            .map_err(|error| error.to_string())?;
        let monitor = native_monitor(ai_store, JsonStore::in_memory(GrantState::default()), None);
        let ai_rule = AiFocusRuleHandle::settings_only(
            monitor,
            mirror_directory.join("grants"),
            mirror_directory.join("blocks"),
            mirror_directory.join("usage"),
        );
        let planner = PlannerAdminUi::new(
            JsonStore::<PlannerConfig>::open(mirror_directory.join("planner.json"), true)
                .map_err(|error| error.to_string())?,
            JsonStore::<PlannerState>::open(mirror_directory.join("planner_state.json"), false)
                .map_err(|error| error.to_string())?,
        );
        // Opening a store may write its file back with defaults filled in; that is the same
        // settings, not an edit to send (once protected it would become a scheduled update).
        for (name, file) in EDITABLE {
            if let Ok(bytes) = fs::read(mirror_directory.join(file)) {
                synced.insert(name, Some(bytes));
            }
        }

        let sync = Arc::new(Sync {
            stop: AtomicBool::new(false),
            synced: Mutex::new(synced),
            message: Mutex::new(String::new()),
        });
        let (session, thread_sync) = (self.session, Arc::clone(&sync));
        thread::spawn(move || sync_loop(session, &mirror_directory, &thread_sync));
        Ok(Loaded {
            rule_uis: vec![ListBasedRuleHandle::new(list_store).ui(), ai_rule.ui()],
            panels: vec![Arc::new(planner)],
            selected: 0,
            sync,
        })
    }
}

/// Every second: send edited files to the agent. Every few seconds: refresh the read-only
/// views. Stops when the tab drops the panels.
fn sync_loop(session: u32, mirror_directory: &Path, sync: &Sync) {
    let mut tick = 0u32;
    while !sync.stop.load(Ordering::SeqCst) {
        for (name, file) in EDITABLE {
            let Ok(bytes) = fs::read(mirror_directory.join(file)) else {
                continue;
            };
            if sync.synced.lock().get(name).cloned().flatten().as_ref() == Some(&bytes) {
                continue;
            }
            let content: Value = match serde_json::from_slice(&bytes) {
                Ok(content) => content,
                Err(_) => continue, // being written; next tick
            };
            let result = call_value(
                session,
                "settings_put",
                json!({ "name": name, "content": content }),
            );
            let at = Local::now().format("%H:%M:%S");
            let outcome = result.and_then(|reply| {
                serde_json::from_value::<PutOutcome>(reply).map_err(|error| error.to_string())
            });
            *sync.message.lock() = match outcome {
                Ok(outcome) => {
                    sync.synced.lock().insert(name, Some(bytes));
                    match outcome {
                        PutOutcome::Applied => format!("Saved {file} at {at}; in force now."),
                        PutOutcome::Scheduled {
                            first_question_at, ..
                        } => format!(
                            "Saved {file} at {at}; scheduled. You will be asked about it from {} \
                             on; it takes effect after {APPROVALS_NEEDED} \"yes\" answers.",
                            local(first_question_at)
                        ),
                    }
                }
                Err(error) => format!("{file} was not saved: {error}"),
            };
        }
        if tick.is_multiple_of(3) {
            if let Ok(snapshot) = call_value(session, "settings_get", json!({}))
                .and_then(|reply| serde_json::from_value(reply).map_err(|error| error.to_string()))
            {
                let _ = write_read_only(mirror_directory, &snapshot);
            }
        }
        tick = tick.wrapping_add(1);
        thread::sleep(Duration::from_secs(1));
    }
}

/// The views the panels read but never write: planner state, grants, blocks.
fn write_read_only(mirror_directory: &Path, snapshot: &SettingsSnapshot) -> Result<(), String> {
    let json = |value: &Value| serde_json::to_vec_pretty(value).map_err(|error| error.to_string());
    if !snapshot.planner_state.is_null() {
        write(
            &mirror_directory.join("planner_state.json"),
            &json(&snapshot.planner_state)?,
        )?;
    }
    for (folder, files) in [
        ("grants", &snapshot.grants),
        ("blocks", &snapshot.blocks),
        ("usage", &snapshot.usage),
    ] {
        let folder = mirror_directory.join(folder);
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
        for (user, value) in files {
            write(&folder.join(format!("{user}.json")), &json(value)?)?;
        }
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    machine::settings::write_atomic(path, bytes)
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// `%LOCALAPPDATA%\DigitalWellbeing\...\settings_mirror`: this user's private copy.
fn mirror_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
        .map(|project_dirs| project_dirs.data_local_dir().join("settings_mirror"))
        .unwrap_or_else(|| std::env::temp_dir().join("DigitalWellbeing_settings_mirror"))
}

fn file_name(name: &str) -> &str {
    EDITABLE
        .iter()
        .find(|(editable_name, _)| *editable_name == name)
        .map_or(name, |(_, file)| file)
}

fn local(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// What a scheduled update changes, setting by setting: `path: old → new`.
fn changes_ui(ui: &mut egui::Ui, in_force: &Value, update: &Value) {
    let mut changes = Vec::new();
    collect_changes("", in_force, update, &mut changes);
    if changes.is_empty() {
        ui.label("Nothing differs from the settings in force.");
    }
    for (path, before, after) in changes {
        ui.label(egui::RichText::new(path).strong());
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(RED, format!("now: {before}"));
        });
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(
                egui::Color32::from_rgb(80, 170, 90),
                format!("after: {after}"),
            );
        });
    }
}

/// Settings that differ between `before` and `after`, descending into objects; other values
/// are compared whole and shown as compact JSON (long texts shortened).
fn collect_changes(
    path: &str,
    before: &Value,
    after: &Value,
    changes: &mut Vec<(String, String, String)>,
) {
    if before == after {
        return;
    }
    let no_fields = serde_json::Map::new();
    let fields = |value: &Value| match value {
        Value::Object(fields) => Some(fields.clone()),
        // A setting that does not exist yet compares like an empty one, field by field.
        Value::Null => Some(no_fields.clone()),
        _ => None,
    };
    let either_is_object = before.is_object() || after.is_object();
    if let (true, Some(before_fields), Some(after_fields)) =
        (either_is_object, fields(before), fields(after))
    {
        let mut keys: Vec<&String> = before_fields.keys().chain(after_fields.keys()).collect();
        keys.sort();
        keys.dedup();
        for key in keys {
            let child = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            collect_changes(
                &child,
                before_fields.get(key).unwrap_or(&Value::Null),
                after_fields.get(key).unwrap_or(&Value::Null),
                changes,
            );
        }
        return;
    }
    let shown = |value: &Value| {
        let text = match value {
            Value::String(text) => text.clone(),
            Value::Null => "(none)".into(),
            other => other.to_string(),
        };
        if text.chars().count() > 400 {
            format!("{}…", text.chars().take(400).collect::<String>())
        } else {
            text
        }
    };
    let label = if path.is_empty() {
        "(whole file)".to_string()
    } else {
        path.to_string()
    };
    changes.push((label, shown(before), shown(after)));
}

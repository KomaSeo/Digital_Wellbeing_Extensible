//! Responsibility: Show the admin settings in the Focus app, send the user's edits to the
//! agent, show and discard the edits that wait for the lock to end, and let the user lock the
//! settings for a while.
//!
//! This tab is where the settings of a machine install are edited, from any managed
//! (standard) account, locked or not. It asks the agent for the settings (`settings_get`),
//! writes them into a private copy ("mirror") under `%LOCALAPPDATA%`, and opens the usual
//! stores and panels on that copy. A background thread sends every file the panels save back
//! to the agent (`settings_put`), which writes the real file as SYSTEM, and refreshes the
//! read-only views (planner state, grants, blocks).
//!
//! While the settings are locked the agent keeps a saved file as a pending edit and puts it
//! in force only when the lock ends. The mirror then shows the pending version, so editing
//! goes on from it; "Discard" drops it (`settings_discard`). Locking (`settings_lock`) only
//! ever extends. Rules: `machine::settings`; guide: `docs/multi_account.md` 6.4.

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::{native_monitor, AiFocusRuleHandle, AiFocusStore};
use chrono::{DateTime, Local, Utc};
use list_based_rule::{FileListPersistence, ListBasedRuleHandle, ListStore};
use machine::settings::{PendingFile, PutOutcome, SettingsSnapshot, EDITABLE};
use machine::user_api::call_value;
use parking_lot::Mutex;
use rule_ui::{Panel, RuleUi};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use task_planner::{JsonStore, PlannerAdminUi, PlannerConfig, PlannerState};

const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);

/// Responsibility: Hold the Settings tab: the loaded panels, the sync thread, the lock form.
pub struct SettingsTab {
    session: u32,
    loaded: Option<Loaded>,
    error: String,
    lock_days: u32,
    lock_hours: u32,
    confirm: bool,
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
    /// Edits the agent keeps until the lock ends (refreshed every few seconds).
    pending: Mutex<BTreeMap<String, PendingFile>>,
    message: Mutex<String>,
}

impl SettingsTab {
    pub fn new(session: u32) -> Self {
        Self {
            session,
            loaded: None,
            error: String::new(),
            lock_days: 7,
            lock_hours: 0,
            confirm: false,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, locked_until: Option<DateTime<Utc>>) {
        match locked_until {
            Some(until) => {
                let until = until.with_timezone(&Local).format("%Y-%m-%d %H:%M");
                ui.colored_label(RED, format!("The admin settings are locked until {until}."));
                ui.label(format!(
                    "You can still edit them, but what you save takes effect only when the \
                     lock ends ({until}). Until then the current settings stay in force."
                ));
            }
            None => {
                ui.colored_label(
                    YELLOW,
                    "The admin settings are unlocked: anyone using this PC can change them \
                     here, and changes take effect at once.",
                );
            }
        }
        self.lock_ui(ui, locked_until);

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
        if self.pending_ui(ui, locked_until) {
            // A pending edit was dropped: reopen on the settings in force.
            self.open();
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
            .map(|r| r.name().to_string())
            .chain(loaded.panels.iter().map(|p| p.name().to_string()))
            .collect();
        ui.horizontal(|ui| {
            for (i, name) in names.iter().enumerate() {
                if ui.selectable_label(loaded.selected == i, name).clicked() {
                    loaded.selected = i;
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
            Err(err) => self.error = err,
        }
    }

    /// The saved edits that wait for the lock to end, each with a Discard button. Returns
    /// whether one was discarded.
    fn pending_ui(&mut self, ui: &mut egui::Ui, locked_until: Option<DateTime<Utc>>) -> bool {
        let Some(loaded) = &self.loaded else {
            return false;
        };
        let pending = loaded.sync.pending.lock().clone();
        if pending.is_empty() {
            return false;
        }
        let when = locked_until.map_or("in a few seconds".to_string(), |u| {
            format!(
                "when the lock ends ({})",
                u.with_timezone(&Local).format("%Y-%m-%d %H:%M")
            )
        });
        ui.strong(format!("Saved changes that take effect {when}:"));
        let mut discarded = false;
        for (name, edit) in &pending {
            let file = EDITABLE
                .iter()
                .find(|(n, _)| n == name)
                .map_or(name.as_str(), |(_, f)| f);
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{file} (saved {})",
                    edit.saved_at.with_timezone(&Local).format("%m-%d %H:%M")
                ));
                if ui.small_button("Discard").clicked() {
                    match call_value(self.session, "settings_discard", json!({ "name": name })) {
                        Ok(_) => discarded = true,
                        Err(err) => self.error = err,
                    }
                }
            });
        }
        ui.small("The tabs below show your saved changes, not the settings in force.");
        ui.separator();
        discarded
    }

    fn lock_ui(&mut self, ui: &mut egui::Ui, locked_until: Option<DateTime<Utc>>) {
        ui.horizontal_wrapped(|ui| {
            ui.label(if locked_until.is_some() {
                "Lock for longer:"
            } else {
                "Lock them for"
            });
            ui.add(egui::DragValue::new(&mut self.lock_days).range(0..=366));
            ui.label("days");
            ui.add(egui::DragValue::new(&mut self.lock_hours).range(0..=23));
            ui.label("hours");
        });
        let hours = self.lock_days * 24 + self.lock_hours;
        ui.checkbox(
            &mut self.confirm,
            "I understand that nobody can unlock them early.",
        );
        if ui
            .add_enabled(hours > 0 && self.confirm, egui::Button::new("Lock"))
            .clicked()
        {
            match call_value(self.session, "settings_lock", json!({ "hours": hours })) {
                Ok(_) => {
                    self.confirm = false;
                    self.error.clear();
                }
                Err(err) => self.error = err,
            }
        }
    }

    /// Ask the agent for the settings, write the mirror, open the panels on it.
    fn load(&self) -> Result<Loaded, String> {
        let snapshot: SettingsSnapshot =
            serde_json::from_value(call_value(self.session, "settings_get", json!({}))?)
                .map_err(|e| e.to_string())?;
        let dir = mirror_dir();
        let _ = fs::remove_dir_all(&dir);
        let mut synced = HashMap::new();
        for (name, file) in EDITABLE {
            // A pending edit is what the user saw last: go on editing from it.
            let current = snapshot
                .pending
                .get(name)
                .map(|p| &p.content)
                .or_else(|| snapshot.files.get(name));
            let bytes = match current.filter(|v| !v.is_null()) {
                Some(value) => {
                    let bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
                    write(&dir.join(file), &bytes)?;
                    Some(bytes)
                }
                None => None,
            };
            synced.insert(name, bytes);
        }
        write_read_only(&dir, &snapshot)?;

        let list_store = ListStore::with_persistence(Arc::new(FileListPersistence::new(
            dir.join("policy.json"),
        )))
        .map_err(|e| e.to_string())?;
        let ai_store = AiFocusStore::open(dir.join("ai_focus.json")).map_err(|e| e.to_string())?;
        let monitor = native_monitor(ai_store, JsonStore::in_memory(GrantState::default()), None);
        let ai_rule = AiFocusRuleHandle::settings_only(
            monitor,
            dir.join("grants"),
            dir.join("blocks"),
            dir.join("usage"),
        );
        let planner = PlannerAdminUi::new(
            JsonStore::<PlannerConfig>::open(dir.join("planner.json"), true)
                .map_err(|e| e.to_string())?,
            JsonStore::<PlannerState>::open(dir.join("planner_state.json"), false)
                .map_err(|e| e.to_string())?,
        );
        // Opening a store may write its file back with defaults filled in; that is the same
        // settings, not an edit to send (while locked it would show as a pending change).
        for (name, file) in EDITABLE {
            if let Ok(bytes) = fs::read(dir.join(file)) {
                synced.insert(name, Some(bytes));
            }
        }

        let sync = Arc::new(Sync {
            stop: AtomicBool::new(false),
            synced: Mutex::new(synced),
            pending: Mutex::new(snapshot.pending.clone()),
            message: Mutex::new(String::new()),
        });
        let (session, thread_sync) = (self.session, Arc::clone(&sync));
        thread::spawn(move || sync_loop(session, &dir, &thread_sync));
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
fn sync_loop(session: u32, dir: &Path, sync: &Sync) {
    let mut tick = 0u32;
    while !sync.stop.load(Ordering::SeqCst) {
        for (name, file) in EDITABLE {
            let Ok(bytes) = fs::read(dir.join(file)) else {
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
            let outcome = result
                .and_then(|v| serde_json::from_value::<PutOutcome>(v).map_err(|e| e.to_string()));
            *sync.message.lock() = match outcome {
                Ok(outcome) => {
                    sync.synced.lock().insert(name, Some(bytes));
                    match outcome {
                        PutOutcome::Applied => format!("Saved {file} at {at}; in force now."),
                        PutOutcome::Pending { applies_at } => {
                            sync.pending.lock().insert(
                                name.to_string(),
                                PendingFile {
                                    content,
                                    saved_at: Utc::now(),
                                },
                            );
                            format!(
                                "Saved {file} at {at}; takes effect when the lock ends ({}).",
                                applies_at.with_timezone(&Local).format("%Y-%m-%d %H:%M")
                            )
                        }
                    }
                }
                Err(err) => format!("{file} was not saved: {err}"),
            };
        }
        if tick.is_multiple_of(3) {
            if let Ok(snapshot) = call_value(session, "settings_get", json!({}))
                .and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
            {
                let _ = write_read_only(dir, &snapshot);
                *sync.pending.lock() = snapshot.pending;
            }
        }
        tick = tick.wrapping_add(1);
        thread::sleep(Duration::from_secs(1));
    }
}

/// The views the panels read but never write: planner state, grants, blocks.
fn write_read_only(dir: &Path, snapshot: &SettingsSnapshot) -> Result<(), String> {
    let json = |v: &Value| serde_json::to_vec_pretty(v).map_err(|e| e.to_string());
    if !snapshot.planner_state.is_null() {
        write(
            &dir.join("planner_state.json"),
            &json(&snapshot.planner_state)?,
        )?;
    }
    for (folder, files) in [
        ("grants", &snapshot.grants),
        ("blocks", &snapshot.blocks),
        ("usage", &snapshot.usage),
    ] {
        let folder = dir.join(folder);
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        for (user, value) in files {
            write(&folder.join(format!("{user}.json")), &json(value)?)?;
        }
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    machine::settings::write_atomic(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}

/// `%LOCALAPPDATA%\DigitalWellbeing\...\settings_mirror`: this user's private copy.
fn mirror_dir() -> PathBuf {
    directories::ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing")
        .map(|d| d.data_local_dir().join("settings_mirror"))
        .unwrap_or_else(|| std::env::temp_dir().join("DigitalWellbeing_settings_mirror"))
}

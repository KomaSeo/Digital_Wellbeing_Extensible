//! Responsibility: Show and edit the settings the app needs (the AI connection, the rules for
//! the AI, blocking, the AI checks), ask the user about scheduled updates ("is this update
//! really right?"), and turn the protection of the settings on.
//!
//! The settings are three files in the data folder (`ai_focus.json`, `policy.json`,
//! `planner.json`) that only the agent (SYSTEM) writes. This tab reads them through the agent
//! (`settings_get`), with the scheduled settings updates already applied so the user goes on
//! from what they saved, edits typed copies, and saves a changed file whole (`settings_put`).
//! During setup a save takes effect at once. Once protected (`settings_protect`, one way) the
//! agent puts the stricter edits in force at once and schedules the looser ones; the tab says
//! which is which before and after saving. Scheduled updates are asked about here
//! (`settings_answer`: three "yes" answers put one in force, one "no" rejects it) and can be
//! withdrawn (`settings_withdraw`). The to-do list itself is edited in the To-do tab.
//! Rules: `machine::settings`, `machine::setting_edits`; guide: `docs/multi_account.md` 6.4.

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::usage::UsageLog;
use ai_focus_rule::{AiFocusConfig, SavedBlocks};
use chrono::{DateTime, Datelike, Local, Utc};
use list_based_rule::ListConfig;
use machine::setting_edits::{self, Direction};
use machine::settings::{
    AnswerOutcome, PutOutcome, Reservation, ReservationOrigin, SettingsSnapshot, APPROVALS_NEEDED,
    EDITABLE,
};
use machine::user_api::call_value;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use task_planner::{PlannerConfig, DAY_PLAN_PROMPT, PICK_PROMPT, PROPOSAL_PROMPT, REVIEW_PROMPT};

const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);
const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
/// How often the settings are fetched again while this tab is shown.
const REFRESH: Duration = Duration::from_secs(3);

/// Responsibility: Name the sections of the Settings tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Connection,
    Rules,
    Blocking,
    Checks,
}

/// Responsibility: Hold the Settings tab: the settings last fetched, the copies being edited,
/// and the form inputs.
pub struct SettingsTab {
    session: u32,
    error: String,
    confirm_protect: bool,
    /// The last `settings_get` answer.
    latest: Option<SettingsSnapshot>,
    fetched_at: Option<Instant>,
    /// What the last answer or save did.
    message: String,
    section: Section,
    /// Copies being edited; `None` until the first fetch, and again after saving.
    drafts: Option<Drafts>,
    show_key: bool,
    new_key: String,
    /// Result of the last "Check key", written by a background thread.
    key_check: Arc<Mutex<String>>,
    /// Text typed into the "add" fields of the lists, by list.
    adding: HashMap<&'static str, String>,
}

/// Responsibility: Hold the editable copies of the three files and what they were when loaded
/// (the files in force with the scheduled settings updates applied), in [`EDITABLE`] order.
struct Drafts {
    policy: ListConfig,
    ai_focus: AiFocusConfig,
    planner: PlannerConfig,
    loaded: [Value; 3],
}

impl Drafts {
    fn as_values(&self) -> [Value; 3] {
        [
            to_value(&self.policy),
            to_value(&self.ai_focus),
            to_value(&self.planner),
        ]
    }
}

impl SettingsTab {
    pub fn new(session: u32) -> Self {
        Self {
            session,
            error: String::new(),
            confirm_protect: false,
            latest: None,
            fetched_at: None,
            message: String::new(),
            section: Section::Connection,
            drafts: None,
            show_key: false,
            new_key: String::new(),
            key_check: Arc::new(Mutex::new(String::new())),
            adding: HashMap::new(),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, protected: bool) {
        self.refresh();
        ui.heading("Protection (lock)");
        if protected {
            ui.label(format!(
                "The settings are protected. A change that makes the rules stricter takes effect \
                 at once. Any other change is scheduled: two days later you are asked whether \
                 it is really right, and it takes effect after {APPROVALS_NEEDED} \"yes\" answers \
                 in a row, two days apart. One \"no\" rejects it."
            ));
        } else {
            self.protect_ui(ui);
        }
        if self.reservations_ui(ui) {
            // Answered or withdrawn: edit on top of what is now true.
            self.fetched_at = None;
            self.drafts = None;
            self.refresh();
        }
        if !self.message.is_empty() {
            ui.label(&self.message);
        }
        if !self.error.is_empty() {
            ui.colored_label(RED, &self.error);
        }
        let Some(snapshot) = self.latest.clone() else {
            ui.label("connecting…");
            return;
        };
        if self.drafts.is_none() {
            self.drafts = Some(drafts_from(&snapshot));
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for (section, label) in [
                (Section::Connection, "AI connection"),
                (Section::Rules, "Rules for the AI"),
                (Section::Blocking, "Blocking"),
                (Section::Checks, "AI checks"),
            ] {
                ui.selectable_value(&mut self.section, section, label);
            }
        });
        ui.separator();
        match self.section {
            Section::Connection => self.connection_ui(ui, &snapshot),
            Section::Rules => self.rules_ui(ui),
            Section::Blocking => self.blocking_ui(ui, &snapshot),
            Section::Checks => self.checks_ui(ui),
        }
        ui.separator();
        self.save_ui(ui, &snapshot, protected);
    }

    fn refresh(&mut self) {
        if self
            .fetched_at
            .is_some_and(|fetched_at| fetched_at.elapsed() < REFRESH)
        {
            return;
        }
        self.fetched_at = Some(Instant::now());
        match call_value(self.session, "settings_get", json!({}))
            .and_then(|reply| serde_json::from_value(reply).map_err(|error| error.to_string()))
        {
            Ok(snapshot) => {
                self.latest = Some(snapshot);
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
            "When everything is set, protect the settings. From then on a change that makes \
             the rules stricter takes effect at once, and any other change only after you \
             confirm it {APPROVALS_NEEDED} times, two days apart."
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
                    self.fetched_at = None;
                }
                Err(error) => self.error = error,
            }
        }
    }

    // --- AI connection ------------------------------------------------------------------

    fn connection_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot) {
        let Some(drafts) = &mut self.drafts else {
            return;
        };
        let config = &mut drafts.ai_focus;
        ui.heading("Anthropic API key");
        let saved = config.api_key.trim();
        if saved.is_empty() {
            ui.colored_label(
                RED,
                "No key is saved: the AI cannot judge or plan anything.",
            );
        } else {
            let tail: String = saved
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            ui.label(format!("A key is saved (…{tail})."));
        }
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_key)
                    .password(!self.show_key)
                    .hint_text("paste a new key")
                    .desired_width(300.0),
            );
            ui.checkbox(&mut self.show_key, "show");
        });
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    !self.new_key.trim().is_empty(),
                    egui::Button::new("Use this key"),
                )
                .clicked()
            {
                config.api_key = self.new_key.trim().to_string();
                self.new_key.clear();
            }
            if ui
                .add_enabled(!config.api_key.is_empty(), egui::Button::new("Clear"))
                .clicked()
            {
                config.api_key.clear();
            }
            if ui
                .add_enabled(!config.api_key.is_empty(), egui::Button::new("Check key"))
                .clicked()
            {
                let (key, model, result) = (
                    config.api_key.clone(),
                    config.model.clone(),
                    Arc::clone(&self.key_check),
                );
                *result.lock() = "checking…".into();
                thread::spawn(move || {
                    let answer = match ai_focus_rule::claude::ClaudeClient::new(key, model).check()
                    {
                        Ok(()) => "✔ The key works (no cost: only the model list was read).".into(),
                        Err(error) => format!("The key was not accepted: {error}"),
                    };
                    *result.lock() = answer;
                });
            }
        });
        let check = self.key_check.lock().clone();
        if !check.is_empty() {
            ui.small(check);
        }
        ui.horizontal(|ui| {
            ui.label("Model");
            ui.add(egui::TextEdit::singleline(&mut config.model).desired_width(200.0));
        });

        ui.separator();
        ui.heading("AI usage");
        ui.horizontal(|ui| {
            ui.label("Daily budget for the AI checks (USD, 0 = none)");
            ui.add(
                egui::DragValue::new(&mut config.daily_budget_usd)
                    .speed(0.05)
                    .range(0.0..=1000.0),
            );
        });
        usage_ui(ui, snapshot);
    }

    // --- rules for the AI ---------------------------------------------------------------

    fn rules_ui(&mut self, ui: &mut egui::Ui) {
        let Some(drafts) = &mut self.drafts else {
            return;
        };
        let planner = &mut drafts.planner;
        ui.checkbox(
            &mut planner.enabled,
            "The AI picks what I do from my to-do list (planner)",
        );
        ui.small(
            "Off: you type what you do yourself, and the AI judges apps against that. Turning it \
             on takes effect at once; turning it off is scheduled.",
        );
        ui.separator();
        ui.heading("Life rules");
        ui.label(
            "Standing rules every AI decision follows: picking what you do, reviewing changes \
             and to-do changes, planning the day.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut planner.life_rules)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text(
                    "e.g. 평일 18시 전에는 게임과 영상을 할당하지 않는다. 2시간 일하면 20분 쉰다.",
                ),
        );
        ui.separator();
        ui.heading("AI instructions");
        ui.label(
            "What the planner's AI is told to do. An empty box means the built-in instruction.",
        );
        let instructions = &mut planner.instructions;
        for (label, own, built_in) in [
            (
                "Picking what you do and for how long",
                &mut instructions.pick,
                PICK_PROMPT,
            ),
            (
                "Reviewing a change of what you do",
                &mut instructions.change_review,
                REVIEW_PROMPT,
            ),
            (
                "Reviewing a to-do change (and filling in a new to-do)",
                &mut instructions.proposal_review,
                PROPOSAL_PROMPT,
            ),
            (
                "Planning the rest of the day",
                &mut instructions.day_plan,
                DAY_PLAN_PROMPT,
            ),
        ] {
            egui::CollapsingHeader::new(label)
                .id_salt(("instruction", label))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if own.trim().is_empty() {
                            ui.small("built-in");
                            if ui
                                .small_button("Edit a copy of the built-in text")
                                .clicked()
                            {
                                *own = built_in.to_string();
                            }
                        } else if ui.small_button("Back to the built-in text").clicked() {
                            own.clear();
                        }
                    });
                    if own.trim().is_empty() {
                        ui.add(
                            egui::Label::new(egui::RichText::new(built_in).small().weak()).wrap(),
                        );
                    } else {
                        ui.add(
                            egui::TextEdit::multiline(own)
                                .desired_rows(8)
                                .desired_width(f32::INFINITY),
                        );
                    }
                });
        }
    }

    // --- blocking -----------------------------------------------------------------------

    fn blocking_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot) {
        let Some(drafts) = &mut self.drafts else {
            return;
        };
        ui.heading("Always blocked programs");
        ui.small(
            "Closed whenever they start. Adding one takes effect at once; removing one is \
             scheduled.",
        );
        set_ui(
            ui,
            "blacklist",
            &mut drafts.policy.blacklist,
            &mut self.adding,
            "e.g. steam.exe",
        );

        ui.separator();
        ui.heading("Blocked now");
        ui.small(
            "Unblocking is scheduled. To get something at once, ask for access in the Blocking \
             tab.",
        );
        let mut any = false;
        for (user, value) in &snapshot.blocks {
            let blocks: SavedBlocks = from_value(value);
            for (key, entry) in blocks.processes.iter().chain(blocks.tabs.iter()) {
                any = true;
                let unblocking = drafts.ai_focus.unblocked.contains(&entry.id);
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{user}: {key} — {}", entry.reason));
                    if unblocking {
                        ui.colored_label(YELLOW, "unblock pending");
                        if ui.small_button("keep blocked").clicked() {
                            drafts.ai_focus.unblocked.remove(&entry.id);
                        }
                    } else if !entry.id.is_empty() && ui.small_button("unblock").clicked() {
                        drafts.ai_focus.unblocked.insert(entry.id.clone());
                    }
                });
            }
        }
        if !any {
            ui.label("Nothing.");
        }

        ui.separator();
        ui.heading("Access granted by the AI");
        ui.small("Revoking takes effect at once.");
        let now = Utc::now();
        let mut any = false;
        for (user, value) in &snapshot.grants {
            let grants: GrantState = from_value(value);
            for grant in grants
                .grants
                .iter()
                .filter(|grant| grant.until.is_none_or(|until| now < until))
            {
                any = true;
                let revoked = drafts.ai_focus.revoked_grants.contains(&grant.id);
                ui.horizontal_wrapped(|ui| {
                    let until = grant.until.map_or("permanent".to_string(), |until| {
                        format!("until {}", local(until))
                    });
                    ui.label(format!(
                        "{user}: {} ({until}) — {}",
                        grant.target.label(),
                        grant.purpose
                    ));
                    if revoked {
                        ui.colored_label(RED, "revoked");
                    } else if ui.small_button("revoke").clicked() {
                        drafts.ai_focus.revoked_grants.insert(grant.id.clone());
                    }
                });
            }
        }
        if !any {
            ui.label("None.");
        }

        ui.separator();
        ui.heading("Asking for access");
        let config = &mut drafts.ai_focus;
        ui.horizontal(|ui| {
            ui.label("Longest timed access (minutes)");
            ui.add(egui::DragValue::new(&mut config.max_grant_minutes).range(5..=1440));
        });
        ui.checkbox(
            &mut config.allow_permanent_grants,
            "The AI may grant access until revoked",
        );
    }

    // --- AI checks ----------------------------------------------------------------------

    fn checks_ui(&mut self, ui: &mut egui::Ui) {
        let Some(drafts) = &mut self.drafts else {
            return;
        };
        let config = &mut drafts.ai_focus;
        ui.small("Stricter values take effect at once; looser ones are scheduled.");
        egui::Grid::new("checks").num_columns(2).show(ui, |ui| {
            ui.label("Judge an on-task window again after (minutes, 0 = as often as unsure ones)");
            ui.add(egui::DragValue::new(&mut config.recheck_minutes).range(0..=240));
            ui.end_row();
            ui.label("Judge an unsure window again after (seconds)");
            ui.add(egui::DragValue::new(&mut config.check_interval_secs).range(15..=3600));
            ui.end_row();
            ui.label("Off-task verdicts before an app is closed (tabs: the first)");
            ui.add(egui::DragValue::new(&mut config.strikes_to_block).range(1..=10));
            ui.end_row();
            ui.label("Send a screenshot with each check");
            ui.checkbox(&mut config.send_screenshot, "");
            ui.end_row();
        });
        ui.separator();
        ui.heading("Never judged (always allowed)");
        set_ui(
            ui,
            "always_allow",
            &mut config.always_allow,
            &mut self.adding,
            "e.g. zoom.exe",
        );
        ui.heading("Judged without a screenshot (private)");
        set_ui(
            ui,
            "private_apps",
            &mut config.private_apps,
            &mut self.adding,
            "e.g. bank.exe",
        );
        ui.heading("Browsers (tabs are closed instead of the program)");
        set_ui(
            ui,
            "browsers",
            &mut config.browsers,
            &mut self.adding,
            "e.g. chrome.exe",
        );
    }

    // --- saving -------------------------------------------------------------------------

    /// What saving would do (in force now or scheduled), and the Save button.
    fn save_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        let Some(drafts) = &self.drafts else {
            return;
        };
        let current = drafts.as_values();
        let changed: Vec<usize> = (0..EDITABLE.len())
            .filter(|index| current[*index] != drafts.loaded[*index])
            .collect();
        if changed.is_empty() {
            ui.small("No unsaved changes.");
            return;
        }
        ui.strong("Unsaved changes");
        for index in &changed {
            let (name, _) = EDITABLE[*index];
            // As the agent compares: defaults filled in.
            let in_force = snapshot.files.get(name).cloned().unwrap_or(Value::Null);
            let in_force = machine::settings::normalize(name, &in_force).unwrap_or(in_force);
            for edit in setting_edits::diff(&drafts.loaded[*index], &current[*index]) {
                let (color, when) = if !protected {
                    (GREEN, "now")
                } else {
                    match setting_edits::direction(name, &edit, &in_force) {
                        Direction::Looser => (YELLOW, "scheduled"),
                        _ => (GREEN, "now"),
                    }
                };
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(color, when);
                    ui.label(redact(&edit.describe()));
                });
            }
        }
        let loaded = drafts.loaded.clone();
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                // Only the user's own edits, applied to the files as they are now: another tab
                // (a to-do added in the To-do tab) or another program may have changed them
                // since this tab loaded its copies.
                let fresh: Option<SettingsSnapshot> =
                    call_value(self.session, "settings_get", json!({}))
                        .and_then(|reply| {
                            serde_json::from_value(reply).map_err(|error| error.to_string())
                        })
                        .ok();
                let mut results = Vec::new();
                for index in &changed {
                    let (name, file) = EDITABLE[*index];
                    let user_edits = setting_edits::diff(&loaded[*index], &current[*index]);
                    let mut content = match &fresh {
                        Some(fresh) => file_view(fresh, name),
                        None => loaded[*index].clone(),
                    };
                    setting_edits::apply(&mut content, &user_edits);
                    let reply = call_value(
                        self.session,
                        "settings_put",
                        json!({ "name": name, "content": content, "via": "settings tab" }),
                    )
                    .and_then(|reply| {
                        serde_json::from_value::<PutOutcome>(reply)
                            .map_err(|error| error.to_string())
                    });
                    results.push(match reply {
                        Ok(outcome) => match outcome.scheduled {
                            None => format!("{file}: in force now."),
                            Some(scheduled) => format!(
                                "{file}: {}scheduled; first question {}.",
                                if outcome.applied.is_empty() {
                                    String::new()
                                } else {
                                    format!("{} in force now, the rest ", outcome.applied.len())
                                },
                                local(scheduled.first_question_at)
                            ),
                        },
                        Err(error) => format!("{file} was not saved: {error}"),
                    });
                }
                self.message = results.join(" ");
                self.fetched_at = None;
                self.drafts = None;
            }
            if ui.button("Discard").clicked() {
                self.drafts = None;
            }
        });
    }

    /// The scheduled updates, with the question for each one that is due, and a Withdraw
    /// button. Returns whether one was answered or withdrawn.
    fn reservations_ui(&mut self, ui: &mut egui::Ui) -> bool {
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
            let what = match reservation.origin {
                ReservationOrigin::Settings => file_name(&reservation.name).to_string(),
                ReservationOrigin::TodoChange => "a to-do change".to_string(),
            };
            let every = if reservation.interval_seconds >= 24 * 3600 {
                format!("{} days", reservation.interval_seconds / (24 * 3600))
            } else {
                format!("{} hours", (reservation.interval_seconds / 3600).max(1))
            };
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&what);
                    ui.label(format!(
                        "scheduled {} via {} — {}/{APPROVALS_NEEDED} confirmed, asked every {every}",
                        local(reservation.scheduled_at),
                        reservation.via,
                        reservation.approvals.len()
                    ));
                });
                if !reservation.note.is_empty() {
                    ui.small(&reservation.note);
                }
                egui::CollapsingHeader::new("What changes")
                    // Read before answering.
                    .default_open(reservation.question_due(now))
                    .id_salt(("changes", &reservation.id))
                    .show(ui, |ui| {
                        for line in reservation.describe() {
                            ui.label(redact(&line));
                        }
                    });
                if reservation.question_due(now) {
                    changed |= self.question_ui(ui, reservation, &what);
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
                            self.message = format!("The update of {what} was withdrawn.");
                            changed = true;
                        }
                        Err(error) => self.error = error,
                    }
                }
            });
        }
        changed
    }

    /// "Is this update really right?" with Yes and No. Returns whether it was answered.
    fn question_ui(&mut self, ui: &mut egui::Ui, reservation: &Reservation, what: &str) -> bool {
        ui.colored_label(
            RED,
            format!(
                "Is this update of {what} really right? (question {} of {APPROVALS_NEEDED})",
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
                self.message = format!(
                    "{what}: {approvals}/{APPROVALS_NEEDED} confirmed. Next question: {}.",
                    local(next_question_at)
                )
            }
            Ok(AnswerOutcome::Applied) => {
                self.message = format!("{what}: confirmed {APPROVALS_NEEDED} times; in force now.")
            }
            Ok(AnswerOutcome::Rejected) => {
                self.message = format!("{what}: the update was rejected.")
            }
            Err(error) => self.error = error,
        }
        true
    }
}

/// The file `name` in force with its scheduled settings updates applied (what the user saved
/// last), with defaults filled in.
fn file_view(snapshot: &SettingsSnapshot, name: &str) -> Value {
    let mut file = snapshot.files.get(name).cloned().unwrap_or(Value::Null);
    for reservation in snapshot
        .reservations
        .scheduled
        .iter()
        .filter(|reservation| {
            reservation.name == name && reservation.origin == ReservationOrigin::Settings
        })
    {
        setting_edits::apply(&mut file, &reservation.edits);
    }
    machine::settings::normalize(name, &file).unwrap_or(file)
}

/// Editable copies of the three files as [`file_view`] shows them.
fn drafts_from(snapshot: &SettingsSnapshot) -> Drafts {
    let policy: ListConfig = from_value(&file_view(snapshot, "policy"));
    let ai_focus: AiFocusConfig = from_value(&file_view(snapshot, "ai_focus"));
    let planner: PlannerConfig = from_value(&file_view(snapshot, "planner"));
    let loaded = [to_value(&policy), to_value(&ai_focus), to_value(&planner)];
    Drafts {
        policy,
        ai_focus,
        planner,
        loaded,
    }
}

/// A list of names used as a set, each with a remove button, and a field to add one.
fn set_ui(
    ui: &mut egui::Ui,
    list: &'static str,
    names: &mut BTreeSet<String>,
    adding: &mut HashMap<&'static str, String>,
    hint: &str,
) {
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for name in names.iter() {
            if ui.small_button(format!("{name}  x")).clicked() {
                remove = Some(name.clone());
            }
        }
        if names.is_empty() {
            ui.small("(none)");
        }
    });
    if let Some(name) = remove {
        names.remove(&name);
    }
    let text = adding.entry(list).or_default();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .desired_width(200.0),
        );
        if ui
            .add_enabled(!text.trim().is_empty(), egui::Button::new("Add"))
            .clicked()
        {
            names.insert(text.trim().to_lowercase());
            text.clear();
        }
    });
}

/// Calls and estimated cost today and this month, all users together.
fn usage_ui(ui: &mut egui::Ui, snapshot: &SettingsSnapshot) {
    let today = Local::now().date_naive();
    let (mut today_calls, mut today_cost, mut month_calls, mut month_cost) = (0u64, 0.0, 0u64, 0.0);
    for value in snapshot.usage.values() {
        let log: UsageLog = from_value(value);
        for (date, day) in &log.days {
            if *date == today {
                today_calls += day.total.calls;
                today_cost += day.total.cost_usd;
            }
            if date.year() == today.year() && date.month() == today.month() {
                month_calls += day.total.calls;
                month_cost += day.total.cost_usd;
            }
        }
    }
    ui.label(format!(
        "Today: {today_calls} calls, about ${today_cost:.2} · this month: {month_calls} calls, \
         about ${month_cost:.2}"
    ));
}

/// Hide an API key in an edit's description.
fn redact(line: &str) -> String {
    if line.starts_with("api_key") {
        "api_key = (a new key)".into()
    } else {
        line.to_string()
    }
}

fn from_value<T: DeserializeOwned + Default>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap_or_default()
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
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

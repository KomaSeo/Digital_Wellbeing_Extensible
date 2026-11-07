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
//! (`settings_answer`: the set number of "yes" answers, by default three, put one in force;
//! one "no" rejects it) and can be
//! withdrawn (`settings_withdraw`). The to-do list itself is edited in the To-do tab.
//! Rules: `machine::settings`, `machine::setting_edits`; guide: `docs/multi_account.md` 6.4.

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::usage::{Totals, UsageLog};
use ai_focus_rule::{AiFocusConfig, SavedBlocks};
use chrono::{DateTime, Datelike, Local, Utc};
use list_based_rule::ListConfig;
use machine::lockdown::Lockdown;
use machine::setting_edits::{self, Direction, Edit};
use machine::settings::{
    AnswerOutcome, PutOutcome, Reservation, ReservationOrigin, SettingsSnapshot, EDITABLE,
};
use machine::system_care::{CareStatus, UpdateOutcome};
use machine::user_api::call_value;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use task_planner::{
    Confirmations, PlannerConfig, APPOINTMENT_PROMPT, EXTENSION_PROMPT, PICK_PROMPT,
    PROPOSAL_PROMPT, REVIEW_PROMPT, WEEK_PLAN_PROMPT,
};

const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);
const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
/// How often the settings are fetched again while this tab is shown.
const REFRESH: Duration = Duration::from_secs(3);
/// Edits are saved to the drafts file this long after the last change.
const AUTOSAVE_AFTER: Duration = Duration::from_secs(1);

/// Responsibility: Name the protection the user is choosing (see
/// `machine::settings::ProtectionChoice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProtectKind {
    Until,
    Permanent,
    Administrator,
    Password,
}

/// Responsibility: Name the sections of the Settings tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Connection,
    Rules,
    Blocking,
    Checks,
    /// Program updates and the time zone: what the service does as administrator.
    System,
    /// Turning the protection of the settings on (or off, the chosen way).
    Protection,
}

/// Responsibility: Name one group of settings that is saved, applied, discarded and reset on
/// its own, with the bar of buttons under it.
struct Block {
    id: &'static str,
    /// One of the [`EDITABLE`] names.
    file: &'static str,
    /// The settings in the group, as paths of keys from the file's root.
    paths: &'static [&'static [&'static str]],
    /// The program has a default for these settings ("기본값으로 되돌리기").
    has_default: bool,
}

const API_KEY: Block = Block {
    id: "api_key",
    file: "ai_focus",
    paths: &[&["api_key"]],
    has_default: false,
};
const MODEL: Block = Block {
    id: "model",
    file: "ai_focus",
    paths: &[
        &["model"],
        &["recheck_model"],
        &["check_effort"],
        &["decision_effort"],
    ],
    has_default: true,
};
const BUDGET: Block = Block {
    id: "budget",
    file: "ai_focus",
    paths: &[&["daily_budget_usd"], &["comparison_percent"]],
    has_default: true,
};
const LIFE_RULES: Block = Block {
    id: "life_rules",
    file: "planner",
    paths: &[&["life_rules"]],
    has_default: false,
};
const PICK_INSTRUCTION: Block = Block {
    id: "instruction_pick",
    file: "planner",
    paths: &[&["instructions", "pick"]],
    has_default: true,
};
const REVIEW_INSTRUCTION: Block = Block {
    id: "instruction_change_review",
    file: "planner",
    paths: &[&["instructions", "change_review"]],
    has_default: true,
};
const PROPOSAL_INSTRUCTION: Block = Block {
    id: "instruction_proposal_review",
    file: "planner",
    paths: &[&["instructions", "proposal_review"]],
    has_default: true,
};
const APPOINTMENT_RULES: Block = Block {
    id: "appointment_rules",
    file: "planner",
    paths: &[&["appointment_rules"]],
    has_default: false,
};
const APPOINTMENT_INSTRUCTION: Block = Block {
    id: "instruction_appointment_review",
    file: "planner",
    paths: &[&["instructions", "appointment_review"]],
    has_default: true,
};
const EXTENSION_INSTRUCTION: Block = Block {
    id: "instruction_extension_review",
    file: "planner",
    paths: &[&["instructions", "extension_review"]],
    has_default: true,
};
const WEEK_PLAN_INSTRUCTION: Block = Block {
    id: "instruction_week_plan",
    file: "planner",
    paths: &[&["instructions", "week_plan"]],
    has_default: true,
};
const BLACKLIST: Block = Block {
    id: "blacklist",
    file: "policy",
    paths: &[&["blacklist"]],
    has_default: false,
};
const BLOCKED_SITES: Block = Block {
    id: "blocked_sites",
    file: "ai_focus",
    paths: &[&["always_blocked_sites"]],
    has_default: false,
};
const UNBLOCK: Block = Block {
    id: "unblock",
    file: "ai_focus",
    paths: &[&["unblocked"]],
    has_default: false,
};
const REVOKE: Block = Block {
    id: "revoke",
    file: "ai_focus",
    paths: &[&["revoked_grants"]],
    has_default: false,
};
const ACCESS: Block = Block {
    id: "access",
    file: "ai_focus",
    paths: &[&["max_grant_minutes"], &["allow_permanent_grants"]],
    has_default: true,
};
const CHECKS: Block = Block {
    id: "checks",
    file: "ai_focus",
    paths: &[
        &["recheck_minutes"],
        &["check_interval_secs"],
        &["strikes_to_block"],
        &["send_screenshot"],
    ],
    has_default: true,
};
const ALWAYS_ALLOW: Block = Block {
    id: "always_allow",
    file: "ai_focus",
    paths: &[&["always_allow"]],
    has_default: true,
};
const PRIVATE_APPS: Block = Block {
    id: "private_apps",
    file: "ai_focus",
    paths: &[&["private_apps"]],
    has_default: true,
};
const BROWSERS: Block = Block {
    id: "browsers",
    file: "ai_focus",
    paths: &[&["browsers"]],
    has_default: true,
};
const PROGRAM_UPDATES: Block = Block {
    id: "program_updates",
    file: "ai_focus",
    paths: &[&["updatable_programs"], &["auto_update_programs"]],
    has_default: true,
};
const CONFIRMATIONS: Block = Block {
    id: "confirmations",
    file: "planner",
    paths: &[
        &["confirmations", "settings_times"],
        &["confirmations", "settings_interval_hours"],
        &["confirmations", "todo_change_times"],
        &["confirmations", "todo_change_interval_hours"],
    ],
    has_default: true,
};
const TIME_ZONE: Block = Block {
    id: "time_zone",
    file: "ai_focus",
    paths: &[&["time_zone"]],
    has_default: false,
};
/// Every group; an edit outside them (such as a timestamp) is not the user's.
const BLOCKS: [&Block; 23] = [
    &API_KEY,
    &MODEL,
    &BUDGET,
    &LIFE_RULES,
    &APPOINTMENT_RULES,
    &APPOINTMENT_INSTRUCTION,
    &EXTENSION_INSTRUCTION,
    &PICK_INSTRUCTION,
    &REVIEW_INSTRUCTION,
    &PROPOSAL_INSTRUCTION,
    &WEEK_PLAN_INSTRUCTION,
    &BLACKLIST,
    &BLOCKED_SITES,
    &UNBLOCK,
    &REVOKE,
    &ACCESS,
    &CHECKS,
    &ALWAYS_ALLOW,
    &PRIVATE_APPS,
    &BROWSERS,
    &PROGRAM_UPDATES,
    &CONFIRMATIONS,
    &TIME_ZONE,
];

/// The user's edits not yet applied, by file name: what the drafts file keeps.
type PendingEdits = BTreeMap<String, Vec<Edit>>;

/// Responsibility: Name what a button in a group's bar asks for.
#[derive(Clone, Copy)]
enum BarAction {
    Save,
    Apply,
    Discard,
    Default,
}

/// Responsibility: Hold the Settings tab: the settings last fetched, the copies being edited,
/// the drafts file, and the form inputs.
pub struct SettingsTab {
    session: u32,
    error: String,
    confirm_protect: bool,
    /// The sync device whose administrator may end an administrator protection (`None`: any).
    protect_admin_device: Option<String>,
    /// The choice above was set (to this computer, at first).
    protect_admin_device_chosen: bool,
    /// The protection the user is choosing, and its length or password.
    protect_kind: ProtectKind,
    protect_days: u32,
    protect_hours: u32,
    protect_password: String,
    protect_password_again: String,
    /// The password typed to turn a password protection off.
    unlock_password: String,
    /// The last `settings_get` answer.
    latest: Option<SettingsSnapshot>,
    fetched_at: Option<Instant>,
    /// What the last answer to a scheduled update did.
    message: String,
    /// What the last button of a group did, by group.
    block_messages: HashMap<&'static str, String>,
    section: Section,
    /// Copies being edited; `None` until the first fetch.
    drafts: Option<Drafts>,
    /// The edits the drafts file holds now.
    persisted: PendingEdits,
    /// When the edits first differed from the drafts file.
    changed_at: Option<Instant>,
    /// When the drafts file was last written, and whether by the 저장 button.
    saved_at: Option<(DateTime<Local>, bool)>,
    show_key: bool,
    new_key: String,
    /// Result of the last "키 확인", written by a background thread.
    key_check: Arc<Mutex<String>>,
    /// Text typed into the "add" fields of the lists, by list.
    adding: HashMap<&'static str, String>,
    /// Settings whose edits take effect at once during a lockdown (the API key).
    fix_now: Vec<Vec<String>>,
    /// The last `system_care_status` answer, and when it was fetched.
    care: Option<Result<CareStatus, String>>,
    care_fetched_at: Option<Instant>,
    /// What the last "지금 업데이트" did.
    care_message: String,
    /// The last `self_update_status` answer (this program's own update).
    self_update: Option<Result<machine::self_update::UpdateStatus, String>>,
    /// What the last "업데이트 확인" / "지금 업데이트" request did.
    self_update_message: String,
    /// "지금 업데이트" was pressed once: the next press installs.
    confirming_install: bool,
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

    fn set_values(&mut self, values: [Value; 3]) {
        let [policy, ai_focus, planner] = values;
        self.policy = from_value(&policy);
        self.ai_focus = from_value(&ai_focus);
        self.planner = from_value(&planner);
    }
}

impl SettingsTab {
    pub fn new(session: u32) -> Self {
        Self {
            session,
            error: String::new(),
            confirm_protect: false,
            protect_admin_device: None,
            protect_admin_device_chosen: false,
            protect_kind: ProtectKind::Until,
            protect_days: 7,
            protect_hours: 0,
            protect_password: String::new(),
            protect_password_again: String::new(),
            unlock_password: String::new(),
            latest: None,
            fetched_at: None,
            message: String::new(),
            block_messages: HashMap::new(),
            section: Section::Connection,
            drafts: None,
            persisted: PendingEdits::new(),
            changed_at: None,
            saved_at: None,
            show_key: false,
            new_key: String::new(),
            key_check: Arc::new(Mutex::new(String::new())),
            adding: HashMap::new(),
            fix_now: Vec::new(),
            care: None,
            care_fetched_at: None,
            care_message: String::new(),
            self_update: None,
            self_update_message: String::new(),
            confirming_install: false,
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        protected: bool,
        protection: Option<&machine::user_api::ProtectionView>,
        lockdown: Option<&Lockdown>,
        sync: Option<&machine::user_api::SyncStatus>,
    ) {
        self.fix_now = lockdown
            .map(|lockdown| lockdown.fix_now.clone())
            .unwrap_or_default();
        self.refresh();
        sync_ui(ui, sync, self.session, &mut self.message);
        if self.reservations_ui(ui) {
            // Answered or withdrawn: the drafts are rebased on what is now true.
            self.fetched_at = None;
            self.refresh();
        }
        if !self.message.is_empty() {
            ui.label(&self.message);
        }
        if !self.error.is_empty() {
            ui.colored_label(RED, &self.error);
        }
        let Some(snapshot) = self.latest.clone() else {
            ui.label("agent에 연결하는 중…");
            return;
        };
        if self.drafts.is_none() {
            let saved = read_drafts_file();
            self.drafts = Some(drafts_from(&snapshot, &saved));
            self.persisted = saved;
        }

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            for (section, label) in [
                (Section::Connection, "AI 연결"),
                (Section::Rules, "AI 규칙"),
                (Section::Blocking, "차단"),
                (Section::Checks, "AI 검사"),
                (Section::System, "시스템"),
                (Section::Protection, "엄격 모드"),
            ] {
                ui.selectable_value(&mut self.section, section, label);
            }
        });
        ui.small(
            "고친 내용은 자동으로 임시 저장됩니다. 각 항목 아래의 \"적용\"을 눌러야 설정에 \
             반영됩니다.",
        );
        match self.section {
            Section::Connection => self.connection_ui(ui, &snapshot, protected),
            Section::Rules => self.rules_ui(ui, &snapshot, protected),
            Section::Blocking => self.blocking_ui(ui, &snapshot, protected),
            Section::Checks => self.checks_ui(ui, &snapshot, protected),
            Section::System => self.system_ui(ui, &snapshot, protected),
            Section::Protection => self.protection_ui(ui, protected, protection, sync),
        }
        self.autosave(ui.ctx());
    }

    /// Show the protection's part of the tab next (from a link elsewhere in the app).
    pub(crate) fn show_protection(&mut self) {
        self.section = Section::Protection;
    }

    /// The protection of the settings: how it works now and how it ends, or turning it on.
    fn protection_ui(
        &mut self,
        ui: &mut egui::Ui,
        protected: bool,
        protection: Option<&machine::user_api::ProtectionView>,
        sync: Option<&machine::user_api::SyncStatus>,
    ) {
        crate::style::section(ui, "엄격 모드");
        if protected {
            let numbers = self.numbers_in_force();
            let every = interval_text(numbers.settings_interval_seconds());
            ui.label(format!(
                "엄격 모드가 켜져 있습니다. 규칙을 더 엄격하게 만드는 변경은 바로 적용됩니다. 그 밖의 \
                 변경은 예약되고, {every} 뒤부터 \"이 업데이트가 정말 맞나요?\"라는 질문이 옵니다. \
                 {every} 간격으로 {}번 연속 \"예\"라고 답하면 적용되고, 한 번이라도 \"아니요\"라고 \
                 답하면 거절됩니다. 이 횟수와 간격은 \"시스템\"의 \"확인 횟수와 간격\"에서 바꿉니다.",
                numbers.settings_times()
            ));
            self.unlock_ui(ui, protection, sync);
        } else {
            self.protect_ui(ui, sync);
        }
    }

    /// The confirmation numbers in force (the defaults until the settings are fetched).
    pub(crate) fn numbers_in_force(&self) -> Confirmations {
        self.latest
            .as_ref()
            .map(|snapshot| {
                from_value::<PlannerConfig>(snapshot.files.get("planner").unwrap_or(&Value::Null))
                    .confirmations
            })
            .unwrap_or_default()
    }

    /// The settings as last fetched (fetched again when due), for the Statistics tab.
    pub(crate) fn latest_snapshot(&mut self) -> Option<SettingsSnapshot> {
        self.refresh();
        self.latest.clone()
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
                // The files changed (an update applied, a to-do added, another program): go on
                // from what is true now, keeping the user's own edits.
                if let Some(drafts) = &self.drafts {
                    if without_timestamps(loaded_values(&snapshot))
                        != without_timestamps(drafts.loaded.clone())
                    {
                        let edits = user_edits(drafts, true);
                        self.drafts = Some(drafts_from(&snapshot, &edits));
                    }
                }
                self.latest = Some(snapshot);
                self.error.clear();
            }
            Err(error) => self.error = error,
        }
    }

    /// Setup: changes take effect at once until the user protects the settings.
    fn protect_ui(&mut self, ui: &mut egui::Ui, sync: Option<&machine::user_api::SyncStatus>) {
        crate::style::notice(
            ui,
            crate::style::Tone::Warning,
            "설정 단계(엄격 모드를 켜기 전): 변경이 바로 적용되고, 이 PC를 쓰는 누구나 설정을 바꿀 수 \
             있습니다.",
        );
        let numbers = self.numbers_in_force();
        ui.label(format!(
            "모두 정했으면 엄격 모드를 켜세요. 그 뒤로는 규칙을 더 엄격하게 만드는 변경만 바로 \
             적용되고, 그 밖의 변경은 {} 간격으로 {}번 확인해야 적용됩니다(횟수와 간격은 \
             \"시스템\"의 \"확인 횟수와 간격\"에서 바꿉니다). 엄격 모드를 끄면 설정 단계로 돌아가고, \
             걸린 차단도 모두 풀립니다.",
            interval_text(numbers.settings_interval_seconds()),
            numbers.settings_times()
        ));
        ui.label("엄격 모드를 어떻게 끌 수 있게 할지 고르세요.");
        ui.radio_value(
            &mut self.protect_kind,
            ProtectKind::Until,
            "기간: 정한 시간이 지나기 전에는 끌 수 없습니다",
        );
        if self.protect_kind == ProtectKind::Until {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.add(egui::DragValue::new(&mut self.protect_days).range(0..=365));
                ui.label("일");
                ui.add(egui::DragValue::new(&mut self.protect_hours).range(0..=23));
                ui.label("시간 동안");
            });
            let until = self.protect_until();
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.small(format!(
                    "{}까지 끌 수 없습니다.",
                    until.with_timezone(&Local).format("%Y-%m-%d %H:%M")
                ));
            });
        }
        ui.radio_value(
            &mut self.protect_kind,
            ProtectKind::Permanent,
            "영구: 프로그램 안에서는 끌 수 없습니다",
        );
        ui.radio_value(
            &mut self.protect_kind,
            ProtectKind::Administrator,
            "관리자 권한: Windows가 관리자 승인을 물을 때 승인할 수 있어야 끌 수 있습니다",
        );
        // With sync: whose administrator. This computer by default, since the one choosing
        // is here; any computer's administrator is a choice too.
        if let (ProtectKind::Administrator, Some(sync)) = (self.protect_kind, sync) {
            if !self.protect_admin_device_chosen {
                self.protect_admin_device = Some(sync.device_id.clone());
                self.protect_admin_device_chosen = true;
            }
            let name_of = |id: &str| {
                sync.devices
                    .iter()
                    .find(|(device_id, _)| device_id == id)
                    .map(|(_, name)| format!("{name} ({id})"))
                    .unwrap_or_else(|| id.to_string())
            };
            let shown = match &self.protect_admin_device {
                Some(id) => name_of(id),
                None => "아무 컴퓨터".to_string(),
            };
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label("어느 컴퓨터의 관리자가 끌 수 있나");
                egui::ComboBox::from_id_salt("protect_admin_device")
                    .selected_text(shown)
                    .show_ui(ui, |ui| {
                        let mut ids: Vec<String> =
                            sync.devices.iter().map(|(id, _)| id.clone()).collect();
                        if !ids.contains(&sync.device_id) {
                            ids.insert(0, sync.device_id.clone());
                        }
                        for id in ids {
                            let label = if id == sync.device_id {
                                format!("{} — 이 컴퓨터", name_of(&id))
                            } else {
                                name_of(&id)
                            };
                            ui.selectable_value(&mut self.protect_admin_device, Some(id), label);
                        }
                        ui.selectable_value(
                            &mut self.protect_admin_device,
                            None,
                            "아무 컴퓨터(동기화하는 어느 컴퓨터든 그 컴퓨터의 관리자)",
                        );
                    });
            });
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.small(
                    "고른 컴퓨터에서만, 그 컴퓨터의 관리자 승인으로 끌 수 있습니다. 끄면 모든 \
                     컴퓨터에서 꺼집니다. 그 컴퓨터를 동기화에서 빼면 엄격 모드를 끌 수 없게 \
                     됩니다.",
                );
            });
        }
        ui.radio_value(
            &mut self.protect_kind,
            ProtectKind::Password,
            "비밀번호: 지금 정하는 비밀번호를 넣어야 끌 수 있습니다",
        );
        if self.protect_kind == ProtectKind::Password {
            egui::Grid::new("protect_password").show(ui, |ui| {
                ui.label("비밀번호");
                ui.add(egui::TextEdit::singleline(&mut self.protect_password).password(true));
                ui.end_row();
                ui.label("한 번 더");
                ui.add(egui::TextEdit::singleline(&mut self.protect_password_again).password(true));
                ui.end_row();
            });
            ui.small(
                "다른 사람에게 비밀번호를 정하고 갖고 있게 하면, 스스로는 끌 수 없게 됩니다. \
                 비밀번호는 되돌릴 수 없는 값(해시)으로만 저장됩니다.",
            );
        }
        let mode = match self.protect_kind {
            ProtectKind::Until => Some(json!({ "kind": "until", "until": self.protect_until() })),
            ProtectKind::Permanent => Some(json!({ "kind": "permanent" })),
            ProtectKind::Administrator => Some(json!({
                "kind": "administrator",
                "device": sync.and(self.protect_admin_device.clone()),
            })),
            ProtectKind::Password => {
                let password = self.protect_password.clone();
                (password.chars().count() >= 4 && password == self.protect_password_again)
                    .then(|| json!({ "kind": "password", "password": password }))
            }
        };
        if self.protect_kind == ProtectKind::Password && mode.is_none() {
            ui.small("비밀번호는 4글자 이상이고, 두 칸이 같아야 합니다.");
        }
        ui.checkbox(
            &mut self.confirm_protect,
            "고른 방법이 아니면 엄격 모드를 끌 수 없다는 것을 이해했습니다.",
        );
        if ui
            .add_enabled(
                self.confirm_protect && mode.is_some(),
                crate::style::primary("엄격 모드 켜기", self.confirm_protect && mode.is_some()),
            )
            .clicked()
        {
            match call_value(self.session, "settings_protect", json!({ "mode": mode })) {
                Ok(_) => {
                    self.confirm_protect = false;
                    self.protect_password.clear();
                    self.protect_password_again.clear();
                    self.fetched_at = None;
                }
                Err(error) => self.error = error,
            }
        }
    }

    /// The end of a time protection chosen now.
    fn protect_until(&self) -> DateTime<Utc> {
        let hours = i64::from(self.protect_days) * 24 + i64::from(self.protect_hours);
        Utc::now() + chrono::Duration::hours(hours.max(1))
    }

    /// How the protection in force may end, and the way to end it.
    fn unlock_ui(
        &mut self,
        ui: &mut egui::Ui,
        protection: Option<&machine::user_api::ProtectionView>,
        sync: Option<&machine::user_api::SyncStatus>,
    ) {
        let Some(protection) = protection else {
            return;
        };
        let mut unlock: Option<Value> = None;
        match protection.kind.as_str() {
            "until" => {
                let until = protection.until.unwrap_or_else(Utc::now);
                if Utc::now() < until {
                    ui.label(format!(
                        "기간을 정한 엄격 모드입니다. {}까지는 끌 수 없습니다.",
                        until.with_timezone(&Local).format("%Y-%m-%d %H:%M")
                    ));
                } else if ui.button("기간이 끝났습니다: 엄격 모드 끄기").clicked() {
                    unlock = Some(json!({}));
                }
            }
            "administrator" => {
                let this_device = sync.map(|sync| sync.device_id.as_str());
                let elsewhere = protection
                    .device
                    .as_deref()
                    .filter(|required| Some(*required) != this_device);
                if let Some(required) = elsewhere {
                    let name = sync
                        .and_then(|sync| {
                            sync.devices
                                .iter()
                                .find(|(device_id, _)| device_id == required)
                                .map(|(_, name)| name.clone())
                        })
                        .unwrap_or_else(|| required.to_string());
                    ui.label(format!(
                        "관리자 권한으로 끄는 엄격 모드입니다. \"{name}\" 컴퓨터에서, 그 컴퓨터의 관리자 \
                         승인으로만 끌 수 있습니다. 거기서 끄면 이 컴퓨터에서도 꺼집니다."
                    ));
                } else if protection.device.is_some() {
                    ui.label(
                        "관리자 권한으로 끄는 엄격 모드입니다. 이 컴퓨터에서 Windows의 관리자 승인 \
                         창에서 승인해야 꺼집니다. 끄면 모든 컴퓨터에서 꺼집니다.",
                    );
                } else {
                    ui.label(
                        "관리자 권한으로 끄는 엄격 모드입니다. Windows의 관리자 승인 창에서 승인해야 \
                         꺼집니다.",
                    );
                }
                if elsewhere.is_none() && ui.button("관리자 권한으로 엄격 모드 끄기").clicked()
                {
                    match crate::elevate::run_settings_unlock() {
                        Ok(()) => {
                            self.message =
                                "관리자 승인 창에서 승인하면 몇 초 안에 엄격 모드가 꺼집니다."
                                    .into();
                            self.fetched_at = None;
                        }
                        Err(error) => self.error = error,
                    }
                }
            }
            "password" => {
                ui.label("비밀번호로 끄는 엄격 모드입니다. 엄격 모드를 켤 때 정한 비밀번호를 넣으면 꺼집니다.");
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.unlock_password).password(true));
                    if ui.button("엄격 모드 끄기").clicked() {
                        unlock = Some(json!({ "password": self.unlock_password }));
                    }
                });
            }
            _ => {
                ui.label("영구 엄격 모드입니다. 프로그램 안에서는 끌 수 없습니다.");
            }
        }
        if protection.kind != "permanent" {
            ui.small("엄격 모드를 끄면 설정 단계로 돌아가고, 걸린 차단도 모두 풀립니다.");
        }
        if let Some(params) = unlock {
            match call_value(self.session, "settings_unprotect", params) {
                Ok(_) => {
                    self.unlock_password.clear();
                    self.message = "엄격 모드를 껐습니다. 걸린 차단도 모두 풀었습니다.".into();
                    self.fetched_at = None;
                }
                Err(error) => self.error = error,
            }
        }
    }

    // --- AI connection ------------------------------------------------------------------

    fn connection_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        crate::style::section(ui, "Anthropic API 키");
        if let Some(drafts) = &mut self.drafts {
            let config = &mut drafts.ai_focus;
            let saved = config.api_key.trim();
            if saved.is_empty() {
                crate::style::notice(
                    ui,
                    crate::style::Tone::Error,
                    "키가 없습니다. AI가 아무것도 판정하거나 고르지 못합니다.",
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
                ui.label(format!("키가 있습니다(…{tail})."));
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.new_key)
                        .password(!self.show_key)
                        .hint_text("새 키를 붙여 넣으세요")
                        .desired_width(300.0),
                );
                ui.checkbox(&mut self.show_key, "보이기");
            });
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.new_key.trim().is_empty(),
                        egui::Button::new("이 키 쓰기"),
                    )
                    .clicked()
                {
                    config.api_key = self.new_key.trim().to_string();
                    self.new_key.clear();
                }
                if ui
                    .add_enabled(!config.api_key.is_empty(), egui::Button::new("키 지우기"))
                    .clicked()
                {
                    config.api_key.clear();
                }
                if ui
                    .add_enabled(!config.api_key.is_empty(), egui::Button::new("키 확인"))
                    .clicked()
                {
                    let (key, model, result) = (
                        config.api_key.clone(),
                        config.model.clone(),
                        Arc::clone(&self.key_check),
                    );
                    *result.lock() = "확인하는 중…".into();
                    thread::spawn(move || {
                        let answer =
                            match ai_focus_rule::claude::ClaudeClient::new(key, model).check() {
                                Ok(()) => "✔ 키가 동작합니다(모델 목록만 읽었으므로 비용이 들지 \
                                           않았습니다)."
                                    .into(),
                                Err(error) => format!("키가 받아들여지지 않았습니다: {error}"),
                            };
                        *result.lock() = answer;
                    });
                }
            });
            let check = self.key_check.lock().clone();
            if !check.is_empty() {
                ui.small(check);
            }
            ui.small("키는 자동 임시 저장에 남기지 않습니다. \"이 키 쓰기\" 뒤에 바로 \"적용\"을 누르세요.");
        }
        self.block_bar(ui, &API_KEY, snapshot, protected);

        crate::style::section(ui, "모델");
        if let Some(drafts) = &mut self.drafts {
            // A choice, not a typed name: no typos, no made-up model. The agent refreshes the
            // list from the Models API once a week, and checks a new model before applying.
            let catalog = &snapshot.models;
            let label_of = |model_id: &str| {
                catalog
                    .models
                    .iter()
                    .find(|model| model.id == model_id)
                    .filter(|model| !model.display_name.is_empty())
                    .map_or(model_id.to_string(), |model| {
                        format!("{} ({})", model.display_name, model.id)
                    })
            };
            let current = drafts.ai_focus.model.clone();
            ui.horizontal(|ui| {
                ui.label("기본 모델");
                egui::ComboBox::from_id_salt("model_choice")
                    .width(320.0)
                    .selected_text(label_of(&current))
                    .show_ui(ui, |ui| {
                        for model_id in catalog.choices(&current) {
                            let label = label_of(&model_id);
                            ui.selectable_value(&mut drafts.ai_focus.model, model_id, label);
                        }
                    });
            });
            // Rechecks of windows already judged: the cheaper model first; a changed verdict
            // goes back to the main model.
            let current_recheck = drafts.ai_focus.recheck_model.clone();
            ui.horizontal(|ui| {
                ui.label("다시 검사할 때 먼저 쓰는 모델");
                egui::ComboBox::from_id_salt("recheck_model_choice")
                    .width(320.0)
                    .selected_text(if current_recheck.is_empty() {
                        "쓰지 않음(기본 모델이 모두 검사)".to_string()
                    } else {
                        label_of(&current_recheck)
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut drafts.ai_focus.recheck_model,
                            String::new(),
                            "쓰지 않음(기본 모델이 모두 검사)",
                        );
                        for model_id in catalog.choices(&current_recheck) {
                            let label = label_of(&model_id);
                            ui.selectable_value(
                                &mut drafts.ai_focus.recheck_model,
                                model_id,
                                label,
                            );
                        }
                    });
            });
            // How much the AI thinks: more thinking judges better, costs more and takes longer.
            let effort_label = |level: &str| match level {
                "low" => "low(가장 적게, 빠르고 쌈)".to_string(),
                "medium" => "medium(보통)".to_string(),
                "high" => "high(많이)".to_string(),
                "xhigh" => "xhigh(아주 많이)".to_string(),
                "max" => "max(최대, 가장 느리고 비쌈)".to_string(),
                other => other.to_string(),
            };
            for (label, value, salt) in [
                (
                    "창 검사의 노력",
                    &mut drafts.ai_focus.check_effort,
                    "check_effort_choice",
                ),
                (
                    "그 밖의 판단의 노력(할 일 고르기와 심사, 접근 요청, AI 비서)",
                    &mut drafts.ai_focus.decision_effort,
                    "decision_effort_choice",
                ),
            ] {
                ui.horizontal(|ui| {
                    ui.label(label);
                    egui::ComboBox::from_id_salt(salt)
                        .selected_text(effort_label(value))
                        .show_ui(ui, |ui| {
                            for level in ai_focus_rule::EFFORT_LEVELS {
                                ui.selectable_value(value, level.to_string(), effort_label(level));
                            }
                        });
                });
            }
            ui.small(
                "노력은 AI가 답하기 전에 얼마나 생각할지입니다. 높을수록 판단이 꼼꼼해지지만, 생각한 \
                 만큼 출력 토큰 비용이 들고 답이 늦어집니다. 창 검사는 자주 일어나므로 낮게 두는 편이 \
                 비용에 좋습니다.",
            );
            ui.small(
                "이미 판정한 창을 다시 검사할 때는 이 모델이 먼저 봅니다. 판정이 지난번과 같으면 그대로 \
                 두고, 바뀌면 기본 모델이 다시 확인해서 그 판정을 따릅니다. 처음 보는 창은 언제나 기본 \
                 모델이 검사합니다.",
            );
            match (catalog.fetched_at, catalog.error.is_empty()) {
                (Some(fetched_at), _) => {
                    ui.small(format!(
                        "모델 목록은 Anthropic의 모델 API에서 {}에 받았습니다. 일주일마다 다시 받습니다. \
                         적용하면 그 모델이 있는지 먼저 확인합니다.",
                        fetched_at.with_timezone(&chrono::Local).format("%Y-%m-%d")
                    ));
                }
                (None, true) => {
                    ui.small(
                        "아직 모델 목록을 받지 못해서 기본 목록을 보여 줍니다. API 키가 동작하면 몇 분 안에 \
                         목록을 받습니다.",
                    );
                }
                (None, false) => {
                    ui.small(format!(
                        "모델 목록을 받지 못해서 기본 목록을 보여 줍니다({}).",
                        catalog.error
                    ));
                }
            }
        }
        self.block_bar(ui, &MODEL, snapshot, protected);

        crate::style::section(ui, "AI 하루 예산");
        if let Some(drafts) = &mut self.drafts {
            ui.horizontal(|ui| {
                ui.label("AI 검사의 하루 예산(USD, 0 = 제한 없음)");
                ui.add(
                    egui::DragValue::new(&mut drafts.ai_focus.daily_budget_usd)
                        .speed(0.05)
                        .range(0.0..=1000.0),
                );
            });
            ui.horizontal(|ui| {
                ui.label("모델 비교: 세 모델에게 따로 물어볼 질문의 비율(%, 0 = 끔)");
                ui.add(
                    egui::DragValue::new(&mut drafts.ai_focus.comparison_percent)
                        .range(0..=100)
                        .suffix("%"),
                );
            });
            ui.small(
                "모델 비교는 창 검사, 변경 요청 심사, 할 일 고르기, 할 일 변경 심사, AI 비서의 질문 중 \
                 이 비율만큼을 Opus, Sonnet, Haiku에게 한 번씩 똑같이 물어 답을 비교합니다. 비교하는 답은 \
                 아무것도 바꾸지 않고, 결과는 통계 탭의 \"모델 비교\"에 나옵니다. 그 비용도 AI 사용량에 \
                 들어갑니다.",
            );
        }
        self.block_bar(ui, &BUDGET, snapshot, protected);
        ui.small("지금까지 쓴 AI 사용량은 통계 탭에 있습니다.");
    }

    // --- rules for the AI ---------------------------------------------------------------

    fn rules_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        crate::style::section(ui, "생활 규칙");
        ui.label(
            "AI의 모든 결정(지금 할 일 고르기, 변경 요청 심사, 할 일 변경 심사, 앞으로 7일 계획)이 따르는 \
             규칙입니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            ui.add(
                egui::TextEdit::multiline(&mut drafts.planner.life_rules)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .hint_text(
                        "예: 평일 18시 전에는 게임과 영상을 할당하지 않는다. 2시간 일하면 20분 쉰다.",
                    ),
            );
        }
        self.block_bar(ui, &LIFE_RULES, snapshot, protected);

        crate::style::section(ui, "약속 규칙");
        ui.label(
            "약속은 정해진 시간에 하던 일을 밀어내고, 그동안 다른 일을 받지 않습니다. 그래서 일을 피하는 데 \
             쓰일 수 있습니다. 사용자가 약속을 추가하면 AI가 이 규칙과 생활 규칙에 따라 받을지 정합니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            ui.add(
                egui::TextEdit::multiline(&mut drafts.planner.appointment_rules)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY)
                    .hint_text(
                        "예: 게임이나 영상 보기는 약속으로 받지 않는다. 평일 18시 전의 약속은 수업, 병원, \
                         일 관련만 받는다. 약속 하나는 3시간을 넘지 않는다.",
                    ),
            );
        }
        self.block_bar(ui, &APPOINTMENT_RULES, snapshot, protected);

        crate::style::section(ui, "AI 지시문");
        ui.label(
            "플래너의 AI에게 무엇을 어떤 기준으로 판단하라고 알려 주는 글입니다. 정확도를 위해 \
             영어로 씁니다. 비어 있으면 프로그램에 들어 있는 기본 지시문을 씁니다.",
        );
        type Accessor = fn(&mut PlannerConfig) -> &mut String;
        let instructions: [(&str, Accessor, &str, &'static Block); 6] = [
            (
                "지금 할 일과 시간 고르기",
                |planner| &mut planner.instructions.pick,
                PICK_PROMPT,
                &PICK_INSTRUCTION,
            ),
            (
                "하는 일을 바꾸는 요청 심사",
                |planner| &mut planner.instructions.change_review,
                REVIEW_PROMPT,
                &REVIEW_INSTRUCTION,
            ),
            (
                "할 일 변경 심사(새 할 일의 빈 칸 채우기 포함)",
                |planner| &mut planner.instructions.proposal_review,
                PROPOSAL_PROMPT,
                &PROPOSAL_INSTRUCTION,
            ),
            (
                "앞으로 7일 계획(날마다 일에 써야 할 시간과 여유)",
                |planner| &mut planner.instructions.week_plan,
                WEEK_PLAN_PROMPT,
                &WEEK_PLAN_INSTRUCTION,
            ),
            (
                "약속을 받을지 심사",
                |planner| &mut planner.instructions.appointment_review,
                APPOINTMENT_PROMPT,
                &APPOINTMENT_INSTRUCTION,
            ),
            (
                "할 일 연장 요청 심사",
                |planner| &mut planner.instructions.extension_review,
                EXTENSION_PROMPT,
                &EXTENSION_INSTRUCTION,
            ),
        ];
        for (label, accessor, built_in, block) in instructions {
            egui::CollapsingHeader::new(label)
                .id_salt(("instruction", block.id))
                .show(ui, |ui| {
                    if let Some(drafts) = &mut self.drafts {
                        let own = accessor(&mut drafts.planner);
                        if own.trim().is_empty() {
                            ui.horizontal(|ui| {
                                ui.small("기본 지시문을 씁니다.");
                                if ui.small_button("기본 지시문을 복사해서 고치기").clicked()
                                {
                                    *own = built_in.to_string();
                                }
                            });
                            ui.add(
                                egui::Label::new(egui::RichText::new(built_in).small().weak())
                                    .wrap(),
                            );
                        } else {
                            ui.add(
                                egui::TextEdit::multiline(own)
                                    .desired_rows(10)
                                    .desired_width(f32::INFINITY),
                            );
                        }
                    }
                    self.block_bar(ui, block, snapshot, protected);
                });
        }
    }

    // --- blocking -----------------------------------------------------------------------

    fn blocking_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        crate::style::section(ui, "항상 막을 프로그램");
        ui.small(
            "목록의 프로그램은 켜질 때마다 꺼집니다. 이름은 정규식으로도 적을 수 있습니다(대소문자 \
             무시, 이름 전체가 맞아야 함). 예: steam.exe, .*game.*\\.exe. 넣기는 바로 적용되고, \
             빼기는 예약됩니다. Windows가 돌아가는 데 꼭 필요한 프로그램은 정규식에 걸려도 끄지 \
             않습니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            set_ui(
                ui,
                "blacklist",
                &mut drafts.policy.blacklist,
                &mut self.adding,
                "예: steam.exe",
            );
        }
        self.block_bar(ui, &BLACKLIST, snapshot, protected);

        crate::style::section(ui, "항상 막을 웹사이트");
        ui.small(
            "브라우저 탭의 제목에 이 말이 들어 있으면, 데일리 웰빙이 꺼져 있어도 AI에게 묻지 않고 \
             그 탭을 닫습니다. 정규식으로도 적을 수 있습니다(대소문자 무시, 제목의 일부와 맞으면 \
             됨). 예: youtube, reddit|twitter. 넣기는 바로 적용되고, 빼기는 예약됩니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            set_ui(
                ui,
                "always_blocked_sites",
                &mut drafts.ai_focus.always_blocked_sites,
                &mut self.adding,
                "예: youtube",
            );
        }
        self.block_bar(ui, &BLOCKED_SITES, snapshot, protected);

        crate::style::section(ui, "걸린 차단");
        ui.small(
            "차단은 그 차단이 생긴 할 일에 붙어 있어서, 그 할 일을 받았을 때만 적용됩니다. 차단 풀기는 \
             예약됩니다. 바로 필요하면 차단 탭에서 접근을 요청하세요.",
        );
        // A to-do's title from its id, for the blocks of each to-do.
        let titles: HashMap<String, String> = snapshot
            .files
            .get("planner")
            .and_then(|planner| planner["todos"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|todo| {
                Some((
                    todo["id"].as_str()?.to_string(),
                    todo["title"].as_str()?.to_string(),
                ))
            })
            .collect();
        let scope_label = |scope: &str| match scope {
            "" => "할 일 없이".to_string(),
            "free" => "목록 밖의 일".to_string(),
            todo_id => format!(
                "할 일 \"{}\"",
                titles.get(todo_id).map_or(todo_id, String::as_str)
            ),
        };
        if let Some(drafts) = &mut self.drafts {
            let mut any = false;
            for (user, value) in &snapshot.blocks {
                let blocks: SavedBlocks = from_value(value);
                let current = blocks
                    .processes
                    .iter()
                    .chain(blocks.tabs.iter())
                    .map(|(key, entry)| (blocks.scope.clone(), key, entry));
                let others = blocks.other_scopes.iter().flat_map(|(scope, set)| {
                    set.processes
                        .iter()
                        .chain(set.tabs.iter())
                        .map(move |(key, entry)| (scope.clone(), key, entry))
                });
                // Parsed anew every frame into hash maps, whose order changes each time:
                // sort, so the list stays still (the current to-do's blocks first).
                let mut rows: Vec<_> = current.chain(others).collect();
                rows.sort_by(|left, right| {
                    (left.0 != blocks.scope, &left.0, left.1).cmp(&(
                        right.0 != blocks.scope,
                        &right.0,
                        right.1,
                    ))
                });
                for (scope, key, entry) in rows {
                    any = true;
                    let unblocking = drafts.ai_focus.unblocked.contains(&entry.id);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!(
                            "{user} · {}: {key} — {}",
                            scope_label(&scope),
                            entry.reason
                        ));
                        if unblocking {
                            ui.colored_label(YELLOW, "풀기 대기 중");
                            if ui.small_button("계속 막기").clicked() {
                                drafts.ai_focus.unblocked.remove(&entry.id);
                            }
                        } else if !entry.id.is_empty() && ui.small_button("차단 풀기").clicked()
                        {
                            drafts.ai_focus.unblocked.insert(entry.id.clone());
                        }
                    });
                }
            }
            if !any {
                ui.label("없습니다.");
            }
        }
        self.block_bar(ui, &UNBLOCK, snapshot, protected);

        crate::style::section(ui, "AI가 준 접근 허용");
        ui.small("허용 취소는 바로 적용됩니다.");
        if let Some(drafts) = &mut self.drafts {
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
                        let until = grant.until.map_or("취소할 때까지".to_string(), |until| {
                            format!("{}까지", local(until))
                        });
                        ui.label(format!(
                            "{user}: {} ({until}) — {}",
                            grant.target.label(),
                            grant.purpose
                        ));
                        if revoked {
                            ui.colored_label(RED, "취소됨");
                        } else if ui.small_button("허용 취소").clicked() {
                            drafts.ai_focus.revoked_grants.insert(grant.id.clone());
                        }
                    });
                }
            }
            if !any {
                ui.label("없습니다.");
            }
        }
        self.block_bar(ui, &REVOKE, snapshot, protected);

        crate::style::section(ui, "접근 요청");
        if let Some(drafts) = &mut self.drafts {
            let config = &mut drafts.ai_focus;
            ui.horizontal(|ui| {
                ui.label("시간을 정한 허용의 최대 길이(분)");
                ui.add(egui::DragValue::new(&mut config.max_grant_minutes).range(5..=1440));
            });
            ui.checkbox(
                &mut config.allow_permanent_grants,
                "AI가 취소할 때까지 가는 허용을 줄 수 있다",
            );
        }
        self.block_bar(ui, &ACCESS, snapshot, protected);
    }

    // --- AI checks ----------------------------------------------------------------------

    fn checks_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        crate::style::section(ui, "검사 간격과 판정");
        ui.small("더 엄격한 값은 바로 적용되고, 더 느슨한 값은 예약됩니다.");
        if let Some(drafts) = &mut self.drafts {
            let config = &mut drafts.ai_focus;
            egui::Grid::new("checks").num_columns(2).show(ui, |ui| {
                ui.label("할 일에 맞는 창을 다시 판정하기까지(분, 0 = 애매한 창과 같은 간격)");
                ui.add(egui::DragValue::new(&mut config.recheck_minutes).range(0..=240));
                ui.end_row();
                ui.label("애매한 창을 다시 판정하기까지(초)");
                ui.add(egui::DragValue::new(&mut config.check_interval_secs).range(15..=3600));
                ui.end_row();
                ui.label("앱을 끄기까지의 딴짓 판정 수(탭은 첫 판정에서 닫음)");
                ui.add(egui::DragValue::new(&mut config.strikes_to_block).range(1..=10));
                ui.end_row();
                ui.label("검사할 때 화면 캡처를 함께 보내기");
                ui.checkbox(&mut config.send_screenshot, "");
                ui.end_row();
            });
        }
        self.block_bar(ui, &CHECKS, snapshot, protected);

        type ListAccessor = fn(&mut AiFocusConfig) -> &mut BTreeSet<String>;
        let lists: [(&str, &'static str, ListAccessor, &str, &'static Block); 3] = [
            (
                "검사하지 않는 앱(언제나 허용)",
                "always_allow",
                |config| &mut config.always_allow,
                "예: zoom.exe",
                &ALWAYS_ALLOW,
            ),
            (
                "화면 캡처 없이 검사하는 앱(비공개 앱)",
                "private_apps",
                |config| &mut config.private_apps,
                "예: bank.exe",
                &PRIVATE_APPS,
            ),
            (
                "브라우저(프로그램 대신 탭을 닫음)",
                "browsers",
                |config| &mut config.browsers,
                "예: chrome.exe",
                &BROWSERS,
            ),
        ];
        for (heading, list, accessor, hint, block) in lists {
            crate::style::section(ui, heading);
            if list == "always_allow" {
                ui.weak(format!(
                    "작업 관리자({})는 이 목록과 상관없이 언제나 허용됩니다.",
                    ai_focus_rule::BUILT_IN_ALWAYS_ALLOW.join(", ")
                ));
            }
            if let Some(drafts) = &mut self.drafts {
                set_ui(
                    ui,
                    list,
                    accessor(&mut drafts.ai_focus),
                    &mut self.adding,
                    hint,
                );
            }
            self.block_bar(ui, block, snapshot, protected);
        }
    }

    // --- what the service does as administrator ----------------------------------------

    /// This program's own update: check, then install through the update guard (every app
    /// stays closed while it installs). Open whether the settings are protected or not.
    fn self_update_ui(&mut self, ui: &mut egui::Ui) {
        use machine::self_update::RequestKind;
        crate::style::section(ui, "이 프로그램 업데이트");
        let status = match &self.self_update {
            Some(Ok(status)) => status.clone(),
            Some(Err(error)) => {
                ui.colored_label(RED, error);
                return;
            }
            None => return,
        };
        let state = &status.state;
        ui.label(format!("지금 버전: {}", status.version));
        ui.small(
            "새 버전은 GitHub의 릴리스 페이지에서 받습니다. 업데이트는 저절로 하지 않고, 아래 버튼으로만 \
             합니다. 설치하는 동안(보통 1분 안) 이 PC의 모든 앱과 브라우저가 닫히고 Focus 앱도 \
             닫힙니다. 그래서 엄격 모드여도 업데이트로 규칙을 피할 수는 없습니다. 끝나면 \
             시작 메뉴에서 Focus 앱을 다시 엽니다. 할 일, 설정, 로그인은 그대로 남습니다.",
        );
        let busy = status.pending.is_some() || state.busy.is_some();
        if let Some(since) = state.installing_since {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!(
                    "{}부터 업데이트를 설치하고 있습니다. 모든 앱이 잠시 닫힙니다.",
                    local(since)
                ));
            });
        } else if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(match status.pending.or(state.busy) {
                    Some(RequestKind::Install) => "새 버전을 받는 중입니다…",
                    _ => "새 버전이 있는지 확인하는 중입니다…",
                });
            });
        } else {
            match (&state.available, state.last_check) {
                (Some(release), _) => {
                    ui.colored_label(YELLOW, format!("새 버전 {}이 있습니다.", release.version));
                    ui.horizontal_wrapped(|ui| {
                        if !self.confirming_install {
                            if ui.button("지금 업데이트").clicked() {
                                self.confirming_install = true;
                            }
                        } else {
                            ui.label("모든 앱과 브라우저가 닫힙니다. 저장하지 않은 것을 먼저 저장하세요.");
                            if ui.button("닫고 업데이트하기").clicked() {
                                self.confirming_install = false;
                                self.request_self_update("install");
                            }
                            if ui.button("취소").clicked() {
                                self.confirming_install = false;
                            }
                        }
                    });
                }
                (None, Some(checked)) => {
                    ui.colored_label(GREEN, format!("최신 버전입니다({} 확인).", local(checked)));
                }
                (None, None) => {}
            }
            if ui.button("업데이트 확인").clicked() {
                self.request_self_update("check");
            }
        }
        if !state.error.is_empty() {
            ui.colored_label(RED, format!("마지막 확인이 실패했습니다: {}", state.error));
        }
        if let Some(run) = &state.last_install {
            ui.colored_label(
                if run.succeeded { GREEN } else { RED },
                format!(
                    "마지막 업데이트({}): {}",
                    local(run.finished_at),
                    run.message
                ),
            );
        }
        if !self.self_update_message.is_empty() {
            ui.small(&self.self_update_message);
        }
    }

    fn request_self_update(&mut self, action: &str) {
        self.self_update_message =
            match call_value(self.session, "self_update", json!({ "action": action })) {
                Ok(_) => String::new(),
                Err(error) => error,
            };
        self.care_fetched_at = None;
    }

    /// How many "yes" answers a scheduled update needs, and how far apart the questions are.
    fn confirmations_ui(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &SettingsSnapshot,
        protected: bool,
    ) {
        crate::style::section(ui, "확인 횟수와 간격");
        ui.small(
            "엄격 모드를 켠 뒤에 규칙을 느슨하게 만드는 변경은 예약되고, 아래 간격마다 \"이 \
             업데이트가 정말 맞나요?\"라고 묻습니다. 아래 횟수만큼 연속으로 \"예\"라고 답해야 \
             적용됩니다. 횟수를 늘리거나 간격을 늘리는 변경은 바로 적용되고, 줄이는 변경은 지금의 \
             횟수와 간격으로 확인해야 적용됩니다. 이미 예약된 업데이트는 예약할 때의 횟수와 \
             간격을 그대로 씁니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            let numbers = &mut drafts.planner.confirmations;
            egui::Grid::new("confirmations")
                .num_columns(3)
                .show(ui, |ui| {
                    ui.label("");
                    ui.strong("확인 횟수");
                    ui.strong("질문 간격(시간)");
                    ui.end_row();
                    ui.label("설정 업데이트");
                    ui.add(
                        egui::DragValue::new(&mut numbers.settings_times)
                            .range(Confirmations::TIMES_RANGE)
                            .suffix("번"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut numbers.settings_interval_hours)
                            .range(Confirmations::HOURS_RANGE)
                            .suffix("시간"),
                    );
                    ui.end_row();
                    ui.label("AI 없이 하는 할 일 변경");
                    ui.add(
                        egui::DragValue::new(&mut numbers.todo_change_times)
                            .range(Confirmations::TIMES_RANGE)
                            .suffix("번"),
                    );
                    ui.add(
                        egui::DragValue::new(&mut numbers.todo_change_interval_hours)
                            .range(Confirmations::HOURS_RANGE)
                            .suffix("시간"),
                    );
                    ui.end_row();
                });
            ui.small(format!(
                "지금 고른 값: 설정 업데이트는 {} 간격으로 {}번, 할 일 변경은 {} 간격으로 {}번 \
                 묻습니다. 기본값은 이틀(48시간) 간격으로 3번, 2시간 간격으로 3번입니다.",
                interval_text(numbers.settings_interval_seconds()),
                numbers.settings_times(),
                interval_text(numbers.todo_change_interval_seconds()),
                numbers.todo_change_times()
            ));
        }
        self.block_bar(ui, &CONFIRMATIONS, snapshot, protected);
    }

    fn system_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        if self
            .care_fetched_at
            .is_none_or(|fetched_at| fetched_at.elapsed() >= REFRESH)
        {
            self.care_fetched_at = Some(Instant::now());
            self.self_update = Some(
                call_value(self.session, "self_update_status", json!({})).and_then(|reply| {
                    serde_json::from_value(reply).map_err(|error| error.to_string())
                }),
            );
            self.care = Some(
                call_value(self.session, "system_care_status", json!({})).and_then(|reply| {
                    serde_json::from_value(reply).map_err(|error| error.to_string())
                }),
            );
        }
        let care = match &self.care {
            Some(Ok(care)) => Some(care.clone()),
            Some(Err(error)) => {
                ui.colored_label(RED, error);
                None
            }
            None => None,
        };

        self.self_update_ui(ui);
        self.confirmations_ui(ui, snapshot, protected);

        crate::style::section(ui, "프로그램 업데이트");
        ui.small(
            "카카오톡처럼 업데이트에 관리자 권한이 필요한 프로그램을 여기에 넣습니다. 사용자에게 \
             관리자 권한을 주지 않고, 이 PC의 서비스(SYSTEM)가 화면 없이 winget으로 그 프로그램만 \
             업데이트합니다. 지우기나 다른 명령은 하지 않습니다. 이름은 winget의 이름(id)으로 \
             적습니다. 명령 프롬프트에서 winget search 카카오톡 처럼 찾으면 \"ID\" 칸에 나옵니다. \
             예: Kakao.KakaoTalk. 넣기는 예약되고, 빼기는 바로 적용됩니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            names_ui(
                ui,
                "updatable_programs",
                &mut drafts.ai_focus.updatable_programs,
                &mut self.adding,
                "예: Kakao.KakaoTalk",
                false,
            );
            ui.checkbox(
                &mut drafts.ai_focus.auto_update_programs,
                format!(
                    "하루 한 번 저절로 업데이트하기({}시 이후, PC가 켜져 있는 첫 때)",
                    machine::system_care::AUTO_UPDATE_FROM_HOUR
                ),
            );
        }
        self.block_bar(ui, &PROGRAM_UPDATES, snapshot, protected);
        if let Some(care) = &care {
            ui.add_space(4.0);
            ui.strong("지금 업데이트하기");
            if care.programs.is_empty() {
                ui.small("적용된 목록이 비어 있습니다.");
            }
            for program in &care.programs {
                ui.horizontal(|ui| {
                    ui.label(program);
                    let waiting = care
                        .pending
                        .iter()
                        .any(|request| &request.program == program)
                        || care.state.running.as_deref() == Some(program.as_str());
                    if waiting {
                        ui.spinner();
                        ui.small("업데이트 중이거나 기다리는 중");
                    } else if ui.button("지금 업데이트").clicked() {
                        self.care_message = match call_value(
                            self.session,
                            "program_update",
                            json!({ "program": program }),
                        ) {
                            Ok(_) => format!(
                                "{program}의 업데이트를 서비스에 맡겼습니다. 몇 초 뒤에 시작됩니다."
                            ),
                            Err(error) => error,
                        };
                        self.care_fetched_at = None;
                    }
                });
            }
            if !self.care_message.is_empty() {
                ui.small(&self.care_message);
            }
            if !care.state.runs.is_empty() {
                ui.small("최근 업데이트:");
            }
            for run in care.state.runs.iter().rev().take(8) {
                let (color, outcome) = match run.outcome {
                    UpdateOutcome::Updated => (GREEN, "업데이트함"),
                    UpdateOutcome::AlreadyUpToDate => (GREEN, "이미 최신"),
                    UpdateOutcome::NotInstalled => (YELLOW, "설치되어 있지 않음"),
                    UpdateOutcome::Failed => (RED, "실패"),
                };
                ui.horizontal_wrapped(|ui| {
                    ui.small(local(run.finished_at));
                    ui.small(if run.automatic {
                        "(저절로)"
                    } else {
                        "(요청)"
                    });
                    ui.label(&run.program);
                    ui.colored_label(color, outcome);
                    if !run.message.is_empty() {
                        ui.small(&run.message);
                    }
                });
            }
        }

        crate::style::section(ui, "시간대 지키기");
        ui.small(
            "표준 사용자도 Windows의 시간대는 바꿀 수 있습니다. 시간대를 바꾸면 생활 규칙의 시각(예: \
             \"18시 전에는\")을 피할 수 있으므로, 서비스가 몇 초 안에 아래 시간대로 되돌립니다. 이 \
             칸이 비어 있으면 서비스가 처음 돌 때 그때의 시간대를 적습니다. 여행 등으로 시간대를 \
             바꿔야 하면 여기서 고칩니다. 바꾸기는 예약됩니다.",
        );
        if let Some(drafts) = &mut self.drafts {
            ui.horizontal(|ui| {
                ui.label("지킬 시간대:");
                ui.add(
                    egui::TextEdit::singleline(&mut drafts.ai_focus.time_zone)
                        .hint_text("예: Korea Standard Time")
                        .desired_width(240.0),
                );
                if let Some(care) = &care {
                    if !care.time_zone_now.is_empty()
                        && ui.button("지금 PC의 시간대로 채우기").clicked()
                    {
                        drafts.ai_focus.time_zone = care.time_zone_now.clone();
                    }
                }
            });
        }
        if let Some(care) = &care {
            if !care.time_zone_now.is_empty() {
                ui.small(format!("지금 PC의 시간대: {}", care.time_zone_now));
            }
            if !care.state.time_zone_error.is_empty() {
                ui.colored_label(RED, &care.state.time_zone_error);
            }
        }
        self.block_bar(ui, &TIME_ZONE, snapshot, protected);
        if let Some(care) = &care {
            for reset in care.state.time_zone_resets.iter().rev().take(5) {
                ui.small(format!(
                    "{} 시간대가 {}(으)로 바뀌어 {}(으)로 되돌렸습니다.",
                    local(reset.at),
                    reset.from,
                    reset.to
                ));
            }
        }
    }

    // --- saving and applying ------------------------------------------------------------

    /// Under a group: what applying would do (in force now or scheduled), when the edits were
    /// saved, and the 저장 / 적용 / 고친 것 버리기 / 기본값으로 되돌리기 buttons.
    fn block_bar(
        &mut self,
        ui: &mut egui::Ui,
        block: &'static Block,
        snapshot: &SettingsSnapshot,
        protected: bool,
    ) {
        let Some(drafts) = &self.drafts else {
            return;
        };
        let index = file_index(block.file);
        let edits = block_edits(drafts, block);
        let current = drafts.as_values();
        let defaults = default_values();
        let differs_from_default = block.has_default
            && block
                .paths
                .iter()
                .any(|path| value_at(&current[index], path) != value_at(&defaults[index], path));
        if !edits.is_empty() {
            // As the agent compares: defaults filled in.
            let in_force = snapshot
                .files
                .get(block.file)
                .cloned()
                .unwrap_or(Value::Null);
            let in_force = machine::settings::normalize(block.file, &in_force).unwrap_or(in_force);
            for edit in &edits {
                let fixes_the_lockdown = block.file == "ai_focus"
                    && self
                        .fix_now
                        .iter()
                        .any(|path| edit.path().starts_with(path.as_slice()));
                let (color, when) = if !protected {
                    (GREEN, "적용하면 바로")
                } else if fixes_the_lockdown {
                    (GREEN, "적용하면 바로(잠금을 풀기 위한 변경)")
                } else {
                    match setting_edits::direction(block.file, edit, &in_force) {
                        Direction::Looser => (YELLOW, "적용하면 예약"),
                        _ => (GREEN, "적용하면 바로"),
                    }
                };
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(color, when);
                    ui.small(redact(&edit.describe()));
                });
            }
            let secret = block.id == API_KEY.id;
            let saved_line = if secret {
                "키는 임시 저장하지 않습니다.".to_string()
            } else if self.changed_at.is_some() {
                "임시 저장하는 중…".to_string()
            } else {
                match self.saved_at {
                    Some((saved_at, by_button)) => format!(
                        "{} {} · 아직 적용 전입니다.",
                        if by_button {
                            "저장됨"
                        } else {
                            "자동으로 임시 저장됨"
                        },
                        saved_at.format("%H:%M:%S")
                    ),
                    // Loaded from the drafts file when the app started.
                    None => "임시 저장되어 있습니다 · 아직 적용 전입니다.".to_string(),
                }
            };
            ui.small(saved_line);
        }
        let mut action = None;
        ui.horizontal(|ui| {
            let has_edits = !edits.is_empty();
            if ui
                .add_enabled(has_edits, egui::Button::new("저장"))
                .clicked()
            {
                action = Some(BarAction::Save);
            }
            if ui
                .add_enabled(has_edits, crate::style::primary("적용", has_edits))
                .clicked()
            {
                action = Some(BarAction::Apply);
            }
            if ui
                .add_enabled(has_edits, egui::Button::new("고친 것 버리기"))
                .clicked()
            {
                action = Some(BarAction::Discard);
            }
            if differs_from_default && ui.button("기본값으로 되돌리기").clicked() {
                action = Some(BarAction::Default);
            }
        });
        if let Some(message) = self.block_messages.get(block.id) {
            ui.small(message);
        }
        match action {
            Some(BarAction::Save) => {
                self.write_drafts(true);
                let saved = self
                    .saved_at
                    .map(|(saved_at, _)| saved_at.format("%H:%M:%S").to_string())
                    .unwrap_or_default();
                self.block_messages.insert(
                    block.id,
                    format!("{saved}에 저장했습니다. \"적용\"을 눌러야 설정에 반영됩니다."),
                );
            }
            Some(BarAction::Apply) => {
                let message = self.apply_block(block, &edits);
                self.block_messages.insert(block.id, message);
            }
            Some(BarAction::Discard) => {
                self.reset_block(block, false);
                self.block_messages
                    .insert(block.id, "고친 내용을 버렸습니다.".to_string());
            }
            Some(BarAction::Default) => {
                self.reset_block(block, true);
                self.block_messages.insert(
                    block.id,
                    "기본값으로 바꿨습니다. \"적용\"을 눌러야 설정에 반영됩니다.".to_string(),
                );
            }
            None => {}
        }
    }

    /// Send the group's edits to the agent, on top of the file as it is now (another tab or
    /// program may have changed it since this tab loaded its copy). Returns what happened.
    fn apply_block(&mut self, block: &'static Block, edits: &[Edit]) -> String {
        let fresh: Option<SettingsSnapshot> = call_value(self.session, "settings_get", json!({}))
            .and_then(|reply| serde_json::from_value(reply).map_err(|error| error.to_string()))
            .ok();
        let index = file_index(block.file);
        let Some(drafts) = &mut self.drafts else {
            return String::new();
        };
        let mut content = match &fresh {
            Some(fresh) => file_view(fresh, block.file),
            None => drafts.loaded[index].clone(),
        };
        setting_edits::apply(&mut content, edits);
        let reply = call_value(
            self.session,
            "settings_put",
            json!({ "name": block.file, "content": content, "via": "settings tab" }),
        )
        .and_then(|reply| {
            serde_json::from_value::<PutOutcome>(reply).map_err(|error| error.to_string())
        });
        match reply {
            Ok(outcome) => {
                // Applied: these edits are no longer the user's pending ones.
                setting_edits::apply(&mut drafts.loaded[index], edits);
                self.fetched_at = None;
                match outcome.scheduled {
                    None => "적용했습니다. 지금부터 효력이 있습니다.".to_string(),
                    Some(scheduled) if outcome.applied.is_empty() => format!(
                        "예약했습니다. 첫 질문은 {}에 옵니다.",
                        local(scheduled.first_question_at)
                    ),
                    Some(scheduled) => format!(
                        "{}개는 바로 적용했고, 나머지는 예약했습니다. 첫 질문은 {}에 옵니다.",
                        outcome.applied.len(),
                        local(scheduled.first_question_at)
                    ),
                }
            }
            Err(error) => format!("적용하지 못했습니다: {error}"),
        }
    }

    /// Put the group's settings back as loaded (`to_default` false) or to the program's
    /// defaults.
    fn reset_block(&mut self, block: &'static Block, to_default: bool) {
        let Some(drafts) = &mut self.drafts else {
            return;
        };
        let index = file_index(block.file);
        let source = if to_default {
            default_values()[index].clone()
        } else {
            drafts.loaded[index].clone()
        };
        let mut values = drafts.as_values();
        for path in block.paths {
            set_value_at(&mut values[index], path, value_at(&source, path));
        }
        drafts.set_values(values);
    }

    /// Write the pending edits to the drafts file a moment after they change.
    fn autosave(&mut self, context: &egui::Context) {
        let Some(drafts) = &self.drafts else {
            return;
        };
        if user_edits(drafts, false) == self.persisted {
            self.changed_at = None;
            return;
        }
        let changed_at = *self.changed_at.get_or_insert_with(Instant::now);
        if changed_at.elapsed() >= AUTOSAVE_AFTER {
            self.write_drafts(false);
        } else {
            context.request_repaint_after(AUTOSAVE_AFTER);
        }
    }

    fn write_drafts(&mut self, by_button: bool) {
        let Some(drafts) = &self.drafts else {
            return;
        };
        let pending = user_edits(drafts, false);
        match write_drafts_file(&pending) {
            Ok(()) => {
                self.persisted = pending;
                self.saved_at = Some((Local::now(), by_button));
                self.changed_at = None;
            }
            Err(error) => self.error = format!("임시 저장하지 못했습니다: {error}"),
        }
    }

    /// The scheduled updates, with the question for each one that is due, and a withdraw
    /// button. Returns whether one was answered or withdrawn.
    fn reservations_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let Some(snapshot) = self.latest.clone() else {
            return false;
        };
        let now = Utc::now();
        let mut changed = false;
        if !snapshot.reservations.scheduled.is_empty() {
            crate::style::section(ui, "예약된 업데이트");
        }
        for reservation in &snapshot.reservations.scheduled {
            let what = match reservation.origin {
                ReservationOrigin::Settings => file_name(&reservation.name).to_string(),
                ReservationOrigin::TodoChange => "할 일 변경".to_string(),
            };
            let every = interval_text(reservation.interval_seconds);
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&what);
                    ui.label(format!(
                        "{} 예약 · 예약한 곳: {} · {}/{}번 확인 · {every}마다 질문",
                        local(reservation.scheduled_at),
                        via_label(&reservation.via),
                        reservation.approvals.len(),
                        reservation.approvals_needed
                    ));
                });
                if !reservation.note.is_empty() {
                    ui.small(&reservation.note);
                }
                egui::CollapsingHeader::new("바뀌는 것")
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
                        "다음 질문: {}",
                        local(reservation.next_question_at())
                    ));
                }
                if ui.small_button("이 업데이트 거두기").clicked() {
                    match call_value(
                        self.session,
                        "settings_withdraw",
                        json!({ "id": reservation.id }),
                    ) {
                        Ok(_) => {
                            self.message = format!("{what}의 업데이트를 거두었습니다.");
                            changed = true;
                        }
                        Err(error) => self.error = error,
                    }
                }
            });
        }
        changed
    }

    /// "이 업데이트가 정말 맞나요?" with yes and no. Returns whether it was answered.
    fn question_ui(&mut self, ui: &mut egui::Ui, reservation: &Reservation, what: &str) -> bool {
        let needed = reservation.approvals_needed;
        ui.colored_label(
            RED,
            format!(
                "{what}의 이 업데이트가 정말 맞나요? ({}/{needed}번째 질문)",
                reservation.approvals.len() + 1
            ),
        );
        ui.small("위의 \"바뀌는 것\"을 읽고, 차분한 날의 나라면 어떻게 답할지 생각해서 답하세요.");
        let mut answer = None;
        ui.horizontal(|ui| {
            if ui.button("예, 여전히 원합니다").clicked() {
                answer = Some(true);
            }
            if ui.button("아니요, 거절합니다").clicked() {
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
                    "{what}: {approvals}/{needed}번 확인했습니다. 다음 질문: {}.",
                    local(next_question_at)
                )
            }
            Ok(AnswerOutcome::Applied) => {
                self.message = format!("{what}: {needed}번 확인되어 지금 적용되었습니다.")
            }
            Ok(AnswerOutcome::Rejected) => {
                self.message = format!("{what}: 업데이트를 거절했습니다.")
            }
            Err(error) => self.error = error,
        }
        true
    }
}

/// `48 * 3600` → "이틀", `72 * 3600` → "3일", `5 * 3600` → "5시간".
pub(crate) fn interval_text(seconds: i64) -> String {
    let hours = (seconds / 3600).max(1);
    match hours {
        24 => "하루".to_string(),
        48 => "이틀".to_string(),
        _ if hours % 24 == 0 => format!("{}일", hours / 24),
        _ => format!("{hours}시간"),
    }
}

/// The file `name` in force with its scheduled settings updates applied (what the user
/// applied last), with defaults filled in.
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

/// The three files as [`file_view`] shows them, through the typed settings (as the drafts are
/// compared).
fn loaded_values(snapshot: &SettingsSnapshot) -> [Value; 3] {
    let policy: ListConfig = from_value(&file_view(snapshot, "policy"));
    let ai_focus: AiFocusConfig = from_value(&file_view(snapshot, "ai_focus"));
    let planner: PlannerConfig = from_value(&file_view(snapshot, "planner"));
    [to_value(&policy), to_value(&ai_focus), to_value(&planner)]
}

/// `values` without `updated_at`, which a file that lacks it gets anew on every read.
fn without_timestamps(mut values: [Value; 3]) -> [Value; 3] {
    for value in &mut values {
        if let Some(fields) = value.as_object_mut() {
            fields.remove("updated_at");
        }
    }
    values
}

/// Editable copies of the three files as [`file_view`] shows them, with `pending` (the user's
/// edits not yet applied) on top.
fn drafts_from(snapshot: &SettingsSnapshot, pending: &PendingEdits) -> Drafts {
    let loaded = loaded_values(snapshot);
    let mut current = loaded.clone();
    for (index, (name, _)) in EDITABLE.iter().enumerate() {
        if let Some(edits) = pending.get(*name) {
            setting_edits::apply(&mut current[index], edits);
        }
    }
    let mut drafts = Drafts {
        policy: ListConfig::default(),
        ai_focus: AiFocusConfig::default(),
        planner: PlannerConfig::default(),
        loaded,
    };
    drafts.set_values(current);
    drafts
}

/// The user's edits not yet applied, by file; the API key only when `with_key`.
fn user_edits(drafts: &Drafts, with_key: bool) -> PendingEdits {
    let current = drafts.as_values();
    let mut pending = PendingEdits::new();
    for (index, (name, _)) in EDITABLE.iter().enumerate() {
        let edits: Vec<Edit> = setting_edits::diff(&drafts.loaded[index], &current[index])
            .into_iter()
            .filter(|edit| {
                BLOCKS
                    .iter()
                    .any(|block| block.file == *name && in_block(edit.path(), block))
            })
            .filter(|edit| with_key || !in_block(edit.path(), &API_KEY))
            .collect();
        if !edits.is_empty() {
            pending.insert(name.to_string(), edits);
        }
    }
    pending
}

/// The user's edits of one group.
fn block_edits(drafts: &Drafts, block: &Block) -> Vec<Edit> {
    let index = file_index(block.file);
    let current = drafts.as_values();
    setting_edits::diff(&drafts.loaded[index], &current[index])
        .into_iter()
        .filter(|edit| in_block(edit.path(), block))
        .collect()
}

fn in_block(path: &[String], block: &Block) -> bool {
    block.paths.iter().any(|block_path| {
        path.len() >= block_path.len()
            && path
                .iter()
                .zip(block_path.iter())
                .all(|(edit_key, block_key)| edit_key == block_key)
    })
}

fn file_index(name: &str) -> usize {
    EDITABLE
        .iter()
        .position(|(editable_name, _)| *editable_name == name)
        .expect("a block names an editable file")
}

/// The program's defaults of the three files, in [`EDITABLE`] order.
fn default_values() -> [Value; 3] {
    [
        to_value(&ListConfig::default()),
        to_value(&AiFocusConfig::default()),
        to_value(&PlannerConfig::default()),
    ]
}

fn value_at(file: &Value, path: &[&str]) -> Value {
    let mut current = file;
    for key in path {
        match current.get(key) {
            Some(child) => current = child,
            None => return Value::Null,
        }
    }
    current.clone()
}

fn set_value_at(file: &mut Value, path: &[&str], value: Value) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = file;
    for key in parents {
        if !current.get(key).is_some_and(Value::is_object) {
            current[*key] = json!({});
        }
        current = &mut current[*key];
    }
    current[*last] = value;
}

/// The drafts file: the user's edits not yet applied, kept in the user's own app data folder
/// so they survive closing the app. The API key is never written there.
fn drafts_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "DigitalWellbeing", "DigitalWellbeing").map(
        |project_directories| {
            project_directories
                .data_local_dir()
                .join("settings_drafts.json")
        },
    )
}

fn read_drafts_file() -> PendingEdits {
    drafts_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_drafts_file(pending: &PendingEdits) -> std::io::Result<()> {
    let path =
        drafts_path().ok_or_else(|| std::io::Error::other("앱 데이터 폴더를 찾지 못했습니다"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(pending).map_err(std::io::Error::other)?;
    let temporary_path = path.with_extension("json.tmp");
    std::fs::write(&temporary_path, bytes)?;
    std::fs::rename(&temporary_path, &path)
}

/// A list of names used as a set, each with a remove button, and a field to add one.
fn set_ui(
    ui: &mut egui::Ui,
    list: &'static str,
    names: &mut BTreeSet<String>,
    adding: &mut HashMap<&'static str, String>,
    hint: &str,
) {
    names_ui(ui, list, names, adding, hint, true);
}

/// [`set_ui`]; `lowercase` false keeps the case as typed (winget ids).
fn names_ui(
    ui: &mut egui::Ui,
    list: &'static str,
    names: &mut BTreeSet<String>,
    adding: &mut HashMap<&'static str, String>,
    hint: &str,
    lowercase: bool,
) {
    let mut remove = None;
    ui.horizontal_wrapped(|ui| {
        for name in names.iter() {
            if ui.small_button(format!("{name}  x")).clicked() {
                remove = Some(name.clone());
            }
        }
        if names.is_empty() {
            ui.small("(없음)");
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
            .add_enabled(!text.trim().is_empty(), egui::Button::new("추가"))
            .clicked()
        {
            let name = text.trim();
            names.insert(if lowercase {
                name.to_lowercase()
            } else {
                name.to_string()
            });
            text.clear();
        }
    });
}

/// Calls and estimated cost today and this month, all users together.
/// What each kind of AI call is for, in Korean: (name, what it does).
fn purpose_label(purpose: &str) -> (&str, &str) {
    match purpose {
        "focus check" => (
            "창 검사(AI 검사)",
            "새 창이 보일 때, 그리고 다시 검사에서 판정이 바뀌어 기본 모델이 재확인할 때 화면 캡처와 창 제목을 보냅니다",
        ),
        "focus recheck" => (
            "창 다시 검사(싼 모델)",
            "이미 판정한 창을 다시 검사할 때 싼 모델이 먼저 봅니다",
        ),
        "access review" => ("접근 요청 심사", "차단 탭의 접근 요청"),
        "planner pick" => ("할 일 고르기", "\"할 일 받기\"를 누를 때"),
        "planner review" => ("변경 요청 심사", "끝냈다, 다른 일 받기, AI 검사 멈추기"),
        "to-do change review" => ("할 일 변경 심사", "할 일 추가, 고치기, 지우기"),
        "appointment review" => ("약속 심사", "약속 추가"),
        "week plan" => ("7일 계획", "통계 탭의 앞으로 7일 계획"),
        "assistant" => ("AI 비서", "AI 비서 탭의 대화(한 번에 여러 번 부를 수 있음)"),
        "model comparison" => (
            "모델 비교",
            "질문 중 일부를 다른 크기의 모델에게도 똑같이 물어 답을 비교할 때(설정 탭의 비율)",
        ),
        "connection check" => (
            "연결 확인",
            "크레딧이 없을 때 30초마다 보내는 가장 짧은 메시지",
        ),
        other => (other, ""),
    }
}

/// How often Opus, Sonnet and Haiku answered as each other did, per feature, from every
/// user's sampled questions (`ai_focus_rule::comparison`), with the latest questions they
/// answered differently.
pub(crate) fn comparison_ui(ui: &mut egui::Ui, snapshot: &SettingsSnapshot) {
    use ai_focus_rule::comparison::{disagrees, summarize, ComparisonRecord, COMPARED_MODELS};
    let mut records: Vec<ComparisonRecord> = snapshot
        .usage
        .values()
        .flat_map(|value| from_value::<UsageLog>(value).comparisons)
        .collect();
    records.sort_by_key(|record| record.at);
    crate::style::section(ui, "모델 비교");
    ui.small(
        "질문 중 일부(설정 탭의 비율)를 Opus, Sonnet, Haiku에게 한 번씩 똑같이 묻고, 세 모델이 서로 \
         같은 답을 했는지 셉니다. 설정에서 고른 모델과 상관없이 세 모델에게 모두 묻습니다. 표의 칸은 \
         왼쪽 모델과 위쪽 모델이 같은 답을 한 비율입니다. \"느슨\"은 위쪽 모델이 왼쪽 모델보다 \
         느슨하게 답한 비율입니다(왼쪽 모델이 딴짓이라고 본 창을 할 일이라고 봄, 왼쪽 모델이 거절한 \
         요청을 승인함). 할 일 고르기와 AI 비서는 답에 방향이 없어 \"느슨\"을 세지 않습니다. 답이 \
         달라도 둘 다 괜찮을 수 있으니, 아래의 예를 읽고 판단하세요.",
    );
    if records.is_empty() {
        ui.label("아직 비교한 질문이 없습니다.");
        return;
    }
    fn model_name(model: &str) -> &str {
        match model {
            "claude-opus-5-5" => "Opus",
            "claude-sonnet-5-5" => "Sonnet",
            "claude-haiku-5-5" => "Haiku",
            other => other,
        }
    }
    let percent = |part: usize, whole: usize| {
        if whole == 0 {
            "-".to_string()
        } else {
            format!(
                "{:.0}% ({part}/{whole})",
                part as f64 / whole as f64 * 100.0
            )
        }
    };
    for feature in summarize(&records) {
        ui.add_space(6.0);
        ui.strong(format!(
            "{} — 질문 {}개",
            purpose_label(&feature.purpose).0,
            feature.samples
        ));
        egui::Grid::new(("model_comparison", feature.purpose.as_str()))
            .striped(true)
            .num_columns(COMPARED_MODELS.len() + 1)
            .show(ui, |ui| {
                ui.label("");
                for column in COMPARED_MODELS {
                    ui.strong(model_name(column));
                }
                ui.end_row();
                for row in COMPARED_MODELS {
                    ui.strong(model_name(row));
                    for column in COMPARED_MODELS {
                        if row == column {
                            ui.weak("-");
                            continue;
                        }
                        let tally = feature
                            .pairs
                            .get(&(row.to_string(), column.to_string()))
                            .cloned()
                            .unwrap_or_default();
                        let mut cell = percent(tally.agreed, tally.compared);
                        if feature.has_direction && tally.compared > 0 {
                            cell.push_str(&format!(
                                " · 느슨 {:.0}%",
                                tally.risky as f64 / tally.compared as f64 * 100.0
                            ));
                        }
                        ui.label(cell);
                    }
                    ui.end_row();
                }
            });
        let failed: Vec<String> = COMPARED_MODELS
            .iter()
            .filter_map(|model| {
                feature
                    .errors
                    .get(*model)
                    .map(|count| format!("{} {count}번", model_name(model)))
            })
            .collect();
        if !failed.is_empty() {
            ui.small(format!("실패한 질문(세지 않음): {}", failed.join(", ")));
        }
    }
    let differing: Vec<&ComparisonRecord> = records
        .iter()
        .rev()
        .filter(|record| disagrees(record))
        .take(20)
        .collect();
    egui::CollapsingHeader::new(format!("답이 다른 질문(최근 {}개)", differing.len()))
        .id_salt("comparison_differences")
        .show(ui, |ui| {
            for record in differing {
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal_wrapped(|ui| {
                        ui.strong(purpose_label(&record.purpose).0);
                        ui.weak(local(record.at));
                    });
                    if !record.asked.is_empty() {
                        ui.small(&record.asked);
                    }
                    // Opus, Sonnet, Haiku, then any other model.
                    let mut answers: Vec<(&String, &ai_focus_rule::comparison::ComparedAnswer)> =
                        record.answers.iter().collect();
                    answers.sort_by_key(|(model, _)| {
                        ai_focus_rule::comparison::COMPARED_MODELS
                            .iter()
                            .position(|compared| compared == model)
                            .unwrap_or(usize::MAX)
                    });
                    for (model, answer) in answers {
                        ui.horizontal_wrapped(|ui| {
                            ui.strong(format!("{}:", model_name(model)));
                            ui.label(&answer.key);
                            if !answer.detail.is_empty() {
                                ui.weak(&answer.detail);
                            }
                        });
                    }
                });
            }
        });
}

/// The AI usage of every user: in total, and per feature (today, the last seven days, this
/// month), so the user can see which feature costs what.
pub(crate) fn usage_ui(ui: &mut egui::Ui, snapshot: &SettingsSnapshot) {
    let today = Local::now().date_naive();
    let week_start = today - chrono::Duration::days(6);
    let (mut today_calls, mut today_cost, mut month_calls, mut month_cost) = (0u64, 0.0, 0u64, 0.0);
    // Per purpose: today, last seven days, this month.
    let mut by_purpose: BTreeMap<String, [Totals; 3]> = BTreeMap::new();
    for value in snapshot.usage.values() {
        let log: UsageLog = from_value(value);
        for (date, day) in &log.days {
            let this_month = date.year() == today.year() && date.month() == today.month();
            if *date == today {
                today_calls += day.total.calls;
                today_cost += day.total.cost_usd;
            }
            if this_month {
                month_calls += day.total.calls;
                month_cost += day.total.cost_usd;
            }
            for (purpose, totals) in &day.by_purpose {
                let entry = by_purpose.entry(purpose.clone()).or_default();
                if *date == today {
                    entry[0].merge(totals);
                }
                if *date >= week_start {
                    entry[1].merge(totals);
                }
                if this_month {
                    entry[2].merge(totals);
                }
            }
        }
    }
    ui.label(format!(
        "오늘: {today_calls}번 호출, 약 ${today_cost:.2} · 이번 달: {month_calls}번 호출, 약 \
         ${month_cost:.2}"
    ));
    if by_purpose.is_empty() {
        return;
    }
    ui.strong("기능별 AI 비용(추정)");
    let mut rows: Vec<(String, [Totals; 3])> = by_purpose.into_iter().collect();
    rows.sort_by(|left, right| right.1[2].cost_usd.total_cmp(&left.1[2].cost_usd));
    egui::Grid::new("usage_by_purpose")
        .striped(true)
        .num_columns(7)
        .show(ui, |ui| {
            for heading in [
                "기능",
                "오늘",
                "최근 7일",
                "이번 달",
                "한 번에 평균",
                "이번 달 비중",
                "토큰(이번 달, 입력/출력)",
            ] {
                ui.strong(heading);
            }
            ui.end_row();
            for (purpose, [today_totals, week_totals, month_totals]) in &rows {
                let (name, what) = purpose_label(purpose);
                ui.label(name).on_hover_text(what);
                let cell =
                    |totals: &Totals| format!("${:.2} ({}번)", totals.cost_usd, totals.calls);
                ui.label(cell(today_totals));
                ui.label(cell(week_totals));
                ui.label(cell(month_totals));
                ui.label(if month_totals.calls > 0 {
                    format!("${:.4}", month_totals.cost_usd / month_totals.calls as f64)
                } else {
                    "-".into()
                });
                ui.label(if month_cost > 0.0 {
                    format!("{:.0}%", month_totals.cost_usd / month_cost * 100.0)
                } else {
                    "-".into()
                });
                ui.label(format!(
                    "{} / {}",
                    month_totals.input_tokens
                        + month_totals.cache_read_tokens
                        + month_totals.cache_write_tokens,
                    month_totals.output_tokens
                ));
                ui.end_row();
            }
        });
    ui.small(
        "비용은 토큰 수와 가격표로 계산한 추정입니다. 실제 청구액은 Claude Console에서 확인하세요. 기능 \
         이름 위에 마우스를 올리면 그 기능이 언제 AI를 부르는지 나옵니다.",
    );
}

/// Where a scheduled update came from, in Korean.
fn via_label(via: &str) -> String {
    match via {
        "settings tab" => "설정 탭".to_string(),
        "to-do tab" => "할 일 탭".to_string(),
        "claude desktop" => "Claude Desktop".to_string(),
        other => other.to_string(),
    }
}

/// Hide an API key in an edit's description.
fn redact(line: &str) -> String {
    if line.starts_with("api_key") {
        "api_key = (새 키)".into()
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

/// Whether this PC syncs with other devices, and how the last sync went (`docs/sync.md`).
/// Logging in is at the top of this tab ([`crate::login`]).
fn sync_ui(
    ui: &mut egui::Ui,
    sync: Option<&machine::user_api::SyncStatus>,
    session: u32,
    message: &mut String,
) {
    crate::style::section(ui, "기기 동기화");
    let Some(sync) = sync else {
        ui.weak(
            "이 PC는 다른 기기와 동기화하지 않습니다. 동기화를 켜려면 이 탭 맨 위의 \
             \"여러 기기 동기화: 서버에 로그인하거나 회원가입하기\"를 엽니다.",
        );
        return;
    };
    ui.label(format!(
        "이 PC는 \"{}\"라는 이름으로 {} 서버와 동기화합니다.",
        sync.device_id, sync.server_url
    ));
    match sync.last_success {
        Some(time) => ui.label(format!(
            "마지막 동기화: {}",
            time.with_timezone(&chrono::Local)
                .format("%m월 %d일 %H:%M:%S")
        )),
        None => ui.label("아직 한 번도 동기화하지 못했습니다."),
    };
    if let Some(error) = &sync.last_error {
        crate::style::notice(
            ui,
            crate::style::Tone::Error,
            format!("마지막 동기화가 실패했습니다: {error}"),
        );
    }
    for refusal in &sync.refused {
        ui.colored_label(YELLOW, format!("서버가 받아들이지 않은 변경: {refusal}"));
    }
    for device in &sync.outdated_devices {
        ui.colored_label(
            YELLOW,
            format!("업데이트해야 다시 동기화하는 다른 PC: {device}"),
        );
    }
    if !sync.outdated_devices.is_empty()
        || sync
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("업데이트"))
    {
        ui.hyperlink_to(
            "새 버전 내려받기(릴리스 페이지)",
            "https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases",
        );
    }
    devices_ui(ui, sync, session, message);
}

/// The paired devices with their version and last sync, and a button that takes another
/// device's entry off the server's list (an old entry of a computer installed again, or a
/// computer no longer used).
fn devices_ui(
    ui: &mut egui::Ui,
    sync: &machine::user_api::SyncStatus,
    session: u32,
    message: &mut String,
) {
    if sync.device_details.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(format!("서버에 붙은 기기({}대)", sync.device_details.len()))
        .id_salt("sync_devices")
        .show(ui, |ui| {
            ui.small(
                "같은 컴퓨터는 다시 설치해도 같은 기기로 붙습니다. 예전 버전에서 다시 설치해 생긴 \
                 옛 기기나 더는 쓰지 않는 컴퓨터는 \"목록에서 지우기\"로 지웁니다. 최근 10분 안에 \
                 동기화한 기기는 서버가 지우지 않습니다. 지운 컴퓨터를 다시 쓰면 그 컴퓨터는 저절로 \
                 다시 붙습니다.",
            );
            egui::Grid::new("sync_device_list")
                .striped(true)
                .num_columns(5)
                .show(ui, |ui| {
                    for heading in ["기기", "이름", "버전", "마지막 동기화", ""] {
                        ui.strong(heading);
                    }
                    ui.end_row();
                    for device in &sync.device_details {
                        let this_pc = device.id == sync.device_id;
                        ui.label(if this_pc {
                            format!("{} (이 PC)", device.id)
                        } else {
                            device.id.clone()
                        });
                        ui.label(&device.name);
                        ui.label(if device.version.is_empty() {
                            "?"
                        } else {
                            device.version.as_str()
                        });
                        ui.label(device.last_seen.map_or("없음".to_string(), |seen| {
                            seen.with_timezone(&chrono::Local)
                                .format("%m월 %d일 %H:%M")
                                .to_string()
                        }));
                        // The server keeps a device that synced in the last 10 minutes.
                        let in_use = device.last_seen.is_some_and(|seen| {
                            (chrono::Utc::now() - seen).num_minutes() < 10
                        });
                        if this_pc {
                            ui.label("");
                        } else if in_use {
                            ui.weak("사용 중").on_hover_text(
                                "최근 10분 안에 동기화한 기기라서 목록에서 지울 수 없습니다",
                            );
                        } else if sync.forget_waiting.contains(&device.id) {
                            ui.weak("다음 동기화 때 지웁니다");
                        } else if ui.small_button("목록에서 지우기").clicked() {
                            *message = match call_value(
                                session,
                                "sync_forget_device",
                                json!({ "id": device.id }),
                            ) {
                                Ok(_) => format!(
                                    "\"{}\"를 다음 동기화 때 서버의 목록에서 지웁니다.",
                                    device.id
                                ),
                                Err(error) => format!("지우지 못했습니다: {error}"),
                            };
                        }
                        ui.end_row();
                    }
                });
            for (device, reason) in &sync.not_forgotten {
                ui.colored_label(YELLOW, format!("\"{device}\"를 지우지 않았습니다: {reason}"));
            }
        });
}

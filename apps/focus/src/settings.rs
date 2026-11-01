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
use machine::setting_edits::{self, Direction, Edit};
use machine::settings::{
    AnswerOutcome, PutOutcome, Reservation, ReservationOrigin, SettingsSnapshot, APPROVALS_NEEDED,
    EDITABLE,
};
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
use task_planner::{PlannerConfig, PICK_PROMPT, PROPOSAL_PROMPT, REVIEW_PROMPT, WEEK_PLAN_PROMPT};

const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);
const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
/// How often the settings are fetched again while this tab is shown.
const REFRESH: Duration = Duration::from_secs(3);
/// Edits are saved to the drafts file this long after the last change.
const AUTOSAVE_AFTER: Duration = Duration::from_secs(1);

/// Responsibility: Name the sections of the Settings tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Connection,
    Rules,
    Blocking,
    Checks,
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
    paths: &[&["model"]],
    has_default: true,
};
const BUDGET: Block = Block {
    id: "budget",
    file: "ai_focus",
    paths: &[&["daily_budget_usd"]],
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
/// Every group; an edit outside them (such as a timestamp) is not the user's.
const BLOCKS: [&Block; 17] = [
    &API_KEY,
    &MODEL,
    &BUDGET,
    &LIFE_RULES,
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
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, protected: bool) {
        self.refresh();
        ui.heading("설정 보호(잠금)");
        if protected {
            ui.label(format!(
                "설정이 보호되어 있습니다. 규칙을 더 엄격하게 만드는 변경은 바로 적용됩니다. 그 밖의 \
                 변경은 예약되고, 이틀 뒤부터 \"이 업데이트가 정말 맞나요?\"라는 질문이 옵니다. 이틀 \
                 간격으로 {APPROVALS_NEEDED}번 연속 \"예\"라고 답하면 적용되고, 한 번이라도 \
                 \"아니요\"라고 답하면 거절됩니다."
            ));
        } else {
            self.protect_ui(ui);
        }
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
            ] {
                ui.selectable_value(&mut self.section, section, label);
            }
        });
        ui.small(
            "고친 내용은 자동으로 임시 저장됩니다. 각 항목 아래의 \"적용\"을 눌러야 설정에 \
             반영됩니다.",
        );
        ui.separator();
        match self.section {
            Section::Connection => self.connection_ui(ui, &snapshot, protected),
            Section::Rules => self.rules_ui(ui, &snapshot, protected),
            Section::Blocking => self.blocking_ui(ui, &snapshot, protected),
            Section::Checks => self.checks_ui(ui, &snapshot, protected),
        }
        self.autosave(ui.ctx());
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
    fn protect_ui(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            YELLOW,
            "설정 단계: 변경이 바로 적용되고, 이 PC를 쓰는 누구나 설정을 바꿀 수 있습니다.",
        );
        ui.label(format!(
            "모두 정했으면 설정을 보호하세요. 그 뒤로는 규칙을 더 엄격하게 만드는 변경만 바로 \
             적용되고, 그 밖의 변경은 이틀 간격으로 {APPROVALS_NEEDED}번 확인해야 적용됩니다."
        ));
        ui.checkbox(
            &mut self.confirm_protect,
            "프로그램 안의 어떤 것도 보호를 끄지 못한다는 것을 이해했습니다.",
        );
        if ui
            .add_enabled(self.confirm_protect, egui::Button::new("설정 보호하기"))
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

    fn connection_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        ui.heading("Anthropic API 키");
        if let Some(drafts) = &mut self.drafts {
            let config = &mut drafts.ai_focus;
            let saved = config.api_key.trim();
            if saved.is_empty() {
                ui.colored_label(
                    RED,
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

        ui.separator();
        ui.heading("모델");
        if let Some(drafts) = &mut self.drafts {
            ui.add(egui::TextEdit::singleline(&mut drafts.ai_focus.model).desired_width(240.0));
        }
        self.block_bar(ui, &MODEL, snapshot, protected);

        ui.separator();
        ui.heading("AI 하루 예산");
        if let Some(drafts) = &mut self.drafts {
            ui.horizontal(|ui| {
                ui.label("AI 검사의 하루 예산(USD, 0 = 제한 없음)");
                ui.add(
                    egui::DragValue::new(&mut drafts.ai_focus.daily_budget_usd)
                        .speed(0.05)
                        .range(0.0..=1000.0),
                );
            });
        }
        self.block_bar(ui, &BUDGET, snapshot, protected);
        ui.small("지금까지 쓴 AI 사용량은 통계 탭에 있습니다.");
    }

    // --- rules for the AI ---------------------------------------------------------------

    fn rules_ui(&mut self, ui: &mut egui::Ui, snapshot: &SettingsSnapshot, protected: bool) {
        ui.heading("생활 규칙");
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

        ui.separator();
        ui.heading("AI 지시문");
        ui.label(
            "플래너의 AI에게 무엇을 어떤 기준으로 판단하라고 알려 주는 글입니다. 정확도를 위해 \
             영어로 씁니다. 비어 있으면 프로그램에 들어 있는 기본 지시문을 씁니다.",
        );
        type Accessor = fn(&mut PlannerConfig) -> &mut String;
        let instructions: [(&str, Accessor, &str, &'static Block); 4] = [
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
        ui.heading("항상 막을 프로그램");
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

        ui.separator();
        ui.heading("항상 막을 웹사이트");
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

        ui.separator();
        ui.heading("지금 걸린 차단");
        ui.small("차단 풀기는 예약됩니다. 바로 필요하면 차단 탭에서 접근을 요청하세요.");
        if let Some(drafts) = &mut self.drafts {
            let mut any = false;
            for (user, value) in &snapshot.blocks {
                let blocks: SavedBlocks = from_value(value);
                for (key, entry) in blocks.processes.iter().chain(blocks.tabs.iter()) {
                    any = true;
                    let unblocking = drafts.ai_focus.unblocked.contains(&entry.id);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{user}: {key} — {}", entry.reason));
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

        ui.separator();
        ui.heading("AI가 준 접근 허용");
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

        ui.separator();
        ui.heading("접근 요청");
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
        ui.heading("검사 간격과 판정");
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
            ui.separator();
            ui.heading(heading);
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
                let (color, when) = if !protected {
                    (GREEN, "적용하면 바로")
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
                .add_enabled(has_edits, egui::Button::new("적용"))
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
            ui.separator();
            ui.heading("예약된 업데이트");
        }
        for reservation in &snapshot.reservations.scheduled {
            let what = match reservation.origin {
                ReservationOrigin::Settings => file_name(&reservation.name).to_string(),
                ReservationOrigin::TodoChange => "할 일 변경".to_string(),
            };
            let every = if reservation.interval_seconds >= 24 * 3600 {
                format!("{}일", reservation.interval_seconds / (24 * 3600))
            } else {
                format!("{}시간", (reservation.interval_seconds / 3600).max(1))
            };
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.strong(&what);
                    ui.label(format!(
                        "{} 예약 · 예약한 곳: {} · {}/{APPROVALS_NEEDED}번 확인 · {every}마다 질문",
                        local(reservation.scheduled_at),
                        via_label(&reservation.via),
                        reservation.approvals.len()
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
        ui.colored_label(
            RED,
            format!(
                "{what}의 이 업데이트가 정말 맞나요? ({}/{APPROVALS_NEEDED}번째 질문)",
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
                    "{what}: {approvals}/{APPROVALS_NEEDED}번 확인했습니다. 다음 질문: {}.",
                    local(next_question_at)
                )
            }
            Ok(AnswerOutcome::Applied) => {
                self.message = format!("{what}: {APPROVALS_NEEDED}번 확인되어 지금 적용되었습니다.")
            }
            Ok(AnswerOutcome::Rejected) => {
                self.message = format!("{what}: 업데이트를 거절했습니다.")
            }
            Err(error) => self.error = error,
        }
        true
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
            names.insert(text.trim().to_lowercase());
            text.clear();
        }
    });
}

/// Calls and estimated cost today and this month, all users together.
pub(crate) fn usage_ui(ui: &mut egui::Ui, snapshot: &SettingsSnapshot) {
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
        "오늘: {today_calls}번 호출, 약 ${today_cost:.2} · 이번 달: {month_calls}번 호출, 약 \
         ${month_cost:.2}"
    ));
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

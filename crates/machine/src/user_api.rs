//! Responsibility: Carry requests from the Focus app to the agent of its session (server and
//! client), exposing only what the user may control.
//!
//! The agent runs as SYSTEM, so the user cannot reach its settings; this API
//! exposes only what the user is meant to control: their own focus session.
//! Admin settings (lists, AI settings) are changed through the files in
//! [`crate::data_dir`], which standard users cannot write.
//!
//! Transport: TCP on `127.0.0.1:(BASE_PORT + session id)`, one JSON request
//! per connection, one JSON line back — same framing as `tool-api`.
//!
//! | method           | params                                   | result     |
//! |------------------|------------------------------------------|------------|
//! | `status`         | `{}`                                     | [`Status`] |
//! | `start_session`  | `{ "task": "..." }` (planner inactive)   | [`Status`] |
//! | `stop_session`   | `{}` (planner inactive)                  | [`Status`] |
//! | `stop_use`       | `{}` ("사용 중지": end the task, lock every app, stop all but blocking) | [`Status`] |
//! | `resume_use`     | `{}` ("다시 사용하기")                    | [`Status`] |
//! | `assign`         | `{}` (planner active)                    | [`Status`] |
//! | `request_change` | `{ "request": ChangeKind, "reason": "…" }` | [`Status`], `last_decision` set |
//! | `request_access` | `{ "target": GrantTarget, "duration": GrantDuration, "purpose": "…" }` | [`Status`], `last_access` set |
//! | `propose_todo`   | `{ "todo": ProposalInput, "via": "…" }` (= `todo_change` add) | [`Status`], `last_proposal` set |
//! | `todo_change`    | `{ "change": TodoChange, "reason": "…", "via": "…" }` | [`Status`], `last_todo_change` set; approved = in force |
//! | `todo_change_schedule` | `{ "id": "…" }` (a change the AI did not approve) | [`Status`], `last_todo_change` set |
//! | `task_notes_put` | `{ "todo_id": "…", "notes": TaskNotes }` | [`Status`] |
//! | `plan_week`      | `{}` (one AI call)                       | [`Status`] with `planner.week_plan` |
//! | `settings_get`   | `{}`                                     | [`crate::settings::SettingsSnapshot`] |
//! | `settings_put`   | `{ "name": "policy"\|"ai_focus"\|"planner", "content": {…}, "via": "…" }` | [`crate::settings::PutOutcome`] (what is in force now, what was scheduled) |
//! | `settings_answer` | `{ "id": "…", "approve": true\|false }` (once the question is due) | [`crate::settings::AnswerOutcome`] |
//! | `settings_withdraw` | `{ "id": "…" }`                        | `{ "ok": true }` |
//! | `timetable` | `{ "from": "YYYY-MM-DD", "to": "YYYY-MM-DD" }` | [`task_planner::TimetableEntry`] list |
//! | `settings_protect` | `{ mode? }` ([`crate::settings::ProtectionChoice`]; permanent without) | [`ProtectionView`] |
//! | `settings_unprotect` | `{ password? }` (time over, or the password) | `{ protected: false }`; every block lifted |
//! | `program_update` | `{ "program": "Kakao.KakaoTalk" }` (listed in `updatable_programs`) | [`crate::system_care::CareStatus`] |
//! | `system_care_status` | `{}`                                 | [`crate::system_care::CareStatus`] |
//! | `installed_programs_refresh` | `{}` (the service lists the programs winget can update) | [`crate::system_care::CareStatus`] |
//! | `self_update` | `{ "action": "check"\|"install" }` (the service does it; see `crate::self_update`) | [`crate::self_update::UpdateStatus`] |
//! | `self_update_status` | `{}`                                 | [`crate::self_update::UpdateStatus`] |
//! | `assistant_send` | `{ "text": "…" }` (starts a turn and returns at once) | [`crate::assistant::AssistantView`] |
//! | `assistant_view` | `{}`                                     | [`crate::assistant::AssistantView`] |
//! | `assistant_reset` | `{}` (new conversation)                 | [`crate::assistant::AssistantView`] |
//!
//! The `settings_*` methods let a standard user edit the admin settings: during setup an
//! edit takes effect at once; once protected it is scheduled and takes effect only after
//! three "yes" answers two days apart; see [`crate::settings`].
//!
//! The `assistant_*` methods are the Focus app's AI assistant tab: Claude reads the user's
//! words and acts through the same tools Claude Desktop gets ([`crate::assistant_tools`]),
//! each a call to [`dispatch`]; see [`crate::assistant`].
//!
//! `propose_todo` asks the AI to add a to-do to the planner's list; it judges
//! the proposal against the admin's life rules (see `task_planner`).
//!
//! While the task planner is active the user cannot pick a task or stop
//! freely: `start_session` / `stop_session` are refused and the Focus app uses
//! `assign` and `request_change` (which the AI reviews) instead.

use ai_focus_rule::grants::{Grant, GrantDuration, GrantLogEntry, GrantTarget};
use ai_focus_rule::{FocusMonitor, VerdictLogEntry};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use task_planner::{
    ChangeKind, ChangeRecord, Evidence, PlannerStatus, ProposalInput, ProposalRecord, TaskNotes,
    TaskPlanner, TodoChange, TodoChangeRecord, TodoListWrite,
};

/// Re-exported so clients need not depend on the rule crates.
pub use ai_focus_rule::grants;
pub use ai_focus_rule::Verdict;
pub use task_planner::{AssignmentSource, ProposalInput as TodoProposal, TodoItem};

pub const BASE_PORT: u16 = 8770;
/// Large enough for a whole settings file in `settings_put`.
const MAX_REQUEST: usize = 512 * 1024;

/// TCP port of the agent of `session` (on Linux the uid): `BASE_PORT + session % 1000`.
/// `DW_PORT_BASE` moves the base (tests run several agents side by side).
pub fn port_for_session(session: u32) -> u16 {
    let base = std::env::var("DW_PORT_BASE")
        .ok()
        .and_then(|base_text| base_text.parse::<u16>().ok())
        .unwrap_or(BASE_PORT);
    base.saturating_add((session % 1000) as u16)
}

/// Responsibility: Describe one block for the Focus app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedItem {
    /// Process name, `process|site` for a browser tab, or
    /// `process|youtube channel: …` / `process|youtube page: …` on YouTube.
    pub key: String,
    pub reason: String,
}

/// Responsibility: Carry everything the Focus app shows; every API call returns it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub session_active: bool,
    pub task: String,
    pub last_check: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub check_interval_secs: u64,
    /// Newest first.
    pub verdicts: Vec<VerdictLogEntry>,
    pub blocked: Vec<BlockedItem>,
    /// `None` when no planner is configured for this agent.
    pub planner: Option<PlannerStatus>,
    /// Set only in the reply to `request_change`.
    pub last_decision: Option<ChangeRecord>,
    /// Access grants in force (granted apps and sites stay observed).
    pub grants: Vec<Grant>,
    /// Recent grant events, newest first.
    pub grant_log: Vec<GrantLogEntry>,
    /// Set only in the reply to `request_access`.
    pub last_access: Option<GrantLogEntry>,
    /// Set only in the reply to `propose_todo`.
    #[serde(default)]
    pub last_proposal: Option<ProposalRecord>,
    /// Set only in the reply to `todo_change`, `propose_todo` and `todo_change_schedule`.
    #[serde(default)]
    pub last_todo_change: Option<TodoChangeRecord>,
    /// Setup is over: settings edits are scheduled and need three confirmations.
    #[serde(default)]
    pub settings_protected: bool,
    /// How the protection may end (`None` during setup).
    #[serde(default)]
    pub settings_protection: Option<ProtectionView>,
    /// Scheduled settings updates whose "is this update really right?" question is due.
    #[serde(default)]
    pub settings_questions_due: usize,
    /// Estimated cost of today's Claude calls on this PC, and the admin's daily
    /// budget for focus checks (`0` = none).
    #[serde(default)]
    pub ai_cost_today_usd: Option<f64>,
    #[serde(default)]
    pub daily_budget_usd: f64,
    /// Every app is locked: the settings are protected and the AI connection is broken.
    #[serde(default)]
    pub lockdown: Option<crate::lockdown::Lockdown>,
    /// The block the AI made last: the Focus app asks at once what to do about it.
    #[serde(default)]
    pub last_block: Option<ai_focus_rule::NewBlock>,
    /// Programs the enforcer closed (or could not close) lately, newest first: blacklisted
    /// ones, AI blocks and the lockdown alike, windowless background programs included.
    #[serde(default)]
    pub recent_kills: Vec<enforcer::BlockEvent>,
    /// Multi-device sync of this PC (`None`: it does not sync). See `docs/sync.md`.
    #[serde(default)]
    pub sync: Option<SyncStatus>,
    /// The login of this Windows account (sync logins; see `crate::login`).
    #[serde(default)]
    pub login: crate::login::LoginStatus,
}

/// Responsibility: Tell the Focus app how this PC's sync went.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncStatus {
    pub server_url: String,
    pub device_id: String,
    pub last_success: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// Changes the server refused (e.g. turning the protection off), with the reason.
    pub refused: Vec<String>,
    /// Other PCs on an older version (they wait for an update): "<device> (<version>)".
    pub outdated_devices: Vec<String>,
    /// Every paired device: (id, name), as the server last told.
    pub devices: Vec<(String, String)>,
    /// The account this PC logged in with; none when it was paired with a code (then it does
    /// not come back by itself after the server is set up again).
    pub account: Option<String>,
    /// Every paired device with its version and last sync, as the server last told.
    #[serde(default)]
    pub device_details: Vec<sync::protocol::DeviceSummary>,
    /// Devices waiting to be taken off the list with the next sync.
    #[serde(default)]
    pub forget_waiting: Vec<String>,
    /// Devices the last sync asked to take off the list that the server kept: (id, why).
    #[serde(default)]
    pub not_forgotten: Vec<(String, String)>,
}

/// Read from `sync.json` and `sync_state.json` (written by the service).
pub fn sync_status(data_directory: &std::path::Path) -> Option<SyncStatus> {
    let read = |name: &str| -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(data_directory.join(name)).ok()?).ok()
    };
    let config = read("sync.json")?;
    let state = read("sync_state.json").unwrap_or(Value::Null);
    let text = |value: &Value| value.as_str().map(str::to_string);
    Some(SyncStatus {
        device_details: serde_json::from_value(state["device_details"].clone()).unwrap_or_default(),
        forget_waiting: sync::client::ForgetRequest::load(data_directory).devices,
        not_forgotten: serde_json::from_value(state["not_forgotten"].clone()).unwrap_or_default(),
        devices: serde_json::from_value(state["devices"].clone()).unwrap_or_default(),
        account: text(&config["account"]["name"]),
        server_url: text(&config["server_url"]).unwrap_or_default(),
        device_id: text(&config["device_id"]).unwrap_or_default(),
        last_success: serde_json::from_value(state["last_success"].clone())
            .ok()
            .flatten(),
        last_error: text(&state["last_error"]),
        refused: state["refused"]
            .as_array()
            .map(|refusals| {
                refusals
                    .iter()
                    .map(|refusal| {
                        format!(
                            "{} ({})",
                            refusal["name"].as_str().unwrap_or_default(),
                            refusal["reason"].as_str().unwrap_or_default()
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
        outdated_devices: state["outdated_devices"]
            .as_array()
            .map(|devices| {
                devices
                    .iter()
                    .map(|device| {
                        format!(
                            "{} ({})",
                            device[0].as_str().unwrap_or_default(),
                            device[1]
                                .as_str()
                                .filter(|version| !version.is_empty())
                                .unwrap_or("버전 모름")
                        )
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Responsibility: Show the Focus app how the protection may end, without the password hash.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtectionView {
    pub since: Option<DateTime<Utc>>,
    /// `permanent`, `until`, `administrator` or `password`.
    pub kind: String,
    /// The end of a time protection.
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    /// The sync device whose administrator may turn an administrator protection off
    /// (`None`: any computer's).
    #[serde(default)]
    pub device: Option<String>,
}

impl ProtectionView {
    pub fn of(protection: &crate::settings::Protection) -> Self {
        use crate::settings::ProtectionMode;
        let (kind, until, device) = match &protection.mode {
            ProtectionMode::Permanent => ("permanent", None, None),
            ProtectionMode::Until { until } => ("until", Some(*until), None),
            ProtectionMode::Administrator { device } => ("administrator", None, device.clone()),
            ProtectionMode::Password { .. } => ("password", None, None),
        };
        Self {
            since: protection.protected_since,
            kind: kind.to_string(),
            until,
            device,
        }
    }
}

/// Responsibility: Hold what the agent exposes (focus monitor, optional planner) and tell
/// whether the planner restricts a request.
pub struct AgentApi {
    pub monitor: Arc<FocusMonitor>,
    /// The enforcer of this session, for its log of terminated programs (`None` in tests).
    pub enforcer: Option<Arc<enforcer::Enforcer>>,
    pub planner: Option<Arc<TaskPlanner>>,
    /// The AI assistant tab's conversation.
    pub assistant: crate::assistant::Assistant,
}

impl AgentApi {
    fn planner_active(&self) -> bool {
        self.planner
            .as_ref()
            .is_some_and(|planner| planner.is_active())
    }
}

fn status(api: &AgentApi) -> Status {
    let monitor = &api.monitor;
    let grants = monitor.grants().get();
    let config = monitor.store().get();
    let state = monitor.state();
    let blocked = state
        .blocked_processes
        .iter()
        .chain(state.blocked_tabs.iter())
        .map(|(key, block)| BlockedItem {
            key: key.clone(),
            reason: block.reason.clone(),
        })
        .collect();
    Status {
        session_active: config.session_active,
        task: config.task.clone(),
        last_check: state.last_check,
        last_error: state.last_error.clone(),
        check_interval_secs: config.check_interval_secs,
        verdicts: state.verdicts.iter().rev().take(30).cloned().collect(),
        blocked,
        planner: api.planner.as_ref().map(|planner| planner.status()),
        last_decision: None,
        grants: grants
            .grants
            .into_iter()
            .filter(|grant| grant.is_active(Utc::now(), &config))
            .collect(),
        grant_log: grants.log.into_iter().rev().take(10).collect(),
        last_access: None,
        last_proposal: None,
        last_todo_change: None,
        settings_protected: crate::settings::is_protected(&crate::data_dir()),
        settings_protection: crate::settings::protection(&crate::data_dir())
            .map(|protection| ProtectionView::of(&protection)),
        settings_questions_due: crate::settings::questions_due(&crate::data_dir(), Utc::now()),
        ai_cost_today_usd: monitor.usage().map(|usage| usage.today_cost()),
        daily_budget_usd: config.daily_budget_usd,
        lockdown: lockdown_now(api.planner.as_deref()),
        last_block: state.last_block.clone(),
        recent_kills: api
            .enforcer
            .as_ref()
            .map(|enforcer| enforcer.recent_blocks(30))
            .unwrap_or_default(),
        sync: sync_status(&crate::data_dir()),
        login: crate::login::status(&crate::data_dir()),
    }
}

/// The lockdown in force now (see [`crate::lockdown`]): a broken AI connection, a sync login
/// not made yet, to-dos with none assigned, or a stop ("사용 중지").
pub fn lockdown_now(planner: Option<&TaskPlanner>) -> Option<crate::lockdown::Lockdown> {
    crate::lockdown::lockdown(
        crate::settings::is_protected(&crate::data_dir()),
        &ai_focus_rule::connection::current().0,
        planner.and_then(|planner| {
            if planner.stopped() {
                Some(crate::lockdown::TaskLock::Stopped)
            } else if crate::login::required(&crate::data_dir()) {
                Some(crate::lockdown::TaskLock::LoginRequired)
            } else {
                planner
                    .waiting_for_task()
                    .then_some(crate::lockdown::TaskLock::WaitingForTask)
            }
        }),
    )
}

/// The settings edits that write `writes` to `planner.json`.
fn todo_list_edits(writes: &[TodoListWrite]) -> Vec<crate::setting_edits::Edit> {
    use crate::setting_edits::Edit;
    writes
        .iter()
        .map(|write| match write {
            TodoListWrite::Put(todo) => Edit::PutListItem {
                path: vec!["todos".into()],
                id: todo.id.clone(),
                item: Some(serde_json::to_value(todo).unwrap_or(Value::Null)),
            },
            TodoListWrite::Merge {
                todo_id,
                changed_fields,
                base,
            } => Edit::MergeListItem {
                path: vec!["todos".into()],
                id: todo_id.clone(),
                changed_fields: changed_fields.clone(),
                base: base
                    .as_ref()
                    .map(|todo| serde_json::to_value(todo).unwrap_or(Value::Null)),
            },
            TodoListWrite::Remove(todo_id) => Edit::PutListItem {
                path: vec!["todos".into()],
                id: todo_id.clone(),
                item: None,
            },
            TodoListWrite::Dismiss(todo_id) => Edit::AddItems {
                path: vec!["dismissed".into()],
                items: vec![Value::String(todo_id.clone())],
            },
            TodoListWrite::PutAppointment(appointment) => Edit::PutListItem {
                path: vec!["appointments".into()],
                id: appointment.id.clone(),
                item: Some(serde_json::to_value(appointment).unwrap_or(Value::Null)),
            },
            TodoListWrite::RemoveAppointment(appointment_id) => Edit::PutListItem {
                path: vec!["appointments".into()],
                id: appointment_id.clone(),
                item: None,
            },
        })
        .collect()
}

/// Handle one request.
pub fn dispatch(api: &AgentApi, method: &str, params: &Value) -> Result<Value, String> {
    const PLANNER_ON: &str =
        "플래너가 켜져 있습니다. 할 일 받기나 변경 요청(이유 필요)을 쓰세요 (assign / request_change)";
    let mut last_decision = None;
    let mut last_access = None;
    let mut last_proposal = None;
    let mut last_todo_change = None;
    let data_directory = crate::data_dir();
    match method {
        "settings_get" => {
            let snapshot = crate::settings::snapshot(&data_directory)?;
            return serde_json::to_value(snapshot).map_err(|error| error.to_string());
        }
        "settings_put" => {
            let name = params["name"].as_str().ok_or("params.name required")?;
            let via = params["via"].as_str().unwrap_or("settings tab");
            // While a broken AI connection locks every app, fixing the key works at once.
            let fix_now = lockdown_now(api.planner.as_deref())
                .map(|lockdown| lockdown.fix_now)
                .unwrap_or_default();
            // A new model is put in force (or scheduled) only once the API knows it.
            if name == "ai_focus" {
                let in_force = api.monitor.store().get();
                let edited: ai_focus_rule::AiFocusConfig =
                    serde_json::from_value(params["content"].clone()).unwrap_or_default();
                let key = edited
                    .resolve_api_key()
                    .or_else(|| in_force.resolve_api_key());
                if let Some(model) = params["content"]["model"].as_str() {
                    if model.trim() != in_force.model {
                        crate::model_catalog::check(key.clone(), model)?;
                    }
                }
                // The recheck model may be empty (rechecks off).
                if let Some(model) = params["content"]["recheck_model"].as_str() {
                    if model.trim() != in_force.recheck_model && !model.trim().is_empty() {
                        crate::model_catalog::check(key, model)?;
                    }
                }
            }
            let outcome = crate::settings::put(
                &data_directory,
                Utc::now(),
                name,
                &params["content"],
                via,
                &fix_now,
            )?;
            if !outcome.applied.is_empty() {
                tracing::info!(
                    "settings '{name}' via {via}: in force now: {}",
                    outcome.applied.join("; ")
                );
            }
            if let Some(scheduled) = &outcome.scheduled {
                tracing::info!(
                    "settings '{name}' via {via}: update {} scheduled ({}); first question at {}",
                    scheduled.id,
                    scheduled.edits.join("; "),
                    scheduled.first_question_at
                );
            }
            return serde_json::to_value(outcome).map_err(|error| error.to_string());
        }
        "settings_answer" => {
            let id = params["id"].as_str().ok_or("params.id required")?;
            let approve = params["approve"]
                .as_bool()
                .ok_or("params.approve (true or false) required")?;
            let outcome = crate::settings::answer(&data_directory, Utc::now(), id, approve)?;
            tracing::info!("settings update {id}: answered {approve}: {outcome:?}");
            return serde_json::to_value(outcome).map_err(|error| error.to_string());
        }
        "settings_withdraw" => {
            let id = params["id"].as_str().ok_or("params.id required")?;
            crate::settings::withdraw(&data_directory, Utc::now(), id)?;
            tracing::info!("settings update {id} withdrawn");
            return Ok(json!({ "ok": true }));
        }
        "program_update" => {
            let program = params["program"]
                .as_str()
                .ok_or("params.program required")?;
            let config = api.monitor.store().get();
            let user = crate::session::session_user(crate::session::current_session_id())
                .unwrap_or_default();
            let request = crate::system_care::request_update(
                &data_directory,
                &config,
                program,
                &user,
                Utc::now(),
            )?;
            tracing::info!("update of {} requested by {user}", request.program);
            return serde_json::to_value(crate::system_care::status(&data_directory, &config))
                .map_err(|error| error.to_string());
        }
        "self_update" => {
            let kind = match params["action"].as_str() {
                Some("check") => crate::self_update::RequestKind::Check,
                Some("install") => crate::self_update::RequestKind::Install,
                _ => return Err("params.action is \"check\" or \"install\"".into()),
            };
            let user = crate::session::session_user(crate::session::current_session_id())
                .unwrap_or_default();
            crate::self_update::request(&data_directory, kind, &user, Utc::now())?;
            tracing::info!("self-update {kind:?} requested by {user}");
            return serde_json::to_value(crate::self_update::status(&data_directory))
                .map_err(|error| error.to_string());
        }
        "self_update_status" => {
            return serde_json::to_value(crate::self_update::status(&data_directory))
                .map_err(|error| error.to_string());
        }
        "installed_programs_refresh" => {
            crate::system_care::request_installed_list(&data_directory)?;
            let config = api.monitor.store().get();
            return serde_json::to_value(crate::system_care::status(&data_directory, &config))
                .map_err(|error| error.to_string());
        }
        "system_care_status" => {
            let config = api.monitor.store().get();
            return serde_json::to_value(crate::system_care::status(&data_directory, &config))
                .map_err(|error| error.to_string());
        }
        "assistant_view" => {
            return serde_json::to_value(api.assistant.view()).map_err(|error| error.to_string());
        }
        "assistant_reset" => {
            api.assistant.reset()?;
            return serde_json::to_value(api.assistant.view()).map_err(|error| error.to_string());
        }
        "assistant_send" => {
            return Err("assistant_send needs the shared agent (dispatch_shared)".into());
        }
        "settings_protect" => {
            // Without a choice (older callers, tests): permanent.
            let choice: crate::settings::ProtectionChoice = match params.get("mode") {
                Some(mode) => serde_json::from_value(mode.clone())
                    .map_err(|error| format!("params.mode: {error}"))?,
                None => crate::settings::ProtectionChoice::Permanent,
            };
            let protection = crate::settings::protect(&data_directory, Utc::now(), choice)?;
            tracing::info!(
                "settings protected since {:?} ({:?})",
                protection.protected_since,
                ProtectionView::of(&protection).kind
            );
            return serde_json::to_value(ProtectionView::of(&protection))
                .map_err(|error| error.to_string());
        }
        // A time protection whose time is over, or a password protection with its password.
        // An administrator protection ends only through `settings-unlock` run as administrator.
        "settings_unprotect" => {
            let unlock = match params["password"].as_str() {
                Some(password) => crate::settings::Unlock::Password(password),
                None => crate::settings::Unlock::Press,
            };
            if let Err(error) = crate::settings::unprotect(&data_directory, Utc::now(), unlock) {
                // Slows down guessing a password.
                std::thread::sleep(Duration::from_secs(1));
                return Err(error);
            }
            api.monitor.lift_all_blocks();
            tracing::warn!("settings protection turned off; every block lifted");
            return Ok(json!({ "protected": false }));
        }
        _ => {}
    }
    match method {
        "status" => {}
        // What the user did between two local dates (inclusive): `{ "from": "YYYY-MM-DD",
        // "to": "YYYY-MM-DD" }`.
        "timetable" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let date = |key: &str| -> Result<chrono::NaiveDate, String> {
                params[key]
                    .as_str()
                    .and_then(|text| text.parse().ok())
                    .ok_or(format!("params.{key}: a date like 2026-11-03"))
            };
            let (from, to) = (date("from")?, date("to")?);
            if to < from || (to - from).num_days() > 366 {
                return Err("the period is one day to a year".into());
            }
            let start_of = |day: chrono::NaiveDate| {
                day.and_hms_opt(0, 0, 0)
                    .and_then(|midnight| midnight.and_local_timezone(chrono::Local).earliest())
                    .map(|midnight| midnight.with_timezone(&Utc))
                    .ok_or("a day without a midnight")
            };
            let entries = planner.timetable(
                start_of(from)?,
                start_of(to + chrono::Duration::days(1))?,
                Utc::now(),
            );
            return serde_json::to_value(entries).map_err(|error| error.to_string());
        }
        // Cheap, for the Focus app to ask several times a second and show its check window
        // the moment a block is made.
        "last_block" => return Ok(json!(api.monitor.state().last_block)),
        "start_session" => {
            if api.planner_active() {
                return Err(PLANNER_ON.into());
            }
            let task = params
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|task_text| !task_text.is_empty())
                .ok_or("params.task required")?
                .to_string();
            api.monitor
                .store()
                .update(|config| {
                    config.task = task;
                    config.session_active = true;
                })
                .map_err(|error| error.to_string())?;
        }
        "stop_session" => {
            if api.planner_active() {
                return Err(PLANNER_ON.into());
            }
            api.monitor
                .store()
                .update(|config| config.session_active = false)
                .map_err(|error| error.to_string())?;
        }
        "assign" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            // The situation the user wrote next to "할 일 받기" (kept for later picks too).
            if let Some(situation) = params["situation"].as_str() {
                planner
                    .set_situation(situation)
                    .map_err(|error| error.to_string())?;
            }
            planner
                .assign(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "sync_forget_device" => {
            // Another device's entry (an old one of a computer installed again, or one no
            // longer used) leaves the server's list with the next sync of this PC. It loosens
            // nothing: a computer still in use logs in again by itself.
            let data_directory = crate::data_dir();
            let id = params["id"].as_str().ok_or("params.id required")?;
            let status = sync_status(&data_directory).ok_or("이 PC는 동기화하지 않습니다")?;
            if id == status.device_id {
                return Err("이 PC 자신은 목록에서 지울 수 없습니다".into());
            }
            if !status.devices.iter().any(|(device, _)| device == id) {
                return Err(format!("'{id}'라는 기기가 목록에 없습니다"));
            }
            sync::client::ForgetRequest::add(&data_directory, id)?;
            tracing::info!("device '{id}' will be taken off the sync list");
        }
        "logout" => {
            let user = crate::login::session_user().ok_or("이 세션의 Windows 계정을 모릅니다")?;
            crate::login::log_out(&crate::data_dir(), &user)?;
            tracing::info!("{user} logged out");
            crate::login::changed();
        }
        "stop_use" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            planner
                .stop_use(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "resume_use" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            planner.resume_use().map_err(|error| error.to_string())?;
        }
        // The user pauses the current task and goes on with it (no AI: the window checks go
        // on during a pause, so it frees nothing).
        "pause_task" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            planner
                .pause(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "unpause_task" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            planner
                .unpause(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "request_change" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let request: ChangeKind = serde_json::from_value(params["request"].clone())
                .map_err(|error| format!("params.request: {error}"))?;
            let reason = params["reason"].as_str().unwrap_or_default();
            // Outside strict mode a done report needs no AI (the owner's request), so no
            // reason and no proof either.
            if matches!(request, ChangeKind::Done | ChangeKind::AlreadyDone)
                && !crate::settings::is_protected(&data_directory)
            {
                let evidence = Evidence {
                    text: params["evidence"].as_str().unwrap_or_default().to_string(),
                    screenshot_jpeg: None,
                };
                let record = planner
                    .report_without_review(Utc::now(), request, reason, evidence)
                    .map_err(|error| error.to_string())?;
                last_decision = Some(record);
                let mut status = status(api);
                status.last_decision = last_decision;
                return serde_json::to_value(status).map_err(|error| error.to_string());
            }
            // Refuse a request that cannot go to the AI before waiting for a screenshot.
            planner
                .check_change(Utc::now(), &request, reason)
                .map_err(|error| error.to_string())?;
            // A proof screenshot (for "done" only) is taken a few seconds later, so the user
            // can switch to the window that shows the result.
            let screenshot_jpeg = match params["proof_screenshot_after_secs"].as_u64() {
                Some(delay_seconds @ 1..=30)
                    if matches!(request, ChangeKind::Done | ChangeKind::AlreadyDone) =>
                {
                    thread::sleep(Duration::from_secs(delay_seconds));
                    Some(api.monitor.capture_proof()?)
                }
                _ => None,
            };
            let evidence = Evidence {
                text: params["evidence"].as_str().unwrap_or_default().to_string(),
                screenshot_jpeg,
            };
            let record = planner
                .request_change(Utc::now(), request, reason, evidence)
                .map_err(|error| error.to_string())?;
            // The list changes the AI made with an approved request: in force at once, like
            // an approved to-do change.
            if record.approved && !record.list_writes.is_empty() {
                crate::settings::apply_now(
                    &data_directory,
                    "planner",
                    &todo_list_edits(&record.list_writes),
                )?;
                planner
                    .config_store()
                    .reload()
                    .map_err(|error| error.to_string())?;
            }
            // "일이 변경되었어요": the next task comes from the list as it is now.
            if record.approved && record.request == ChangeKind::Changed {
                if let Err(error) = planner.assign(Utc::now()) {
                    tracing::warn!("no next task after a changed to-do: {error}");
                }
            }
            last_decision = Some(record);
        }
        "request_access" => {
            let target: GrantTarget = serde_json::from_value(params["target"].clone())
                .map_err(|error| format!("params.target: {error}"))?;
            let duration: GrantDuration = serde_json::from_value(params["duration"].clone())
                .map_err(|error| format!("params.duration: {error}"))?;
            let purpose = params["purpose"].as_str().unwrap_or_default();
            let entry = api
                .monitor
                .request_access(Utc::now(), target, duration, purpose)
                .map_err(|error| error.to_string())?;
            last_access = Some(entry);
        }
        "propose_todo" | "todo_change" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let change: TodoChange = if method == "propose_todo" {
                let input: ProposalInput = serde_json::from_value(params["todo"].clone())
                    .map_err(|error| format!("params.todo: {error}"))?;
                TodoChange::Add { todo: input }
            } else {
                serde_json::from_value(params["change"].clone())
                    .map_err(|error| format!("params.change: {error}"))?
            };
            let via = params["via"].as_str().unwrap_or("to-do tab");
            let reason = params["reason"].as_str().unwrap_or_default();
            // In the setup stage anyone may change anything at once, so editing and removing
            // a to-do needs no AI there (the owner's request); additions still go to the AI,
            // which also fills in the new to-do.
            let record = if crate::settings::is_protected(&data_directory) {
                planner.review_todo_change(Utc::now(), change, reason, via)
            } else {
                planner.todo_change_without_review(Utc::now(), change, reason, via)
            }
            .map_err(|error| error.to_string())?;
            if record.approved {
                // Approved (by the AI, or in the setup stage): in force at once.
                crate::settings::apply_now(
                    &data_directory,
                    "planner",
                    &todo_list_edits(&planner.writes_for(&record)),
                )?;
                planner
                    .config_store()
                    .reload()
                    .map_err(|error| error.to_string())?;
                // An edited appointment that runs now starts again on its new times.
                if let TodoChange::EditAppointment { appointment_id, .. } = &record.change {
                    planner
                        .appointment_edited(Utc::now(), appointment_id)
                        .map_err(|error| error.to_string())?;
                }
            }
            last_proposal = Some(ProposalRecord {
                at: record.at,
                item: record
                    .after
                    .clone()
                    .or_else(|| record.before.clone())
                    .unwrap_or_else(|| TodoItem::new("")),
                via: record.via.clone(),
                approved: record.approved,
                reply: record.reply.clone(),
            });
            last_todo_change = Some(record);
        }
        "task_notes_put" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let todo_id = params["todo_id"]
                .as_str()
                .ok_or("params.todo_id required")?;
            let notes: TaskNotes = serde_json::from_value(params["notes"].clone())
                .map_err(|error| format!("params.notes: {error}"))?;
            planner
                .put_task_notes(todo_id, notes)
                .map_err(|error| error.to_string())?;
        }
        "plan_week" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            planner
                .plan_week(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "todo_change_schedule" => {
            // The user's way around an AI that said no (or could not answer): the change
            // takes effect after the to-do change confirmations set in the settings.
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let id = params["id"].as_str().ok_or("params.id required")?;
            let record = planner
                .todo_change(id)
                .ok_or_else(|| format!("no to-do change '{id}'"))?;
            if record.approved {
                return Err("AI가 이미 승인해서 적용된 변경입니다".into());
            }
            // Whether to take an appointment is the AI's call (the owner's request).
            if matches!(
                record.change,
                TodoChange::AddAppointment { .. } | TodoChange::EditAppointment { .. }
            ) {
                return Err("약속은 AI가 받을지 정합니다. AI 없이 예약할 수 없습니다".into());
            }
            if record.scheduled_update.is_some() {
                return Err("이미 예약된 변경입니다".into());
            }
            let note = format!(
                "{} — reason: {} — AI: {}",
                task_planner::change_summary(&record.change, record.before.as_ref()),
                record.reason,
                record.reply
            );
            let scheduled = crate::settings::schedule(
                &data_directory,
                Utc::now(),
                "planner",
                todo_list_edits(&planner.writes_for(&record)),
                &record.via,
                &note,
            )?;
            planner
                .mark_scheduled(id, &scheduled.id)
                .map_err(|error| error.to_string())?;
            tracing::info!("to-do change {id} scheduled as update {}", scheduled.id);
            last_todo_change = planner.todo_change(id);
        }
        other => return Err(format!("unknown method '{other}'")),
    }
    let mut status = status(api);
    status.last_decision = last_decision;
    status.last_access = last_access;
    status.last_proposal = last_proposal;
    status.last_todo_change = last_todo_change;
    serde_json::to_value(status).map_err(|error| error.to_string())
}

/// Handle one request, including `assistant_send`, whose turn runs in its own thread with
/// the agent shared.
pub fn dispatch_shared(api: &Arc<AgentApi>, method: &str, params: &Value) -> Result<Value, String> {
    if method != "assistant_send" {
        return dispatch(api, method, params);
    }
    if api
        .planner
        .as_ref()
        .is_some_and(|planner| planner.stopped())
    {
        return Err(
            "사용을 멈춘 동안에는 AI 비서를 쓸 수 없습니다. 먼저 \"다시 사용하기\"를 누르세요"
                .into(),
        );
    }
    let text = params["text"]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or("params.text required")?
        .to_string();
    if !api.assistant.try_begin() {
        return Err("AI 비서가 아직 앞의 말에 답하는 중입니다".into());
    }
    let shared_api = Arc::clone(api);
    let spawned = thread::Builder::new().spawn(move || {
        let config = shared_api.monitor.store().get();
        let now_local = chrono::Local::now().format("%Y-%m-%d %A %H:%M").to_string();
        let agent = |method: &str, params: Value| dispatch(&shared_api, method, &params);
        let run_tool = |name: &str, input: &Value| {
            crate::assistant_tools::call_tool(name, input, &agent, "assistant")
        };
        shared_api
            .assistant
            .run_turn(&config, &text, &now_local, &run_tool);
    });
    if let Err(error) = spawned {
        api.assistant.release();
        return Err(format!("could not start the assistant: {error}"));
    }
    serde_json::to_value(api.assistant.view()).map_err(|error| error.to_string())
}

/// Serve forever on the port of `session` (blocking; run in a thread).
pub fn serve(api: Arc<AgentApi>, session: u32) -> std::io::Result<()> {
    let port = port_for_session(session);
    // The port may still be held by an agent that is shutting down: retry.
    let listener = loop {
        match TcpListener::bind(("127.0.0.1", port)) {
            Ok(listener) => break listener,
            Err(error) => {
                tracing::warn!("user api cannot bind 127.0.0.1:{port} ({error}); retrying");
                thread::sleep(Duration::from_secs(5));
            }
        }
    };
    tracing::info!("user api listening on 127.0.0.1:{port}");
    let active_connections = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        if active_connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active_connections.fetch_sub(1, Ordering::SeqCst);
            let _ = stream.set_write_timeout(Some(REQUEST_DEADLINE));
            let _ = reply(&mut stream, &json!({ "error": "agent busy" }));
            continue;
        }
        let (api, thread_active_connections) = (Arc::clone(&api), Arc::clone(&active_connections));
        let spawned = thread::Builder::new().spawn(move || {
            if let Err(error) = handle(&api, stream) {
                tracing::debug!("user api client error: {error}");
            }
            thread_active_connections.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            active_connections.fetch_sub(1, Ordering::SeqCst);
        }
    }
    Ok(())
}

/// Most simultaneous clients; the API runs in a SYSTEM process any local
/// user can connect to, so it must not be possible to exhaust it.
const MAX_CONNECTIONS: usize = 16;
/// The whole request line must arrive within this time.
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);

fn handle(api: &Arc<AgentApi>, mut stream: TcpStream) -> std::io::Result<()> {
    // Answers may wait for the AI, so the write timeout is generous.
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let response = match read_line(&mut stream) {
        Ok(line) => match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let method = request["method"].as_str().unwrap_or_default();
                match dispatch_shared(api, method, &request["params"]) {
                    Ok(result) => json!({ "result": result }),
                    Err(message) => json!({ "error": message }),
                }
            }
            Err(error) => json!({ "error": format!("parse error: {error}") }),
        },
        Err(error) => json!({ "error": error }),
    };
    reply(&mut stream, &response)
}

/// One request line, within [`REQUEST_DEADLINE`] overall and
/// [`MAX_REQUEST`] bytes (a per-read timeout alone lets a slow client keep
/// the connection for minutes).
fn read_line(stream: &mut TcpStream) -> Result<String, String> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let time_left = deadline.saturating_duration_since(Instant::now());
        if time_left.is_zero() {
            return Err("request timed out".into());
        }
        stream
            .set_read_timeout(Some(time_left))
            .map_err(|error| error.to_string())?;
        let bytes_read = stream
            .read(&mut chunk)
            .map_err(|_| "request timed out".to_string())?;
        if bytes_read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..bytes_read]);
        if buffer.contains(&b'\n') {
            break;
        }
        if buffer.len() > MAX_REQUEST {
            return Err("request too large".into());
        }
    }
    let end = buffer
        .iter()
        .position(|&byte| byte == b'\n')
        .unwrap_or(buffer.len());
    String::from_utf8(buffer[..end].to_vec()).map_err(|_| "request is not UTF-8".into())
}

fn reply(stream: &mut TcpStream, value: &Value) -> std::io::Result<()> {
    stream.write_all(serde_json::to_string(value)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Client side, used by the Focus app.
pub fn call(session: u32, method: &str, params: Value) -> Result<Status, String> {
    serde_json::from_value(call_value(session, method, params)?).map_err(|error| error.to_string())
}

/// Like [`call`] for methods whose result is not a [`Status`] (`settings_*`).
pub fn call_value(session: u32, method: &str, params: Value) -> Result<Value, String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port_for_session(session)));
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_millis(500)).map_err(|error| {
            format!(
                "agent not running for this session (session {session}, port {}: {error}). \
                 Is the service installed and this account managed? An administrator can \
                 read C:\\ProgramData\\DigitalWellbeing\\logs\\service.log and \
                 agent-session{session}.log.",
                address.port()
            )
        })?;
    stream
        // assign / request_change wait for the AI; an approved "done" makes two
        // calls of up to 90 s each, so allow more than that.
        .set_read_timeout(Some(Duration::from_secs(300)))
        .map_err(|error| error.to_string())?;
    let request = json!({ "method": method, "params": params });
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let mut response: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
    if let Some(error) = response.get("error").and_then(Value::as_str) {
        return Err(error.to_string());
    }
    Ok(response["result"].take())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_focus_rule::{platform, AiFocusConfig, AiFocusStore, JudgeError};

    #[test]
    fn start_status_stop_over_tcp() {
        let monitor = Arc::new(FocusMonitor::new(
            AiFocusStore::in_memory(AiFocusConfig::default()),
            platform::native(),
            Arc::new(|_: &AiFocusConfig| Err(JudgeError::NoApiKey)),
        ));
        let api = Arc::new(AgentApi {
            enforcer: None,
            monitor,
            planner: None,
            assistant: Default::default(),
        });
        // A session id far from real ones so the test port is free.
        let session = 900;
        thread::spawn(move || serve(api, session));
        thread::sleep(Duration::from_millis(200));

        let status = call(session, "start_session", json!({ "task": "논문 쓰기" })).unwrap();
        assert!(status.session_active);
        assert_eq!(status.task, "논문 쓰기");
        assert!(call(session, "start_session", json!({ "task": " " })).is_err());
        let status = call(session, "stop_session", json!({})).unwrap();
        assert!(!status.session_active);
        assert!(call(session, "set_blacklist", json!({})).is_err());
        assert!(call(session, "assign", json!({})).is_err());
        // No focus session running: access requests are refused before any AI call.
        let error = call(
            session,
            "request_access",
            json!({
                "target": { "kind": "site", "keyword": "youtube" },
                "duration": { "kind": "minutes", "minutes": 30 },
                "purpose": "Rust 강의 시청"
            }),
        )
        .unwrap_err();
        assert!(error.contains("데일리 웰빙"));
        let status = call(session, "status", json!({})).unwrap();
        assert!(status.grants.is_empty());
    }

    #[test]
    fn a_client_that_never_finishes_its_request_is_cut_off() {
        let monitor = Arc::new(FocusMonitor::new(
            AiFocusStore::in_memory(AiFocusConfig::default()),
            platform::native(),
            Arc::new(|_: &AiFocusConfig| Err(JudgeError::NoApiKey)),
        ));
        let api = Arc::new(AgentApi {
            enforcer: None,
            monitor,
            planner: None,
            assistant: Default::default(),
        });
        let session = 901;
        thread::spawn(move || serve(api, session));
        thread::sleep(Duration::from_millis(200));
        let mut stream = TcpStream::connect(("127.0.0.1", port_for_session(session))).unwrap();
        stream.write_all(b"{\"method\":").unwrap(); // and then nothing
        let started = Instant::now();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        assert!(line.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    fn free_task_and_stop_are_refused_while_planner_is_active() {
        use task_planner::{JsonStore, PlannerConfig, PlannerState, TodoItem};
        let monitor = Arc::new(FocusMonitor::new(
            AiFocusStore::in_memory(AiFocusConfig::default()),
            platform::native(),
            Arc::new(|_: &AiFocusConfig| Err(JudgeError::NoApiKey)),
        ));
        let planner = Arc::new(TaskPlanner::new(
            JsonStore::in_memory(PlannerConfig {
                enabled: true,
                todos: vec![TodoItem::new("논문 3장")],
                ..Default::default()
            }),
            JsonStore::in_memory(PlannerState::default()),
            Arc::clone(&monitor),
            Arc::new(|_: &AiFocusConfig| Err(JudgeError::NoApiKey)),
        ));
        let api = AgentApi {
            enforcer: None,
            monitor,
            planner: Some(planner),
            assistant: Default::default(),
        };
        let error = dispatch(&api, "start_session", &json!({ "task": "유튜브" })).unwrap_err();
        assert!(error.contains("플래너"));
        assert!(dispatch(&api, "stop_session", &json!({})).is_err());
        let status = dispatch(&api, "status", &json!({})).unwrap();
        assert_eq!(status["planner"]["active"], true);
        // The AI call fails: nothing changes.
        assert!(dispatch(&api, "assign", &json!({})).is_err());
        assert_eq!(
            dispatch(&api, "status", &json!({})).unwrap()["session_active"],
            false
        );
        // A change the AI decides (an appointment: even outside strict mode) reaches the
        // planner; the AI cannot review it here, so it is logged and the list does not
        // change. Malformed params are refused before that.
        let start = Utc::now() + chrono::Duration::hours(2);
        let status = dispatch(
            &api,
            "todo_change",
            &json!({ "change": { "kind": "add_appointment", "appointment": {
                "title": "치과", "start": start, "end": start + chrono::Duration::hours(1),
            } }, "via": "claude desktop" }),
        )
        .unwrap();
        assert_eq!(status["last_todo_change"]["ai_failed"], true);
        assert_eq!(status["last_todo_change"]["approved"], false);
        assert_eq!(status["planner"]["open_todos"].as_array().unwrap().len(), 1);
        assert!(dispatch(&api, "propose_todo", &json!({ "todo": 5 }))
            .unwrap_err()
            .starts_with("params.todo"));
    }
}

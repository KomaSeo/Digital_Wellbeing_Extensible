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
//! | `assign`         | `{}` (planner active)                    | [`Status`] |
//! | `request_change` | `{ "request": ChangeKind, "reason": "…" }` | [`Status`], `last_decision` set |
//! | `request_access` | `{ "target": GrantTarget, "duration": GrantDuration, "purpose": "…" }` | [`Status`], `last_access` set |
//! | `propose_todo`   | `{ "todo": ProposalInput, "via": "…" }` (= `todo_change` add) | [`Status`], `last_proposal` set |
//! | `todo_change`    | `{ "change": TodoChange, "reason": "…", "via": "…" }` | [`Status`], `last_todo_change` set; approved = in force |
//! | `todo_change_schedule` | `{ "id": "…" }` (a change the AI did not approve) | [`Status`], `last_todo_change` set |
//! | `task_notes_put` | `{ "todo_id": "…", "notes": TaskNotes }` | [`Status`] |
//! | `plan_day`       | `{}` (one AI call)                       | [`Status`] with `planner.day_plan` |
//! | `settings_get`   | `{}`                                     | [`crate::settings::SettingsSnapshot`] |
//! | `settings_put`   | `{ "name": "policy"\|"ai_focus"\|"planner", "content": {…}, "via": "…" }` | [`crate::settings::PutOutcome`] (what is in force now, what was scheduled) |
//! | `settings_answer` | `{ "id": "…", "approve": true\|false }` (once the question is due) | [`crate::settings::AnswerOutcome`] |
//! | `settings_withdraw` | `{ "id": "…" }`                        | `{ "ok": true }` |
//! | `settings_protect` | `{}` (one way)                         | [`crate::settings::Protection`] |
//!
//! The `settings_*` methods let a standard user edit the admin settings: during setup an
//! edit takes effect at once; once protected it is scheduled and takes effect only after
//! three "yes" answers two days apart; see [`crate::settings`].
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
    ChangeKind, ChangeRecord, PlannerStatus, ProposalInput, ProposalRecord, TaskNotes, TaskPlanner,
    TodoChange, TodoChangeRecord, TodoListWrite,
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
    /// Scheduled settings updates whose "is this update really right?" question is due.
    #[serde(default)]
    pub settings_questions_due: usize,
    /// Estimated cost of today's Claude calls on this PC, and the admin's daily
    /// budget for focus checks (`0` = none).
    #[serde(default)]
    pub ai_cost_today_usd: Option<f64>,
    #[serde(default)]
    pub daily_budget_usd: f64,
}

/// Responsibility: Hold what the agent exposes (focus monitor, optional planner) and tell
/// whether the planner restricts a request.
pub struct AgentApi {
    pub monitor: Arc<FocusMonitor>,
    pub planner: Option<Arc<TaskPlanner>>,
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
        settings_questions_due: crate::settings::questions_due(&crate::data_dir(), Utc::now()),
        ai_cost_today_usd: monitor.usage().map(|usage| usage.today_cost()),
        daily_budget_usd: config.daily_budget_usd,
    }
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
            TodoListWrite::Remove(todo_id) => Edit::PutListItem {
                path: vec!["todos".into()],
                id: todo_id.clone(),
                item: None,
            },
            TodoListWrite::Dismiss(todo_id) => Edit::AddItems {
                path: vec!["dismissed".into()],
                items: vec![Value::String(todo_id.clone())],
            },
        })
        .collect()
}

/// Handle one request.
pub fn dispatch(api: &AgentApi, method: &str, params: &Value) -> Result<Value, String> {
    const PLANNER_ON: &str = "the task planner is on: use assign / request_change (needs a reason)";
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
            let outcome =
                crate::settings::put(&data_directory, Utc::now(), name, &params["content"], via)?;
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
        "settings_protect" => {
            let protection = crate::settings::protect(&data_directory, Utc::now())?;
            tracing::info!("settings protected since {:?}", protection.protected_since);
            return serde_json::to_value(protection).map_err(|error| error.to_string());
        }
        _ => {}
    }
    match method {
        "status" => {}
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
            planner
                .assign(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "request_change" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let request: ChangeKind = serde_json::from_value(params["request"].clone())
                .map_err(|error| format!("params.request: {error}"))?;
            let reason = params["reason"].as_str().unwrap_or_default();
            let record = planner
                .request_change(Utc::now(), request, reason)
                .map_err(|error| error.to_string())?;
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
            let record = planner
                .review_todo_change(Utc::now(), change, reason, via)
                .map_err(|error| error.to_string())?;
            if record.approved {
                // Approved by the AI: in force at once, protected or not.
                crate::settings::apply_now(
                    &data_directory,
                    "planner",
                    &todo_list_edits(&planner.writes_for(&record)),
                )?;
                planner
                    .config_store()
                    .reload()
                    .map_err(|error| error.to_string())?;
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
        "plan_day" => {
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            planner
                .plan_day(Utc::now())
                .map_err(|error| error.to_string())?;
        }
        "todo_change_schedule" => {
            // The user's way around an AI that said no (or could not answer): the change
            // takes effect after three confirmations, two hours apart.
            let planner = api.planner.as_ref().ok_or("no task planner")?;
            let id = params["id"].as_str().ok_or("params.id required")?;
            let record = planner
                .todo_change(id)
                .ok_or_else(|| format!("no to-do change '{id}'"))?;
            if record.approved {
                return Err("the AI approved this change; it is already in force".into());
            }
            if record.scheduled_update.is_some() {
                return Err("this change is already scheduled".into());
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
                crate::settings::TODO_QUESTION_INTERVAL_SECONDS,
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

fn handle(api: &AgentApi, mut stream: TcpStream) -> std::io::Result<()> {
    // Answers may wait for the AI, so the write timeout is generous.
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let response = match read_line(&mut stream) {
        Ok(line) => match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                let method = request["method"].as_str().unwrap_or_default();
                match dispatch(api, method, &request["params"]) {
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
            monitor,
            planner: None,
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
        assert!(error.contains("focus session"));
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
            monitor,
            planner: None,
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
            monitor,
            planner: Some(planner),
        };
        let error = dispatch(&api, "start_session", &json!({ "task": "유튜브" })).unwrap_err();
        assert!(error.contains("planner"));
        assert!(dispatch(&api, "stop_session", &json!({})).is_err());
        let status = dispatch(&api, "status", &json!({})).unwrap();
        assert_eq!(status["planner"]["active"], true);
        // The AI call fails: nothing changes.
        assert!(dispatch(&api, "assign", &json!({})).is_err());
        assert_eq!(
            dispatch(&api, "status", &json!({})).unwrap()["session_active"],
            false
        );
        // A proposal reaches the planner; the AI cannot review it here, so it is logged
        // (the user may schedule it) and the list does not change. Malformed params are
        // refused before that.
        let status = dispatch(
            &api,
            "propose_todo",
            &json!({ "todo": { "title": "장보기" }, "via": "claude desktop" }),
        )
        .unwrap();
        assert_eq!(status["last_todo_change"]["ai_failed"], true);
        assert_eq!(status["last_proposal"]["approved"], false);
        assert_eq!(status["planner"]["open_todos"].as_array().unwrap().len(), 1);
        assert!(dispatch(&api, "propose_todo", &json!({ "todo": 5 }))
            .unwrap_err()
            .starts_with("params.todo"));
    }
}

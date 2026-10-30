//! Local API between a session's agent (server) and the Focus app (client).
//!
//! The agent runs as SYSTEM, so the user cannot reach its settings; this API
//! exposes only what the user is meant to control: their own focus session.
//! Admin settings (lists, AI settings) are changed through the files in
//! [`crate::data_dir`], which standard users cannot write.
//!
//! Transport: TCP on `127.0.0.1:(BASE_PORT + session id)`, one JSON request
//! per connection, one JSON line back — same framing as `tool-api`.
//!
//! | method          | params             | result                    |
//! |-----------------|--------------------|---------------------------|
//! | `status`        | `{}`               | [`Status`]                |
//! | `start_session` | `{ "task": "..." }`| [`Status`]                |
//! | `stop_session`  | `{}`               | [`Status`]                |

use ai_focus_rule::{FocusMonitor, VerdictLogEntry};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub const BASE_PORT: u16 = 8770;
const MAX_REQUEST: usize = 64 * 1024;

pub fn port_for_session(session: u32) -> u16 {
    BASE_PORT.saturating_add(session.min(1000) as u16)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockedItem {
    /// Process name, or `process|tab title` for a browser tab.
    pub key: String,
    pub reason: String,
    pub until: Option<DateTime<Utc>>,
}

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
}

fn status(monitor: &FocusMonitor) -> Status {
    let config = monitor.store().get();
    let state = monitor.state();
    let blocked = state
        .blocked_processes
        .iter()
        .chain(state.blocked_tabs.iter())
        .map(|(key, b)| BlockedItem {
            key: key.clone(),
            reason: b.reason.clone(),
            until: b.until,
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
    }
}

/// Handle one request against the agent's monitor.
pub fn dispatch(monitor: &FocusMonitor, method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "status" => {}
        "start_session" => {
            let task = params
                .get("task")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or("params.task required")?
                .to_string();
            monitor
                .store()
                .update(|c| {
                    c.task = task;
                    c.session_active = true;
                })
                .map_err(|e| e.to_string())?;
        }
        "stop_session" => {
            monitor
                .store()
                .update(|c| c.session_active = false)
                .map_err(|e| e.to_string())?;
        }
        other => return Err(format!("unknown method '{other}'")),
    }
    serde_json::to_value(status(monitor)).map_err(|e| e.to_string())
}

/// Serve forever on the port of `session` (blocking; run in a thread).
pub fn serve(monitor: Arc<FocusMonitor>, session: u32) -> std::io::Result<()> {
    let port = port_for_session(session);
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    tracing::info!("user api listening on 127.0.0.1:{port}");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let monitor = Arc::clone(&monitor);
        thread::spawn(move || {
            if let Err(err) = handle(&monitor, stream) {
                tracing::debug!("user api client error: {err}");
            }
        });
    }
    Ok(())
}

fn handle(monitor: &FocusMonitor, mut stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new((&stream).take(MAX_REQUEST as u64)).read_line(&mut line)?;
    let response = match serde_json::from_str::<Value>(&line) {
        Ok(req) => {
            let method = req["method"].as_str().unwrap_or_default();
            match dispatch(monitor, method, &req["params"]) {
                Ok(result) => json!({ "result": result }),
                Err(message) => json!({ "error": message }),
            }
        }
        Err(err) => json!({ "error": format!("parse error: {err}") }),
    };
    stream.write_all(serde_json::to_string(&response)?.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// Client side, used by the Focus app.
pub fn call(session: u32, method: &str, params: Value) -> Result<Status, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port_for_session(session)));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
        .map_err(|_| "agent not running for this session (is the service installed?)".to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|e| e.to_string())?;
    let request = json!({ "method": method, "params": params });
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let mut response: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if let Some(err) = response.get("error").and_then(Value::as_str) {
        return Err(err.to_string());
    }
    serde_json::from_value(response["result"].take()).map_err(|e| e.to_string())
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
        // A session id far from real ones so the test port is free.
        let session = 900;
        let server = Arc::clone(&monitor);
        thread::spawn(move || serve(server, session));
        thread::sleep(Duration::from_millis(200));

        let s = call(session, "start_session", json!({ "task": "논문 쓰기" })).unwrap();
        assert!(s.session_active);
        assert_eq!(s.task, "논문 쓰기");
        assert!(call(session, "start_session", json!({ "task": " " })).is_err());
        let s = call(session, "stop_session", json!({})).unwrap();
        assert!(!s.session_active);
        assert!(call(session, "set_blacklist", json!({})).is_err());
    }
}

/// Re-exported so clients need not depend on `ai_focus_rule`.
pub use ai_focus_rule::Verdict;

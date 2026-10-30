//! Responsibility: Serve the standalone JSON-RPC API on 127.0.0.1: parse requests and route
//! them to the enforcer and ListStore.

use enforcer::Enforcer;
use list_based_rule::{ListConfig, ListMode, ListStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Local function-call endpoint (JSON-RPC over TCP).
pub const DEFAULT_PORT: u16 = 8765;

/// Responsibility: Parse one JSON-RPC request.
#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Responsibility: Serialize one JSON-RPC response.
#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorBody>,
}

/// Responsibility: Describe the error part of a JSON-RPC response.
#[derive(Debug, Serialize)]
pub struct RpcErrorBody {
    pub code: i32,
    pub message: String,
}

/// Responsibility: Report why a method call failed.
#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
}

/// Responsibility: Map method names to enforcer and list-store operations.
#[derive(Clone)]
pub struct ToolRouter {
    enforcer: Arc<Enforcer>,
    store: ListStore,
}

impl ToolRouter {
    pub fn new(enforcer: Arc<Enforcer>, store: ListStore) -> Self {
        Self { enforcer, store }
    }

    pub fn store(&self) -> &ListStore {
        &self.store
    }

    pub fn enforcer(&self) -> &Arc<Enforcer> {
        &self.enforcer
    }

    pub fn handle_value(&self, request: Value) -> Value {
        match serde_json::from_value::<RpcRequest>(request) {
            Ok(req) => serde_json::to_value(self.handle(req)).unwrap_or_else(|e| {
                json!({
                    "id": null,
                    "error": { "code": -32603, "message": e.to_string() }
                })
            }),
            Err(err) => json!({
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {err}") }
            }),
        }
    }

    pub fn handle(&self, req: RpcRequest) -> RpcResponse {
        match self.dispatch(&req.method, req.params) {
            Ok(result) => RpcResponse {
                id: req.id,
                result: Some(result),
                error: None,
            },
            Err(err) => RpcResponse {
                id: req.id,
                result: None,
                error: Some(RpcErrorBody {
                    code: -32000,
                    message: err.to_string(),
                }),
            },
        }
    }

    pub fn dispatch(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        match method {
            "get_policy" | "get_list_config" => {
                let config = self.store().get();
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "set_mode" => {
                let mode = parse_mode(&params)?;
                let config = self.store().set_mode(mode).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "add_blacklist" => {
                let name = require_name(&params)?;
                let config = self.store().add_blacklist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "remove_blacklist" => {
                let name = require_name(&params)?;
                let config = self.store().remove_blacklist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "add_whitelist" => {
                let name = require_name(&params)?;
                let config = self.store().add_whitelist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "remove_whitelist" => {
                let name = require_name(&params)?;
                let config = self.store().remove_whitelist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "set_blacklist" => {
                let names = require_names(&params)?;
                let config = self.store().set_blacklist(&names).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "set_whitelist" => {
                let names = require_names(&params)?;
                let config = self.store().set_whitelist(&names).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "list_processes" => {
                let list = self.enforcer.list_processes();
                Ok(serde_json::to_value(list).map_err(to_tool)?)
            }
            "list_blocks" => {
                let limit = params
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50) as usize;
                let list = self.enforcer.recent_blocks(limit);
                Ok(serde_json::to_value(list).map_err(to_tool)?)
            }
            "list_rules" => {
                let list = self.enforcer.list_rules();
                Ok(serde_json::to_value(list).map_err(to_tool)?)
            }
            "save_strategy" => {
                let name = require_name(&params)?;
                let config = self.store().save_strategy(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "apply_strategy" => {
                let name = require_name(&params)?;
                let config = self.store().apply_strategy(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "replace_policy" | "replace_list_config" => {
                let config: ListConfig = serde_json::from_value(params).map_err(to_tool)?;
                let config = self.store().replace_config(config).map_err(to_tool)?;
                Ok(serde_json::to_value(config).map_err(to_tool)?)
            }
            "ping" => Ok(json!({ "ok": true })),
            other => Err(ToolError::Message(format!("unknown method: {other}"))),
        }
    }
}

fn to_tool<E: std::fmt::Display>(err: E) -> ToolError {
    ToolError::Message(err.to_string())
}

fn require_name(params: &Value) -> Result<String, ToolError> {
    params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolError::Message("params.name (string) required".into()))
}

fn require_names(params: &Value) -> Result<Vec<String>, ToolError> {
    let arr = params
        .get("names")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ToolError::Message("params.names (string array) required".into()))?;
    let mut names = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(s) = item.as_str() else {
            return Err(ToolError::Message(
                "params.names must be an array of strings".into(),
            ));
        };
        names.push(s.to_string());
    }
    Ok(names)
}

fn parse_mode(params: &Value) -> Result<ListMode, ToolError> {
    let raw = params
        .get("mode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::Message("params.mode required".into()))?;
    match raw.to_lowercase().as_str() {
        "blacklist" => Ok(ListMode::Blacklist),
        "whitelist" => Ok(ListMode::Whitelist),
        other => Err(ToolError::Message(format!(
            "invalid mode '{other}', expected blacklist|whitelist"
        ))),
    }
}

/// Most simultaneous connections; more are refused with an error reply.
const MAX_CONNECTIONS: usize = 16;
/// A whole request must arrive within this time (a slow client cannot hold a thread).
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 1_000_000;

/// Bind `127.0.0.1:port` now (so the caller learns about a busy port) and
/// serve on a background thread.
pub fn spawn_tcp(router: ToolRouter, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    tracing::info!("tool-api listening on 127.0.0.1:{port}");
    thread::Builder::new()
        .name("tool-api".into())
        .spawn(move || serve(router, listener))?;
    Ok(())
}

fn serve(router: ToolRouter, listener: TcpListener) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                tracing::warn!("accept error: {err}");
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = reply(&mut stream, &error_value(-32000, "server busy"));
            continue;
        }
        let (router, active_conn) = (router.clone(), Arc::clone(&active));
        let spawned = thread::Builder::new().spawn(move || {
            if let Err(err) = handle_stream(&router, &mut stream) {
                tracing::debug!("client error: {err}");
            }
            active_conn.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Read until the first complete JSON value (trailing data is ignored, so two
/// requests sent together do not hang), answer it, close.
fn handle_stream(router: &ToolRouter, stream: &mut TcpStream) -> std::io::Result<()> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    stream.set_write_timeout(Some(REQUEST_DEADLINE))?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return reply(stream, &error_value(-32700, "request timed out"));
        }
        stream.set_read_timeout(Some(left))?;
        let n = match stream.read(&mut tmp) {
            Ok(n) => n,
            Err(err) if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return reply(stream, &error_value(-32700, "request timed out"));
            }
            Err(err) => return Err(err),
        };
        buf.extend_from_slice(&tmp[..n]);
        match serde_json::Deserializer::from_slice(&buf).into_iter::<Value>().next() {
            Some(Ok(value)) => return reply(stream, &router.handle_value(value)),
            Some(Err(err)) if !err.is_eof() => {
                return reply(stream, &error_value(-32700, &format!("parse error: {err}")));
            }
            _ if n == 0 => return reply(stream, &error_value(-32700, "incomplete request")),
            _ if buf.len() > MAX_REQUEST_BYTES => {
                return reply(stream, &error_value(-32600, "request too large"));
            }
            _ => {}
        }
    }
}

fn error_value(code: i32, message: &str) -> Value {
    json!({ "id": null, "error": { "code": code, "message": message } })
}

fn reply(stream: &mut TcpStream, value: &Value) -> std::io::Result<()> {
    stream.write_all(&serde_json::to_vec(value)?)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use list_based_rule::MemoryListPersistence;
    use std::io::{BufRead, BufReader};
    use std::net::Shutdown;

    fn server() -> u16 {
        let store = ListStore::with_persistence(Arc::new(MemoryListPersistence::new())).unwrap();
        let router = ToolRouter::new(Arc::new(Enforcer::new()), store);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        thread::spawn(move || serve(router, listener));
        port
    }

    fn send(port: u16, payload: &[u8], half_close: bool) -> Value {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.write_all(payload).unwrap();
        if half_close {
            s.shutdown(Shutdown::Write).unwrap();
        }
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn answers_like_the_powershell_client_expects() {
        let port = server();
        let v = send(port, br#"{"id":1,"method":"ping","params":{}}"#, true);
        assert_eq!(v["id"], 1);
        assert!(v.get("error").is_none());
    }

    #[test]
    fn two_requests_in_one_write_do_not_hang() {
        let port = server();
        let v = send(port, br#"{"id":1,"method":"ping"}{"id":2,"method":"ping"}"#, false);
        assert_eq!(v["id"], 1);
    }

    #[test]
    fn bad_and_incomplete_input_get_an_error_reply() {
        let port = server();
        assert!(send(port, b"nonsense", true)["error"].is_object());
        assert!(send(port, br#"{"id":1,"meth"#, true)["error"].is_object());
    }

    #[test]
    fn busy_port_is_reported() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        let store = ListStore::with_persistence(Arc::new(MemoryListPersistence::new())).unwrap();
        let router = ToolRouter::new(Arc::new(Enforcer::new()), store);
        assert!(spawn_tcp(router, port).is_err());
    }
}

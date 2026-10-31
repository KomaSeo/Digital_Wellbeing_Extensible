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
            Ok(rpc_request) => {
                serde_json::to_value(self.handle(rpc_request)).unwrap_or_else(|error| {
                    json!({
                        "id": null,
                        "error": { "code": -32603, "message": error.to_string() }
                    })
                })
            }
            Err(error) => json!({
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {error}") }
            }),
        }
    }

    pub fn handle(&self, request: RpcRequest) -> RpcResponse {
        match self.dispatch(&request.method, request.params) {
            Ok(result) => RpcResponse {
                id: request.id,
                result: Some(result),
                error: None,
            },
            Err(error) => RpcResponse {
                id: request.id,
                result: None,
                error: Some(RpcErrorBody {
                    code: -32000,
                    message: error.to_string(),
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
                    .and_then(|value| value.as_u64())
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

fn to_tool<E: std::fmt::Display>(error: E) -> ToolError {
    ToolError::Message(error.to_string())
}

fn require_name(params: &Value) -> Result<String, ToolError> {
    params
        .get("name")
        .and_then(|value| value.as_str())
        .map(|name| name.to_string())
        .ok_or_else(|| ToolError::Message("params.name (string) required".into()))
}

fn require_names(params: &Value) -> Result<Vec<String>, ToolError> {
    let name_values = params
        .get("names")
        .and_then(|value| value.as_array())
        .ok_or_else(|| ToolError::Message("params.names (string array) required".into()))?;
    let mut names = Vec::with_capacity(name_values.len());
    for item in name_values {
        let Some(name) = item.as_str() else {
            return Err(ToolError::Message(
                "params.names must be an array of strings".into(),
            ));
        };
        names.push(name.to_string());
    }
    Ok(names)
}

fn parse_mode(params: &Value) -> Result<ListMode, ToolError> {
    let raw = params
        .get("mode")
        .and_then(|value| value.as_str())
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
    let active_connections = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!("accept error: {error}");
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        if active_connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active_connections.fetch_sub(1, Ordering::SeqCst);
            let _ = reply(&mut stream, &error_value(-32000, "server busy"));
            continue;
        }
        let (router, thread_active_connections) = (router.clone(), Arc::clone(&active_connections));
        let spawned = thread::Builder::new().spawn(move || {
            if let Err(error) = handle_stream(&router, &mut stream) {
                tracing::debug!("client error: {error}");
            }
            thread_active_connections.fetch_sub(1, Ordering::SeqCst);
        });
        if spawned.is_err() {
            active_connections.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Read until the first complete JSON value (trailing data is ignored, so two
/// requests sent together do not hang), answer it, close.
fn handle_stream(router: &ToolRouter, stream: &mut TcpStream) -> std::io::Result<()> {
    let deadline = Instant::now() + REQUEST_DEADLINE;
    stream.set_write_timeout(Some(REQUEST_DEADLINE))?;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let time_left = deadline.saturating_duration_since(Instant::now());
        if time_left.is_zero() {
            return reply(stream, &error_value(-32700, "request timed out"));
        }
        stream.set_read_timeout(Some(time_left))?;
        let bytes_read = match stream.read(&mut chunk) {
            Ok(count) => count,
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return reply(stream, &error_value(-32700, "request timed out"));
            }
            Err(error) => return Err(error),
        };
        buffer.extend_from_slice(&chunk[..bytes_read]);
        match serde_json::Deserializer::from_slice(&buffer)
            .into_iter::<Value>()
            .next()
        {
            Some(Ok(value)) => return reply(stream, &router.handle_value(value)),
            Some(Err(error)) if !error.is_eof() => {
                return reply(
                    stream,
                    &error_value(-32700, &format!("parse error: {error}")),
                );
            }
            _ if bytes_read == 0 => {
                return reply(stream, &error_value(-32700, "incomplete request"))
            }
            _ if buffer.len() > MAX_REQUEST_BYTES => {
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
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.write_all(payload).unwrap();
        if half_close {
            stream.shutdown(Shutdown::Write).unwrap();
        }
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn answers_like_the_powershell_client_expects() {
        let port = server();
        let response = send(port, br#"{"id":1,"method":"ping","params":{}}"#, true);
        assert_eq!(response["id"], 1);
        assert!(response.get("error").is_none());
    }

    #[test]
    fn two_requests_in_one_write_do_not_hang() {
        let port = server();
        let response = send(
            port,
            br#"{"id":1,"method":"ping"}{"id":2,"method":"ping"}"#,
            false,
        );
        assert_eq!(response["id"], 1);
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

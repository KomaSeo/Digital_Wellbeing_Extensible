use enforcer::Enforcer;
use policy::{EnforcementMode, Policy, PolicyStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use thiserror::Error;

/// Local function-call endpoint (JSON-RPC over TCP).
pub const DEFAULT_PORT: u16 = 8765;

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorBody>,
}

#[derive(Debug, Serialize)]
pub struct RpcErrorBody {
    pub code: i32,
    pub message: String,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
}

#[derive(Clone)]
pub struct ToolRouter {
    enforcer: Arc<Enforcer>,
    store: PolicyStore,
}

impl ToolRouter {
    pub fn new(enforcer: Arc<Enforcer>, store: PolicyStore) -> Self {
        Self { enforcer, store }
    }

    pub fn store(&self) -> &PolicyStore {
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
            "get_policy" => {
                let policy = self.store().get();
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "set_mode" => {
                let mode = parse_mode(&params)?;
                let policy = self.store().set_mode(mode).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "add_blacklist" => {
                let name = require_name(&params)?;
                let policy = self.store().add_blacklist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "remove_blacklist" => {
                let name = require_name(&params)?;
                let policy = self.store().remove_blacklist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "add_whitelist" => {
                let name = require_name(&params)?;
                let policy = self.store().add_whitelist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "remove_whitelist" => {
                let name = require_name(&params)?;
                let policy = self.store().remove_whitelist(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "set_blacklist" => {
                let names = require_names(&params)?;
                let policy = self.store().set_blacklist(&names).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "set_whitelist" => {
                let names = require_names(&params)?;
                let policy = self.store().set_whitelist(&names).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
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
                let policy = self.store().save_strategy(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "apply_strategy" => {
                let name = require_name(&params)?;
                let policy = self.store().apply_strategy(&name).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
            }
            "replace_policy" => {
                let policy: Policy = serde_json::from_value(params).map_err(to_tool)?;
                let policy = self.store().replace_policy(policy).map_err(to_tool)?;
                Ok(serde_json::to_value(policy).map_err(to_tool)?)
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

fn parse_mode(params: &Value) -> Result<EnforcementMode, ToolError> {
    let raw = params
        .get("mode")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::Message("params.mode required".into()))?;
    match raw.to_lowercase().as_str() {
        "blacklist" => Ok(EnforcementMode::Blacklist),
        "whitelist" => Ok(EnforcementMode::Whitelist),
        other => Err(ToolError::Message(format!(
            "invalid mode '{other}', expected blacklist|whitelist"
        ))),
    }
}

pub fn serve_tcp(router: ToolRouter, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    tracing::info!("tool-api listening on 127.0.0.1:{port}");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let router = router.clone();
                thread::spawn(move || {
                    if let Err(err) = handle_stream(&router, &mut stream) {
                        tracing::debug!("client error: {err}");
                    }
                });
            }
            Err(err) => {
                tracing::warn!("accept error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    Ok(())
}

fn handle_stream(router: &ToolRouter, stream: &mut (impl Read + Write)) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Ok(value) = serde_json::from_slice::<Value>(&buf) {
            let response = router.handle_value(value);
            let bytes = serde_json::to_vec(&response)?;
            stream.write_all(&bytes)?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            break;
        }
        if buf.len() > 1_000_000 {
            break;
        }
    }
    Ok(())
}

pub fn spawn_tcp(router: ToolRouter, port: u16) -> mpsc::Sender<()> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = rx;
        if let Err(err) = serve_tcp(router, port) {
            tracing::error!("tool-api server failed: {err}");
        }
    });
    tx
}

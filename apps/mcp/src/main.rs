//! Responsibility: Let the user's AI assistant (Claude Desktop) manage their to-dos through the
//! agent of their session: an MCP server whose tools do only what the Focus app can do.
//!
//! Claude Desktop starts this program in the user's own account and talks to it over
//! stdin/stdout (MCP stdio transport: one JSON-RPC 2.0 message per line). Every tool is a
//! call to [`machine::user_api`], the same API the Focus app uses, so the assistant gains no
//! power the user does not already have. A proposed to-do joins the list only if the agent's
//! own AI approves it against the owner's life rules. The assistant may also schedule an
//! update of the settings (life rules, the planner's AI instructions, any other setting) once
//! they are protected; such an update takes effect only after the user answered "is this
//! update really right?" with yes three times, two days apart, in the Focus app. No tool
//! answers that question.
//!
//! Setup and tool reference (Korean): `docs/claude_desktop.md`.

use machine::assistant_tools::{call_tool, tools, Agent, INSTRUCTIONS};
use machine::session::current_session_id;
use machine::user_api::call_value;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

/// Protocol version answered when the client asks for none.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

fn main() {
    let session = current_session_id();
    let agent = move |method: &str, params: Value| call_value(session, method, params);
    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&message, &agent),
            Err(error) => Some(error_reply(
                Value::Null,
                -32700,
                &format!("parse error: {error}"),
            )),
        };
        if let Some(reply) = reply {
            // A closed stdout means the client is gone.
            if writeln!(stdout, "{reply}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    }
}

/// Answer one JSON-RPC message; `None` for notifications (no `id`).
fn handle(message: &Value, agent: Agent) -> Option<Value> {
    let id = message.get("id").cloned()?;
    let method = message["method"].as_str().unwrap_or_default();
    let params = &message["params"];
    let result = match method {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or(DEFAULT_PROTOCOL),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "digital-wellbeing", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or_default();
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match call_tool(name, &arguments, agent, "claude desktop") {
                Ok(text) => json!({ "content": [{ "type": "text", "text": text }] }),
                Err(text) => json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": true
                }),
            }
        }
        other => {
            return Some(error_reply(
                id,
                -32601,
                &format!("unknown method '{other}'"),
            ))
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error_reply(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use machine::user_api::Status;
    use std::cell::RefCell;

    fn empty_status() -> Status {
        serde_json::from_value(json!({
            "session_active": false,
            "task": "",
            "last_check": null,
            "last_error": null,
            "check_interval_secs": 60,
            "verdicts": [],
            "blocked": [],
            "planner": null,
            "last_decision": null,
            "grants": [],
            "grant_log": [],
            "last_access": null
        }))
        .unwrap()
    }

    #[test]
    fn initialize_and_list_tools() {
        let agent = |_: &str, _: Value| Ok(serde_json::to_value(empty_status()).unwrap());
        let initialize_reply = handle(
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                     "params": { "protocolVersion": "2025-03-26" } }),
            &agent,
        )
        .unwrap();
        assert_eq!(initialize_reply["result"]["protocolVersion"], "2025-03-26");
        assert!(initialize_reply["result"]["capabilities"]["tools"].is_object());
        // Notifications get no reply.
        assert!(handle(
            &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            &agent
        )
        .is_none());
        let list = handle(
            &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
            &agent,
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "get_status",
                "propose_todo",
                "change_todo",
                "add_appointment",
                "remove_appointment",
                "get_my_task",
                "request_change",
                "plan_week",
                "request_access",
                "program_update",
                "get_settings",
                "schedule_settings_update"
            ]
        );
    }

    #[test]
    fn tools_map_to_agent_methods_only() {
        let seen = RefCell::new(Vec::new());
        let agent = |method: &str, params: Value| {
            seen.borrow_mut().push((method.to_string(), params));
            Err::<Value, _>("agent not running".to_string())
        };
        let reply = handle(
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
                "name": "propose_todo",
                "arguments": { "title": "장보기", "priority": 2, "deadline": null }
            }}),
            &agent,
        )
        .unwrap();
        assert_eq!(reply["result"]["isError"], true);
        let (method, params) = seen.borrow()[0].clone();
        assert_eq!(method, "todo_change");
        assert_eq!(params["via"], "claude desktop");
        assert_eq!(
            params["change"],
            json!({ "kind": "add", "todo": { "title": "장보기", "priority": 2 } })
        );

        handle(
            &json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {
                "name": "request_change",
                "arguments": { "change": "repick", "reason": "blocked on data" }
            }}),
            &agent,
        );
        let (method, params) = seen.borrow()[1].clone();
        assert_eq!(method, "request_change");
        assert_eq!(params["request"], json!({ "kind": "repick" }));
        // Choosing the next task is not offered.
        let refused = handle(
            &json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
                "name": "request_change",
                "arguments": { "change": "switch_todo", "todo_id": "t1", "reason": "blocked on data" }
            }}),
            &agent,
        );
        assert!(refused.unwrap().to_string().contains("unknown change"));

        // No tool reaches anything else (unblocking, rules, the admin's list).
        let unknown = handle(
            &json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                     "params": { "name": "unblock", "arguments": {} } }),
            &agent,
        )
        .unwrap();
        assert_eq!(unknown["result"]["isError"], true);
        assert_eq!(seen.borrow().len(), 2);
    }

    #[test]
    fn settings_updates_are_scheduled_only_once_protected_and_keep_the_key() {
        let protected = RefCell::new(false);
        let seen = RefCell::new(Vec::new());
        let agent = |method: &str, params: Value| {
            seen.borrow_mut().push((method.to_string(), params));
            match method {
                "settings_get" => Ok(json!({
                    "protected": *protected.borrow(),
                    "files": { "ai_focus": { "api_key": "secret", "model": "m" } },
                    "reservations": { "scheduled": [], "history": [] },
                })),
                _ => Ok(json!({ "outcome": "scheduled", "id": "u1" })),
            }
        };
        let schedule = |content: Value| {
            handle(
                &json!({ "jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {
                    "name": "schedule_settings_update",
                    "arguments": { "name": "ai_focus", "content": content }
                }}),
                &agent,
            )
            .unwrap()
        };
        // Setup: refused, nothing is written.
        assert_eq!(
            schedule(json!({ "model": "other" }))["result"]["isError"],
            true
        );
        assert!(seen
            .borrow()
            .iter()
            .all(|(method, _)| method == "settings_get"));

        *protected.borrow_mut() = true;
        assert_ne!(
            schedule(json!({ "model": "other" }))["result"]["isError"],
            true
        );
        let (method, params) = seen.borrow().last().unwrap().clone();
        assert_eq!(method, "settings_put");
        assert_eq!(params["via"], "claude desktop");
        assert_eq!(params["content"]["api_key"], "secret");

        // The key is never shown to the assistant.
        let read = handle(
            &json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                     "params": { "name": "get_settings", "arguments": {} } }),
            &agent,
        )
        .unwrap();
        let text = read["result"]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("secret"), "{text}");
        assert!(text.contains("built_in_instructions"));
    }

    #[test]
    fn unknown_methods_are_protocol_errors() {
        let agent = |_: &str, _: Value| Ok(serde_json::to_value(empty_status()).unwrap());
        let reply = handle(
            &json!({ "jsonrpc": "2.0", "id": 9, "method": "resources/list" }),
            &agent,
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], -32601);
    }
}

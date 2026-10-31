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

use machine::session::current_session_id;
use machine::user_api::{call_value, Status};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};

/// Protocol version answered when the client asks for none.
const DEFAULT_PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "\
Tools for the user's Digital Wellbeing app on this PC. The app assigns the user's work \
from a to-do list and blocks apps and websites the assigned task does not need. These \
tools can show the list and the current task, propose a to-do, ask to change the \
current task, and schedule an update of the settings. The app's own AI decides every \
proposal and change request against rules the account owner set. When a proposal or \
request is rejected, tell the user the reason; do not retry it in other words. A \
scheduled settings update takes effect only after the user confirms it in the app three \
times, two days apart; tell the user that, and never press them to confirm.";

/// Responsibility: Reach the agent (the real one in production, a fake in tests).
type Agent<'a> = &'a dyn Fn(&str, Value) -> Result<Value, String>;

/// Call an agent method whose answer is the [`Status`].
fn status_from(agent: Agent, method: &str, params: Value) -> Result<Status, String> {
    serde_json::from_value(agent(method, params)?).map_err(|error| error.to_string())
}

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
            match call_tool(name, &arguments, agent) {
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

fn tools() -> Value {
    json!([
        {
            "name": "get_status",
            "description": "The focus session, the task assigned now, the open to-dos (id, title, deadline, priority, whether the user proposed it), recent proposals and change requests with the app's answers, and what is blocked.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "propose_todo",
            "description": "Propose a to-do for the user's list. The app's own AI approves or rejects it against the account owner's life rules; only approved to-dos join the list, at once. Leave out fields the user did not give: the app's AI fills them in (notes, deadline, priority, estimate). Use the user's own words for the title; do not inflate deadline or priority.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short title, e.g. \"Reply to the advisor's email\"" },
                    "notes": { "type": "string", "description": "What done means, which apps or sites it needs" },
                    "deadline": { "type": "string", "description": "YYYY-MM-DD" },
                    "priority": { "type": "integer", "minimum": 1, "maximum": 5, "description": "1 low .. 5 urgent (default 3)" },
                    "estimate_minutes": { "type": "integer", "minimum": 1 },
                    "reason": { "type": "string", "description": "Why it belongs on the list (optional)" }
                },
                "required": ["title"]
            }
        },
        {
            "name": "change_todo",
            "description": "Ask to edit or remove an open to-do (ids from get_status). The app's AI reviews the reason against the owner's life rules; an approved change takes effect at once. For an edit, give only the fields that change. When the AI says no, tell the user its reason; only the user, in the app, can push a rejected change through.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "todo_id": { "type": "string" },
                    "remove": { "type": "boolean", "description": "true to remove it" },
                    "title": { "type": "string" },
                    "notes": { "type": "string" },
                    "deadline": { "type": "string", "description": "YYYY-MM-DD" },
                    "priority": { "type": "integer", "minimum": 1, "maximum": 5 },
                    "estimate_minutes": { "type": "integer", "minimum": 1 },
                    "reason": { "type": "string", "description": "The user's reason, in their words" }
                },
                "required": ["todo_id", "reason"]
            }
        },
        {
            "name": "get_my_task",
            "description": "Ask the app to assign the user's task now (the app's AI picks it from the list) and start the AI checks. Only works while the planner is on.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "request_change",
            "description": "Ask to change the assigned task: it is done, switch to another open to-do, switch to something not on the list, or pause the AI checks (end). Every task runs until a planned end (\"until\" in get_status) and then the app's AI picks the next one; notify_when_time_is_up asks to keep the current task past that end and only be notified, reassign_when_time_is_up goes back to a new pick. The app's AI approves or rejects the reason (going back to reassign needs no approval).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "change": { "type": "string", "enum": ["done", "switch_todo", "switch_free", "end", "notify_when_time_is_up", "reassign_when_time_is_up"] },
                    "todo_id": { "type": "string", "description": "For switch_todo: id from get_status" },
                    "task": { "type": "string", "description": "For switch_free: what to do instead" },
                    "reason": { "type": "string", "description": "The user's reason, in their words" }
                },
                "required": ["change", "reason"]
            }
        },
        {
            "name": "get_settings",
            "description": "Read the app's settings: whether they are protected, the files in force (policy: block lists; ai_focus: focus checks, without the API key; planner: to-dos, life_rules, and instructions for the planner's AI, where an empty text means the built-in one), the built-in instructions, and the scheduled updates with their confirmations.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "schedule_settings_update",
            "description": "Schedule an update of one settings file: send the whole new file (start from get_settings). Accepted only once the settings are protected. The update takes effect only after the user answers \"is this update really right?\" with yes in the app three times, two days apart (the first question comes two days from now); one no rejects it. Edits that make the rules stricter (blocking more, allowing less, checking more closely) take effect at once; the others become the scheduled update. Start from files_with_scheduled_updates in get_settings: leaving out a change that is already scheduled withdraws it, and changing it replaces the scheduled update and its confirmations start over; tell the user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "enum": ["policy", "ai_focus", "planner"] },
                    "content": { "type": "object", "description": "The whole new file" }
                },
                "required": ["name", "content"]
            }
        }
    ])
}

/// Run one tool; `Err` carries a message for the assistant (a tool error, not a protocol one).
/// The AI's decision on a to-do change, for the assistant.
fn todo_change_answer(status: &Status) -> Result<String, String> {
    let record = status
        .last_todo_change
        .as_ref()
        .ok_or("the agent did not answer")?;
    Ok(pretty(&json!({
        "approved": record.approved,
        "reply": record.reply,
        "ai_could_not_review": record.ai_failed,
        "todo": record.after.as_ref().or(record.before.as_ref()),
    })))
}

/// A scheduled update's edits as lines, leaving out any edit of the API key.
fn changes_without_key(edits: &Value) -> Vec<String> {
    let edits: Vec<machine::setting_edits::Edit> =
        serde_json::from_value(edits.clone()).unwrap_or_default();
    edits
        .iter()
        .filter(|edit| edit.path().first().map(String::as_str) != Some("api_key"))
        .map(machine::setting_edits::Edit::describe)
        .collect()
}

fn call_tool(name: &str, arguments: &Value, agent: Agent) -> Result<String, String> {
    match name {
        "get_status" => {
            status_from(agent, "status", json!({})).map(|status| pretty(&summary(&status)))
        }
        "propose_todo" => {
            let mut todo = json!({ "title": arguments["title"].as_str().unwrap_or_default() });
            for key in ["notes", "deadline", "priority", "estimate_minutes"] {
                if let Some(value) = arguments.get(key).filter(|value| !value.is_null()) {
                    todo[key] = value.clone();
                }
            }
            let status = status_from(
                agent,
                "todo_change",
                json!({
                    "change": { "kind": "add", "todo": todo },
                    "reason": arguments["reason"].as_str().unwrap_or_default(),
                    "via": "claude desktop",
                }),
            )?;
            todo_change_answer(&status)
        }
        "change_todo" => {
            let todo_id = arguments["todo_id"].as_str().ok_or("todo_id is required")?;
            let change = if arguments["remove"].as_bool() == Some(true) {
                json!({ "kind": "remove", "todo_id": todo_id })
            } else {
                let mut todo = json!({});
                for key in ["title", "notes", "deadline", "priority", "estimate_minutes"] {
                    if let Some(value) = arguments.get(key).filter(|value| !value.is_null()) {
                        todo[key] = value.clone();
                    }
                }
                json!({ "kind": "edit", "todo_id": todo_id, "todo": todo })
            };
            let status = status_from(
                agent,
                "todo_change",
                json!({
                    "change": change,
                    "reason": arguments["reason"].as_str().unwrap_or_default(),
                    "via": "claude desktop",
                }),
            )?;
            todo_change_answer(&status)
        }
        "get_my_task" => status_from(agent, "assign", json!({}))
            .map(|status| pretty(&summary(&status)["assignment"])),
        "request_change" => {
            let request = match arguments["change"].as_str().unwrap_or_default() {
                "done" => json!({ "kind": "done" }),
                "end" => json!({ "kind": "end" }),
                "notify_when_time_is_up" => {
                    json!({ "kind": "set_when_time_is_up", "action": "notify_only" })
                }
                "reassign_when_time_is_up" => {
                    json!({ "kind": "set_when_time_is_up", "action": "reassign" })
                }
                "switch_todo" => json!({
                    "kind": "switch_todo",
                    "todo_id": arguments["todo_id"].as_str().ok_or("todo_id is required for switch_todo")?,
                }),
                "switch_free" => json!({
                    "kind": "switch_free",
                    "task": arguments["task"].as_str().ok_or("task is required for switch_free")?,
                }),
                other => return Err(format!("unknown change '{other}'")),
            };
            let reason = arguments["reason"].as_str().unwrap_or_default();
            let status = status_from(
                agent,
                "request_change",
                json!({ "request": request, "reason": reason }),
            )?;
            let decision = status
                .last_decision
                .clone()
                .ok_or("the agent did not answer")?;
            Ok(pretty(&json!({
                "approved": decision.approved,
                "reply": decision.reply,
                "assignment": summary(&status)["assignment"],
            })))
        }
        "get_settings" => {
            let mut settings = agent("settings_get", json!({}))?;
            // The key stays with the app; schedule_settings_update keeps it.
            if let Some(ai_focus) = settings["files"]["ai_focus"].as_object_mut() {
                ai_focus.remove("api_key");
            }
            let scheduled: Vec<Value> = settings["reservations"]["scheduled"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|reservation| {
                    json!({
                        "id": reservation["id"],
                        "name": reservation["name"],
                        "via": reservation["via"],
                        "origin": reservation["origin"],
                        "scheduled_at": reservation["scheduled_at"],
                        "confirmations": reservation["approvals"].as_array().map_or(0, Vec::len),
                        "changes": changes_without_key(&reservation["edits"]),
                    })
                })
                .collect();
            // What the Settings tab shows: the files in force with their scheduled settings
            // updates applied. Scheduling a file is a change from this.
            let mut with_scheduled = settings["files"].clone();
            for reservation in settings["reservations"]["scheduled"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if reservation["origin"] != "settings" {
                    continue;
                }
                let Some(name) = reservation["name"].as_str() else {
                    continue;
                };
                let edits: Vec<machine::setting_edits::Edit> =
                    serde_json::from_value(reservation["edits"].clone()).unwrap_or_default();
                machine::setting_edits::apply(&mut with_scheduled[name], &edits);
            }
            if let Some(ai_focus) = with_scheduled["ai_focus"].as_object_mut() {
                ai_focus.remove("api_key");
            }
            Ok(pretty(&json!({
                "protected": settings["protected"],
                "files": settings["files"],
                "files_with_scheduled_updates": with_scheduled,
                "built_in_instructions": {
                    "pick": task_planner::PICK_PROMPT,
                    "change_review": task_planner::REVIEW_PROMPT,
                    "proposal_review": task_planner::PROPOSAL_PROMPT,
                },
                "scheduled_updates": scheduled,
                "recent_outcomes": settings["reservations"]["history"],
            })))
        }
        "schedule_settings_update" => {
            let name = arguments["name"].as_str().ok_or("name is required")?;
            let mut content = arguments["content"].clone();
            if !content.is_object() {
                return Err("content must be the whole settings file (an object)".into());
            }
            let current = agent("settings_get", json!({}))?;
            if current["protected"] != json!(true) {
                return Err(
                    "the settings are not protected yet (setup): the user changes them \
                            in the app's Settings tab; Claude Desktop can schedule updates once \
                            they are protected"
                        .into(),
                );
            }
            if name == "ai_focus" && content.get("api_key").is_none() {
                content["api_key"] = current["files"]["ai_focus"]["api_key"].clone();
            }
            let outcome = agent(
                "settings_put",
                json!({ "name": name, "content": content, "via": "claude desktop" }),
            )?;
            Ok(pretty(&json!({
                "scheduled": outcome,
                "note": "It takes effect only after the user confirms it in the app three times, two days apart.",
            })))
        }
        other => Err(format!("unknown tool '{other}'")),
    }
}

/// What the assistant needs from a status, without the verdict log and other noise.
fn summary(status: &Status) -> Value {
    let planner = status.planner.as_ref();
    json!({
        "focus_session_active": status.session_active,
        "focus_task": status.task,
        "planner_on": planner.is_some_and(|planner| planner.active),
        "assignment": planner.and_then(|planner| planner.assignment.as_ref()).map(|assignment| json!({
            "task": assignment.task,
            "why": assignment.why,
            "todo_id": assignment.todo_id,
            "since": assignment.at,
            "until": assignment.until,
            "when_time_is_up": assignment.when_time_is_up,
        })),
        "open_todos": planner.map(|planner| planner.open_todos.iter().map(|todo| json!({
            "id": todo.id,
            "title": todo.title,
            "notes": todo.notes,
            "deadline": todo.deadline,
            "priority": todo.priority,
            "proposed_by_user": todo.proposed,
        })).collect::<Vec<_>>()),
        "recent_proposals": planner.map(|planner| planner.recent_proposals.iter().map(|proposal| json!({
            "title": proposal.item.title,
            "approved": proposal.approved,
            "reply": proposal.reply,
            "at": proposal.at,
        })).collect::<Vec<_>>()),
        "recent_change_requests": planner.map(|planner| planner.recent_requests.iter().map(|change_request| json!({
            "request": change_request.request,
            "reason": change_request.reason,
            "approved": change_request.approved,
            "reply": change_request.reply,
            "at": change_request.at,
        })).collect::<Vec<_>>()),
        "blocked": status.blocked.iter().map(|blocked_entry| json!({ "what": blocked_entry.key, "reason": blocked_entry.reason })).collect::<Vec<_>>(),
    })
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
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
                "get_my_task",
                "request_change",
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
                "arguments": { "change": "switch_todo", "todo_id": "t1", "reason": "blocked on data" }
            }}),
            &agent,
        );
        let (method, params) = seen.borrow()[1].clone();
        assert_eq!(method, "request_change");
        assert_eq!(
            params["request"],
            json!({ "kind": "switch_todo", "todo_id": "t1" })
        );

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

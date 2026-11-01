//! Responsibility: Offer the user's AI assistants (Claude Desktop through the MCP server, and
//! the AI assistant tab of the Focus app) the same tools, each a call to [`crate::user_api`].
//!
//! Every tool does only what the Focus app can do, so an assistant gains no power the user
//! does not already have. The app's own AI decides every proposal and change request.
//! Setup and tool reference (Korean): `docs/claude_desktop.md`, `docs/assistant.md`.

use crate::user_api::Status;
use serde_json::{json, Value};

pub const INSTRUCTIONS: &str = "\
Tools for the user's Digital Wellbeing app on this PC. The app assigns the user's work \
from a to-do list and blocks apps and websites the assigned task does not need. These \
tools can show the list and the current task, propose a to-do, ask to change the \
current task, and schedule an update of the settings. The app's own AI decides every \
proposal and change request against rules the account owner set. When a proposal or \
request is rejected, tell the user the reason; do not retry it in other words. The \
user never chooses what to work on next, not even through you: the app's AI does. When \
they want to stop the current task, send request_change with change \"repick\" and their \
reason, not a task of your or their choosing. A \
scheduled settings update takes effect only after the user confirms it in the app three \
times, two days apart; tell the user that, and never press them to confirm.";

/// Responsibility: Reach the agent (the real one in production, a fake in tests).
pub type Agent<'a> = &'a dyn Fn(&str, Value) -> Result<Value, String>;

/// Call an agent method whose answer is the [`Status`].
fn status_from(agent: Agent, method: &str, params: Value) -> Result<Status, String> {
    serde_json::from_value(agent(method, params)?).map_err(|error| error.to_string())
}

pub fn tools() -> Value {
    json!([
        {
            "name": "get_status",
            "description": "Whether the AI checks (daily wellbeing) are on, the task assigned now, the open to-dos (id, title, deadline, priority, whether the user added it), recent to-do changes and change requests with the app's answers, and what is blocked.",
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
            "description": "Ask to change the assigned task: it is done, get something else (repick: the app's AI picks what comes next; the user only gives the reason and never chooses the next task), or pause the AI checks (end). Every task runs until a planned end (\"until\" in get_status) and then the app's AI picks the next one; notify_when_time_is_up asks to keep the current task past that end and only be notified, reassign_when_time_is_up goes back to a new pick. The app's AI approves or rejects the reason (going back to reassign needs no approval).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "change": { "type": "string", "enum": ["done", "repick", "end", "notify_when_time_is_up", "reassign_when_time_is_up"] },
                    "reason": { "type": "string", "description": "The user's reason, in their words" },
                    "evidence": { "type": "string", "description": "For done: proof of the result in the user's words (what was sent, to whom, an order number). The app's AI asks for proof when the result is a fact like a sent mail or an order; work that can always be improved (a report, practice) is accepted and the planned time left comes back later as an \"Improve: ...\" to-do." }
                },
                "required": ["change", "reason"]
            }
        },
        {
            "name": "plan_week",
            "description": "Ask the app's AI for an overview of the next seven days (today and the six days after): how many hours of work each day needs so that the to-dos meet their deadlines, how much free time is left, and deadlines at risk. It is not a timetable and does not change what the user works on. One AI call; takes up to a minute.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "request_access",
            "description": "Ask the app's AI to allow a blocked app or website for a purpose (the same as the Blocking tab of the app). The AI checks the purpose against the current task and the owner's life rules. Always-blocked programs and websites are never allowed. Only works while daily wellbeing (the AI checks) is on.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["app", "site"] },
                    "target": { "type": "string", "description": "For app: the process name, e.g. \"spotify.exe\". For site: a word in the tab title, e.g. \"youtube\"" },
                    "minutes": { "type": "integer", "minimum": 1, "description": "How long; leave out to ask until revoked (the settings may forbid that)" },
                    "purpose": { "type": "string", "description": "What the user will use it for, in their words" }
                },
                "required": ["kind", "target", "purpose"]
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
    let edits: Vec<crate::setting_edits::Edit> =
        serde_json::from_value(edits.clone()).unwrap_or_default();
    edits
        .iter()
        .filter(|edit| edit.path().first().map(String::as_str) != Some("api_key"))
        .map(crate::setting_edits::Edit::describe)
        .collect()
}

/// Run one tool; `Err` carries a message for the assistant (a tool error, not a protocol one).
/// `via` names the assistant in the logs ("claude desktop", "assistant").
pub fn call_tool(name: &str, arguments: &Value, agent: Agent, via: &str) -> Result<String, String> {
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
                    "via": via,
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
                    "via": via,
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
                "repick" => json!({ "kind": "repick" }),
                other => return Err(format!("unknown change '{other}'")),
            };
            let reason = arguments["reason"].as_str().unwrap_or_default();
            let status = status_from(
                agent,
                "request_change",
                json!({
                    "request": request,
                    "reason": reason,
                    "evidence": arguments["evidence"].as_str().unwrap_or_default(),
                }),
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
        "plan_week" => {
            let status = status_from(agent, "plan_week", json!({}))?;
            let plan = status
                .planner
                .and_then(|planner| planner.week_plan)
                .ok_or("the agent did not answer")?;
            Ok(pretty(&json!(plan)))
        }
        "request_access" => {
            let target_text = arguments["target"].as_str().unwrap_or_default();
            let target = match arguments["kind"].as_str().unwrap_or_default() {
                "app" => json!({ "kind": "app", "process": target_text }),
                "site" => json!({ "kind": "site", "keyword": target_text }),
                other => return Err(format!("unknown kind '{other}'")),
            };
            let duration = match arguments["minutes"].as_u64() {
                Some(minutes) => json!({ "kind": "minutes", "minutes": minutes }),
                None => json!({ "kind": "permanent" }),
            };
            let status = status_from(
                agent,
                "request_access",
                json!({
                    "target": target,
                    "duration": duration,
                    "purpose": arguments["purpose"].as_str().unwrap_or_default(),
                }),
            )?;
            let entry = status.last_access.ok_or("the agent did not answer")?;
            let (approved, granted_minutes) = match &entry.event {
                ai_focus_rule::grants::GrantEvent::Requested {
                    approved,
                    granted_minutes,
                    ..
                } => (*approved, *granted_minutes),
                _ => (false, None),
            };
            Ok(pretty(&json!({
                "approved": approved,
                "reply": entry.message,
                "granted_minutes": granted_minutes,
                "target": entry.target,
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
                let edits: Vec<crate::setting_edits::Edit> =
                    serde_json::from_value(reservation["edits"].clone()).unwrap_or_default();
                crate::setting_edits::apply(&mut with_scheduled[name], &edits);
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
                    "week_plan": task_planner::WEEK_PLAN_PROMPT,
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
                            in the app's Settings tab; an assistant can schedule updates once \
                            they are protected"
                        .into(),
                );
            }
            if name == "ai_focus" && content.get("api_key").is_none() {
                content["api_key"] = current["files"]["ai_focus"]["api_key"].clone();
            }
            let outcome = agent(
                "settings_put",
                json!({ "name": name, "content": content, "via": via }),
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
pub fn summary(status: &Status) -> Value {
    let planner = status.planner.as_ref();
    json!({
        "daily_wellbeing_on": status.session_active,
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
        "recent_todo_changes": planner.map(|planner| planner.recent_todo_changes.iter().map(|record| json!({
            "change": task_planner::change_summary(&record.change, record.before.as_ref()),
            "approved": record.approved,
            "ai_could_not_review": record.ai_failed,
            "reply": record.reply,
            "at": record.at,
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

pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

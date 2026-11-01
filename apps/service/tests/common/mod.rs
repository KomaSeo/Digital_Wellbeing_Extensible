//! Responsibility: What both end-to-end tests share: a local stand-in for the Claude Messages
//! API and a polling helper.
#![allow(dead_code)] // each test binary uses part of it

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Protect the settings (if not yet), then check that an edit is scheduled instead of taking
/// effect, that its question cannot be answered before it is due, and that it can be
/// withdrawn. Works with the default two-day interval.
pub fn check_protected_edits_are_scheduled(session: u32) {
    use machine::user_api::{call, call_value};
    call_value(session, "settings_protect", json!({})).unwrap();
    assert!(
        call(session, "status", json!({}))
            .unwrap()
            .settings_protected
    );
    let before = call_value(session, "settings_get", json!({})).unwrap();
    let in_force = before["files"]["planner"].clone();
    let edit = json!({ "enabled": false, "todos": [], "life_rules": "Games all day." });
    let outcome = call_value(
        session,
        "settings_put",
        json!({ "name": "planner", "content": edit, "via": "e2e" }),
    )
    .unwrap();
    let id = outcome["scheduled"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("not scheduled: {outcome}"))
        .to_string();
    let after = call_value(session, "settings_get", json!({})).unwrap();
    assert_eq!(
        after["files"]["planner"], in_force,
        "a protected edit took effect at once"
    );
    let scheduled = &after["reservations"]["scheduled"];
    assert!(scheduled
        .as_array()
        .unwrap()
        .iter()
        .any(|reservation| reservation["id"] == id.as_str()
            && reservation["edits"].to_string().contains("Games all day.")));
    let early = call_value(
        session,
        "settings_answer",
        json!({ "id": id, "approve": true }),
    )
    .unwrap_err();
    assert!(early.contains("due"), "{early}");
    call_value(session, "settings_withdraw", json!({ "id": id })).unwrap();
    let withdrawn = call_value(session, "settings_get", json!({})).unwrap();
    assert!(withdrawn["reservations"]["scheduled"]
        .as_array()
        .is_none_or(|scheduled| scheduled.is_empty()));
    assert_eq!(withdrawn["files"]["planner"], in_force);
}

pub fn wait_for<T>(limit: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return Some(value);
        }
        thread::sleep(Duration::from_millis(200));
    }
    None
}

/// A stand-in for the Claude Messages API on `address` (e.g. `127.0.0.1:0`): windows whose title
/// mentions cats are off task, everything else is on task, and every request is approved
/// unless it says "please reject".
/// Returns its base URL and a call counter.
pub fn fake_claude(address: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(address).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream);
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(length_text) = line.to_ascii_lowercase().strip_prefix("content-length:")
                {
                    length = length_text.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let (stop_reason, content) = if request["tools"].is_array() {
                assistant_answer(&request)
            } else {
                (
                    "end_turn",
                    json!([{ "type": "text", "text": answer(&request).to_string() }]),
                )
            };
            let reply = json!({
                "model": "claude-opus-5-5",
                "stop_reason": stop_reason,
                "content": content,
                "usage": { "input_tokens": 1200, "output_tokens": 60 },
            })
            .to_string();
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                reply.len(),
                reply
            );
        }
    });
    (url, calls)
}

/// The AI assistant: "할 일에 올려 줘: A, B" proposes A and B at once; after the tool
/// results it answers in words.
fn assistant_answer(request: &Value) -> (&'static str, Value) {
    let last = request["messages"]
        .as_array()
        .and_then(|messages| messages.last())
        .cloned()
        .unwrap_or(Value::Null);
    let blocks = last["content"].as_array().cloned().unwrap_or_default();
    if blocks.iter().any(|block| block["type"] == "tool_result") {
        return (
            "end_turn",
            json!([{ "type": "text", "text": format!("{}개를 요청했습니다.", blocks.len()) }]),
        );
    }
    let text: String = blocks
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect();
    let Some(items) = text.split("할 일에 올려 줘: ").nth(1) else {
        return (
            "end_turn",
            json!([{ "type": "text", "text": "무엇을 도와 드릴까요?" }]),
        );
    };
    let tool_uses: Vec<Value> = items
        .split(", ")
        .enumerate()
        .map(|(index, title)| {
            json!({ "type": "tool_use", "id": format!("toolu_{index}"), "name": "propose_todo",
                    "input": { "title": title.trim() } })
        })
        .collect();
    ("tool_use", Value::Array(tool_uses))
}

fn answer(request: &Value) -> Value {
    let system = request["system"].as_str().unwrap_or_default();
    let text: String = request["messages"][0]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect();
    if system.contains("focus checker") {
        let verdict = |window_text: &str| {
            if window_text.contains("cats") {
                "off_task"
            } else {
                "on_task"
            }
        };
        let front = text
            .lines()
            .find(|line| line.starts_with("In front right now:"))
            .unwrap_or_default();
        let others: Vec<Value> = text
            .lines()
            .filter_map(|line| {
                let id = line
                    .strip_prefix('[')?
                    .split(']')
                    .next()?
                    .parse::<u64>()
                    .ok()?;
                Some(json!({ "id": id, "verdict": verdict(line), "reason": "test" }))
            })
            .collect();
        json!({ "verdict": verdict(front), "reason": "test", "channel": "", "others": others })
    } else if system.contains("overview of the coming week") {
        // Today, as the question lists it first: "Days to plan: 2026-11-06 Friday, …".
        let today = text
            .split("Days to plan: ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .unwrap_or_default();
        json!({
            "days": [{ "date": today, "work_minutes": 180, "free_minutes": 240, "todos": [],
                       "note": "stand-in" }],
            "summary": "a calm week",
            "warnings": [],
        })
    } else if system.contains("You assign the user's next piece of work") {
        // The first open to-do, as the list shows it: `"id": "..."`.
        let first_id = text
            .split("\"id\": \"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .unwrap_or_default();
        json!({ "todo_id": first_id, "task": "work on it", "why": "first", "minutes": 30 })
    } else {
        // A change review or a to-do review; a to-do review also fills in the to-do.
        // "please reject" in the request makes it say no.
        let approved = !text.contains("please reject");
        json!({
            "approved": approved,
            "reply": if approved { "ok" } else { "no" },
            "minutes": 30,
            "outcome": "not_applicable",
            "todo": {
                "title": "", "notes": "filled by the stand-in", "deadline": "",
                "priority": 2, "estimate_minutes": 25
            },
        })
    }
}

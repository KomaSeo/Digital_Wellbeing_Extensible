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
            let answer = answer(&request);
            let reply = json!({
                "model": "claude-opus-5-5",
                "stop_reason": "end_turn",
                "content": [{ "type": "text", "text": answer.to_string() }],
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
    } else {
        // A change review or a to-do review; a to-do review also fills in the to-do.
        // "please reject" in the request makes it say no.
        let approved = !text.contains("please reject");
        json!({
            "approved": approved,
            "reply": if approved { "ok" } else { "no" },
            "minutes": 30,
            "todo": {
                "title": "", "notes": "filled by the stand-in", "deadline": "",
                "priority": 2, "estimate_minutes": 25
            },
        })
    }
}

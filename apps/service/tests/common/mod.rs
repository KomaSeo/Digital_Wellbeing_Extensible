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

/// Lock the settings, then check that an edit saved while locked waits instead of taking
/// effect, and that it can be discarded.
pub fn check_lock_delays_edits(session: u32) {
    use machine::user_api::{call, call_value};
    call_value(session, "settings_lock", json!({ "hours": 1 })).unwrap();
    assert!(call(session, "status", json!({}))
        .unwrap()
        .settings_locked_until
        .is_some());
    let before = call_value(session, "settings_get", json!({})).expect("readable while locked");
    let in_force = before["files"]["planner"].clone();
    let edit = json!({ "enabled": false, "todos": [], "life_rules": "Games all day." });
    let outcome = call_value(
        session,
        "settings_put",
        json!({ "name": "planner", "content": edit }),
    )
    .unwrap();
    assert_eq!(outcome["outcome"], "pending", "{outcome}");
    let after = call_value(session, "settings_get", json!({})).unwrap();
    assert_eq!(
        after["files"]["planner"], in_force,
        "the edit took effect while locked"
    );
    assert_eq!(after["pending"]["planner"]["content"], edit);
    assert!(after["locked_until"].is_string());
    call_value(session, "settings_discard", json!({ "name": "planner" })).unwrap();
    let discarded = call_value(session, "settings_get", json!({})).unwrap();
    assert!(discarded["pending"]
        .as_object()
        .is_none_or(|p| p.is_empty()));
    assert_eq!(discarded["files"]["planner"], in_force);
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

/// A stand-in for the Claude Messages API on `addr` (e.g. `127.0.0.1:0`): windows whose title
/// mentions cats are off task, everything else is on task, and every proposal is approved.
/// Returns its base URL and a call counter.
pub fn fake_claude(addr: &str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(addr).unwrap();
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
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap_or(0);
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
        .filter_map(|b| b["text"].as_str())
        .collect();
    if system.contains("focus checker") {
        let verdict = |t: &str| {
            if t.contains("cats") {
                "off_task"
            } else {
                "on_task"
            }
        };
        let front = text
            .lines()
            .find(|l| l.starts_with("In front right now:"))
            .unwrap_or_default();
        let others: Vec<Value> = text
            .lines()
            .filter_map(|l| {
                let id = l
                    .strip_prefix('[')?
                    .split(']')
                    .next()?
                    .parse::<u64>()
                    .ok()?;
                Some(json!({ "id": id, "verdict": verdict(l), "reason": "test" }))
            })
            .collect();
        json!({ "verdict": verdict(front), "reason": "test", "channel": "", "others": others })
    } else {
        json!({ "approved": true, "reply": "ok" })
    }
}

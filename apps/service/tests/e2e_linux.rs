//! Responsibility: Run the real agent end to end on Linux: an X server (Xvfb), real windows,
//! the agent binary, its user API, and a local stand-in for the Claude Messages API.
//!
//! Run with `xvfb-run -a cargo test -p digital-wellbeing-service --test e2e_linux`; without
//! `DISPLAY` the test is skipped. What it checks, in order:
//! 1. a focus session starts through the user API (as the Focus app does);
//! 2. a window without a verdict is judged at once: the off-task tab in front gets Ctrl+W,
//!    and the on-task window behind it is judged by title too;
//! 3. the verdicts, the block and the token usage show in the status;
//! 4. settings_get / settings_put work while unlocked, a to-do proposal goes through the AI,
//!    and settings_lock makes the agent refuse settings_get.
#![cfg(target_os = "linux")]

use machine::user_api::{call, call_value, Status};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, KeyButMask, PropMode,
    WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;

/// Kills the agent when the test ends, however it ends.
struct Agent(Child);

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn agent_end_to_end_on_x11() {
    let Some(display) = std::env::var_os("DISPLAY") else {
        eprintln!("skipped: no DISPLAY (run under xvfb-run)");
        return;
    };
    let data = std::env::temp_dir().join(format!("dw-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    // Our own process plays "the browser": its windows are tabs.
    let exe = std::env::current_exe().unwrap();
    let browser = exe.file_name().unwrap().to_string_lossy().to_string();
    std::fs::write(
        data.join("ai_focus.json"),
        json!({
            "api_key": "test-key",
            "model": "claude-opus-5-5",
            "send_screenshot": false,
            "browsers": [browser],
        })
        .to_string(),
    )
    .unwrap();

    let (claude_url, claude_calls) = fake_claude();
    let port_base = 20000 + (std::process::id() % 20000) as u16;
    // SAFETY (set_var): set before any other thread of this test reads the environment.
    std::env::set_var("DW_PORT_BASE", port_base.to_string());
    let _agent = Agent(
        Command::new(env!("CARGO_BIN_EXE_digital-wellbeing-service"))
            .arg("agent")
            .env("DW_DATA_DIR", &data)
            .env("DW_PORT_BASE", port_base.to_string())
            .env("DW_CLAUDE_API_URL", &claude_url)
            .env("DISPLAY", &display)
            .env("RUST_LOG", "debug,ureq=info,rustls=info")
            .spawn()
            .unwrap(),
    );
    let session = machine::session::current_session_id();
    let status = wait_for(Duration::from_secs(15), || {
        call(session, "status", json!({})).ok()
    })
    .expect("the agent answers on its user API");
    assert!(!status.session_active);

    // Two "tabs": an off-task one in front, an on-task one behind.
    let (x, screen) = x11rb::connect(None).unwrap();
    let root = x.setup().roots[screen].root;
    let cats = window(&x, root, "Funny cats - YouTube");
    let _docs = window(&x, root, "Rust docs - Google Chrome");
    x.set_input_focus(InputFocus::PARENT, cats, 0u32).unwrap();
    x.sync().unwrap();

    let started = call(
        session,
        "start_session",
        json!({ "task": "write my thesis" }),
    )
    .unwrap();
    assert!(started.session_active);

    // 2. Ctrl+W reaches the off-task tab within a few samples.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut closed = false;
    while Instant::now() < deadline && !closed {
        match x.poll_for_event().unwrap() {
            Some(Event::KeyPress(e)) if e.event == cats => {
                closed = u16::from(e.state) & u16::from(KeyButMask::CONTROL) != 0;
            }
            Some(_) => {}
            None => thread::sleep(Duration::from_millis(50)),
        }
    }
    assert!(closed, "the off-task tab did not get Ctrl+W");

    // 3. What the Focus app shows.
    let status = wait_for(Duration::from_secs(10), || {
        let s: Status = call(session, "status", json!({})).ok()?;
        s.verdicts
            .iter()
            .any(|v| v.title.starts_with("(background) Rust docs"))
            .then_some(s)
    })
    .expect("the window behind was judged by title");
    assert!(status
        .verdicts
        .iter()
        .any(|v| v.title == "Funny cats - YouTube" && v.action.contains("tab closed")));
    assert!(
        status
            .blocked
            .iter()
            .any(|b| b.key.contains("youtube page: funny cats")),
        "{:?}",
        status.blocked
    );
    assert!(status.ai_cost_today_usd.unwrap_or(0.0) > 0.0);
    assert!(claude_calls.load(Ordering::SeqCst) >= 1);

    // 4. Settings while unlocked, a proposal, then the lock.
    let settings = call_value(session, "settings_get", json!({})).unwrap();
    assert_eq!(settings["files"]["ai_focus"]["api_key"], "test-key");
    call_value(
        session,
        "settings_put",
        json!({ "name": "planner", "content": { "enabled": false, "todos": [], "life_rules": "No games." } }),
    )
    .unwrap();
    let settings = call_value(session, "settings_get", json!({})).unwrap();
    assert_eq!(settings["files"]["planner"]["life_rules"], "No games.");
    let proposed = call(
        session,
        "propose_todo",
        json!({ "todo": { "title": "Buy groceries" }, "via": "e2e" }),
    )
    .unwrap();
    assert!(proposed.last_proposal.as_ref().is_some_and(|p| p.approved));
    call_value(session, "settings_lock", json!({ "hours": 1 })).unwrap();
    let refused = call_value(session, "settings_get", json!({})).unwrap_err();
    assert!(refused.contains("locked"), "{refused}");
    assert!(call(session, "status", json!({}))
        .unwrap()
        .settings_locked_until
        .is_some());

    let _ = std::fs::remove_dir_all(&data);
}

/// A mapped, titled window owned by this process, listening for key presses.
fn window(x: &impl Connection, root: u32, title: &str) -> u32 {
    let win = x.generate_id().unwrap();
    x.create_window(
        0,
        win,
        root,
        0,
        0,
        320,
        200,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS),
    )
    .unwrap();
    x.change_property8(
        PropMode::REPLACE,
        win,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        title.as_bytes(),
    )
    .unwrap();
    let pid_atom = x
        .intern_atom(false, b"_NET_WM_PID")
        .unwrap()
        .reply()
        .unwrap()
        .atom;
    x.change_property32(
        PropMode::REPLACE,
        win,
        pid_atom,
        AtomEnum::CARDINAL,
        &[std::process::id()],
    )
    .unwrap();
    x.map_window(win).unwrap();
    x.sync().unwrap();
    win
}

fn wait_for<T>(limit: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if let Some(value) = probe() {
            return Some(value);
        }
        thread::sleep(Duration::from_millis(200));
    }
    None
}

/// A stand-in for the Claude Messages API: windows whose title mentions cats are off task,
/// everything else is on task, and every proposal is approved. Returns its base URL and a
/// call counter.
fn fake_claude() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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

//! Responsibility: Run the real agent end to end on Linux: an X server (Xvfb), real windows,
//! the agent binary, its user API, and either a local stand-in for the Claude Messages API or
//! the real one.
//!
//! Run with `xvfb-run -a cargo test -p digital-wellbeing-service --test e2e_linux`; without
//! `DISPLAY` both tests are skipped. The real-API test runs only when
//! `DW_E2E_ANTHROPIC_API_KEY` holds an API key (each run costs a few cents); it checks only
//! what any sensible answer satisfies and prints the AI's reasons. What they check, in order:
//! 1. a focus session starts through the user API (as the Focus app does);
//! 2. a window without a verdict is judged at once: the off-task tab in front gets Ctrl+W,
//!    and the on-task window behind it is judged by title too;
//! 3. the verdicts, the block and the token usage show in the status;
//! 4. settings_get / settings_put work while unlocked, a to-do proposal goes through the AI,
//!    and settings_lock makes the agent refuse settings_get.
#![cfg(target_os = "linux")]

mod common;

use common::{fake_claude, wait_for};
use machine::user_api::{call, call_value, Status};
use serde_json::json;
use std::process::{Child, Command};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
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

/// The tests share the X server, the agent port (`DW_PORT_BASE` is process-wide) and the
/// keyboard focus, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn agent_end_to_end_on_x11() {
    end_to_end(None);
}

#[test]
fn agent_end_to_end_on_x11_with_real_claude() {
    match std::env::var("DW_E2E_ANTHROPIC_API_KEY") {
        Ok(key) if !key.trim().is_empty() => end_to_end(Some(key.trim().to_string())),
        _ => eprintln!("skipped: DW_E2E_ANTHROPIC_API_KEY is not set"),
    }
}

/// `real_key`: call the real Claude API with this key; `None`: the local stand-in.
fn end_to_end(real_key: Option<String>) {
    let Some(display) = std::env::var_os("DISPLAY") else {
        eprintln!("skipped: no DISPLAY (run under xvfb-run)");
        return;
    };
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let real = real_key.is_some();
    let data = std::env::temp_dir().join(format!(
        "dw-e2e-{}-{}",
        std::process::id(),
        if real { "real" } else { "fake" }
    ));
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(&data).unwrap();
    // Our own process plays "the browser": its windows are tabs.
    let exe = std::env::current_exe().unwrap();
    let browser = exe.file_name().unwrap().to_string_lossy().to_string();
    let api_key = real_key.clone().unwrap_or_else(|| "test-key".to_string());
    std::fs::write(
        data.join("ai_focus.json"),
        json!({
            "api_key": api_key,
            "send_screenshot": false,
            "browsers": [browser],
        })
        .to_string(),
    )
    .unwrap();

    let mut agent = Command::new(env!("CARGO_BIN_EXE_digital-wellbeing-service"));
    let fake_calls = (!real).then(|| {
        let (claude_url, calls) = fake_claude("127.0.0.1:0");
        agent.env("DW_CLAUDE_API_URL", &claude_url);
        calls
    });
    let port_base = 20000 + (std::process::id() % 20000) as u16;
    // SAFETY (set_var): the tests run one at a time (SERIAL), and this test's threads read
    // the environment only after this line.
    std::env::set_var("DW_PORT_BASE", port_base.to_string());
    let _agent = Agent(
        agent
            .arg("agent")
            .env("DW_DATA_DIR", &data)
            .env("DW_PORT_BASE", port_base.to_string())
            .env("DISPLAY", &display)
            .env("RUST_LOG", "debug,ureq=info,rustls=info")
            .spawn()
            .unwrap(),
    );
    // The real API takes seconds per call; the stand-in answers at once.
    let patience = if real { 4 } else { 1 };
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
        json!({ "task": "write a command-line tool in Rust" }),
    )
    .unwrap();
    assert!(started.session_active);

    // 2. Ctrl+W reaches the off-task tab within a few samples.
    let deadline = Instant::now() + Duration::from_secs(20 * patience);
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
    let status = wait_for(Duration::from_secs(10 * patience), || {
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
    if real {
        for v in &status.verdicts {
            println!("verdict: {} -> {:?} ({})", v.title, v.verdict, v.reason);
        }
    }
    // The stand-in never names a channel; the real AI may, so only the site is fixed.
    let block_key = if real {
        "youtube "
    } else {
        "youtube page: funny cats"
    };
    assert!(
        status.blocked.iter().any(|b| b.key.contains(block_key)),
        "{:?}",
        status.blocked
    );
    assert!(
        !status
            .blocked
            .iter()
            .any(|b| b.key.to_lowercase().contains("rust docs")),
        "{:?}",
        status.blocked
    );
    let cost = status.ai_cost_today_usd.unwrap_or(0.0);
    println!("estimated cost so far: ${cost:.4}");
    assert!(cost > 0.0);
    if let Some(calls) = &fake_calls {
        assert!(calls.load(Ordering::SeqCst) >= 1);
    }

    // 4. Settings while unlocked, a proposal, then the lock.
    let settings = call_value(session, "settings_get", json!({})).unwrap();
    assert_eq!(settings["files"]["ai_focus"]["api_key"], api_key.as_str());
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
    let proposal = proposed.last_proposal.expect("the proposal was reviewed");
    println!(
        "proposal approved: {} ({})",
        proposal.approved, proposal.reply
    );
    // Buying groceries breaks no rule; the real AI could still be strict, so only the
    // stand-in's answer is fixed.
    assert!(proposal.approved || real);
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

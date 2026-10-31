//! Responsibility: Drive the *installed* service end to end on Windows through the agent's user
//! API, the way the Focus app does: real windows, the agent the service started in this
//! session, and a local stand-in for the Claude Messages API.
//!
//! Needs an installed, running service whose environment points the agents at the stand-in
//! (`DW_CLAUDE_API_URL=http://<addr>`), and `DW_E2E_CLAUDE_ADDR=<addr>` for this test; CI sets
//! both (`.github/workflows/ci.yml`). Without `DW_E2E_CLAUDE_ADDR` the test is skipped.
//! What it checks, in order:
//! 1. the service started an agent in this session, and it answers on its user API;
//! 2. `settings_put` changes the AI focus settings, and the agent picks them up;
//! 3. a focus session starts; the off-task window is enforced (Ctrl+W when it is in front,
//!    minimized when it is behind), and both windows are judged by title;
//! 4. the verdicts, the block and the token usage show in the status;
//! 5. a to-do proposal goes through the AI; after `settings_lock`, a saved edit waits for the
//!    lock to end instead of taking effect, and can be discarded.
#![cfg(windows)]

mod common;

use common::fake_claude;
use machine::user_api::{call, call_value, Status};
use serde_json::json;
use std::ptr::null;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, GetKeyState, KEYEVENTF_KEYUP, VK_CONTROL, VK_MENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetForegroundWindow, IsIconic, PeekMessageW,
    RegisterClassW, SetForegroundWindow, TranslateMessage, CW_USEDEFAULT, MSG, PM_REMOVE,
    SC_KEYMENU, WM_KEYDOWN, WM_SYSCOMMAND, WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const VK_W: usize = 0x57;

#[test]
fn installed_agent_end_to_end() {
    let Ok(addr) = std::env::var("DW_E2E_CLAUDE_ADDR") else {
        eprintln!("skipped: DW_E2E_CLAUDE_ADDR is not set (needs the installed service)");
        return;
    };
    // A test that hangs would hold the CI job for hours; end it with a message instead.
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(300));
        eprintln!("the end-to-end test took more than 5 minutes; giving up");
        std::process::exit(2);
    });
    let (_, claude_calls) = fake_claude(&addr);
    let session = machine::session::current_session_id();
    let port = machine::user_api::port_for_session(session);

    // 1. The agent the service started for this session.
    let mut desk = Desktop::default();
    let status = desk
        .wait(Duration::from_secs(60), || {
            call(session, "status", json!({})).ok()
        })
        .unwrap_or_else(|| panic!("no agent answers for session {session} on port {port}"));
    println!("agent answers for session {session} on port {port}");
    if status.session_active {
        call(session, "stop_session", json!({})).unwrap();
    }

    // 2. Our own process plays "the browser": its windows are tabs.
    let exe = std::env::current_exe().unwrap();
    let browser = exe.file_name().unwrap().to_string_lossy().to_string();
    let ai_focus = json!({
        "api_key": "test-key",
        "model": "claude-opus-5-5",
        "send_screenshot": false,
        "browsers": [browser],
    });
    call_value(
        session,
        "settings_put",
        json!({ "name": "ai_focus", "content": ai_focus }),
    )
    .unwrap();
    let settings = call_value(session, "settings_get", json!({})).unwrap();
    assert_eq!(settings["files"]["ai_focus"]["api_key"], "test-key");
    // The agent reloads changed settings every few seconds.
    desk.pump_for(Duration::from_secs(8));

    // 3. An on-task tab behind, an off-task tab in front (if Windows lets us bring it there).
    let docs = desk.window("Rust docs - Google Chrome");
    let cats = desk.window("Funny cats - YouTube");
    let in_front = desk.bring_to_front(cats);
    println!("off-task window in front: {in_front}");

    let started = call(
        session,
        "start_session",
        json!({ "task": "write my thesis" }),
    )
    .unwrap();
    assert!(started.session_active);

    let enforced = desk.wait(Duration::from_secs(40), || {
        if desk_ctrl_w(cats) {
            Some("Ctrl+W")
        } else if unsafe { IsIconic(cats) } != 0 {
            Some("minimized")
        } else {
            None
        }
    });
    let how = enforced.expect("the off-task window was neither sent Ctrl+W nor minimized");
    println!("off-task window: {how}");
    assert_eq!(
        unsafe { IsIconic(docs) },
        0,
        "the on-task window was minimized"
    );

    // 4. What the Focus app shows.
    let status = desk
        .wait(Duration::from_secs(20), || {
            let s: Status = call(session, "status", json!({})).ok()?;
            let judged = |t: &str| s.verdicts.iter().any(|v| v.title.contains(t));
            (judged("Rust docs") && judged("Funny cats")).then_some(s)
        })
        .expect("both windows were judged by title");
    assert!(
        status
            .blocked
            .iter()
            .any(|b| b.key.contains("youtube page: funny cats")),
        "{:?}",
        status.blocked
    );
    assert!(
        !status.blocked.iter().any(|b| b.key.contains("rust docs")),
        "{:?}",
        status.blocked
    );
    assert!(status.ai_cost_today_usd.unwrap_or(0.0) > 0.0);
    assert!(claude_calls.load(Ordering::SeqCst) >= 1);

    // 5. A proposal, then the lock.
    let proposed = call(
        session,
        "propose_todo",
        json!({ "todo": { "title": "Buy groceries" }, "via": "e2e" }),
    )
    .unwrap();
    assert!(proposed.last_proposal.as_ref().is_some_and(|p| p.approved));
    call(session, "stop_session", json!({})).unwrap();
    common::check_lock_delays_edits(session);
}

/// Set by the message pump when a window gets Ctrl+W; read by the wait probe.
static CTRL_W: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

fn desk_ctrl_w(window: HWND) -> bool {
    CTRL_W.lock().unwrap().contains(&(window as usize))
}

/// Our windows, and the message pump that keeps them responsive: a window whose thread stops
/// pumping is "not responding" after a few seconds, and Windows puts a ghost window in front.
#[derive(Default)]
struct Desktop {
    class: Vec<u16>,
}

impl Desktop {
    fn window(&mut self, title: &str) -> HWND {
        // SAFETY: the class and title buffers outlive the calls; DefWindowProcW is a valid
        // window procedure.
        unsafe {
            let instance = GetModuleHandleW(null());
            if self.class.is_empty() {
                self.class = wide("DwE2eWindow");
                let mut class: WNDCLASSW = std::mem::zeroed();
                class.lpfnWndProc = Some(window_proc);
                class.hInstance = instance;
                class.lpszClassName = self.class.as_ptr();
                assert_ne!(RegisterClassW(&class), 0, "RegisterClassW failed");
            }
            let title = wide(title);
            let hwnd = CreateWindowExW(
                0,
                self.class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                640,
                400,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                null(),
            );
            assert!(!hwnd.is_null(), "CreateWindowExW failed");
            self.pump_for(Duration::from_millis(300));
            hwnd
        }
    }

    /// Windows only lets a process take the foreground in some cases; a key press first
    /// (Alt, which does nothing on its own) is the usual way to be allowed.
    fn bring_to_front(&mut self, hwnd: HWND) -> bool {
        // SAFETY: plain Win32 calls on our own window.
        unsafe {
            keybd_event(VK_MENU as u8, 0, 0, 0);
            keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
            SetForegroundWindow(hwnd);
        }
        self.pump_for(Duration::from_millis(500));
        unsafe { GetForegroundWindow() == hwnd }
    }

    fn pump(&mut self) {
        // SAFETY: `msg` is a valid out-buffer; messages go to our own windows.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                // GetKeyState is the key state as of the message just taken from the queue.
                if msg.message == WM_KEYDOWN
                    && msg.wParam == VK_W
                    && GetKeyState(VK_CONTROL as i32) < 0
                {
                    CTRL_W.lock().unwrap().push(msg.hwnd as usize);
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    fn pump_for(&mut self, time: Duration) {
        let deadline = Instant::now() + time;
        while Instant::now() < deadline {
            self.pump();
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Like `common::wait_for`, but keeps the windows responsive while it waits.
    fn wait<T>(&mut self, limit: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            self.pump();
            if let Some(value) = probe() {
                return Some(value);
            }
            self.pump_for(Duration::from_millis(200));
        }
        None
    }
}

/// The default window procedure, except that a lone Alt press does not open the window's
/// system menu: that menu runs a modal loop inside `DispatchMessageW`, and the test would wait
/// there until someone pressed a key (`bring_to_front` presses Alt).
unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_SYSCOMMAND && (w & 0xFFF0) == SC_KEYMENU as usize {
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

//! Responsibility: Implement Desktop on Windows with Win32 calls and xcap.

use super::{Desktop, Foreground, OpenWindow, Screenshot};
use parking_lot::Mutex;
use std::mem::size_of;
use sysinfo::{Pid, ProcessesToUpdate, System};
use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
    VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, EnumWindows, GetForegroundWindow, GetWindow, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, ShowWindow,
    GWL_EXSTYLE, GW_OWNER, SW_SHOWMINNOACTIVE, WS_EX_TOOLWINDOW,
};

const VK_W: VIRTUAL_KEY = 0x57;

/// Responsibility: Answer Desktop queries on Windows; cache a sysinfo System to turn pids into
/// process names.
pub struct WindowsDesktop {
    system: Mutex<System>,
}

impl WindowsDesktop {
    pub fn new() -> Self {
        Self {
            system: Mutex::new(System::new()),
        }
    }

    fn process_name(&self, pid: u32) -> Option<String> {
        let pid = Pid::from_u32(pid);
        let mut system = self.system.lock();
        system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        system
            .process(pid)
            .map(|p| p.name().to_string_lossy().to_string())
    }
}

impl Desktop for WindowsDesktop {
    fn foreground(&self) -> Option<Foreground> {
        // SAFETY: plain Win32 queries on the foreground HWND; buffers are sized by us.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() {
                return None;
            }
            let mut pid = window_pid(hwnd);
            if pid == 0 {
                return None;
            }
            let mut process_name = self.process_name(pid)?;
            // Store (UWP) apps sit inside a frame owned by ApplicationFrameHost,
            // which is protected; judge the app process hosted in the frame.
            if process_name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
                if let Some(app_pid) = hosted_app_pid(hwnd, pid) {
                    if let Some(name) = self.process_name(app_pid) {
                        pid = app_pid;
                        process_name = name;
                    }
                }
            }
            Some(Foreground {
                window: hwnd as isize,
                pid,
                process_name,
                title: window_title(hwnd),
            })
        }
    }

    fn screenshot(&self, fg: &Foreground) -> Option<Screenshot> {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: rect is a valid out-pointer; a stale HWND just makes the call fail.
        let ok = unsafe { GetWindowRect(fg.window as HWND, &mut rect) } != 0;
        let monitor = if ok {
            let cx = (rect.left + rect.right) / 2;
            let cy = (rect.top + rect.bottom) / 2;
            xcap::Monitor::from_point(cx, cy).ok()
        } else {
            None
        };
        let monitor = match monitor {
            Some(m) => m,
            None => xcap::Monitor::all()
                .ok()?
                .into_iter()
                .find(|m| m.is_primary().unwrap_or(false))?,
        };
        let image = monitor.capture_image().ok()?;
        Some(Screenshot {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }

    fn visible_processes(&self) -> Vec<String> {
        let mut pids: Vec<u32> = Vec::new();
        // SAFETY: the callback only writes to `pids`, which outlives the call.
        unsafe {
            EnumWindows(Some(collect_visible), &mut pids as *mut Vec<u32> as LPARAM);
        }
        pids.sort_unstable();
        pids.dedup();
        pids.into_iter()
            .filter_map(|pid| self.process_name(pid))
            .collect()
    }

    fn open_windows(&self) -> Vec<OpenWindow> {
        let mut handles: Vec<HWND> = Vec::new();
        // SAFETY: the callback only pushes into `handles`, which outlives the call.
        unsafe {
            EnumWindows(Some(collect_open), &mut handles as *mut Vec<HWND> as LPARAM);
        }
        handles
            .into_iter()
            .filter_map(|hwnd| {
                let mut pid = window_pid(hwnd);
                let mut process_name = self.process_name(pid)?;
                // Store apps: name the app inside the frame, as `foreground` does.
                if process_name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
                    let app_pid = hosted_app_pid(hwnd, pid)?;
                    process_name = self.process_name(app_pid)?;
                    pid = app_pid;
                }
                Some(OpenWindow {
                    window: hwnd as isize,
                    pid,
                    process_name,
                    title: window_title(hwnd),
                    // SAFETY: a plain query on a handle just enumerated.
                    minimized: unsafe { IsIconic(hwnd) } != 0,
                })
            })
            .collect()
    }

    fn minimize(&self, window: isize) -> bool {
        // SAFETY: a stale handle just makes the call fail. SW_SHOWMINNOACTIVE keeps the
        // focus where it is.
        unsafe { ShowWindow(window as HWND, SW_SHOWMINNOACTIVE) != 0 }
    }

    fn close_active_tab(&self, fg: &Foreground) -> bool {
        // SAFETY: SendInput reads `inputs` for the given count only.
        unsafe {
            let hwnd = GetForegroundWindow();
            // Same window *and* same tab, checked right before the keys go out.
            if hwnd as isize != fg.window || window_title(hwnd) != fg.title {
                return false;
            }
            // Keys the user holds would combine with ours: Shift makes Ctrl+W
            // close the whole window. Try again on a later sample instead.
            let held = |vk: VIRTUAL_KEY| (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0;
            if [VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN, VK_CONTROL]
                .into_iter()
                .any(held)
            {
                return false;
            }
            let inputs = [
                key(VK_CONTROL, false),
                key(VK_W, false),
                key(VK_W, true),
                key(VK_CONTROL, true),
            ];
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                size_of::<INPUT>() as i32,
            ) == inputs.len() as u32
        }
    }
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: valid out-pointer; a stale HWND just yields 0.
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
    }
    pid
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    // SAFETY: the buffer length is passed along.
    let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Pid of the app inside an ApplicationFrameHost frame: the first child
/// window owned by a different process.
fn hosted_app_pid(frame: HWND, frame_pid: u32) -> Option<u32> {
    struct Search {
        frame_pid: u32,
        found: u32,
    }
    unsafe extern "system" fn visit(child: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Search passed below, alive for the call.
        let search = unsafe { &mut *(lparam as *mut Search) };
        let pid = window_pid(child);
        if pid != 0 && pid != search.frame_pid {
            search.found = pid;
            return 0; // stop
        }
        1
    }
    let mut search = Search {
        frame_pid,
        found: 0,
    };
    // SAFETY: `search` outlives the enumeration.
    unsafe {
        EnumChildWindows(frame, Some(visit), &mut search as *mut Search as LPARAM);
    }
    (search.found != 0).then_some(search.found)
}

/// EnumWindows callback: collect the windows `open_windows` reports.
unsafe extern "system" fn collect_open(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec<HWND> passed by open_windows; the rest are plain
    // queries on the enumerated handle.
    unsafe {
        let mut cloaked = 0u32;
        let is_cloaked = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        ) == 0
            && cloaked != 0;
        let tool = (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW) != 0;
        let owned = !GetWindow(hwnd, GW_OWNER).is_null();
        if IsWindowVisible(hwnd) != 0
            && !is_cloaked
            && !tool
            && !owned
            && !window_title(hwnd).trim().is_empty()
        {
            (*(lparam as *mut Vec<HWND>)).push(hwnd);
        }
    }
    1
}

/// EnumWindows callback: collect pids of visible, non-minimized windows.
unsafe extern "system" fn collect_visible(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec<u32> passed by visible_processes.
    unsafe {
        if IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 {
            let pids = &mut *(lparam as *mut Vec<u32>);
            let pid = window_pid(hwnd);
            if pid != 0 {
                pids.push(pid);
            }
        }
    }
    1
}

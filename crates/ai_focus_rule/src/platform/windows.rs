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

    fn process_name(&self, process_id: u32) -> Option<String> {
        let sysinfo_pid = Pid::from_u32(process_id);
        let mut system = self.system.lock();
        system.refresh_processes(ProcessesToUpdate::Some(&[sysinfo_pid]), true);
        system
            .process(sysinfo_pid)
            .map(|process| process.name().to_string_lossy().to_string())
    }
}

impl Desktop for WindowsDesktop {
    fn foreground(&self) -> Option<Foreground> {
        // SAFETY: plain Win32 queries on the foreground HWND; buffers are sized by us.
        unsafe {
            let window_handle = GetForegroundWindow();
            if window_handle.is_null() {
                return None;
            }
            let mut process_id = window_process_id(window_handle);
            if process_id == 0 {
                return None;
            }
            let mut process_name = self.process_name(process_id)?;
            // Store (UWP) apps sit inside a frame owned by ApplicationFrameHost,
            // which is protected; judge the app process hosted in the frame.
            if process_name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
                if let Some(app_process_id) = hosted_app_process_id(window_handle, process_id) {
                    if let Some(name) = self.process_name(app_process_id) {
                        process_id = app_process_id;
                        process_name = name;
                    }
                }
            }
            Some(Foreground {
                window: window_handle as isize,
                pid: process_id,
                process_name,
                title: window_title(window_handle),
            })
        }
    }

    fn screenshot(&self, foreground: &Foreground) -> Option<Screenshot> {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: rect is a valid out-pointer; a stale HWND just makes the call fail.
        let found_rect = unsafe { GetWindowRect(foreground.window as HWND, &mut rect) } != 0;
        let monitor = if found_rect {
            let center_x = (rect.left + rect.right) / 2;
            let center_y = (rect.top + rect.bottom) / 2;
            xcap::Monitor::from_point(center_x, center_y).ok()
        } else {
            None
        };
        let monitor = match monitor {
            Some(monitor) => monitor,
            None => xcap::Monitor::all()
                .ok()?
                .into_iter()
                .find(|candidate| candidate.is_primary().unwrap_or(false))?,
        };
        let image = monitor.capture_image().ok()?;
        Some(Screenshot {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }

    fn visible_processes(&self) -> Vec<String> {
        let mut process_ids: Vec<u32> = Vec::new();
        // SAFETY: the callback only writes to `process_ids`, which outlives the call.
        unsafe {
            EnumWindows(
                Some(collect_visible),
                &mut process_ids as *mut Vec<u32> as LPARAM,
            );
        }
        process_ids.sort_unstable();
        process_ids.dedup();
        process_ids
            .into_iter()
            .filter_map(|process_id| self.process_name(process_id))
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
            .filter_map(|window_handle| {
                let mut process_id = window_process_id(window_handle);
                let mut process_name = self.process_name(process_id)?;
                // Store apps: name the app inside the frame, as `foreground` does.
                if process_name.eq_ignore_ascii_case("ApplicationFrameHost.exe") {
                    let app_process_id = hosted_app_process_id(window_handle, process_id)?;
                    process_name = self.process_name(app_process_id)?;
                    process_id = app_process_id;
                }
                Some(OpenWindow {
                    window: window_handle as isize,
                    pid: process_id,
                    process_name,
                    title: window_title(window_handle),
                    // SAFETY: a plain query on a handle just enumerated.
                    minimized: unsafe { IsIconic(window_handle) } != 0,
                })
            })
            .collect()
    }

    fn minimize(&self, window: isize) -> bool {
        // SAFETY: a stale handle just makes the call fail. SW_SHOWMINNOACTIVE keeps the
        // focus where it is.
        unsafe { ShowWindow(window as HWND, SW_SHOWMINNOACTIVE) != 0 }
    }

    fn close_active_tab(&self, foreground: &Foreground) -> bool {
        // SAFETY: SendInput reads `inputs` for the given count only.
        unsafe {
            let window_handle = GetForegroundWindow();
            // Same window *and* same tab, checked right before the keys go out.
            if window_handle as isize != foreground.window
                || window_title(window_handle) != foreground.title
            {
                return false;
            }
            // Keys the user holds would combine with ours: Shift makes Ctrl+W
            // close the whole window. Try again on a later sample instead.
            let held = |virtual_key: VIRTUAL_KEY| {
                (GetAsyncKeyState(virtual_key as i32) as u16 & 0x8000) != 0
            };
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

fn key(virtual_key: VIRTUAL_KEY, key_up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: 0,
                dwFlags: if key_up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn window_process_id(window_handle: HWND) -> u32 {
    let mut process_id = 0u32;
    // SAFETY: valid out-pointer; a stale HWND just yields 0.
    unsafe {
        GetWindowThreadProcessId(window_handle, &mut process_id);
    }
    process_id
}

fn window_title(window_handle: HWND) -> String {
    let mut buffer = [0u16; 512];
    // SAFETY: the buffer length is passed along.
    let length = unsafe { GetWindowTextW(window_handle, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

/// Pid of the app inside an ApplicationFrameHost frame: the first child
/// window owned by a different process.
fn hosted_app_process_id(frame: HWND, frame_process_id: u32) -> Option<u32> {
    struct Search {
        frame_process_id: u32,
        found: u32,
    }
    unsafe extern "system" fn visit(child: HWND, search_pointer: LPARAM) -> BOOL {
        // SAFETY: `search_pointer` is the &mut Search passed below, alive for the call.
        let search = unsafe { &mut *(search_pointer as *mut Search) };
        let process_id = window_process_id(child);
        if process_id != 0 && process_id != search.frame_process_id {
            search.found = process_id;
            return 0; // stop
        }
        1
    }
    let mut search = Search {
        frame_process_id,
        found: 0,
    };
    // SAFETY: `search` outlives the enumeration.
    unsafe {
        EnumChildWindows(frame, Some(visit), &mut search as *mut Search as LPARAM);
    }
    (search.found != 0).then_some(search.found)
}

/// EnumWindows callback: collect the windows `open_windows` reports.
unsafe extern "system" fn collect_open(window_handle: HWND, handles_pointer: LPARAM) -> BOOL {
    // SAFETY: `handles_pointer` is the &mut Vec<HWND> passed by open_windows; the rest are plain
    // queries on the enumerated handle.
    unsafe {
        let mut cloaked = 0u32;
        let is_cloaked = DwmGetWindowAttribute(
            window_handle,
            DWMWA_CLOAKED as u32,
            &mut cloaked as *mut u32 as *mut _,
            size_of::<u32>() as u32,
        ) == 0
            && cloaked != 0;
        let is_tool_window =
            (GetWindowLongPtrW(window_handle, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW) != 0;
        let owned = !GetWindow(window_handle, GW_OWNER).is_null();
        if IsWindowVisible(window_handle) != 0
            && !is_cloaked
            && !is_tool_window
            && !owned
            && !window_title(window_handle).trim().is_empty()
        {
            (*(handles_pointer as *mut Vec<HWND>)).push(window_handle);
        }
    }
    1
}

/// EnumWindows callback: collect pids of visible, non-minimized windows.
unsafe extern "system" fn collect_visible(
    window_handle: HWND,
    process_ids_pointer: LPARAM,
) -> BOOL {
    // SAFETY: `process_ids_pointer` is the &mut Vec<u32> passed by visible_processes.
    unsafe {
        if IsWindowVisible(window_handle) != 0 && IsIconic(window_handle) == 0 {
            let process_ids = &mut *(process_ids_pointer as *mut Vec<u32>);
            let process_id = window_process_id(window_handle);
            if process_id != 0 {
                process_ids.push(process_id);
            }
        }
    }
    1
}

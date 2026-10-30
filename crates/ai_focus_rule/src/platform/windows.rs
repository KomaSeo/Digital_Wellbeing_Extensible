//! Responsibility: Implement Desktop on Windows with Win32 calls and xcap.

use super::{Desktop, Foreground, Screenshot};
use parking_lot::Mutex;
use std::mem::size_of;
use sysinfo::{Pid, ProcessesToUpdate, System};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
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
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == 0 {
                return None;
            }
            let mut buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            let title = String::from_utf16_lossy(&buf[..len.max(0) as usize]);
            Some(Foreground {
                window: hwnd as isize,
                pid,
                process_name: self.process_name(pid)?,
                title,
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

    fn close_active_tab(&self, fg: &Foreground) -> bool {
        // SAFETY: SendInput reads `inputs` for the given count only.
        unsafe {
            if GetForegroundWindow() as isize != fg.window {
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

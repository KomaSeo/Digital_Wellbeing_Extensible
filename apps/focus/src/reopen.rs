//! Responsibility: Open again the tab the AI closed, once access to it is granted: bring its
//! browser window to the front and press Ctrl+Shift+T there (the browser's "reopen closed
//! tab"). The Focus app does this, not the agent, because only the program the user is
//! using may bring another window to the front.
//!
//! Ctrl+Shift+T opens the tab closed last in that window, so a tab the user closed after the
//! block would come back instead. The check window says so.

/// `true` when the keys were sent to `window` (Windows only). Waits a quarter second for the
/// window to come to the front, so call it off the UI thread.
#[cfg(windows)]
pub(crate) fn reopen_closed_tab(window: isize) -> bool {
    use std::mem::size_of;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP,
        VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_T,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, IsWindow, SetForegroundWindow, ShowWindow, SW_RESTORE,
    };
    if window == 0 {
        return false;
    }
    let key = |virtual_key: VIRTUAL_KEY, key_up: bool| INPUT {
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
    };
    // SAFETY: Win32 calls on a window handle that IsWindow checked; SendInput reads `inputs`
    // for the given count only.
    unsafe {
        let handle = window as windows_sys::Win32::Foundation::HWND;
        if IsWindow(handle) == 0 {
            return false;
        }
        ShowWindow(handle, SW_RESTORE);
        SetForegroundWindow(handle);
        std::thread::sleep(std::time::Duration::from_millis(250));
        if GetForegroundWindow() != handle {
            return false;
        }
        // Keys the user holds would combine with ours (Alt makes it Ctrl+Alt+Shift+T, which
        // reopens nothing): the user is asked to press it instead, as the agent does for Ctrl+W.
        let held =
            |virtual_key: VIRTUAL_KEY| (GetAsyncKeyState(virtual_key as i32) as u16 & 0x8000) != 0;
        if [VK_SHIFT, VK_MENU, VK_LWIN, VK_RWIN, VK_CONTROL]
            .into_iter()
            .any(held)
        {
            return false;
        }
        let inputs = [
            key(VK_CONTROL, false),
            key(VK_SHIFT, false),
            key(VK_T, false),
            key(VK_T, true),
            key(VK_SHIFT, true),
            key(VK_CONTROL, true),
        ];
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        ) == inputs.len() as u32
    }
}

#[cfg(not(windows))]
pub(crate) fn reopen_closed_tab(_window: isize) -> bool {
    false
}

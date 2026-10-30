//! Responsibility: Keep the Focus app in the notification area (tray) and bring its window back:
//! a click on the icon opens the window, the icon's menu opens it, opens its Settings tab, or
//! quits the app.
//!
//! The icon lives on its own thread with its own hidden window and message loop (plain Win32,
//! `Shell_NotifyIconW`), so it keeps working while the egui window is hidden. Closing the
//! window only hides it (`main.rs`); "Quit" in the menu ends the app. Quitting the Focus app
//! changes nothing about enforcement: the agent keeps running.
//!
//! On other systems there is no tray: [`start`] does nothing and the window closes normally.

/// Responsibility: Name what the user picked on the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum TrayCommand {
    Open,
    Settings,
}

#[cfg(windows)]
pub use imp::{activate_existing, show_window, start};

/// Whether this build has a tray icon (closing the window then hides it).
pub const AVAILABLE: bool = cfg!(windows);

#[cfg(not(windows))]
pub fn start(_on_command: impl Fn(TrayCommand) + Send + Sync + 'static) {}

#[cfg(not(windows))]
pub fn show_window(_title: &str) {}

#[cfg(not(windows))]
pub fn activate_existing(_title: &str) -> bool {
    false
}

#[cfg(windows)]
mod imp {
    use super::TrayCommand;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::OnceLock;
    use std::thread;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, FindWindowExW, GetCursorPos, GetMessageW, GetWindowThreadProcessId,
        LoadIconW, PostMessageW, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow,
        ShowWindow, TrackPopupMenu, TranslateMessage, IDI_APPLICATION, MF_SEPARATOR, MF_STRING,
        MSG, SW_RESTORE, SW_SHOW, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU,
        WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    };

    /// Message the icon sends to our hidden window; `lParam` is the mouse message.
    const WM_TRAY: u32 = WM_APP + 1;
    const ID_OPEN: usize = 1;
    const ID_SETTINGS: usize = 2;
    const ID_QUIT: usize = 3;
    const TIP: &str = "Digital Wellbeing Focus";

    static HANDLER: OnceLock<Box<dyn Fn(TrayCommand) + Send + Sync>> = OnceLock::new();
    /// Explorer broadcasts this after it restarts; the icon must then be added again.
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    /// Start the icon; `on_command` runs on the tray thread.
    pub fn start(on_command: impl Fn(TrayCommand) + Send + Sync + 'static) {
        if HANDLER.set(Box::new(on_command)).is_err() {
            return; // already running
        }
        thread::spawn(|| {
            // SAFETY: plain Win32 window and message loop owned by this thread; every
            // pointer passed points at a live local.
            unsafe { run() }
        });
    }

    unsafe fn run() {
        let instance = GetModuleHandleW(null());
        let class = wide("DigitalWellbeingFocusTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
            Ordering::SeqCst,
        );
        // A hidden top-level window (not message-only: those miss TaskbarCreated).
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if hwnd.is_null() {
            tracing_fallback("could not create the tray window");
            return;
        }
        add_icon(hwnd);
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    unsafe fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data: NOTIFYICONDATAW = zeroed();
        data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        data
    }

    unsafe fn add_icon(hwnd: HWND) {
        let mut data = icon_data(hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
        for (slot, c) in data.szTip.iter_mut().zip(TIP.encode_utf16()) {
            *slot = c;
        }
        Shell_NotifyIconW(NIM_ADD, &data);
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let handle = |command| {
            if let Some(handler) = HANDLER.get() {
                handler(command);
            }
        };
        if msg == WM_TRAY {
            match lparam as u32 {
                WM_LBUTTONUP => handle(TrayCommand::Open),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            return 0;
        }
        if msg == WM_COMMAND {
            match wparam & 0xffff {
                ID_OPEN => handle(TrayCommand::Open),
                ID_SETTINGS => handle(TrayCommand::Settings),
                ID_QUIT => {
                    Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd));
                    std::process::exit(0);
                }
                _ => {}
            }
            return 0;
        }
        let taskbar_created = TASKBAR_CREATED.load(Ordering::SeqCst);
        if taskbar_created != 0 && msg == taskbar_created {
            add_icon(hwnd);
            return 0;
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    unsafe fn show_menu(hwnd: HWND) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        AppendMenuW(menu, MF_STRING, ID_OPEN, wide("Open").as_ptr());
        AppendMenuW(menu, MF_STRING, ID_SETTINGS, wide("Settings").as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(menu, MF_STRING, ID_QUIT, wide("Quit").as_ptr());
        let mut point = POINT { x: 0, y: 0 };
        GetCursorPos(&mut point);
        // Without this the menu stays open when the user clicks elsewhere.
        SetForegroundWindow(hwnd);
        TrackPopupMenu(menu, TPM_RIGHTBUTTON, point.x, point.y, 0, hwnd, null());
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(menu);
    }

    /// The first top-level window titled `title` that belongs to `pid` (or, with `others`,
    /// to any other process).
    unsafe fn find(title: &str, pid: u32, others: bool) -> Option<HWND> {
        let title = wide(title);
        let mut after: HWND = null_mut();
        loop {
            let hwnd = FindWindowExW(null_mut(), after, null(), title.as_ptr());
            if hwnd.is_null() {
                return None;
            }
            let mut owner = 0u32;
            GetWindowThreadProcessId(hwnd, &mut owner);
            if (owner == pid) != others {
                return Some(hwnd);
            }
            after = hwnd;
        }
    }

    unsafe fn bring_to_front(hwnd: HWND) {
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }

    /// Show this process's window titled `title` (hidden to the tray or minimized).
    pub fn show_window(title: &str) {
        // SAFETY: Win32 window lookups and show calls on handles just found.
        unsafe {
            if let Some(hwnd) = find(title, std::process::id(), false) {
                bring_to_front(hwnd);
            }
        }
    }

    /// If another Focus app already runs (perhaps hidden in the tray), show its window and
    /// return true: this one should exit instead of adding a second icon.
    pub fn activate_existing(title: &str) -> bool {
        // SAFETY: as in `show_window`.
        unsafe {
            match find(title, std::process::id(), true) {
                Some(hwnd) => {
                    bring_to_front(hwnd);
                    true
                }
                None => false,
            }
        }
    }

    /// The Focus app has no log file; stderr is the best it can do.
    fn tracing_fallback(message: &str) {
        eprintln!("tray: {message}");
    }
}

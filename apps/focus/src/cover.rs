//! Responsibility: Cover every screen with the Focus app while everything is locked (no task
//! assigned, use stopped, a login needed), instead of closing every program (the owner:
//! "앱이나 웹사이트를 도륙내는 대신에, digital wellbeing focus앱이 전체화면으로 전환되게
//! 만들고 항상 맨 앞으로 나오게 만들 수 있나?").
//!
//! [`Cover::follow`] runs every frame. When the agent's status says the lock covers the screen
//! (`Lockdown::covers_screen`), the window loses its frame, goes on top of every other window
//! and, about once a second, is put back over the whole desktop (every monitor) and brought to
//! the front. When the lock ends, the window gets its frame, place and size back. The agent
//! closes a program kept in front of it for a few seconds (`machine::lockdown`).

use eframe::egui;
use std::time::{Duration, Instant};

/// How often the window is put back over the screen and in front.
const HOLD_EVERY: Duration = Duration::from_secs(1);

/// Responsibility: Remember whether the window covers the screen and where it was before.
#[derive(Default)]
pub(crate) struct Cover {
    covering: bool,
    /// The window's outer position and inner size before it covered the screen.
    place_before: Option<(egui::Pos2, egui::Vec2)>,
    last_hold: Option<Instant>,
}

impl Cover {
    /// The window covers the screen now.
    pub(crate) fn covering(&self) -> bool {
        self.covering
    }

    /// Start or stop covering the screen as `covers` says, and keep the window in front
    /// meanwhile. Returns true when covering has just started.
    pub(crate) fn follow(&mut self, context: &egui::Context, title: &str, covers: bool) -> bool {
        let started = covers && !self.covering;
        if started {
            self.covering = true;
            self.last_hold = None;
            self.place_before = context.input(|input| {
                let viewport = input.viewport();
                viewport
                    .outer_rect
                    .zip(viewport.inner_rect)
                    .map(|(outer, inner)| (outer.min, inner.size()))
            });
            context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            context.send_viewport_cmd(egui::ViewportCommand::Decorations(false));
            context.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            if !cfg!(windows) {
                // Windows: `hold_in_front` spans every monitor instead. Elsewhere, fullscreen,
                // and the monitor's size too for a desktop without a window manager.
                context.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                if let Some(monitor) = context.input(|input| input.viewport().monitor_size) {
                    context
                        .send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::Pos2::ZERO));
                    context.send_viewport_cmd(egui::ViewportCommand::InnerSize(monitor));
                }
            }
        } else if !covers && self.covering {
            self.covering = false;
            release(title);
            if !cfg!(windows) {
                context.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            }
            context.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::Normal,
            ));
            context.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
            if let Some((position, size)) = self.place_before.take() {
                context.send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                context.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            }
        }
        if self.covering {
            if self
                .last_hold
                .is_none_or(|held| held.elapsed() >= HOLD_EVERY)
            {
                self.last_hold = Some(Instant::now());
                if !hold_in_front(title) {
                    context.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
            context.request_repaint_after(HOLD_EVERY / 2);
        }
        started
    }
}

/// Put this process's window titled `title` over the whole desktop (every monitor), on top
/// of every other window, and make it the window in front. Windows only lets the program in
/// front hand over the front, so the window joins that program's input for the moment it
/// takes it; when that is not enough, one press of Alt (the last input then is this
/// program's) lets it take the front. A program left in front would be closed by the agent
/// after a few seconds, and the next one after it. Returns false when the window was not
/// found.
#[cfg(windows)]
fn hold_in_front(title: &str) -> bool {
    use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_MENU};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetSystemMetrics, GetWindowThreadProcessId,
        IsIconic, SetForegroundWindow, SetWindowPos, ShowWindow, HWND_TOPMOST, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_SHOWWINDOW, SW_RESTORE,
    };
    let Some(own) = crate::tray::own_window(title) else {
        return false;
    };
    // SAFETY: Win32 calls on this process's own window handle, just found, and on the
    // foreground window's thread, which AttachThreadInput only joins for these calls.
    unsafe {
        if IsIconic(own) != 0 {
            ShowWindow(own, SW_RESTORE);
        }
        SetWindowPos(
            own,
            HWND_TOPMOST,
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
            SWP_SHOWWINDOW,
        );
        let front = GetForegroundWindow();
        if front != own {
            let own_thread = GetCurrentThreadId();
            let front_thread = if front.is_null() {
                0
            } else {
                GetWindowThreadProcessId(front, std::ptr::null_mut())
            };
            let joined = front_thread != 0
                && front_thread != own_thread
                && AttachThreadInput(own_thread, front_thread, 1) != 0;
            BringWindowToTop(own);
            SetForegroundWindow(own);
            if joined {
                AttachThreadInput(own_thread, front_thread, 0);
            }
            if GetForegroundWindow() != own {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(own);
            }
        }
    }
    true
}

#[cfg(not(windows))]
fn hold_in_front(_title: &str) -> bool {
    false
}

/// Take this process's window titled `title` off the top of every other window.
#[cfg(windows)]
fn release(title: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, HWND_NOTOPMOST, SWP_NOMOVE, SWP_NOSIZE,
    };
    if let Some(own) = crate::tray::own_window(title) {
        // SAFETY: a Win32 call on this process's own window handle, just found.
        unsafe {
            SetWindowPos(own, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE);
        }
    }
}

#[cfg(not(windows))]
fn release(_title: &str) {}

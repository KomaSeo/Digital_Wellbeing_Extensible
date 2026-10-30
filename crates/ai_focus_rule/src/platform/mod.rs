//! Responsibility: Define the operating-system access the monitor needs (foreground window,
//! screenshot, tab close) and pick the implementation for the current OS.

#[cfg(windows)]
mod windows;

/// Responsibility: Describe the window in front: handle, process and title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    /// Opaque native window handle (HWND on Windows).
    pub window: isize,
    pub pid: u32,
    pub process_name: String,
    pub title: String,
}

/// Responsibility: Hold the raw RGBA pixels of one captured monitor.
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Responsibility: Give the monitor the foreground window, a screenshot of its monitor, and a
/// way to close the active tab.
pub trait Desktop: Send + Sync {
    fn foreground(&self) -> Option<Foreground>;

    /// Capture the monitor that shows `fg`.
    fn screenshot(&self, fg: &Foreground) -> Option<Screenshot>;

    /// Process names owning visible, non-minimized top-level windows. A
    /// screenshot shows the whole monitor, so a private app does not have to be
    /// in front to appear in it.
    fn visible_processes(&self) -> Vec<String>;

    /// Close the active tab of a browser window, if it is still in front.
    fn close_active_tab(&self, fg: &Foreground) -> bool;
}

/// Desktop access for the current OS. On non-Windows builds this reports no
/// foreground window, so the monitor stays idle.
pub fn native() -> Box<dyn Desktop> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsDesktop::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(Unsupported)
    }
}

/// Responsibility: Stand in on systems without an implementation: report nothing, so the
/// monitor stays idle.
#[cfg(not(windows))]
struct Unsupported;

#[cfg(not(windows))]
impl Desktop for Unsupported {
    fn foreground(&self) -> Option<Foreground> {
        None
    }

    fn screenshot(&self, _fg: &Foreground) -> Option<Screenshot> {
        None
    }

    fn visible_processes(&self) -> Vec<String> {
        Vec::new()
    }

    fn close_active_tab(&self, _fg: &Foreground) -> bool {
        false
    }
}

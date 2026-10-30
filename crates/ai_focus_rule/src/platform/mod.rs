//! Responsibility: Define the operating-system access the monitor needs (foreground window,
//! screenshot, tab close) and pick the implementation for the current OS.

#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

/// Responsibility: Describe the window in front: handle, process and title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    /// Opaque native window handle (HWND on Windows).
    pub window: isize,
    pub pid: u32,
    pub process_name: String,
    pub title: String,
}

/// Responsibility: Describe one open top-level window (in front or not): handle, process,
/// title, and whether it is minimized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    pub window: isize,
    pub pid: u32,
    pub process_name: String,
    /// For a browser window: the title of the tab it shows.
    pub title: String,
    pub minimized: bool,
}

impl OpenWindow {
    /// The same window as a [`Foreground`], so the monitor's key functions apply.
    pub fn as_foreground(&self) -> Foreground {
        Foreground {
            window: self.window,
            pid: self.pid,
            process_name: self.process_name.clone(),
            title: self.title.clone(),
        }
    }
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

    /// Every open top-level window a user would call "a program" or "a browser window":
    /// visible (minimized included), titled, not a tool window, dialog or hidden Store app.
    /// A browser window reports only the tab it shows; its other tabs are not visible here.
    fn open_windows(&self) -> Vec<OpenWindow> {
        Vec::new()
    }

    /// Minimize `window` without taking the focus (used for a blocked tab shown in a
    /// background window, e.g. a video on a second monitor).
    fn minimize(&self, _window: isize) -> bool {
        false
    }
}

/// Desktop access for the current OS: Win32 on Windows, X11 on Linux when `DISPLAY` is set.
/// Elsewhere (and on Linux without X11) this reports no foreground window, so the monitor
/// stays idle.
pub fn native() -> Box<dyn Desktop> {
    #[cfg(windows)]
    {
        Box::new(windows::WindowsDesktop::new())
    }
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("DISPLAY").is_some_and(|d| !d.is_empty()) {
            return Box::new(x11::X11Desktop::new());
        }
        Box::new(Unsupported)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
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

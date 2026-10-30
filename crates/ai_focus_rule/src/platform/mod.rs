//! OS access the focus monitor needs: foreground window, screenshot, tab close.

#[cfg(windows)]
mod windows;

/// The window the user is currently looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Foreground {
    /// Opaque native window handle (HWND on Windows).
    pub window: isize,
    pub pid: u32,
    pub process_name: String,
    pub title: String,
}

/// Raw RGBA8 pixels.
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub trait Desktop: Send + Sync {
    fn foreground(&self) -> Option<Foreground>;

    /// Capture the monitor that shows `fg`.
    fn screenshot(&self, fg: &Foreground) -> Option<Screenshot>;

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

    fn close_active_tab(&self, _fg: &Foreground) -> bool {
        false
    }
}

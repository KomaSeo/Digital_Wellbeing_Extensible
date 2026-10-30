//! Responsibility: Find the Windows login session of the current process or of any pid, and
//! the user logged on to a session.
//!
//! Every logged-on user has their own session (desktop, windows, processes);
//! services run in session 0. The agent runs inside one user's session and
//! only enforces on processes of that session.

/// Session of the calling process.
pub fn current_session_id() -> u32 {
    process_session_id(std::process::id()).unwrap_or(0)
}

/// Session that owns `pid`, or `None` if the process is gone / inaccessible.
#[cfg(windows)]
pub fn process_session_id(pid: u32) -> Option<u32> {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut session = 0u32;
    // SAFETY: `session` is a valid out-pointer.
    (unsafe { ProcessIdToSessionId(pid, &mut session) } != 0).then_some(session)
}

/// Non-Windows: everything is "session 0".
#[cfg(not(windows))]
pub fn process_session_id(_pid: u32) -> Option<u32> {
    Some(0)
}

/// User name logged on to `session` (bare account name, no domain), if any.
#[cfg(windows)]
pub fn session_user(session: u32) -> Option<String> {
    use std::ffi::c_void;
    use std::ptr::null_mut;
    use windows_sys::Win32::System::RemoteDesktop::{
        WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName, WTS_CURRENT_SERVER_HANDLE,
    };
    let mut buf: *mut u16 = null_mut();
    let mut bytes = 0u32;
    // SAFETY: WTS allocates a NUL-terminated UTF-16 string; freed below.
    unsafe {
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session,
            WTSUserName,
            &mut buf,
            &mut bytes,
        ) == 0
        {
            return None;
        }
        let len = (0..).take_while(|&i| *buf.add(i) != 0).count();
        let user = String::from_utf16_lossy(std::slice::from_raw_parts(buf, len));
        WTSFreeMemory(buf as *mut c_void);
        Some(user).filter(|u| !u.is_empty())
    }
}

/// Non-Windows: the `USER` environment variable.
#[cfg(not(windows))]
pub fn session_user(_session: u32) -> Option<String> {
    std::env::var("USER").ok().filter(|u| !u.is_empty())
}

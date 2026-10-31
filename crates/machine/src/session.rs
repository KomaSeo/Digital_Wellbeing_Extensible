//! Responsibility: Find the login session of the current process or of any pid, and the user
//! logged on to a session.
//!
//! Windows: every logged-on user has their own session (desktop, windows, processes);
//! services run in session 0. The agent runs inside one user's session and only enforces
//! on processes of that session.
//!
//! Linux and other Unix: the "session" is the user id that owns the process, so an agent
//! started by (or for) a user enforces on that user's processes only.

/// Session of the calling process.
pub fn current_session_id() -> u32 {
    process_session_id(std::process::id()).unwrap_or(0)
}

/// Session that owns `process_id`, or `None` if the process is gone / inaccessible.
#[cfg(windows)]
pub fn process_session_id(process_id: u32) -> Option<u32> {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    let mut session = 0u32;
    // SAFETY: `session` is a valid out-pointer.
    (unsafe { ProcessIdToSessionId(process_id, &mut session) } != 0).then_some(session)
}

/// Unix: the uid owning the process (`/proc/<pid>`), `None` if it is gone.
#[cfg(target_os = "linux")]
pub fn process_session_id(process_id: u32) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(format!("/proc/{process_id}"))
        .ok()
        .map(|metadata| metadata.uid())
}

/// Other systems: everything is "session 0".
#[cfg(not(any(windows, target_os = "linux")))]
pub fn process_session_id(_process_id: u32) -> Option<u32> {
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
    let mut buffer: *mut u16 = null_mut();
    let mut bytes = 0u32;
    // SAFETY: WTS allocates a NUL-terminated UTF-16 string; freed below.
    unsafe {
        if WTSQuerySessionInformationW(
            WTS_CURRENT_SERVER_HANDLE,
            session,
            WTSUserName,
            &mut buffer,
            &mut bytes,
        ) == 0
        {
            return None;
        }
        let length = (0..).take_while(|&offset| *buffer.add(offset) != 0).count();
        let user = String::from_utf16_lossy(std::slice::from_raw_parts(buffer, length));
        WTSFreeMemory(buffer as *mut c_void);
        Some(user).filter(|user_name| !user_name.is_empty())
    }
}

/// Non-Windows: the `USER` environment variable.
#[cfg(not(windows))]
pub fn session_user(_session: u32) -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|user_name| !user_name.is_empty())
}

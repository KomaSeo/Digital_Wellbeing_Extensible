//! Responsibility: Find the Windows login session of the current process or of any pid.
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

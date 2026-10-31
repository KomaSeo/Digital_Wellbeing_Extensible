//! Responsibility: Wrap the Win32 calls that list logged-on sessions and start a SYSTEM process
//! inside a given session.

use std::ffi::c_void;
use std::io;
use std::mem::{size_of, zeroed};
use std::path::Path;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows_sys::Win32::Security::{
    DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TokenPrimary, TokenSessionId,
    TOKEN_ALL_ACCESS,
};
use windows_sys::Win32::System::RemoteDesktop::{
    WTSActive, WTSDisconnected, WTSEnumerateSessionsW, WTSFreeMemory, WTS_CURRENT_SERVER_HANDLE,
    WTS_SESSION_INFOW,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, GetExitCodeProcess, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION, STARTUPINFOW,
};

/// Responsibility: Describe one logged-on user session (id and user name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSession {
    pub id: u32,
    pub user: String,
}

/// Sessions with a user logged on: active (in front) or disconnected
/// (another user switched in; its programs keep running).
/// Session 0 (services) and the empty login-screen session are skipped.
pub fn user_sessions() -> Vec<UserSession> {
    let mut sessions = Vec::new();
    let mut list: *mut WTS_SESSION_INFOW = null_mut();
    let mut count = 0u32;
    // SAFETY: WTS allocates `list` (count entries); we free it with WTSFreeMemory.
    unsafe {
        if WTSEnumerateSessionsW(WTS_CURRENT_SERVER_HANDLE, 0, 1, &mut list, &mut count) == 0 {
            return sessions;
        }
        for session_info in std::slice::from_raw_parts(list, count as usize) {
            if session_info.SessionId == 0
                || (session_info.State != WTSActive && session_info.State != WTSDisconnected)
            {
                continue;
            }
            if let Some(user) = machine::session::session_user(session_info.SessionId) {
                sessions.push(UserSession {
                    id: session_info.SessionId,
                    user,
                });
            }
        }
        WTSFreeMemory(list as *mut c_void);
    }
    sessions
}

/// Responsibility: Own the handle of a process the service started: report whether it runs,
/// terminate it, close the handle on drop.
///
/// A process we started; the handle is closed on drop (the process keeps running).
pub struct Child {
    process: HANDLE,
    pub pid: u32,
}

// SAFETY: a process HANDLE is a kernel object reference, usable from any thread.
unsafe impl Send for Child {}

impl Child {
    pub fn is_running(&self) -> bool {
        // SAFETY: valid handle owned by self.
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }

    /// The exit code once the process has ended.
    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: valid handle owned by self; `code` is a valid out-pointer.
        let queried = unsafe { GetExitCodeProcess(self.process, &mut code) } != 0;
        (queried && !self.is_running()).then_some(code)
    }

    pub fn kill(&self) {
        // SAFETY: valid handle owned by self.
        unsafe {
            TerminateProcess(self.process, 1);
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        unsafe {
            CloseHandle(self.process);
        }
    }
}

/// Start `executable arguments` in `session` with a copy of *our* (SYSTEM) token.
///
/// The trick: duplicate the service's token, change only its session id
/// (needs SeTcbPrivilege, which SYSTEM has), and start the process on that
/// session's interactive desktop `winsta0\default`. The result runs as SYSTEM
/// but lives in the user's session, so it can see and act on their desktop.
pub fn spawn_in_session(session: u32, executable: &Path, arguments: &str) -> io::Result<Child> {
    let mut desktop = wide("winsta0\\default");
    let mut command_line = wide(&format!("\"{}\" {arguments}", executable.display()));
    // SAFETY: every handle opened here is closed before returning; pointers
    // passed to Win32 point at live, NUL-terminated buffers.
    unsafe {
        let mut own_token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own_token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut token: HANDLE = null_mut();
        let duplicated = DuplicateTokenEx(
            own_token,
            TOKEN_ALL_ACCESS,
            null_mut(),
            SecurityIdentification,
            TokenPrimary,
            &mut token,
        );
        CloseHandle(own_token);
        if duplicated == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| {
            if SetTokenInformation(
                token,
                TokenSessionId,
                &session as *const u32 as *const c_void,
                size_of::<u32>() as u32,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut startup: STARTUPINFOW = zeroed();
            startup.cb = size_of::<STARTUPINFOW>() as u32;
            startup.lpDesktop = desktop.as_mut_ptr();
            let mut process_info: PROCESS_INFORMATION = zeroed();
            if CreateProcessAsUserW(
                token,
                std::ptr::null(),
                command_line.as_mut_ptr(),
                null_mut(),
                null_mut(),
                0,
                CREATE_NO_WINDOW,
                null_mut(),
                std::ptr::null(),
                &startup,
                &mut process_info,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            CloseHandle(process_info.hThread);
            Ok(Child {
                process: process_info.hProcess,
                pid: process_info.dwProcessId,
            })
        })();
        CloseHandle(token);
        result
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

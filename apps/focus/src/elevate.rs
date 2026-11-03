//! Responsibility: Ask Windows to run `digital-wellbeing-service settings-unlock` with
//! administrator rights. Windows shows its administrator approval window (UAC); only when
//! someone who may approve does so does the command run and turn off an "administrator"
//! protection.

/// Started (Windows asks for approval next), or why not.
#[cfg(windows)]
pub(crate) fn run_settings_unlock() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let program = std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name("digital-wellbeing-service.exe");
    let wide = |text: &std::ffi::OsStr| -> Vec<u16> { text.encode_wide().chain([0]).collect() };
    let verb = wide("runas".as_ref());
    let file = wide(program.as_os_str());
    let arguments = wide("settings-unlock".as_ref());
    // SAFETY: every pointer is a NUL-terminated wide string that lives for the call.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            arguments.as_ptr(),
            std::ptr::null(),
            SW_HIDE,
        )
    };
    // More than 32 means started; less means refused (e.g. the approval was declined).
    if result as isize > 32 {
        Ok(())
    } else {
        Err("관리자 승인을 받지 못했습니다".into())
    }
}

#[cfg(not(windows))]
pub(crate) fn run_settings_unlock() -> Result<(), String> {
    Err("관리자 권한으로 풀기는 Windows에서만 됩니다".into())
}

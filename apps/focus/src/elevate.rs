//! Responsibility: Ask Windows to run a `digital-wellbeing-service` command with administrator
//! rights. Windows shows its administrator approval window (UAC); only when someone who may
//! approve does so does the command run:
//! - `settings-unlock` turns off an "administrator" protection;
//! - `sync-login --request <file>` signs up or logs in to the sync server.

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

/// The service program next to this one.
fn service_program() -> Result<std::path::PathBuf, String> {
    let name = if cfg!(windows) {
        "digital-wellbeing-service.exe"
    } else {
        "digital-wellbeing-service"
    };
    Ok(std::env::current_exe()
        .map_err(|error| error.to_string())?
        .with_file_name(name))
}

/// Sign up or log in to the sync server through `digital-wellbeing-service sync-login
/// --request <file>`, run with administrator rights (Windows asks for approval). Waits until
/// it ends (call off the UI thread) and returns its message.
///
/// The request (with the password) goes through a file in this user's temporary folder,
/// which the command deletes as soon as it read it; its outcome comes back in
/// `<file>.result`.
pub(crate) fn run_sync_login(request: &machine::login::LoginRequestFile) -> Result<String, String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let request_path =
        std::env::temp_dir().join(format!("dw-sync-login-{}-{nanos}.json", std::process::id()));
    let mut result_path = request_path.clone().into_os_string();
    result_path.push(".result");
    let result_path = std::path::PathBuf::from(result_path);
    std::fs::write(
        &request_path,
        serde_json::to_string(request).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let ran = run_elevated_and_wait(&service_program()?, &request_path);
    let _ = std::fs::remove_file(&request_path);
    let outcome = std::fs::read_to_string(&result_path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let _ = std::fs::remove_file(&result_path);
    ran?;
    let outcome = outcome.ok_or("로그인 명령이 결과를 남기지 않았습니다")?;
    let message = outcome["message"].as_str().unwrap_or_default().to_string();
    if outcome["ok"].as_bool() == Some(true) {
        Ok(message)
    } else {
        Err(message)
    }
}

#[cfg(windows)]
fn run_elevated_and_wait(
    program: &std::path::Path,
    request_path: &std::path::Path,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
    let wide = |text: &std::ffi::OsStr| -> Vec<u16> { text.encode_wide().chain([0]).collect() };
    let verb = wide("runas".as_ref());
    let file = wide(program.as_os_str());
    let arguments = wide(format!("sync-login --request \"{}\"", request_path.display()).as_ref());
    // SAFETY: a zeroed SHELLEXECUTEINFOW is valid (all pointers null); its size is set below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = arguments.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: `info` and the strings it points to live for the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 || info.hProcess.is_null() {
        return Err("관리자 승인을 받지 못했습니다".into());
    }
    // SAFETY: the process handle is ours until CloseHandle; two minutes is plenty for a login.
    unsafe {
        WaitForSingleObject(info.hProcess, 120_000);
        CloseHandle(info.hProcess);
    }
    Ok(())
}

/// Elsewhere (Linux, for development) the command runs as this user.
#[cfg(not(windows))]
fn run_elevated_and_wait(
    program: &std::path::Path,
    request_path: &std::path::Path,
) -> Result<(), String> {
    std::process::Command::new(program)
        .arg("sync-login")
        .arg("--request")
        .arg(request_path)
        .output()
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", program.display()))
}

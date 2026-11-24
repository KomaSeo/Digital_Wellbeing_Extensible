//! Responsibility: Shut and open the ways into safe mode, and say whether this program runs
//! in safe mode.
//!
//! Safe mode starts Windows with only the services listed under the `SafeBoot` registry key,
//! so by default this program's service does not run there. Shutting safe mode
//! (`docs/multi_account.md` 4.4) does two things:
//! - it keeps users out of the recovery screens: the Windows recovery environment
//!   (`reagentc /disable`), the recovery screen after failed starts and the boot menu
//!   (`bcdedit`);
//! - it lists the service under `SafeBoot\Minimal` and `SafeBoot\Network`, so that the service
//!   also runs in safe mode. This is the part that holds: with the recovery environment off,
//!   `shutdown /r /o` still reaches the startup settings and their safe mode (the owner:
//!   "아직도 안전모드를 shutdown /r /o /t 0 이후에 문제 해결 들어가서 할 수가 있는데").
//!
//! Every command needs administrator rights. The installer (`install --block-safe-mode`), the
//! agent (it runs as SYSTEM; the Focus app's "안전 모드 끄기") and the service's
//! `safe-mode-open` command (run with the administrator's approval; the Focus app's "안전 모드
//! 켜기") run them.

use serde::{Deserialize, Serialize};

/// The registry keys (under `HKEY_LOCAL_MACHINE`) that list the services safe mode starts:
/// plain safe mode, and safe mode with networking.
pub const SAFE_BOOT_KEYS: [&str; 2] = [
    r"SYSTEM\CurrentControlSet\Control\SafeBoot\Minimal",
    r"SYSTEM\CurrentControlSet\Control\SafeBoot\Network",
];

/// Responsibility: Report one command that shut or opened a way into safe mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandOutcome {
    /// The command as typed in a terminal.
    pub command: String,
    pub ok: bool,
    /// What it printed, or why it could not run.
    pub detail: String,
}

/// The commands that shut the ways into safe mode.
pub fn block_commands() -> Vec<Vec<String>> {
    let mut commands = vec![
        words(&["reagentc", "/disable"]),
        words(&["bcdedit", "/set", "{default}", "recoveryenabled", "No"]),
        words(&[
            "bcdedit",
            "/set",
            "{default}",
            "bootstatuspolicy",
            "IgnoreAllFailures",
        ]),
        words(&["bcdedit", "/set", "{bootmgr}", "displaybootmenu", "No"]),
    ];
    for key in SAFE_BOOT_KEYS {
        commands.push(words(&[
            "reg",
            "add",
            &service_key(key),
            "/ve",
            "/t",
            "REG_SZ",
            "/d",
            "Service",
            "/f",
        ]));
    }
    commands
}

/// The commands that open them again: Windows' own settings come back.
pub fn open_commands() -> Vec<Vec<String>> {
    let mut commands = vec![
        words(&["reagentc", "/enable"]),
        words(&["bcdedit", "/set", "{default}", "recoveryenabled", "Yes"]),
        words(&["bcdedit", "/deletevalue", "{default}", "bootstatuspolicy"]),
        words(&["bcdedit", "/deletevalue", "{bootmgr}", "displaybootmenu"]),
    ];
    for key in SAFE_BOOT_KEYS {
        commands.push(words(&["reg", "delete", &service_key(key), "/f"]));
    }
    commands
}

/// Shut the ways into safe mode. Each command runs even when one before it failed.
pub fn block() -> Vec<CommandOutcome> {
    run_all(&block_commands())
}

/// Open the ways into safe mode again.
pub fn open() -> Vec<CommandOutcome> {
    run_all(&open_commands())
}

/// `HKLM\<key>\<this program's service>`, as `reg` takes it.
fn service_key(key: &str) -> String {
    format!(r"HKLM\{key}\{}", app_identity::SERVICE_NAME)
}

fn words(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}

/// A line for each outcome ("성공: reagentc /disable", "실패: …: why"), for the Focus app, a
/// log or a terminal. Words, not check marks: the Focus app's font has no check marks.
pub fn outcome_lines(outcomes: &[CommandOutcome]) -> Vec<String> {
    outcomes
        .iter()
        .map(|outcome| {
            if outcome.ok {
                format!("성공: {}", outcome.command)
            } else {
                format!("실패: {}: {}", outcome.command, outcome.detail)
            }
        })
        .collect()
}

#[cfg(windows)]
fn run_all(commands: &[Vec<String>]) -> Vec<CommandOutcome> {
    use std::os::windows::process::CommandExt;
    /// No console window: the agent runs on the user's desktop.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    commands
        .iter()
        .map(|command| {
            let (program, arguments) = command.split_first().expect("a command");
            let text = command.join(" ");
            match std::process::Command::new(program)
                .args(arguments)
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                Ok(output) => {
                    let printed = format!(
                        "{} {}",
                        String::from_utf8_lossy(&output.stdout).trim(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                    CommandOutcome {
                        command: text,
                        ok: output.status.success(),
                        detail: printed.trim().to_string(),
                    }
                }
                Err(error) => CommandOutcome {
                    command: text,
                    ok: false,
                    detail: error.to_string(),
                },
            }
        })
        .collect()
}

/// Elsewhere (Linux, for development) nothing runs: there is no safe mode to shut.
#[cfg(not(windows))]
fn run_all(commands: &[Vec<String>]) -> Vec<CommandOutcome> {
    commands
        .iter()
        .map(|command| CommandOutcome {
            command: command.join(" "),
            ok: false,
            detail: "Windows에서만 됩니다".to_string(),
        })
        .collect()
}

/// Whether this program's service is listed to start in both kinds of safe mode, so that safe
/// mode is no way around it; `None` where that cannot be read (not Windows).
#[cfg(windows)]
pub fn runs_in_safe_mode() -> Option<bool> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ,
    };
    let listed = |key: &str| {
        let path: Vec<u16> = format!(r"{key}\{}", app_identity::SERVICE_NAME)
            .encode_utf16()
            .chain([0])
            .collect();
        let mut handle: HKEY = std::ptr::null_mut();
        // SAFETY: `path` is a NUL-terminated wide string that lives for the call; the key
        // opened is closed right after.
        unsafe {
            let opened = RegOpenKeyExW(HKEY_LOCAL_MACHINE, path.as_ptr(), 0, KEY_READ, &mut handle);
            if opened == ERROR_SUCCESS {
                RegCloseKey(handle);
                true
            } else {
                false
            }
        }
    };
    Some(SAFE_BOOT_KEYS.iter().all(|key| listed(key)))
}

#[cfg(not(windows))]
pub fn runs_in_safe_mode() -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutting_lists_the_service_for_safe_mode_and_opening_takes_it_off() {
        let block: Vec<String> = block_commands()
            .iter()
            .map(|command| command.join(" "))
            .collect();
        assert!(block.contains(&"reagentc /disable".to_string()));
        assert!(block.contains(
            &r"reg add HKLM\SYSTEM\CurrentControlSet\Control\SafeBoot\Minimal\DigitalWellbeing /ve /t REG_SZ /d Service /f"
                .to_string()
        ));
        assert!(block
            .iter()
            .any(|command| command.contains(r"SafeBoot\Network")));
        let open: Vec<String> = open_commands()
            .iter()
            .map(|command| command.join(" "))
            .collect();
        assert!(open.contains(&"reagentc /enable".to_string()));
        assert!(open.contains(
            &r"reg delete HKLM\SYSTEM\CurrentControlSet\Control\SafeBoot\Network\DigitalWellbeing /f"
                .to_string()
        ));
        // Opening undoes every change shutting makes.
        assert_eq!(block.len(), open.len());
    }
}

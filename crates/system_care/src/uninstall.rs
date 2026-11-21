//! Responsibility: Uninstall a program winget knows, with administrator rights, when the user
//! asks for it.
//!
//! The user's account is a standard account, so they cannot remove a program installed for
//! every account (the owner: "내가 실수로 설치한 프로그램이 있는데 삭제할 수가 없군"). The
//! agent writes a request naming a winget id from the service's last list of installed
//! programs; the service (SYSTEM, in session 0) runs `winget uninstall --id <id> --exact
//! --silent` for it. Nothing but the id comes from the user: no command, no arguments.
//! - Only programs on the service's list ([`crate::INSTALLED_FILE`]) can be named.
//! - This app and winget itself ([`is_protected`]) are never uninstalled.
//! - A program uninstalled (or found not installed) comes off the update list too, or the daily
//!   update would install it again for every account.
//!
//! Session 0 shows no window, so an uninstaller that waits for a click does not finish; it is
//! stopped after 30 minutes. Files: `uninstall_requests\<id>.json`, written by agents and removed
//! by the service; results in `system_care.json`.

use crate::{is_program_id, read_installed_list, read_state, write_state, INSTALLED_REQUEST_FILE};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const UNINSTALL_FOLDER: &str = "uninstall_requests";
const KEEP_UNINSTALLS: usize = 20;

/// winget ids that are never uninstalled: winget itself (App Installer), without which the
/// updates and uninstalls stop working.
const PROTECTED_IDS: [&str; 2] = ["Microsoft.AppInstaller", "Microsoft.DesktopAppInstaller"];

/// Responsibility: Carry one uninstall the user asked for, from the agent to the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallRequest {
    pub id: String,
    /// The winget id.
    pub program: String,
    /// The program's name on the list, for the user.
    pub name: String,
    pub requested_at: DateTime<Utc>,
    /// The Windows user who asked.
    pub requested_by: String,
}

/// Responsibility: Name how one uninstall ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UninstallOutcome {
    Removed,
    /// winget found nothing installed under that id.
    NotInstalled,
    Failed,
    /// Not run: the program may not be uninstalled.
    Refused,
}

/// Responsibility: Record one uninstall the service ran (or refused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UninstallRun {
    pub program: String,
    pub name: String,
    pub requested_by: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: UninstallOutcome,
    pub message: String,
    /// It was on the update list and came off it.
    #[serde(default)]
    pub left_update_list: bool,
}

/// This app and winget itself: never uninstalled.
pub fn is_protected(program: &str) -> bool {
    let lower = program.to_lowercase();
    PROTECTED_IDS
        .iter()
        .any(|protected| protected.eq_ignore_ascii_case(program))
        || lower.contains("digitalwellbeing")
        || lower.contains("digital-wellbeing")
}

/// Why `program` may not be uninstalled, or `None` when it may.
fn refusal(program: &str) -> Option<String> {
    if !is_program_id(program) {
        return Some("winget 이름의 모양이 아닙니다".into());
    }
    if is_protected(program) {
        return Some("이 프로그램과 winget은 지울 수 없습니다".into());
    }
    None
}

/// Ask the service to uninstall `program` (agent side). Refused unless the service's last list
/// of installed programs holds it, for a protected program, and when it already waits.
pub fn request_uninstall(
    data_directory: &Path,
    program: &str,
    requested_by: &str,
    now: DateTime<Utc>,
) -> Result<UninstallRequest, String> {
    let program = program.trim();
    if let Some(reason) = refusal(program) {
        return Err(reason);
    }
    let listed = read_installed_list(data_directory)
        .and_then(|list| {
            list.programs
                .into_iter()
                .find(|installed| installed.id.eq_ignore_ascii_case(program))
        })
        .ok_or("설치된 프로그램 목록에 없습니다. 목록을 다시 읽어 보세요")?;
    if pending_uninstalls(data_directory)
        .iter()
        .any(|(_, request)| request.program.eq_ignore_ascii_case(&listed.id))
    {
        return Err(format!("{}의 삭제를 이미 기다리고 있습니다", listed.name));
    }
    let id = format!(
        "{}-{}",
        now.timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    );
    let request = UninstallRequest {
        id: id.clone(),
        program: listed.id,
        name: listed.name,
        requested_at: now,
        requested_by: requested_by.to_string(),
    };
    let folder = data_directory.join(UNINSTALL_FOLDER);
    fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&request).map_err(|error| error.to_string())?;
    let temporary = folder.join(format!("{id}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, folder.join(format!("{id}.json"))).map_err(|error| error.to_string())?;
    Ok(request)
}

/// The uninstalls the service has not taken yet, oldest first.
pub fn pending_uninstalls(data_directory: &Path) -> Vec<(PathBuf, UninstallRequest)> {
    let Ok(entries) = fs::read_dir(data_directory.join(UNINSTALL_FOLDER)) else {
        return Vec::new();
    };
    let mut requests: Vec<(PathBuf, UninstallRequest)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let request = fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<UninstallRequest>(&raw).ok())?;
            Some((path, request))
        })
        .collect();
    requests.sort_by_key(|(_, request)| request.requested_at);
    requests
}

/// Run every waiting uninstall (service side) with `uninstall` (winget in production). A
/// program removed or not installed comes off the update list through `leave_update_list`
/// when `on_update_list` says it is there. Afterwards the list of installed programs is read
/// again, so the user sees it gone.
pub fn run_uninstalls(
    data_directory: &Path,
    on_update_list: &dyn Fn(&str) -> bool,
    uninstall: &dyn Fn(&str) -> (UninstallOutcome, String),
    leave_update_list: &dyn Fn(&str) -> Result<(), String>,
) {
    let requests = pending_uninstalls(data_directory);
    if requests.is_empty() {
        return;
    }
    for (path, request) in requests {
        // Taken first, so a crash in the uninstaller does not repeat it forever.
        let _ = fs::remove_file(&path);
        let started_at = Utc::now();
        let (outcome, mut message) = match refusal(&request.program) {
            Some(reason) => (UninstallOutcome::Refused, reason),
            None => {
                let mut state = read_state(data_directory);
                state.uninstalling = Some(request.name.clone());
                write_state(data_directory, &state);
                tracing::info!("uninstalling {}", request.program);
                let result = uninstall(&request.program);
                tracing::info!(
                    "uninstall of {}: {:?} {}",
                    request.program,
                    result.0,
                    result.1
                );
                result
            }
        };
        let mut left_update_list = false;
        if matches!(
            outcome,
            UninstallOutcome::Removed | UninstallOutcome::NotInstalled
        ) && on_update_list(&request.program)
        {
            match leave_update_list(&request.program) {
                Ok(()) => left_update_list = true,
                Err(error) => {
                    message = format!("{message} 업데이트 목록에서 빼지 못했습니다: {error}")
                        .trim()
                        .to_string();
                }
            }
        }
        let mut state = read_state(data_directory);
        state.uninstalling = None;
        state.uninstalls.push(UninstallRun {
            program: request.program,
            name: request.name,
            requested_by: request.requested_by,
            started_at,
            finished_at: Utc::now(),
            outcome,
            message,
            left_update_list,
        });
        let extra = state.uninstalls.len().saturating_sub(KEEP_UNINSTALLS);
        state.uninstalls.drain(..extra);
        write_state(data_directory, &state);
    }
    // The list the user sees: read again by the Updates job's next pass.
    let _ = fs::write(
        data_directory.join(INSTALLED_REQUEST_FILE),
        Utc::now().to_rfc3339(),
    );
}

/// The outcome of `winget uninstall`'s exit code.
pub fn uninstall_outcome_of(code: Option<i32>, output: String) -> (UninstallOutcome, String) {
    match code.map(|code| code as u32) {
        Some(0) => (UninstallOutcome::Removed, output),
        Some(crate::WINGET_NOT_INSTALLED) => (UninstallOutcome::NotInstalled, output),
        Some(code) => (
            UninstallOutcome::Failed,
            format!("winget 오류 {code:#X}. {output}"),
        ),
        None => (UninstallOutcome::Failed, output),
    }
}

/// Uninstall `program` with winget, silently and without a window (service side), at most
/// 30 minutes.
#[cfg(windows)]
pub fn winget_uninstall(program: &str) -> (UninstallOutcome, String) {
    if let Some(reason) = refusal(program) {
        return (UninstallOutcome::Refused, reason);
    }
    let winget = match crate::winget_path() {
        Ok(winget) => winget,
        Err(diagnosis) => {
            return (
                UninstallOutcome::Failed,
                format!("서비스가 winget을 찾지 못했습니다 ({diagnosis})"),
            )
        }
    };
    match crate::run_winget(
        &winget,
        &[
            "uninstall",
            "--id",
            program,
            "--exact",
            "--source",
            "winget",
            "--silent",
            "--disable-interactivity",
            "--accept-source-agreements",
        ],
    ) {
        Ok((code, stdout, stderr)) => {
            uninstall_outcome_of(code, crate::short_output(&stdout, &stderr))
        }
        Err(message) => (UninstallOutcome::Failed, message),
    }
}

#[cfg(not(windows))]
pub fn winget_uninstall(_program: &str) -> (UninstallOutcome, String) {
    (
        UninstallOutcome::Failed,
        "프로그램 지우기는 Windows에서만 됩니다".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InstalledList, InstalledProgram, INSTALLED_FILE};
    use std::cell::RefCell;

    fn folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("dw_uninstall_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn with_installed(data_directory: &Path, ids: &[(&str, &str)]) {
        let list = InstalledList {
            listed_at: Some(Utc::now()),
            programs: ids
                .iter()
                .map(|(id, name)| InstalledProgram {
                    id: id.to_string(),
                    name: name.to_string(),
                    ..Default::default()
                })
                .collect(),
            error: String::new(),
        };
        fs::write(
            data_directory.join(INSTALLED_FILE),
            serde_json::to_vec(&list).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn only_listed_and_unprotected_programs_can_be_asked_for() {
        let data_directory = folder("request");
        with_installed(
            &data_directory,
            &[
                ("Mozilla.Firefox", "Mozilla Firefox"),
                ("Microsoft.AppInstaller", "App Installer"),
            ],
        );
        let request =
            request_uninstall(&data_directory, " mozilla.firefox ", "owner", Utc::now()).unwrap();
        // The id and name as the list has them.
        assert_eq!(request.program, "Mozilla.Firefox");
        assert_eq!(request.name, "Mozilla Firefox");
        assert_eq!(pending_uninstalls(&data_directory).len(), 1);
        // Asked twice: refused while the first one waits.
        assert!(
            request_uninstall(&data_directory, "Mozilla.Firefox", "owner", Utc::now())
                .unwrap_err()
                .contains("이미 기다리고")
        );
        // Not on the list, protected, or not an id.
        assert!(
            request_uninstall(&data_directory, "Valve.Steam", "owner", Utc::now())
                .unwrap_err()
                .contains("목록에 없습니다")
        );
        assert!(request_uninstall(
            &data_directory,
            "Microsoft.AppInstaller",
            "owner",
            Utc::now()
        )
        .unwrap_err()
        .contains("지울 수 없습니다"));
        assert!(request_uninstall(&data_directory, "rm -rf", "owner", Utc::now()).is_err());
        assert!(is_protected("KomaSeo.DigitalWellbeing"));
        let _ = fs::remove_dir_all(&data_directory);
    }

    #[test]
    fn an_uninstalled_program_leaves_the_update_list_and_the_list_is_read_again() {
        let data_directory = folder("run");
        with_installed(
            &data_directory,
            &[
                ("Mozilla.Firefox", "Mozilla Firefox"),
                ("Valve.Steam", "Steam"),
            ],
        );
        request_uninstall(&data_directory, "Mozilla.Firefox", "owner", Utc::now()).unwrap();
        request_uninstall(&data_directory, "Valve.Steam", "owner", Utc::now()).unwrap();
        let ran = RefCell::new(Vec::new());
        let left = RefCell::new(Vec::new());
        run_uninstalls(
            &data_directory,
            &|program| program == "Mozilla.Firefox" || program == "Valve.Steam",
            &|program| {
                ran.borrow_mut().push(program.to_string());
                if program == "Valve.Steam" {
                    (UninstallOutcome::Failed, "winget 오류 0x1.".into())
                } else {
                    (UninstallOutcome::Removed, String::new())
                }
            },
            &|program| {
                left.borrow_mut().push(program.to_string());
                Ok(())
            },
        );
        assert_eq!(*ran.borrow(), ["Mozilla.Firefox", "Valve.Steam"]);
        // Only the one that went comes off the update list.
        assert_eq!(*left.borrow(), ["Mozilla.Firefox"]);
        let state = read_state(&data_directory);
        assert_eq!(state.uninstalls.len(), 2);
        assert_eq!(state.uninstalls[0].outcome, UninstallOutcome::Removed);
        assert!(state.uninstalls[0].left_update_list);
        assert_eq!(state.uninstalls[1].outcome, UninstallOutcome::Failed);
        assert!(state.uninstalling.is_none());
        assert!(pending_uninstalls(&data_directory).is_empty());
        assert!(data_directory.join(INSTALLED_REQUEST_FILE).exists());
        let _ = fs::remove_dir_all(&data_directory);
    }

    #[test]
    fn winget_codes_are_read() {
        assert_eq!(
            uninstall_outcome_of(Some(0), String::new()).0,
            UninstallOutcome::Removed
        );
        assert_eq!(
            uninstall_outcome_of(Some(crate::WINGET_NOT_INSTALLED as i32), String::new()).0,
            UninstallOutcome::NotInstalled
        );
        assert_eq!(
            uninstall_outcome_of(Some(5), String::new()).0,
            UninstallOutcome::Failed
        );
    }
}

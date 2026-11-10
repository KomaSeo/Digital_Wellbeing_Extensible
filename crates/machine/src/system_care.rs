//! Responsibility: Do the two administrator jobs the user may not do themselves, in the
//! service: update the programs the settings list (on request and once a day), and keep the
//! time zone of the settings.
//!
//! The user's account is a standard account, so they cannot uninstall this app or change the
//! clock. Some programs (KakaoTalk, for example) need an administrator to update. The user
//! never gets administrator rights for that: the agent writes a request file, and the service
//! (SYSTEM, in session 0, where no window can be seen or used) runs
//! `winget upgrade --id <program> --silent` for programs in `updatable_programs` only. winget
//! checks the downloaded installer against the hash in its catalogue. Nothing else is run:
//! no uninstall, no other command.
//!
//! A standard user may change the time zone, which would shift the hours of the life rules.
//! The service puts the time zone of the settings (`time_zone`) back within seconds and
//! records every reset.
//!
//! Files in the data folder: `update_requests\<id>.json` (written by agents, removed by the
//! service) and `system_care.json` (written by the service only).
//! Walkthrough (Korean): `docs/multi_account.md` 6.5.

use ai_focus_rule::AiFocusConfig;
use chrono::{DateTime, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const REQUESTS_FOLDER: &str = "update_requests";
pub const STATE_FILE: &str = "system_care.json";
/// The daily automatic update runs once a day, from this local hour on.
pub const AUTO_UPDATE_FROM_HOUR: u32 = 4;
const KEEP_RUNS: usize = 30;
const KEEP_RESETS: usize = 20;

/// Responsibility: Name how one update ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    Updated,
    AlreadyUpToDate,
    NotInstalled,
    Failed,
}

/// Responsibility: Carry one update the user asked for, from the agent to the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub id: String,
    pub program: String,
    pub requested_at: DateTime<Utc>,
    /// The Windows user who asked.
    pub requested_by: String,
}

/// Responsibility: Record one update the service ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRun {
    pub program: String,
    /// The daily update, not a request.
    pub automatic: bool,
    pub requested_by: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: UpdateOutcome,
    /// What winget said, shortened.
    pub message: String,
}

/// Responsibility: Record one time zone the service put back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeZoneReset {
    pub at: DateTime<Utc>,
    pub from: String,
    pub to: String,
}

/// Responsibility: Hold what the service did (the contents of `system_care.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CareState {
    /// Newest last.
    pub runs: Vec<UpdateRun>,
    pub last_automatic: Option<DateTime<Utc>>,
    /// The program being updated now.
    pub running: Option<String>,
    /// Newest last.
    pub time_zone_resets: Vec<TimeZoneReset>,
    /// Why the time zone could not be read or put back, if it could not.
    pub time_zone_error: String,
}

/// Responsibility: Carry what the Focus app shows about updates and the time zone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CareStatus {
    pub programs: Vec<String>,
    pub auto_update: bool,
    pub pending: Vec<UpdateRequest>,
    pub state: CareState,
    /// The time zone of the settings, and the PC's time zone now.
    pub time_zone: String,
    pub time_zone_now: String,
}

/// A winget id: letters, digits and `.`, `-`, `_`, `+` only.
pub fn is_program_id(program: &str) -> bool {
    !program.is_empty()
        && program.len() <= 128
        && program
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_+".contains(character))
}

/// The listed id `program` stands for, ignoring case.
fn listed(config: &AiFocusConfig, program: &str) -> Option<String> {
    config
        .updatable_programs
        .iter()
        .find(|listed| listed.eq_ignore_ascii_case(program.trim()))
        .cloned()
}

/// Ask the service to update `program` (agent side). Refused for a program the settings do
/// not list, and when that program already waits.
pub fn request_update(
    data_directory: &Path,
    config: &AiFocusConfig,
    program: &str,
    requested_by: &str,
    now: DateTime<Utc>,
) -> Result<UpdateRequest, String> {
    let program = listed(config, program).ok_or_else(|| {
        format!(
            "'{program}'은 업데이트할 프로그램 목록에 없습니다. 설정 탭의 '프로그램 업데이트'에서 \
             먼저 넣으세요"
        )
    })?;
    if !is_program_id(&program) {
        return Err(format!("'{program}'은 winget 이름의 모양이 아닙니다"));
    }
    if pending_requests(data_directory)
        .iter()
        .any(|(_, request)| request.program == program)
    {
        return Err(format!("{program}의 업데이트를 이미 기다리고 있습니다"));
    }
    let request = UpdateRequest {
        id: format!(
            "{}-{}",
            now.timestamp_nanos_opt().unwrap_or_default(),
            std::process::id()
        ),
        program,
        requested_at: now,
        requested_by: requested_by.to_string(),
    };
    let folder = data_directory.join(REQUESTS_FOLDER);
    fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(&request).map_err(|error| error.to_string())?;
    let temporary = folder.join(format!("{}.tmp", request.id));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, folder.join(format!("{}.json", request.id)))
        .map_err(|error| error.to_string())?;
    Ok(request)
}

/// The requests the service has not taken yet, oldest first.
pub fn pending_requests(data_directory: &Path) -> Vec<(PathBuf, UpdateRequest)> {
    let Ok(entries) = fs::read_dir(data_directory.join(REQUESTS_FOLDER)) else {
        return Vec::new();
    };
    let mut requests: Vec<(PathBuf, UpdateRequest)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let request = serde_json::from_str(&fs::read_to_string(&path).ok()?).ok()?;
            Some((path, request))
        })
        .collect();
    requests.sort_by_key(|(_, request)| request.requested_at);
    requests
}

pub fn read_state(data_directory: &Path) -> CareState {
    fs::read_to_string(data_directory.join(STATE_FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn write_state(data_directory: &Path, state: &CareState) {
    let path = data_directory.join(STATE_FILE);
    let temporary = path.with_extension("json.tmp");
    let written = serde_json::to_vec_pretty(state)
        .map_err(std::io::Error::other)
        .and_then(|bytes| fs::write(&temporary, bytes))
        .and_then(|_| fs::rename(&temporary, &path));
    if let Err(error) = written {
        tracing::warn!("could not save {STATE_FILE}: {error}");
    }
}

/// What the Focus app shows.
pub fn status(data_directory: &Path, config: &AiFocusConfig) -> CareStatus {
    CareStatus {
        programs: config.updatable_programs.iter().cloned().collect(),
        auto_update: config.auto_update_programs,
        pending: pending_requests(data_directory)
            .into_iter()
            .map(|(_, request)| request)
            .collect(),
        state: read_state(data_directory),
        time_zone: config.time_zone.clone(),
        time_zone_now: current_time_zone().unwrap_or_default(),
    }
}

/// The daily update is due: not yet run on this local day, and it is past
/// [`AUTO_UPDATE_FROM_HOUR`].
pub fn automatic_due(last: Option<DateTime<Utc>>, now: DateTime<Local>) -> bool {
    now.hour() >= AUTO_UPDATE_FROM_HOUR
        && last.is_none_or(|last| last.with_timezone(&Local).date_naive() < now.date_naive())
}

/// One pass of the service: run the requested updates, then the daily one when it is due.
/// `update` runs one program (winget in production).
pub fn run_updates(
    data_directory: &Path,
    config: &AiFocusConfig,
    now: DateTime<Local>,
    update: &dyn Fn(&str) -> (UpdateOutcome, String),
) {
    for (path, request) in pending_requests(data_directory) {
        // Taken first, so a crash in the update does not repeat it forever.
        let _ = fs::remove_file(&path);
        match listed(config, &request.program).filter(|program| is_program_id(program)) {
            Some(program) => run_one(
                data_directory,
                &program,
                false,
                &request.requested_by,
                update,
            ),
            None => record(
                data_directory,
                UpdateRun {
                    program: request.program.clone(),
                    automatic: false,
                    requested_by: request.requested_by.clone(),
                    started_at: Utc::now(),
                    finished_at: Utc::now(),
                    outcome: UpdateOutcome::Failed,
                    message: "업데이트할 프로그램 목록에 없습니다".into(),
                },
            ),
        }
    }
    if !config.auto_update_programs || config.updatable_programs.is_empty() {
        return;
    }
    let mut state = read_state(data_directory);
    if !automatic_due(state.last_automatic, now) {
        return;
    }
    // Marked before running: a failing update is tried again tomorrow, not every few seconds.
    state.last_automatic = Some(now.with_timezone(&Utc));
    write_state(data_directory, &state);
    for program in config
        .updatable_programs
        .iter()
        .filter(|program| is_program_id(program))
    {
        run_one(data_directory, program, true, "", update);
    }
}

fn run_one(
    data_directory: &Path,
    program: &str,
    automatic: bool,
    requested_by: &str,
    update: &dyn Fn(&str) -> (UpdateOutcome, String),
) {
    let mut state = read_state(data_directory);
    state.running = Some(program.to_string());
    write_state(data_directory, &state);
    let started_at = Utc::now();
    tracing::info!("updating {program} (automatic: {automatic})");
    let (outcome, message) = update(program);
    tracing::info!("update of {program}: {outcome:?} {message}");
    record(
        data_directory,
        UpdateRun {
            program: program.to_string(),
            automatic,
            requested_by: requested_by.to_string(),
            started_at,
            finished_at: Utc::now(),
            outcome,
            message,
        },
    );
}

fn record(data_directory: &Path, run: UpdateRun) {
    let mut state = read_state(data_directory);
    state.running = None;
    state.runs.push(run);
    let extra = state.runs.len().saturating_sub(KEEP_RUNS);
    state.runs.drain(..extra);
    write_state(data_directory, &state);
}

/// Responsibility: Say what [`guard_time_zone`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimeZoneAction {
    Nothing,
    /// The settings had no time zone; the PC's one is now the setting.
    Recorded(String),
    /// The PC's time zone differed and was put back.
    PutBack {
        from: String,
        to: String,
    },
    Failed(String),
}

/// One pass of the service: keep the time zone of the settings. `current` is the PC's time
/// zone; `set` changes it.
pub fn guard_time_zone(
    data_directory: &Path,
    config: &AiFocusConfig,
    current: Result<String, String>,
    set: &dyn Fn(&str) -> Result<(), String>,
) -> TimeZoneAction {
    let action = match current {
        Err(error) => TimeZoneAction::Failed(format!("시간대를 읽지 못했습니다: {error}")),
        Ok(current) if config.time_zone.trim().is_empty() => {
            let edit = crate::setting_edits::Edit::Set {
                path: vec!["time_zone".into()],
                value: serde_json::Value::String(current.clone()),
            };
            match crate::settings::apply_now(data_directory, "ai_focus", &[edit]) {
                Ok(()) => TimeZoneAction::Recorded(current),
                Err(error) => {
                    TimeZoneAction::Failed(format!("시간대를 설정에 적지 못했습니다: {error}"))
                }
            }
        }
        Ok(current) if current == config.time_zone.trim() => TimeZoneAction::Nothing,
        Ok(current) => match set(config.time_zone.trim()) {
            Ok(()) => TimeZoneAction::PutBack {
                from: current,
                to: config.time_zone.trim().to_string(),
            },
            Err(error) => TimeZoneAction::Failed(format!(
                "시간대를 '{}'(으)로 되돌리지 못했습니다: {error}",
                config.time_zone.trim()
            )),
        },
    };
    let mut state = read_state(data_directory);
    let before = state.clone();
    match &action {
        TimeZoneAction::PutBack { from, to } => {
            tracing::warn!("time zone changed to {from}; put back to {to}");
            state.time_zone_resets.push(TimeZoneReset {
                at: Utc::now(),
                from: from.clone(),
                to: to.clone(),
            });
            let extra = state.time_zone_resets.len().saturating_sub(KEEP_RESETS);
            state.time_zone_resets.drain(..extra);
            state.time_zone_error.clear();
        }
        TimeZoneAction::Failed(error) => {
            if state.time_zone_error != *error {
                tracing::warn!("{error}");
            }
            state.time_zone_error = error.clone();
        }
        TimeZoneAction::Recorded(zone) => {
            tracing::info!("time zone {zone} recorded in the settings");
            state.time_zone_error.clear();
        }
        TimeZoneAction::Nothing => state.time_zone_error.clear(),
    }
    if state != before {
        write_state(data_directory, &state);
    }
    action
}

/// The PC's time zone as `tzutil /g` names it (`Korea Standard Time`, `…_dstoff`).
#[cfg(windows)]
pub fn current_time_zone() -> Result<String, String> {
    use windows_sys::Win32::System::Time::{
        GetDynamicTimeZoneInformation, DYNAMIC_TIME_ZONE_INFORMATION,
    };
    // SAFETY: an all-zero DYNAMIC_TIME_ZONE_INFORMATION is valid, and the call fills it.
    let mut information: DYNAMIC_TIME_ZONE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `information` is a valid out-pointer.
    let result = unsafe { GetDynamicTimeZoneInformation(&mut information) };
    if result == u32::MAX {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let name_units = &information.TimeZoneKeyName;
    let length = name_units
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name_units.len());
    let mut name = String::from_utf16_lossy(&name_units[..length]);
    if u8::from(information.DynamicDaylightTimeDisabled) != 0 {
        name.push_str("_dstoff");
    }
    Ok(name)
}

#[cfg(not(windows))]
pub fn current_time_zone() -> Result<String, String> {
    Err("시간대 지키기는 Windows에서만 됩니다".into())
}

/// Set the PC's time zone with `tzutil /s` (needs the service's rights).
#[cfg(windows)]
pub fn set_time_zone(name: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("tzutil")
        .args(["/s", name])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(short_output(&output.stdout, &output.stderr))
    }
}

#[cfg(not(windows))]
pub fn set_time_zone(_name: &str) -> Result<(), String> {
    Err("시간대 지키기는 Windows에서만 됩니다".into())
}

/// winget's error "no applicable update found".
const WINGET_NO_UPDATE: u32 = 0x8A15_002B;
/// winget's error "no installed package found".
const WINGET_NOT_INSTALLED: u32 = 0x8A15_0014;
/// An update that runs longer is stopped.
#[cfg(windows)]
const UPDATE_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Update `program` with winget, silently and without a window (service side).
#[cfg(windows)]
pub fn winget_upgrade(program: &str) -> (UpdateOutcome, String) {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if !is_program_id(program) {
        return (
            UpdateOutcome::Failed,
            "winget 이름의 모양이 아닙니다".into(),
        );
    }
    let winget = match winget_path() {
        Ok(winget) => winget,
        Err(diagnosis) => {
            return (
                UpdateOutcome::Failed,
                format!("서비스가 winget을 찾지 못했습니다 ({diagnosis})"),
            )
        }
    };
    let child = Command::new(&winget)
        .args([
            "upgrade",
            "--id",
            program,
            "--exact",
            "--source",
            "winget",
            "--silent",
            "--disable-interactivity",
            "--accept-source-agreements",
            "--accept-package-agreements",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            return (
                UpdateOutcome::Failed,
                format!("winget을 실행하지 못했습니다: {error}"),
            )
        }
    };
    // Read the output on threads, or a full pipe would stop winget.
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() > UPDATE_TIME_LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(500)),
            Err(_) => break None,
        }
    };
    let output = short_output(
        &stdout_reader.join().unwrap_or_default(),
        &stderr_reader.join().unwrap_or_default(),
    );
    let Some(status) = status else {
        return (
            UpdateOutcome::Failed,
            format!("30분 안에 끝나지 않아 멈췄습니다. {output}"),
        );
    };
    outcome_of(status.code(), output)
}

#[cfg(not(windows))]
pub fn winget_upgrade(_program: &str) -> (UpdateOutcome, String) {
    (
        UpdateOutcome::Failed,
        "프로그램 업데이트는 Windows에서만 됩니다".into(),
    )
}

/// The outcome of winget's exit code.
pub fn outcome_of(code: Option<i32>, output: String) -> (UpdateOutcome, String) {
    match code.map(|code| code as u32) {
        Some(0) => (UpdateOutcome::Updated, output),
        Some(WINGET_NO_UPDATE) => (UpdateOutcome::AlreadyUpToDate, output),
        Some(WINGET_NOT_INSTALLED) => (UpdateOutcome::NotInstalled, output),
        Some(code) => (
            UpdateOutcome::Failed,
            format!("winget 오류 {code:#X}. {output}"),
        ),
        None => (UpdateOutcome::Failed, output),
    }
}

/// The App Installer package's publisher id, the end of its folder names.
const APP_INSTALLER_PUBLISHER: &str = "8wekyb3d8bbwe";

/// The winget of the newest App Installer package. The service has no user profile, so winget
/// is not on its PATH: the packages come from the WindowsApps folder and from Windows' own list
/// (`Get-AppxPackage -AllUsers`, which also knows packages installed on another drive). On
/// failure, says what was looked at.
#[cfg(windows)]
fn winget_path() -> Result<PathBuf, String> {
    let program_files = std::env::var_os("ProgramW6432")
        .or_else(|| std::env::var_os("ProgramFiles"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    let apps_folder = program_files.join("WindowsApps");
    let mut notes = Vec::new();
    let mut packages: Vec<PathBuf> = Vec::new();
    match fs::read_dir(&apps_folder) {
        Ok(entries) => packages.extend(
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| is_app_installer_folder(path)),
        ),
        Err(error) => notes.push(format!("{}을 읽지 못함: {error}", apps_folder.display())),
    }
    match installed_app_installers() {
        Ok(locations) => packages.extend(locations),
        Err(error) => notes.push(format!("Windows의 앱 목록을 읽지 못함: {error}")),
    }
    newest_winget(packages).map_err(|searched| {
        notes.insert(0, searched);
        notes.join("; ")
    })
}

/// The install folders of App Installer for every account, as Windows lists them.
#[cfg(windows)]
fn installed_app_installers() -> Result<Vec<PathBuf>, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let output = std::process::Command::new(
        system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
    )
    .args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Get-AppxPackage -AllUsers -Name Microsoft.DesktopAppInstaller | \
         ForEach-Object { $_.InstallLocation }",
    ])
    .stdin(std::process::Stdio::null())
    .creation_flags(CREATE_NO_WINDOW)
    .output()
    .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(short_output(&output.stdout, &output.stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// An App Installer package's own folder (`Microsoft.DesktopAppInstaller_<version>_<cpu>__<publisher>`),
/// not its bundle or language folders.
#[cfg_attr(not(windows), allow(dead_code))]
fn is_app_installer_folder(path: &Path) -> bool {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .is_some_and(|name| {
            name.starts_with("Microsoft.DesktopAppInstaller_")
                && name.ends_with(&format!("__{APP_INSTALLER_PUBLISHER}"))
        })
}

/// The version in a package folder's name, for ordering (`1.29.380.0` -> [1, 29, 380, 0]).
#[cfg_attr(not(windows), allow(dead_code))]
fn package_version(path: &Path) -> Vec<u32> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
        .split('_')
        .nth(1)
        .unwrap_or_default()
        .split('.')
        .map(|part| part.parse().unwrap_or(0))
        .collect()
}

/// The winget of the newest of `packages` that has one; otherwise, which folders were searched.
#[cfg_attr(not(windows), allow(dead_code))]
fn newest_winget(mut packages: Vec<PathBuf>) -> Result<PathBuf, String> {
    packages.sort();
    packages.dedup();
    packages.sort_by_key(|package| std::cmp::Reverse(package_version(package)));
    if let Some(winget) = packages.iter().find_map(|package| winget_in(package)) {
        return Ok(winget);
    }
    let names: Vec<String> = packages
        .iter()
        .map(|package| package.display().to_string())
        .collect();
    Err(if names.is_empty() {
        "앱 설치 관리자 폴더를 찾지 못함".into()
    } else {
        format!("앱 설치 관리자 폴더 [{}]에 winget이 없음", names.join(", "))
    })
}

/// The winget program inside one App Installer package folder: the program its manifest names
/// for the `winget.exe` command, else a `winget.exe` in the folder or one level below.
#[cfg_attr(not(windows), allow(dead_code))]
fn winget_in(package: &Path) -> Option<PathBuf> {
    let from_manifest = fs::read_to_string(package.join("AppxManifest.xml"))
        .ok()
        .and_then(|manifest| winget_executable_in_manifest(&manifest))
        .map(|executable| package.join(executable));
    let below = fs::read_dir(package)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().join("winget.exe"));
    from_manifest
        .into_iter()
        .chain(std::iter::once(package.join("winget.exe")))
        .chain(below)
        .find(|candidate| candidate.is_file())
}

/// The `Executable` of the manifest entry that gives the `winget.exe` command (the attribute
/// nearest before `Alias="winget.exe"`).
#[cfg_attr(not(windows), allow(dead_code))]
fn winget_executable_in_manifest(manifest: &str) -> Option<String> {
    let alias = manifest.find("Alias=\"winget.exe\"")?;
    let attribute = "Executable=\"";
    let start = manifest[..alias].rfind(attribute)? + attribute.len();
    let length = manifest[start..].find('"')?;
    let executable = manifest[start..start + length].trim();
    (!executable.is_empty() && !executable.contains("..")).then(|| executable.to_string())
}

/// The last few meaningful lines of a program's output (winget draws progress bars).
#[cfg_attr(not(windows), allow(dead_code))]
fn short_output(stdout: &[u8], stderr: &[u8]) -> String {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    );
    let lines: Vec<&str> = text
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| line.chars().any(char::is_alphanumeric))
        .filter(|line| !line.contains('█') && !line.contains('▒'))
        .collect();
    let start = lines.len().saturating_sub(4);
    lines[start..].join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::cell::RefCell;

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("dw_care_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    fn config_with(programs: &[&str]) -> AiFocusConfig {
        AiFocusConfig {
            updatable_programs: programs.iter().map(|program| program.to_string()).collect(),
            ..AiFocusConfig::default()
        }
    }

    #[test]
    fn only_listed_programs_are_requested_and_run() {
        let data = folder("requests");
        let config = config_with(&["Kakao.KakaoTalk"]);
        let now = Utc::now();
        assert!(request_update(&data, &config, "Valve.Steam", "me", now).is_err());
        let request = request_update(&data, &config, "kakao.kakaotalk", "me", now).unwrap();
        assert_eq!(request.program, "Kakao.KakaoTalk");
        assert!(
            request_update(&data, &config, "Kakao.KakaoTalk", "me", now).is_err(),
            "already waiting"
        );
        let ran = RefCell::new(Vec::new());
        let update = |program: &str| {
            ran.borrow_mut().push(program.to_string());
            (UpdateOutcome::Updated, "done".to_string())
        };
        // 03:00: the request runs, the daily update is not due yet.
        let night = Local.with_ymd_and_hms(2026, 11, 2, 3, 0, 0).unwrap();
        run_updates(&data, &config, night, &update);
        assert_eq!(*ran.borrow(), ["Kakao.KakaoTalk"]);
        assert!(pending_requests(&data).is_empty());
        let state = read_state(&data);
        assert_eq!(state.runs.len(), 1);
        assert!(!state.runs[0].automatic);
        assert_eq!(state.runs[0].requested_by, "me");
        assert!(state.running.is_none());

        // 09:00: the daily update runs once.
        let morning = Local.with_ymd_and_hms(2026, 11, 2, 9, 0, 0).unwrap();
        run_updates(&data, &config, morning, &update);
        run_updates(&data, &config, morning, &update);
        assert_eq!(ran.borrow().len(), 2);
        assert!(read_state(&data).runs[1].automatic);

        // A program taken off the list after the request is not run.
        request_update(&data, &config, "Kakao.KakaoTalk", "me", now).unwrap();
        run_updates(&data, &config_with(&[]), morning, &update);
        assert_eq!(ran.borrow().len(), 2);
        assert_eq!(read_state(&data).runs[2].outcome, UpdateOutcome::Failed);
        fs::remove_dir_all(&data).unwrap();
    }

    #[test]
    fn program_ids_cannot_carry_other_arguments() {
        assert!(is_program_id("Kakao.KakaoTalk"));
        assert!(is_program_id("Notepad++.Notepad++"));
        assert!(!is_program_id("Kakao.KakaoTalk --uninstall"));
        assert!(!is_program_id("\"x\""));
        assert!(!is_program_id(""));
    }

    #[test]
    fn winget_is_found_through_the_manifest_or_the_folder() {
        let root = folder("winget_lookup");
        let old = root.join("Microsoft.DesktopAppInstaller_1.9.100.0_x64__8wekyb3d8bbwe");
        let new = root.join("Microsoft.DesktopAppInstaller_1.29.380.0_x64__8wekyb3d8bbwe");
        let language = root.join(
            "Microsoft.DesktopAppInstaller_1.29.380.0_neutral_split.language-ko_8wekyb3d8bbwe",
        );
        for package in [&old, &new, &language] {
            fs::create_dir_all(package).unwrap();
        }
        assert!(is_app_installer_folder(&old) && is_app_installer_folder(&new));
        assert!(!is_app_installer_folder(&language));
        // The old package has winget.exe at its top; the new one only through its manifest.
        fs::write(old.join("winget.exe"), b"").unwrap();
        fs::create_dir_all(new.join("cli")).unwrap();
        fs::write(new.join(r"cli").join("wg.exe"), b"").unwrap();
        fs::write(
            new.join("AppxManifest.xml"),
            r#"<Application Id="winget" Executable="cli\wg.exe"><uap3:Extension Category="windows.appExecutionAlias" Executable="cli/wg.exe"><desktop:ExecutionAlias Alias="winget.exe"/></uap3:Extension></Application>"#,
        )
        .unwrap();
        assert_eq!(
            newest_winget(vec![old.clone(), new.clone(), old.clone()]).unwrap(),
            new.join("cli/wg.exe")
        );
        // Without the manifest, a winget.exe one level below is found.
        fs::remove_file(new.join("AppxManifest.xml")).unwrap();
        fs::write(new.join("cli").join("winget.exe"), b"").unwrap();
        assert_eq!(
            newest_winget(vec![old.clone(), new.clone()]).unwrap(),
            new.join("cli").join("winget.exe")
        );
        // Nothing found: the error names the folders searched.
        fs::remove_file(new.join("cli").join("winget.exe")).unwrap();
        let error = newest_winget(vec![new.clone()]).unwrap_err();
        assert!(error.contains("1.29.380.0"), "{error}");
        assert_eq!(
            newest_winget(Vec::new()).unwrap_err(),
            "앱 설치 관리자 폴더를 찾지 못함"
        );
        assert_eq!(
            winget_executable_in_manifest(r#"Executable="..\x.exe" Alias="winget.exe""#),
            None
        );
    }

    #[test]
    fn winget_exit_codes() {
        assert_eq!(outcome_of(Some(0), String::new()).0, UpdateOutcome::Updated);
        assert_eq!(
            outcome_of(Some(0x8A15_002Bu32 as i32), String::new()).0,
            UpdateOutcome::AlreadyUpToDate
        );
        assert_eq!(
            outcome_of(Some(0x8A15_0014u32 as i32), String::new()).0,
            UpdateOutcome::NotInstalled
        );
        assert_eq!(outcome_of(Some(1), String::new()).0, UpdateOutcome::Failed);
        assert_eq!(
            short_output(
                "Found KakaoTalk\r  ██████▒▒ 50%\r\nSuccessfully installed\n".as_bytes(),
                b""
            ),
            "Found KakaoTalk / Successfully installed"
        );
    }

    #[test]
    fn the_time_zone_is_recorded_once_and_then_kept() {
        let data = folder("time_zone");
        fs::write(data.join("ai_focus.json"), "{}").unwrap();
        let set_to = RefCell::new(Vec::new());
        let set = |name: &str| {
            set_to.borrow_mut().push(name.to_string());
            Ok(())
        };
        let config = AiFocusConfig::default();
        assert_eq!(
            guard_time_zone(&data, &config, Ok("Korea Standard Time".into()), &set),
            TimeZoneAction::Recorded("Korea Standard Time".into())
        );
        let saved: AiFocusConfig =
            serde_json::from_str(&fs::read_to_string(data.join("ai_focus.json")).unwrap()).unwrap();
        assert_eq!(saved.time_zone, "Korea Standard Time");

        assert_eq!(
            guard_time_zone(&data, &saved, Ok("Korea Standard Time".into()), &set),
            TimeZoneAction::Nothing
        );
        assert_eq!(
            guard_time_zone(&data, &saved, Ok("Pacific Standard Time".into()), &set),
            TimeZoneAction::PutBack {
                from: "Pacific Standard Time".into(),
                to: "Korea Standard Time".into()
            }
        );
        assert_eq!(*set_to.borrow(), ["Korea Standard Time"]);
        assert_eq!(read_state(&data).time_zone_resets.len(), 1);
        let failing = |_: &str| Err("denied".to_string());
        assert!(matches!(
            guard_time_zone(&data, &saved, Ok("UTC".into()), &failing),
            TimeZoneAction::Failed(_)
        ));
        assert!(read_state(&data).time_zone_error.contains("denied"));
        fs::remove_dir_all(&data).unwrap();
    }
}

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
/// The service's list of installed programs (written by the service only).
pub const INSTALLED_FILE: &str = "installed_programs.json";
/// Present while an agent waits for a new list of installed programs.
pub const INSTALLED_REQUEST_FILE: &str = "installed_programs_request";
/// Where winget writes the ids of the installed programs (`winget export`).
#[cfg_attr(not(windows), allow(dead_code))]
const EXPORT_FILE: &str = "installed_programs_export.json";
/// The daily automatic update runs once a day, from this local hour on.
pub const AUTO_UPDATE_FROM_HOUR: u32 = 4;
const KEEP_RUNS: usize = 30;
const KEEP_RESETS: usize = 20;

/// Responsibility: Name how one update ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOutcome {
    Updated,
    /// It was not installed, so the service installed it (for every account).
    Installed,
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
    /// The programs winget knows on this PC, as the service last listed them.
    pub installed: Option<InstalledList>,
    /// A new list was asked for and the service has not written it yet.
    pub installed_waiting: bool,
}

/// Responsibility: Describe one program installed on this PC that winget can update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledProgram {
    /// The winget id (what `updatable_programs` holds).
    pub id: String,
    pub name: String,
    pub version: String,
    /// The newer version winget offers, if any.
    pub available: String,
}

/// Responsibility: Hold the service's last list of installed programs
/// (the contents of `installed_programs.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstalledList {
    pub listed_at: Option<DateTime<Utc>>,
    pub programs: Vec<InstalledProgram>,
    /// Why the list could not be read, if it could not.
    pub error: String,
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
        installed: read_installed_list(data_directory),
        installed_waiting: data_directory.join(INSTALLED_REQUEST_FILE).exists(),
    }
}

/// Ask the service for a new list of installed programs (agent side).
pub fn request_installed_list(data_directory: &Path) -> Result<(), String> {
    fs::write(
        data_directory.join(INSTALLED_REQUEST_FILE),
        Utc::now().to_rfc3339(),
    )
    .map_err(|error| error.to_string())
}

pub fn read_installed_list(data_directory: &Path) -> Option<InstalledList> {
    serde_json::from_str(&fs::read_to_string(data_directory.join(INSTALLED_FILE)).ok()?).ok()
}

/// One pass of the service: when an agent asked for it, list the installed programs with
/// `list` (winget in production) and write the list.
pub fn list_installed_if_asked(
    data_directory: &Path,
    list: &dyn Fn(&Path) -> Result<Vec<InstalledProgram>, String>,
) {
    let request = data_directory.join(INSTALLED_REQUEST_FILE);
    if !request.exists() {
        return;
    }
    tracing::info!("listing the installed programs");
    let (programs, error) = match list(data_directory) {
        Ok(programs) => (programs, String::new()),
        Err(error) => (Vec::new(), error),
    };
    let installed = InstalledList {
        listed_at: Some(Utc::now()),
        programs,
        error,
    };
    let path = data_directory.join(INSTALLED_FILE);
    let temporary = path.with_extension("json.tmp");
    let written = serde_json::to_vec_pretty(&installed)
        .map_err(std::io::Error::other)
        .and_then(|bytes| fs::write(&temporary, bytes))
        .and_then(|_| fs::rename(&temporary, &path));
    if let Err(error) = written {
        tracing::warn!("could not save {INSTALLED_FILE}: {error}");
    }
    // Removed last, so the Focus app shows "waiting" until the list is there.
    let _ = fs::remove_file(request);
}

/// The installed programs: each id of `winget export` (the exact ids winget can update), with
/// the name, version and newer version from the table `winget list` printed. The table's
/// headers are in the PC's language and long cells end in "…", so rows are found by the id.
pub fn installed_from(
    export_json: &str,
    list_table: &str,
) -> Result<Vec<InstalledProgram>, String> {
    let export: serde_json::Value =
        serde_json::from_str(export_json).map_err(|error| format!("winget export: {error}"))?;
    let mut ids: Vec<String> = export["Sources"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|source| source["Packages"].as_array().cloned().unwrap_or_default())
        .filter_map(|package| package["PackageIdentifier"].as_str().map(str::to_string))
        .filter(|id| is_program_id(id))
        .collect();
    ids.sort_by_key(|id| id.to_lowercase());
    ids.dedup_by(|first, second| first.eq_ignore_ascii_case(second));
    let rows: Vec<&str> = list_table
        .lines()
        .skip_while(|line| !is_rule_line(line))
        .skip(1)
        .collect();
    Ok(ids
        .into_iter()
        .map(|id| {
            rows.iter()
                .find_map(|row| program_in_row(row, &id))
                .unwrap_or_else(|| InstalledProgram {
                    name: id.clone(),
                    id,
                    ..InstalledProgram::default()
                })
        })
        .collect())
}

/// The line of dashes under a winget table's headers.
fn is_rule_line(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 10 && line.chars().all(|character| character == '-')
}

/// `id`'s program in one row of `winget list` (name, id, version, newer version, source), if
/// the row is that program's.
fn program_in_row(row: &str, id: &str) -> Option<InstalledProgram> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (position, character) in row.char_indices() {
        match (character.is_whitespace(), start) {
            (true, Some(token_start)) => {
                tokens.push((token_start, &row[token_start..position]));
                start = None;
            }
            (false, None) => start = Some(position),
            _ => {}
        }
    }
    if let Some(token_start) = start {
        tokens.push((token_start, &row[token_start..]));
    }
    let matches_id = |token: &str| {
        token.eq_ignore_ascii_case(id)
            || token.strip_suffix('…').is_some_and(|shortened| {
                !shortened.is_empty() && id.to_lowercase().starts_with(&shortened.to_lowercase())
            })
    };
    // The id is not the first cell: that is the name.
    let index = (1..tokens.len()).find(|&index| matches_id(tokens[index].1))?;
    let name = row[..tokens[index].0].trim().to_string();
    let mut cells: Vec<String> = Vec::new();
    let mut after = tokens[index + 1..]
        .iter()
        .map(|(_, token)| *token)
        .peekable();
    while let Some(token) = after.next() {
        // winget writes an unknown version as "< 1.2" or "> 1.2".
        if (token == "<" || token == ">") && after.peek().is_some() {
            cells.push(format!("{token} {}", after.next().unwrap_or_default()));
        } else {
            cells.push(token.to_string());
        }
    }
    if cells.last().is_some_and(|last| last == "winget") {
        cells.pop();
    }
    Some(InstalledProgram {
        id: id.to_string(),
        name: if name.is_empty() {
            id.to_string()
        } else {
            name
        },
        version: cells.first().cloned().unwrap_or_default(),
        available: cells.get(1).cloned().unwrap_or_default(),
    })
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
/// winget's error "no applicable installer found" (here: none that installs for every account).
const WINGET_NO_APPLICABLE_INSTALLER: u32 = 0x8A15_0010;
/// winget's error "package is already installed".
const WINGET_ALREADY_INSTALLED: u32 = 0x8A15_0061;
/// An update that runs longer is stopped.
#[cfg(windows)]
const UPDATE_TIME_LIMIT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Update `program` with winget, silently and without a window (service side). When winget
/// finds it not installed, install it for every account (`--scope machine`): a program the
/// service installs for itself alone would not reach the user.
#[cfg(windows)]
pub fn winget_upgrade(program: &str) -> (UpdateOutcome, String) {
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
    let common = [
        "--id",
        program,
        "--exact",
        "--source",
        "winget",
        "--silent",
        "--disable-interactivity",
        "--accept-source-agreements",
        "--accept-package-agreements",
    ];
    let upgrade = match run_winget(&winget, &[&["upgrade"][..], &common].concat()) {
        Ok((code, stdout, stderr)) => outcome_of(code, short_output(&stdout, &stderr)),
        Err(message) => return (UpdateOutcome::Failed, message),
    };
    if upgrade.0 != UpdateOutcome::NotInstalled {
        return upgrade;
    }
    tracing::info!("{program} is not installed; installing it");
    match run_winget(
        &winget,
        &[&["install", "--scope", "machine"][..], &common].concat(),
    ) {
        Ok((code, stdout, stderr)) => install_outcome_of(code, short_output(&stdout, &stderr)),
        Err(message) => (UpdateOutcome::Failed, message),
    }
}

#[cfg(not(windows))]
pub fn winget_upgrade(_program: &str) -> (UpdateOutcome, String) {
    (
        UpdateOutcome::Failed,
        "프로그램 업데이트는 Windows에서만 됩니다".into(),
    )
}

/// The installed programs winget can update, as the service sees them (`winget export` for
/// the ids, `winget list` for the names). `work_folder` holds the export file meanwhile.
#[cfg(windows)]
pub fn winget_installed(work_folder: &Path) -> Result<Vec<InstalledProgram>, String> {
    let winget = winget_path()
        .map_err(|diagnosis| format!("서비스가 winget을 찾지 못했습니다 ({diagnosis})"))?;
    let export_path = work_folder.join(EXPORT_FILE);
    let _ = fs::remove_file(&export_path);
    let export_argument = export_path.to_string_lossy().to_string();
    let (code, stdout, stderr) = run_winget(
        &winget,
        &[
            "export",
            "--output",
            &export_argument,
            "--source",
            "winget",
            "--accept-source-agreements",
            "--disable-interactivity",
        ],
    )?;
    let export_json = fs::read_to_string(&export_path).map_err(|error| {
        format!(
            "winget export가 목록을 쓰지 못했습니다 (코드 {code:?}, {error}). {}",
            short_output(&stdout, &stderr)
        )
    })?;
    let _ = fs::remove_file(&export_path);
    let (_, list_stdout, _) = run_winget(
        &winget,
        &[
            "list",
            "--source",
            "winget",
            "--accept-source-agreements",
            "--disable-interactivity",
        ],
    )?;
    installed_from(&export_json, &String::from_utf8_lossy(&list_stdout))
}

#[cfg(not(windows))]
pub fn winget_installed(_work_folder: &Path) -> Result<Vec<InstalledProgram>, String> {
    Err("설치된 프로그램 목록은 Windows에서만 읽을 수 있습니다".into())
}

/// Run winget with `arguments`, without a window, for at most [`UPDATE_TIME_LIMIT`]: its exit
/// code and output.
#[cfg(windows)]
fn run_winget(
    winget: &Path,
    arguments: &[&str],
) -> Result<(Option<i32>, Vec<u8>, Vec<u8>), String> {
    use std::io::Read;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut child = Command::new(winget)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|error| format!("winget을 실행하지 못했습니다: {error}"))?;
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
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    match status {
        Some(status) => Ok((status.code(), stdout, stderr)),
        None => Err(format!(
            "30분 안에 끝나지 않아 멈췄습니다. {}",
            short_output(&stdout, &stderr)
        )),
    }
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

/// The outcome of `winget install`'s exit code (run when the update found nothing installed).
pub fn install_outcome_of(code: Option<i32>, output: String) -> (UpdateOutcome, String) {
    match code.map(|code| code as u32) {
        Some(0) => (UpdateOutcome::Installed, output),
        Some(WINGET_NO_APPLICABLE_INSTALLER) => (
            UpdateOutcome::Failed,
            format!(
                "설치되어 있지 않아 설치하려 했지만, 이 프로그램에는 모든 사용자에게 설치하는 설치 \
                 파일이 없어 서비스가 설치할 수 없습니다. {output}"
            ),
        ),
        Some(WINGET_NOT_INSTALLED) => (
            UpdateOutcome::Failed,
            format!("winget 목록에 이 이름의 프로그램이 없습니다. {output}"),
        ),
        Some(WINGET_ALREADY_INSTALLED) => (
            UpdateOutcome::Failed,
            format!(
                "업데이트할 때는 설치되어 있지 않다고 했는데, 설치할 때는 이미 설치되어 있다고 \
                 합니다. {output}"
            ),
        ),
        Some(code) => (
            UpdateOutcome::Failed,
            format!(
                "설치되어 있지 않아 설치하려 했지만 실패했습니다: winget 오류 {code:#X}. {output}"
            ),
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
    fn installed_programs_come_from_the_export_with_the_tables_names() {
        let export = r#"{"Sources":[{"Packages":[
            {"PackageIdentifier":"Kakao.KakaoTalk"},
            {"PackageIdentifier":"Microsoft.VisualStudioCode"},
            {"PackageIdentifier":"Anthropic.Claude"},
            {"PackageIdentifier":"Git.Git"},
            {"PackageIdentifier":"bad id; rm"}],
            "SourceDetails":{"Name":"winget"}}]}"#;
        // A Korean table: headers in Korean, a long id cut short with "…", an unknown version.
        let table = "   - \\ |\r\n\
이름                          ID                           버전          사용 가능     원본\n\
--------------------------------------------------------------------------------------------\n\
카카오톡                      Kakao.KakaoTalk              4.2.1.3683    4.2.3.3720    winget\n\
Microsoft Visual Studio Code  Microsoft.VisualStudioCo…    1.95.0                      winget\n\
Git                           Git.Git                      < 2.40.0                    winget\n";
        let programs = installed_from(export, table).unwrap();
        let ids: Vec<&str> = programs.iter().map(|program| program.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "Anthropic.Claude",
                "Git.Git",
                "Kakao.KakaoTalk",
                "Microsoft.VisualStudioCode"
            ]
        );
        let kakao = &programs[2];
        assert_eq!(
            (
                kakao.name.as_str(),
                kakao.version.as_str(),
                kakao.available.as_str()
            ),
            ("카카오톡", "4.2.1.3683", "4.2.3.3720")
        );
        let code = &programs[3];
        assert_eq!(
            (
                code.name.as_str(),
                code.version.as_str(),
                code.available.as_str()
            ),
            ("Microsoft Visual Studio Code", "1.95.0", "")
        );
        assert_eq!(programs[1].version, "< 2.40.0");
        // Not in the table: shown by its id alone.
        assert_eq!(programs[0].name, "Anthropic.Claude");
        assert!(installed_from("not json", table).is_err());
    }

    #[test]
    fn the_installed_list_is_written_only_when_asked() {
        let data_directory = folder("installed_list");
        let calls = RefCell::new(0);
        let list = |_: &Path| {
            *calls.borrow_mut() += 1;
            Ok(vec![InstalledProgram {
                id: "Kakao.KakaoTalk".into(),
                name: "카카오톡".into(),
                ..InstalledProgram::default()
            }])
        };
        list_installed_if_asked(&data_directory, &list);
        assert_eq!(*calls.borrow(), 0);
        assert!(read_installed_list(&data_directory).is_none());
        request_installed_list(&data_directory).unwrap();
        let config = AiFocusConfig::default();
        assert!(status(&data_directory, &config).installed_waiting);
        list_installed_if_asked(&data_directory, &list);
        list_installed_if_asked(&data_directory, &list);
        assert_eq!(*calls.borrow(), 1);
        let shown = status(&data_directory, &config);
        assert!(!shown.installed_waiting);
        let installed = shown.installed.unwrap();
        assert_eq!(installed.programs[0].name, "카카오톡");
        assert!(installed.listed_at.is_some() && installed.error.is_empty());
        // A failed listing is written with its reason.
        request_installed_list(&data_directory).unwrap();
        list_installed_if_asked(&data_directory, &|_: &Path| Err("winget 없음".to_string()));
        let installed = read_installed_list(&data_directory).unwrap();
        assert!(installed.programs.is_empty());
        assert_eq!(installed.error, "winget 없음");
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
            install_outcome_of(Some(0), String::new()).0,
            UpdateOutcome::Installed
        );
        let (outcome, message) = install_outcome_of(Some(0x8A15_0010u32 as i32), String::new());
        assert_eq!(outcome, UpdateOutcome::Failed);
        assert!(message.contains("모든 사용자에게 설치하는"), "{message}");
        assert_eq!(
            install_outcome_of(Some(1), String::new()).0,
            UpdateOutcome::Failed
        );
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

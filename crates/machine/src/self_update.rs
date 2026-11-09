//! Responsibility: Update this program on the owner's request (Settings tab: "업데이트 확인").
//!
//! - The agent writes a request ([`request`]): check for a newer release, or install it.
//! - The service (SYSTEM) checks GitHub's releases of this repository, downloads the setup
//!   program of the newest one into the data folder and checks it against the SHA-256 GitHub
//!   gives for it ([`newest_release`], [`sha256_hex`]).
//! - Installing stops the service, so the blocks would lapse for a few seconds. The service
//!   therefore hands the install to an **update guard**: a copy of its own program, run from
//!   the data folder, that closes every app and browser in the users' sessions while the setup
//!   program runs silently (keeping the owner's data), and until the new service has started
//!   its agents again. So an update is never a way around the rules, whether the settings are
//!   protected or not.
//! - There is no automatic update: the owner chose to update by hand.
//! - Builds made on the old sync branch (`sync-windows-latest`) show a build after their
//!   version; they update to the numbered releases like any other.
//!
//! Files in the data folder: `self_update_request.json` (agent -> service),
//! `self_update.json` (written by the service and the guard), `self_update\` (the downloaded
//! setup program and the guard's copy of the service program).
//! Walkthrough (Korean): `docs/multi_account.md` 6.6.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub const REQUEST_FILE: &str = "self_update_request.json";
pub const STATE_FILE: &str = "self_update.json";
pub const WORK_FOLDER: &str = "self_update";
/// Where the releases are published.
pub const RELEASES_API: &str =
    "https://api.github.com/repos/KomaSeo/Digital_Wellbeing_Extensible/releases?per_page=30";

/// The commit this program was built from (the first 7 characters; GitHub Actions sets
/// `GITHUB_SHA` while it builds), or `None` for a build made elsewhere.
pub fn this_build() -> Option<&'static str> {
    option_env!("GITHUB_SHA").and_then(|commit| commit.get(..7))
}

/// This version with its build, as the Settings tab shows it.
pub fn this_version_and_build() -> String {
    match this_build() {
        Some(build) => format!("{} ({build})", this_version()),
        None => this_version().to_string(),
    }
}

/// What this build updates to: the newest numbered release newer than this one.
pub fn newest_available(fetched: &Value) -> Option<Release> {
    newest_release(fetched, this_version())
}

/// This program's release version (`installer/RELEASE_VERSION`, e.g. `0.1.0-rc.36`).
pub fn this_version() -> &'static str {
    include_str!("../../../installer/RELEASE_VERSION").trim()
}

/// Order of release versions: `0.1.0-rc.9` < `0.1.0-rc.31` < `0.1.0` < `0.2.0-rc.1`. An
/// unreadable version comes first.
pub fn version_order(version: &str) -> (u64, u64, u64, u64) {
    let version = version.trim().trim_start_matches('v');
    let (release, candidate) = match version.split_once("-rc.") {
        Some((release, number)) => (release, number.parse().unwrap_or(0)),
        None => (version, u64::MAX),
    };
    let mut parts = release.split('.').map(|part| part.parse::<u64>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(major)), Some(Some(minor)), Some(Some(patch))) => {
            (major, minor, patch, candidate)
        }
        _ => (0, 0, 0, 0),
    }
}

/// Responsibility: Name what the owner asked the service to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Check,
    Install,
}

/// Responsibility: Carry one request from the agent to the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub kind: RequestKind,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
}

/// Responsibility: Describe one release that can be installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    pub setup_url: String,
    /// Lower-case hex, as GitHub gives it for the setup program.
    pub sha256: String,
}

/// Responsibility: Record how one install ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRun {
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub from: String,
    pub to: String,
    pub succeeded: bool,
    pub message: String,
}

/// Responsibility: Hold what the service and the guard did (`self_update.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// The request being worked on now (a check or an install).
    pub busy: Option<RequestKind>,
    pub last_check: Option<DateTime<Utc>>,
    /// A newer release than this program, as the last check found.
    pub available: Option<Release>,
    /// Why the last check or download failed.
    pub error: String,
    /// The guard is installing (since then).
    pub installing_since: Option<DateTime<Utc>>,
    pub last_install: Option<InstallRun>,
}

/// Responsibility: Tell the Focus app about updates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateStatus {
    pub version: String,
    pub pending: Option<RequestKind>,
    pub state: UpdateState,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())
}

pub fn read_state(data_directory: &Path) -> UpdateState {
    read_json(&data_directory.join(STATE_FILE)).unwrap_or_default()
}

/// Change `self_update.json` (the service and the guard; never at the same time, since the
/// service stops while the guard installs).
pub fn change_state(data_directory: &Path, change: impl FnOnce(&mut UpdateState)) {
    let mut state = read_state(data_directory);
    change(&mut state);
    if let Err(error) = write_json(&data_directory.join(STATE_FILE), &state) {
        tracing::warn!("could not save {STATE_FILE}: {error}");
    }
}

pub fn pending_request(data_directory: &Path) -> Option<UpdateRequest> {
    read_json(&data_directory.join(REQUEST_FILE))
}

pub fn status(data_directory: &Path) -> UpdateStatus {
    UpdateStatus {
        version: this_version_and_build(),
        pending: pending_request(data_directory).map(|request| request.kind),
        state: read_state(data_directory),
    }
}

/// Ask the service to check or install (agent side). Refused while another request waits or
/// an install runs, and an install when no newer release was found.
pub fn request(
    data_directory: &Path,
    kind: RequestKind,
    requested_by: &str,
    now: DateTime<Utc>,
) -> Result<(), String> {
    let state = read_state(data_directory);
    if pending_request(data_directory).is_some() || state.busy.is_some() {
        return Err("업데이트 요청을 이미 처리하고 있습니다".into());
    }
    if state.installing_since.is_some() {
        return Err("지금 업데이트를 설치하고 있습니다".into());
    }
    if kind == RequestKind::Install && state.available.is_none() {
        return Err("설치할 새 버전이 없습니다. 먼저 업데이트를 확인하세요".into());
    }
    write_json(
        &data_directory.join(REQUEST_FILE),
        &UpdateRequest {
            kind,
            requested_by: requested_by.to_string(),
            requested_at: now,
        },
    )
}

/// The newest release in GitHub's release list (`GET …/releases`) that is newer than
/// `current` and has a setup program with a SHA-256; `None` when `current` is the newest.
pub fn newest_release(releases: &Value, current: &str) -> Option<Release> {
    releases
        .as_array()?
        .iter()
        .filter(|release| !release["draft"].as_bool().unwrap_or(false))
        .filter_map(|release| {
            let version = release["tag_name"].as_str()?.strip_prefix('v')?.to_string();
            let setup_name = format!("DigitalWellbeing-Setup-{version}.exe");
            let asset = release["assets"]
                .as_array()?
                .iter()
                .find(|asset| asset["name"].as_str() == Some(setup_name.as_str()))?;
            let sha256 = asset["digest"]
                .as_str()?
                .strip_prefix("sha256:")?
                .to_lowercase();
            if sha256.len() != 64
                || !sha256
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
            {
                return None;
            }
            Some(Release {
                setup_url: asset["browser_download_url"].as_str()?.to_string(),
                version,
                sha256,
            })
        })
        .filter(|release| version_order(&release.version) > version_order(current))
        .max_by_key(|release| version_order(&release.version))
}

/// GitHub's release list of this repository.
/// `DW_GITHUB_TOKEN` (CI only) lifts GitHub's limit for requests without a login, which
/// shared CI machines reach; a PC asks a few times a day at most and needs none.
pub fn fetch_releases() -> Result<Value, String> {
    let agent = web_client::agent_builder()
        .timeout(std::time::Duration::from_secs(60))
        .build();
    let mut request = agent
        .get(RELEASES_API)
        .set("User-Agent", "digital-wellbeing-updater")
        .set("Accept", "application/vnd.github+json");
    if let Some(token) = std::env::var("DW_GITHUB_TOKEN")
        .ok()
        .filter(|token| !token.is_empty())
    {
        request = request.set("Authorization", &format!("Bearer {token}"));
    }
    match request.call() {
        Ok(response) => response
            .into_json()
            .map_err(|error| format!("GitHub의 답을 읽지 못했습니다: {error}")),
        // No build published yet (or CI is replacing it this minute): nothing to update to.
        Err(ureq::Error::Status(404, _)) => Ok(serde_json::json!({ "assets": [] })),
        Err(error) => Err(format!("GitHub에 닿지 못했습니다: {error}")),
    }
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Download `release`'s setup program into the work folder and check its SHA-256.
pub fn download_setup(data_directory: &Path, release: &Release) -> Result<PathBuf, String> {
    if !release
        .setup_url
        .starts_with("https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases/download/")
    {
        return Err("업데이트 주소가 이 프로그램의 릴리스가 아닙니다".into());
    }
    let mut bytes = Vec::new();
    use std::io::Read;
    web_client::agent_builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .get(&release.setup_url)
        .set("User-Agent", "digital-wellbeing-updater")
        .call()
        .map_err(|error| format!("설치 파일을 받지 못했습니다: {error}"))?
        .into_reader()
        .take(200 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("설치 파일을 받지 못했습니다: {error}"))?;
    let actual = sha256_hex(&bytes);
    if actual != release.sha256 {
        return Err(format!(
            "받은 설치 파일이 릴리스의 것과 다릅니다 (SHA-256 {actual}, 기대한 값 {})",
            release.sha256
        ));
    }
    let folder = data_directory.join(WORK_FOLDER);
    fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let file_name = release
        .setup_url
        .rsplit('/')
        .next()
        .filter(|name| name.ends_with(".exe") && !name.contains(['\\', ':']))
        .unwrap_or("DigitalWellbeing-Setup.exe");
    let path = folder.join(file_name);
    fs::write(&path, &bytes).map_err(|error| error.to_string())?;
    Ok(path)
}

/// The service's side of a request: check, or download and hand over to `start_guard` (which
/// starts the update guard with the setup program's path). Runs every few seconds.
pub fn handle_request(
    data_directory: &Path,
    fetch: &dyn Fn() -> Result<Value, String>,
    download: &dyn Fn(&Release) -> Result<PathBuf, String>,
    start_guard: &dyn Fn(&Path) -> Result<(), String>,
) {
    let Some(request) = pending_request(data_directory) else {
        return;
    };
    // Taken first, so a failing request is not tried again every few seconds.
    let _ = fs::remove_file(data_directory.join(REQUEST_FILE));
    change_state(data_directory, |state| state.busy = Some(request.kind));
    tracing::info!(
        "self-update {:?} requested by {}",
        request.kind,
        request.requested_by
    );
    let checked = fetch().map(|releases| newest_available(&releases));
    let outcome = match (request.kind, checked) {
        (_, Err(error)) => Err(error),
        (RequestKind::Check, Ok(available)) => {
            change_state(data_directory, |state| {
                state.available = available;
                state.last_check = Some(Utc::now());
            });
            Ok(())
        }
        (RequestKind::Install, Ok(None)) => {
            change_state(data_directory, |state| {
                state.available = None;
                state.last_check = Some(Utc::now());
            });
            Err("설치할 새 버전이 없습니다".into())
        }
        (RequestKind::Install, Ok(Some(release))) => download(&release).and_then(|setup| {
            change_state(data_directory, |state| {
                state.installing_since = Some(Utc::now());
            });
            tracing::info!("installing {} with the update guard", release.version);
            start_guard(&setup).inspect_err(|_| {
                change_state(data_directory, |state| state.installing_since = None);
            })
        }),
    };
    change_state(data_directory, |state| {
        state.busy = None;
        state.error = outcome.err().unwrap_or_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(tag: &str, draft: bool) -> Value {
        let version = tag.trim_start_matches('v');
        json!({
            "tag_name": tag,
            "draft": draft,
            "assets": [
                { "name": format!("DigitalWellbeing-{version}-windows-x64.zip"),
                  "digest": format!("sha256:{}", "a".repeat(64)),
                  "browser_download_url": "https://example/zip" },
                { "name": format!("DigitalWellbeing-Setup-{version}.exe"),
                  "digest": format!("sha256:{}", "B".repeat(64)),
                  "browser_download_url": format!("https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases/download/{tag}/DigitalWellbeing-Setup-{version}.exe") }
            ]
        })
    }

    #[test]
    fn the_newest_newer_release_with_a_setup_program_is_picked() {
        let releases = json!([
            release("v0.1.0-rc.9", false),
            release("v0.1.0-rc.40", false),
            release("v0.1.0-rc.41", true),
            { "tag_name": "sync-latest", "assets": [] },
            release("v0.1.0-rc.38", false),
        ]);
        let newest = newest_release(&releases, "0.1.0-rc.36").unwrap();
        assert_eq!(newest.version, "0.1.0-rc.40");
        assert_eq!(newest.sha256, "b".repeat(64));
        assert!(newest.setup_url.ends_with("Setup-0.1.0-rc.40.exe"));
        // Nothing newer: up to date.
        assert_eq!(newest_release(&releases, "0.1.0-rc.40"), None);
        assert_eq!(newest_release(&releases, "0.1.0"), None);
    }

    #[test]
    fn versions_are_ordered_like_releases() {
        assert!(version_order("0.1.0-rc.9") < version_order("0.1.0-rc.31"));
        assert!(version_order("v0.1.0-rc.31") < version_order("0.1.0"));
        assert!(version_order("0.1.0") < version_order("0.2.0-rc.1"));
        assert!(!this_version().is_empty());
    }

    #[test]
    fn a_check_records_what_it_found_and_an_install_hands_over_to_the_guard() {
        let folder = std::env::temp_dir().join(format!("dw_self_update_{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let newer = format!("{}9", this_version());
        let releases = json!([release(&format!("v{newer}"), false)]);
        let fetch = || Ok(releases.clone());
        let download = |release: &Release| Ok(PathBuf::from(format!("setup-{}", release.version)));
        let started = std::cell::RefCell::new(Vec::new());
        let start_guard = |setup: &Path| {
            started.borrow_mut().push(setup.to_path_buf());
            Ok(())
        };

        // An install before any check is refused.
        assert!(request(&folder, RequestKind::Install, "work", Utc::now()).is_err());
        request(&folder, RequestKind::Check, "work", Utc::now()).unwrap();
        assert!(request(&folder, RequestKind::Check, "work", Utc::now()).is_err());
        handle_request(&folder, &fetch, &download, &start_guard);
        let state = read_state(&folder);
        assert_eq!(state.busy, None);
        assert_eq!(state.error, "");
        assert_eq!(
            state
                .available
                .as_ref()
                .map(|release| release.version.clone()),
            Some(newer.clone())
        );
        assert!(started.borrow().is_empty());

        request(&folder, RequestKind::Install, "work", Utc::now()).unwrap();
        handle_request(&folder, &fetch, &download, &start_guard);
        assert_eq!(started.borrow().len(), 1);
        assert!(read_state(&folder).installing_since.is_some());
        // While it installs, nothing else is taken.
        assert!(request(&folder, RequestKind::Check, "work", Utc::now()).is_err());

        // A failing check is reported.
        change_state(&folder, |state| state.installing_since = None);
        request(&folder, RequestKind::Check, "work", Utc::now()).unwrap();
        handle_request(&folder, &|| Err("offline".into()), &download, &start_guard);
        assert_eq!(read_state(&folder).error, "offline");
        let _ = fs::remove_dir_all(&folder);
    }
}

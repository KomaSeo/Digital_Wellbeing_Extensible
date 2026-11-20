//! Responsibility: Install a program from an installer file the user chose, with administrator
//! rights, when the file carries a valid digital signature of its own.
//!
//! Some programs cannot come from winget (the owner: Clip Studio Paint 5.0.4, the version their
//! licence covers, while winget offers the newest). The user's account is a standard account,
//! so the service (SYSTEM, in session 0) runs the installer. The owner decided that no AI
//! reviews it: what an installer may do is limited by these rules instead.
//! - **Signed files only.** The file must carry a valid Authenticode signature inside it
//!   (`Get-AuthenticodeSignature`: status `Valid`, signature type `Authenticode`) and must not
//!   be a Windows component (`IsOSBinary`). Windows' own programs (cmd.exe, powershell.exe)
//!   are signed through catalogues, not inside the file, so they are refused.
//! - **No arguments from the user.** The user picks the kind of installer; each kind has fixed
//!   silent arguments ([`InstallerKind::arguments`]). A signed tool that takes commands as
//!   arguments therefore only ever gets "install silently".
//! - **No swapping.** The agent copies the file into the data folder (which only SYSTEM and
//!   administrators may write) and checks the copy; the service checks the copy again and runs
//!   that copy, then deletes it.
//!
//! Session 0 shows no window, so an installer that waits for a click does not finish; it is
//! stopped after 30 minutes. Files: `install_requests\<id>.json` and `install_requests\<id>\`
//! (the copy), written by agents and removed by the service; results in `system_care.json`.

use crate::{read_state, write_state};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const INSTALL_FOLDER: &str = "install_requests";
/// The largest installer accepted.
const MAX_INSTALLER_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const KEEP_INSTALLS: usize = 20;

/// Responsibility: Name an installer technology and the silent arguments it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallerKind {
    /// A Windows Installer package (`.msi`), run by `msiexec /i … /qn /norestart`.
    Msi,
    /// NSIS: `/S`.
    Nsis,
    /// Inno Setup: `/VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-`.
    InnoSetup,
    /// InstallShield (Clip Studio Paint, for example): `/s`.
    InstallShield,
    /// InstallShield around an MSI: `/s /v/qn`.
    InstallShieldMsi,
    /// WiX bundles and many others: `/quiet /norestart`.
    Quiet,
}

impl InstallerKind {
    pub const ALL: [InstallerKind; 6] = [
        InstallerKind::InstallShield,
        InstallerKind::InstallShieldMsi,
        InstallerKind::Nsis,
        InstallerKind::InnoSetup,
        InstallerKind::Quiet,
        InstallerKind::Msi,
    ];

    /// The fixed arguments after the installer (for [`Self::Msi`], after `msiexec /i <file>`).
    pub fn arguments(self) -> &'static [&'static str] {
        match self {
            InstallerKind::Msi => &["/qn", "/norestart"],
            InstallerKind::Nsis => &["/S"],
            InstallerKind::InnoSetup => &["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-"],
            InstallerKind::InstallShield => &["/s"],
            InstallerKind::InstallShieldMsi => &["/s", "/v/qn"],
            InstallerKind::Quiet => &["/quiet", "/norestart"],
        }
    }

    /// What the Focus app shows.
    pub fn label(self) -> &'static str {
        match self {
            InstallerKind::Msi => "MSI 패키지(.msi)",
            InstallerKind::Nsis => "NSIS (/S)",
            InstallerKind::InnoSetup => "Inno Setup (/VERYSILENT)",
            InstallerKind::InstallShield => "InstallShield (/s)",
            InstallerKind::InstallShieldMsi => "InstallShield + MSI (/s /v/qn)",
            InstallerKind::Quiet => "WiX 등 (/quiet)",
        }
    }

    /// The file extension this kind takes, lower case.
    pub fn extension(self) -> &'static str {
        match self {
            InstallerKind::Msi => "msi",
            _ => "exe",
        }
    }
}

/// Responsibility: Carry one installer the user asked for, from the agent to the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRequest {
    pub id: String,
    /// The installer's file name, as the user's file had it.
    pub file_name: String,
    pub kind: InstallerKind,
    /// The signer the agent found (the certificate's name).
    pub publisher: String,
    pub requested_at: DateTime<Utc>,
    /// The Windows user who asked.
    pub requested_by: String,
}

/// Responsibility: Name how one installer run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallOutcome {
    Installed,
    /// Installed; Windows needs a restart to finish.
    RestartNeeded,
    Failed,
    /// Not run: the signature did not hold when the service checked.
    Refused,
}

/// Responsibility: Record one installer the service ran (or refused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRun {
    pub file_name: String,
    pub kind: InstallerKind,
    pub publisher: String,
    pub requested_by: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: InstallOutcome,
    pub message: String,
}

/// Responsibility: Hold what `Get-AuthenticodeSignature` said about a file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SignatureCheck {
    /// "Valid", "NotSigned", "HashMismatch", …
    #[serde(rename = "Status")]
    pub status: String,
    /// "Authenticode" (inside the file) or "Catalog".
    #[serde(rename = "SignatureType")]
    pub signature_type: String,
    #[serde(rename = "IsOSBinary")]
    pub is_os_binary: bool,
    /// The signer certificate's subject (`CN=CELSYS,Inc., O=…`).
    #[serde(rename = "Subject")]
    pub subject: String,
}

/// The publisher a check vouches for, or why the file is refused.
pub fn verdict(check: &SignatureCheck) -> Result<String, String> {
    if check.status != "Valid" {
        return Err(match check.status.as_str() {
            "NotSigned" | "" => {
                "서명이 없는 파일입니다. 서명된 설치 파일만 설치할 수 있습니다".into()
            }
            "HashMismatch" => "서명한 뒤에 파일이 바뀌었습니다".into(),
            other => format!("서명이 유효하지 않습니다({other})"),
        });
    }
    if check.signature_type != "Authenticode" {
        return Err(
            "파일 안에 서명이 없습니다(Windows 목록으로만 서명된 파일). 설치 파일이 아닙니다"
                .into(),
        );
    }
    if check.is_os_binary {
        return Err("Windows의 구성 요소는 실행하지 않습니다".into());
    }
    Ok(publisher_name(&check.subject))
}

/// The common name of a certificate subject: `CN=CELSYS,Inc., O=…` -> `CELSYS,Inc.`.
fn publisher_name(subject: &str) -> String {
    let Some(start) = subject.find("CN=") else {
        return subject.trim().to_string();
    };
    let rest = &subject[start + 3..];
    let name = if let Some(quoted) = rest.strip_prefix('"') {
        quoted.split('"').next().unwrap_or_default()
    } else {
        // The next attribute starts with ", X=" (the name itself may hold commas).
        let mut end = rest.len();
        for (index, _) in rest.match_indices(", ") {
            let after = &rest[index + 2..];
            if after
                .split_once('=')
                .is_some_and(|(key, _)| key.chars().all(|character| character.is_ascii_uppercase()))
            {
                end = index;
                break;
            }
        }
        &rest[..end]
    };
    name.trim().to_string()
}

/// The kind fits the file: `.msi` for an MSI package, `.exe` for the others.
fn check_name(file_name: &str, kind: InstallerKind) -> Result<(), String> {
    let extension = Path::new(file_name)
        .extension()
        .map(|extension| extension.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if extension != kind.extension() {
        return Err(format!(
            "{}에는 .{} 파일을 고르세요",
            kind.label(),
            kind.extension()
        ));
    }
    Ok(())
}

/// Ask the service to run the installer at `source` (agent side, as SYSTEM in the user's
/// session): the file is copied into the data folder and its signature checked with `check`.
pub fn request_install(
    data_directory: &Path,
    source: &Path,
    kind: InstallerKind,
    requested_by: &str,
    now: DateTime<Utc>,
    check: &dyn Fn(&Path) -> Result<SignatureCheck, String>,
) -> Result<InstallRequest, String> {
    let file_name = source
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
        .ok_or("설치 파일의 경로가 아닙니다")?;
    check_name(&file_name, kind)?;
    let metadata =
        fs::metadata(source).map_err(|error| format!("파일을 읽지 못했습니다: {error}"))?;
    if !metadata.is_file() {
        return Err("파일이 아닙니다".into());
    }
    if metadata.len() > MAX_INSTALLER_BYTES {
        return Err("8GB보다 큰 파일은 받지 않습니다".into());
    }
    if pending_installs(data_directory)
        .iter()
        .any(|(_, request)| request.file_name == file_name)
    {
        return Err(format!("{file_name}의 설치를 이미 기다리고 있습니다"));
    }
    let id = format!(
        "{}-{}",
        now.timestamp_nanos_opt().unwrap_or_default(),
        std::process::id()
    );
    let folder = data_directory.join(INSTALL_FOLDER);
    let copy_folder = folder.join(&id);
    fs::create_dir_all(&copy_folder).map_err(|error| error.to_string())?;
    let copy = copy_folder.join(&file_name);
    let checked = fs::copy(source, &copy)
        .map_err(|error| format!("파일을 복사하지 못했습니다: {error}"))
        .and_then(|_| check(&copy))
        .and_then(|signature| verdict(&signature));
    let publisher = match checked {
        Ok(publisher) => publisher,
        Err(reason) => {
            let _ = fs::remove_dir_all(&copy_folder);
            return Err(reason);
        }
    };
    let request = InstallRequest {
        id: id.clone(),
        file_name,
        kind,
        publisher,
        requested_at: now,
        requested_by: requested_by.to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&request).map_err(|error| error.to_string())?;
    let temporary = folder.join(format!("{id}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temporary, folder.join(format!("{id}.json"))).map_err(|error| error.to_string())?;
    Ok(request)
}

/// The installers the service has not taken yet, oldest first.
pub fn pending_installs(data_directory: &Path) -> Vec<(PathBuf, InstallRequest)> {
    let Ok(entries) = fs::read_dir(data_directory.join(INSTALL_FOLDER)) else {
        return Vec::new();
    };
    let mut requests: Vec<(PathBuf, InstallRequest)> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| {
            let request = fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<InstallRequest>(&raw).ok())?;
            Some((path, request))
        })
        .collect();
    requests.sort_by_key(|(_, request)| request.requested_at);
    requests
}

/// Run every waiting installer (service side): check the copy's signature again with `check`,
/// then `run` it with its kind's arguments; record the outcome and delete the copy.
pub fn run_installs(
    data_directory: &Path,
    check: &dyn Fn(&Path) -> Result<SignatureCheck, String>,
    run: &dyn Fn(&Path, InstallerKind) -> (InstallOutcome, String),
) {
    for (path, request) in pending_installs(data_directory) {
        // Taken first, so a crash in the installer does not repeat it forever.
        let _ = fs::remove_file(&path);
        let copy_folder = data_directory.join(INSTALL_FOLDER).join(&request.id);
        let copy = copy_folder.join(&request.file_name);
        let started_at = Utc::now();
        let checked = check_name(&request.file_name, request.kind)
            .and_then(|()| check(&copy))
            .and_then(|signature| verdict(&signature));
        let (outcome, message, publisher) = match checked {
            Ok(publisher) => {
                let mut state = read_state(data_directory);
                state.installing = Some(request.file_name.clone());
                write_state(data_directory, &state);
                tracing::info!("installing {} ({publisher})", request.file_name);
                let (outcome, message) = run(&copy, request.kind);
                tracing::info!("install of {}: {outcome:?} {message}", request.file_name);
                (outcome, message, publisher)
            }
            Err(reason) => (InstallOutcome::Refused, reason, request.publisher.clone()),
        };
        let _ = fs::remove_dir_all(&copy_folder);
        let mut state = read_state(data_directory);
        state.installing = None;
        state.installs.push(InstallRun {
            file_name: request.file_name,
            kind: request.kind,
            publisher,
            requested_by: request.requested_by,
            started_at,
            finished_at: Utc::now(),
            outcome,
            message,
        });
        let extra = state.installs.len().saturating_sub(KEEP_INSTALLS);
        state.installs.drain(..extra);
        write_state(data_directory, &state);
    }
}

/// The outcome of an installer's exit code (Windows Installer codes, which most installers
/// follow: 3010 and 1641 mean "installed, restart needed").
pub fn install_outcome(code: Option<i32>, output: String) -> (InstallOutcome, String) {
    match code {
        Some(0) => (InstallOutcome::Installed, output),
        Some(3010) | Some(1641) => (InstallOutcome::RestartNeeded, output),
        Some(code) => (
            InstallOutcome::Failed,
            format!("설치 파일이 오류 {code}로 끝났습니다. {output}"),
        ),
        None => (InstallOutcome::Failed, output),
    }
}

/// Check `file`'s signature with PowerShell's `Get-AuthenticodeSignature` (the path goes in
/// through an environment variable, never into the command text).
#[cfg(windows)]
pub fn check_signature(file: &Path) -> Result<SignatureCheck, String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let powershell = std::env::var("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let script = "$signature = Get-AuthenticodeSignature -LiteralPath $env:DW_SIGNED_FILE; \
                  [pscustomobject]@{ Status = [string]$signature.Status; \
                  SignatureType = [string]$signature.SignatureType; \
                  IsOSBinary = [bool]$signature.IsOSBinary; \
                  Subject = [string]$signature.SignerCertificate.Subject } | ConvertTo-Json -Compress";
    let output = Command::new(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .env("DW_SIGNED_FILE", file)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("서명을 확인하지 못했습니다: {error}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.trim()).map_err(|error| {
        format!(
            "서명을 확인하지 못했습니다: {error} {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
    })
}

#[cfg(not(windows))]
pub fn check_signature(_file: &Path) -> Result<SignatureCheck, String> {
    Err("설치 파일 실행은 Windows에서만 됩니다".into())
}

/// Run `installer` silently with its kind's arguments (service side), at most 30 minutes.
#[cfg(windows)]
pub fn run_installer(installer: &Path, kind: InstallerKind) -> (InstallOutcome, String) {
    let installer_text = installer.to_string_lossy().to_string();
    let result = match kind {
        InstallerKind::Msi => {
            let msiexec = std::env::var("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(r"C:\Windows"))
                .join(r"System32\msiexec.exe");
            let arguments: Vec<&str> = ["/i", installer_text.as_str()]
                .into_iter()
                .chain(kind.arguments().iter().copied())
                .collect();
            crate::run_hidden(&msiexec, &arguments)
        }
        _ => crate::run_hidden(installer, kind.arguments()),
    };
    match result {
        Ok((code, stdout, stderr)) => install_outcome(code, crate::short_output(&stdout, &stderr)),
        Err(message) => (InstallOutcome::Failed, message),
    }
}

#[cfg(not(windows))]
pub fn run_installer(_installer: &Path, _kind: InstallerKind) -> (InstallOutcome, String) {
    (
        InstallOutcome::Failed,
        "설치 파일 실행은 Windows에서만 됩니다".into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn signed(subject: &str) -> SignatureCheck {
        SignatureCheck {
            status: "Valid".into(),
            signature_type: "Authenticode".into(),
            is_os_binary: false,
            subject: subject.into(),
        }
    }

    #[test]
    fn only_a_valid_signature_inside_a_non_windows_file_passes() {
        assert_eq!(
            verdict(&signed("CN=\"CELSYS,Inc.\", O=\"CELSYS,Inc.\", C=JP")),
            Ok("CELSYS,Inc.".to_string())
        );
        assert_eq!(
            verdict(&signed("CN=CELSYS,Inc., O=CELSYS,Inc., L=Shibuya-ku, C=JP")),
            Ok("CELSYS,Inc.".to_string())
        );
        let unsigned = SignatureCheck {
            status: "NotSigned".into(),
            ..SignatureCheck::default()
        };
        assert!(verdict(&unsigned).unwrap_err().contains("서명이 없는"));
        let changed = SignatureCheck {
            status: "HashMismatch".into(),
            ..signed("CN=X")
        };
        assert!(verdict(&changed).is_err());
        // cmd.exe and powershell.exe: signed through Windows' catalogues.
        let catalog = SignatureCheck {
            signature_type: "Catalog".into(),
            ..signed("CN=Microsoft Windows")
        };
        assert!(verdict(&catalog).is_err());
        let windows_part = SignatureCheck {
            is_os_binary: true,
            ..signed("CN=Microsoft Windows")
        };
        assert!(verdict(&windows_part).is_err());
    }

    #[test]
    fn the_signature_check_reads_powershells_answer() {
        let answer = r#"{"Status":"Valid","SignatureType":"Authenticode","IsOSBinary":false,"Subject":"CN=Vendor"}"#;
        let check: SignatureCheck = serde_json::from_str(answer).unwrap();
        assert_eq!(verdict(&check), Ok("Vendor".to_string()));
    }

    #[test]
    fn every_kind_has_fixed_silent_arguments_and_one_extension() {
        for kind in InstallerKind::ALL {
            assert!(!kind.arguments().is_empty());
            assert!(
                check_name(&format!("Setup.{}", kind.extension().to_uppercase()), kind).is_ok()
            );
        }
        assert_eq!(InstallerKind::InstallShield.arguments(), ["/s"]);
        assert!(check_name("setup.msi", InstallerKind::Nsis).is_err());
        assert!(check_name("tool.bat", InstallerKind::Quiet).is_err());
    }

    #[test]
    fn a_signed_installer_is_copied_checked_again_and_run_once() {
        let folder = std::env::temp_dir().join(format!("dw_install_{}", std::process::id()));
        let data = folder.join("data");
        fs::create_dir_all(&data).unwrap();
        let source = folder.join("CSP_504w_setup.exe");
        fs::write(&source, b"installer").unwrap();
        let ok = |_: &Path| Ok(signed("CN=CELSYS,Inc."));
        let request = request_install(
            &data,
            &source,
            InstallerKind::InstallShield,
            "me",
            Utc::now(),
            &ok,
        )
        .unwrap();
        assert_eq!(request.publisher, "CELSYS,Inc.");
        // The same file cannot wait twice.
        assert!(request_install(
            &data,
            &source,
            InstallerKind::InstallShield,
            "me",
            Utc::now(),
            &ok
        )
        .is_err());
        // The user's file changes after the request: the service runs the copy.
        fs::write(&source, b"something else").unwrap();
        let ran = RefCell::new(Vec::new());
        run_installs(&data, &ok, &|installer, kind| {
            ran.borrow_mut().push((fs::read(installer).unwrap(), kind));
            install_outcome(Some(3010), String::new())
        });
        assert_eq!(
            *ran.borrow(),
            [(b"installer".to_vec(), InstallerKind::InstallShield)]
        );
        let state = read_state(&data);
        assert_eq!(state.installs.len(), 1);
        assert_eq!(state.installs[0].outcome, InstallOutcome::RestartNeeded);
        assert!(state.installing.is_none());
        assert!(pending_installs(&data).is_empty());
        assert!(
            !data.join(INSTALL_FOLDER).join(&request.id).exists(),
            "the copy is deleted"
        );
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn unsigned_files_are_refused_and_not_kept() {
        let folder = std::env::temp_dir().join(format!("dw_unsigned_{}", std::process::id()));
        let data = folder.join("data");
        fs::create_dir_all(&data).unwrap();
        let source = folder.join("tool.exe");
        fs::write(&source, b"x").unwrap();
        let unsigned = |_: &Path| {
            Ok(SignatureCheck {
                status: "NotSigned".into(),
                ..SignatureCheck::default()
            })
        };
        assert!(request_install(
            &data,
            &source,
            InstallerKind::Quiet,
            "me",
            Utc::now(),
            &unsigned
        )
        .is_err());
        assert!(pending_installs(&data).is_empty());
        assert_eq!(
            fs::read_dir(data.join(INSTALL_FOLDER)).unwrap().count(),
            0,
            "no copy is left behind"
        );
        // Signed when asked, unsigned when the service looks again: refused, not run.
        let ok = |_: &Path| Ok(signed("CN=Vendor"));
        request_install(&data, &source, InstallerKind::Quiet, "me", Utc::now(), &ok).unwrap();
        run_installs(&data, &unsigned, &|_, _| panic!("must not run"));
        assert_eq!(
            read_state(&data).installs[0].outcome,
            InstallOutcome::Refused
        );
        let _ = fs::remove_dir_all(&folder);
    }
}

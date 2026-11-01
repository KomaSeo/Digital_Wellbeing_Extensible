//! Responsibility: Install and uninstall the machine-wide setup: copy the binaries, lock down
//! the data folder, register the service and the Focus app autostart.
//!
//! `install` / `uninstall`, run once from an administrator terminal.
//!
//! What `install` does, in order:
//! 1. stop an already-installed service (so its binaries can be replaced);
//! 2. copy the three executables to `%ProgramFiles%\DigitalWellbeing` (and delete the old
//!    admin settings window an older version installed)
//!    (standard users cannot modify files there — otherwise they could swap
//!    in their own program and have the service run it as SYSTEM);
//! 3. create `%ProgramData%\DigitalWellbeing` (on a first install a folder
//!    already there is moved aside: any user may have created it; junctions
//!    are refused), make Administrators the owner and restrict it to SYSTEM
//!    and Administrators (users can neither read the API key nor edit policy);
//! 4. write `machine.json` when `--manage` is given;
//! 5. register the service: automatic start, LocalSystem, restart on failure;
//! 6. register the Focus app to start at every user's logon (HKLM Run key);
//! 7. start the service.
//!
//! If a later step fails during an upgrade, the previous service is started
//! again. Running executables are renamed to `*.old` before the new copy.

use crate::service::SERVICE_NAME;
use crate::{FOCUS_EXE, LEGACY_ADMIN_EXE, MCP_EXE, SERVICE_EXE};
use anyhow::{bail, Context};
use machine::MachineConfig;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceType,
};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

const RUN_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "DigitalWellbeingFocus";
/// Well-known SIDs, so this works on non-English Windows too.
const SID_SYSTEM: &str = "*S-1-5-18";
const SID_ADMINISTRATORS: &str = "*S-1-5-32-544";

fn service_access() -> ServiceAccess {
    ServiceAccess::QUERY_STATUS
        | ServiceAccess::START
        | ServiceAccess::STOP
        | ServiceAccess::CHANGE_CONFIG
        | ServiceAccess::DELETE
}

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("ProgramFiles")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
    base.join("DigitalWellbeing")
}

pub fn install(arguments: &[String]) -> anyhow::Result<()> {
    let managed = parse_manage(arguments)?;
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("cannot open the service manager — run this from an administrator terminal")?;

    // 1. stop old service
    let existing = manager.open_service(SERVICE_NAME, service_access()).ok();
    if let Some(existing) = &existing {
        println!("stopping the running service…");
        stop_and_wait(existing)?;
    }
    let upgrade = existing.is_some();

    let result = install_steps(&manager, managed, upgrade);
    if result.is_err() {
        // Never leave the PC unprotected because an upgrade failed half-way.
        if let Some(old) = &existing {
            if old.start::<&str>(&[]).is_ok() {
                println!("install failed; the previous service was started again");
            }
        }
    }
    result
}

fn install_steps(
    manager: &ServiceManager,
    managed: Option<Vec<String>>,
    upgrade: bool,
) -> anyhow::Result<()> {
    // 2. binaries
    let install_directory = install_dir();
    fs::create_dir_all(&install_directory)
        .with_context(|| format!("create {}", install_directory.display()))?;
    let source_dir = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .context("no exe dir")?;
    for executable_name in [SERVICE_EXE, FOCUS_EXE, MCP_EXE] {
        let from = source_dir.join(executable_name);
        let to = install_directory.join(executable_name);
        if !from.exists() {
            if executable_name == SERVICE_EXE {
                bail!("{} not found", from.display());
            }
            println!(
                "warning: {} not found next to the installer, skipped",
                executable_name
            );
            continue;
        }
        if same_file(&from, &to) {
            continue;
        }
        replace_file(&from, &to).with_context(|| format!("copy {}", to.display()))?;
        println!("copied {}", to.display());
    }

    // Settings are edited in the Focus app now; a leftover admin window would
    // only confuse. If it is running, it goes on the next install.
    let legacy = install_directory.join(LEGACY_ADMIN_EXE);
    if legacy.exists() {
        match fs::remove_file(&legacy) {
            Ok(()) => println!("removed {}", legacy.display()),
            Err(error) => println!("warning: could not remove {}: {error}", legacy.display()),
        }
    }

    // 3. data dir, locked down
    let data = machine::data_dir();
    prepare_data_dir(&data, upgrade)?;
    // Owner first: an owner can always rewrite the ACL, so a folder owned by
    // someone else would not stay locked.
    run(Command::new("icacls").arg(&data).args([
        "/setowner",
        SID_ADMINISTRATORS,
        "/T",
        "/C",
        "/L",
        "/Q",
    ]))
    .context("set data dir owner")?;
    // The folder alone gets the explicit, inheritable ACL. (Applying this with
    // /T also hit every existing file: the (OI)(CI) grant does not apply to a
    // file, so files kept no ACE at all and even SYSTEM could not open them.)
    run(Command::new("icacls").arg(&data).args([
        "/inheritance:r",
        "/grant:r",
        &format!("{SID_SYSTEM}:(OI)(CI)F"),
        "/grant:r",
        &format!("{SID_ADMINISTRATORS}:(OI)(CI)F"),
        "/L",
        "/Q",
    ]))
    .context("restrict data dir")?;
    // Everything inside inherits that ACL; this also repairs files an earlier
    // version left without any ACE.
    if let Err(error) = run(Command::new("icacls")
        .arg(data.join("*"))
        .args(["/reset", "/T", "/C", "/L", "/Q"]))
    {
        println!(
            "warning: could not reset the ACLs inside {}: {error:#}",
            data.display()
        );
    }
    println!("locked {} to SYSTEM + Administrators", data.display());

    // 4. managed users.
    let mut config = MachineConfig::load();
    if let Some(managed) = managed {
        config.managed_users = managed;
    }
    config.save()?;
    if config.managed_users.is_empty() {
        println!("managed accounts: ALL, administrators included (--manage <user> limits them)");
    } else {
        println!("managed accounts: {}", config.managed_users.join(", "));
    }

    // 5. service
    let service_info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("Digital Wellbeing"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: install_directory.join(SERVICE_EXE),
        launch_arguments: vec![OsString::from("run")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = match manager.open_service(SERVICE_NAME, service_access()) {
        Ok(service) => {
            service.change_config(&service_info)?;
            service
        }
        Err(_) => manager.create_service(&service_info, service_access())?,
    };
    service.set_description(
        "Blocks apps and websites according to your rules and the AI focus check. \
         Starts agents in managed user sessions.",
    )?;
    let restart = ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(5),
    };
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    })?;
    service.set_failure_actions_on_non_crash_failures(true)?;
    println!("registered service '{SERVICE_NAME}' (automatic start)");

    // 6. Focus app at logon
    let focus = install_directory.join(FOCUS_EXE);
    if focus.exists() {
        run(Command::new("reg")
            .args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d"])
            .arg(format!("\"{}\"", focus.display()))
            .arg("/f"))?;
        println!("Focus app starts at every logon");
    }

    // 7. go
    service.start::<&str>(&[])?;
    println!("service started. Agents appear in managed sessions within a few seconds.");
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("cannot open the service manager — run this from an administrator terminal")?;
    match manager.open_service(SERVICE_NAME, service_access()) {
        Ok(service) => {
            stop_and_wait(&service)?;
            service.delete()?;
            println!("service removed");
        }
        Err(_) => println!("service was not installed"),
    }
    let _ = run(Command::new("reg").args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]));
    println!(
        "left in place: {} and {} (delete them by hand if you want)",
        install_dir().display(),
        machine::data_dir().display()
    );
    Ok(())
}

/// Make `data` a fresh, trusted folder. On a first install anything already
/// there was created by someone else (any user may create folders under
/// ProgramData) and may hide links or files meant to be overwritten, so it is
/// moved aside. Junctions are refused: the service would write through them.
fn prepare_data_dir(data: &Path, upgrade: bool) -> anyhow::Result<()> {
    for path in [data.to_path_buf(), machine::log_dir()] {
        if is_reparse_point(&path) {
            bail!(
                "{} is a link/junction; remove it and run install again",
                path.display()
            );
        }
    }
    // A folder an earlier install locked down (uninstalled, then installed again) keeps the
    // settings, the API key and the protection. Any other folder may have been made by a
    // standard user, who could have planted settings in it, so it is moved aside.
    if data.exists() && !upgrade && is_locked_down(data) {
        println!(
            "kept the data folder of an earlier install ({}): settings, API key and protection stay",
            data.display()
        );
    } else if data.exists() && !upgrade {
        let aside = data.with_file_name(format!(
            "DigitalWellbeing.untrusted-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since_epoch| since_epoch.as_secs())
                .unwrap_or(0)
        ));
        fs::rename(data, &aside)
            .with_context(|| format!("move pre-existing {} aside", data.display()))?;
        println!("moved a pre-existing data folder to {}", aside.display());
    }
    fs::create_dir_all(machine::log_dir())?;
    Ok(())
}

/// Whether `path` is as `install` leaves the data folder: owned by Administrators or SYSTEM,
/// and nobody else may write, delete or change permissions in it.
fn is_locked_down(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::{
        GetAce, IsWellKnownSid, WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE,
        ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION,
    };
    /// Rights that change the folder or what is in it.
    const WRITE_RIGHTS: u32 = 0x2 // add file / write data
        | 0x4 // add subfolder / append data
        | 0x10 // write extended attributes
        | 0x40 // delete child
        | 0x100 // write attributes
        | 0x1_0000 // delete
        | 0x4_0000 // write DACL
        | 0x8_0000 // write owner
        | 0x1000_0000 // generic all
        | 0x4000_0000; // generic write
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut owner = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: valid NUL-terminated path and out-pointers; the descriptor is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return false;
    }
    // SAFETY: `owner` and every SID below point into `descriptor`, alive until LocalFree.
    let trusted_sid = |sid| unsafe {
        IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
            || IsWellKnownSid(sid, WinLocalSystemSid) != 0
    };
    let mut locked = !owner.is_null() && trusted_sid(owner) && !dacl.is_null();
    if locked {
        // SAFETY: `dacl` is a valid ACL inside `descriptor`.
        let count = unsafe { (*dacl).AceCount };
        for index in 0..u32::from(count) {
            let mut ace = std::ptr::null_mut();
            // SAFETY: index < AceCount; `ace` receives a pointer into the ACL.
            if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
                locked = false;
                break;
            }
            // SAFETY: every ACE starts with an ACE_HEADER.
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            match header.AceType {
                ACCESS_DENIED_ACE_TYPE => {}
                ACCESS_ALLOWED_ACE_TYPE => {
                    // SAFETY: an access-allowed ACE is an ACCESS_ALLOWED_ACE.
                    let allowed = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
                    let sid = std::ptr::addr_of!(allowed.SidStart) as *mut core::ffi::c_void;
                    if allowed.Mask & WRITE_RIGHTS != 0 && !trusted_sid(sid) {
                        locked = false;
                        break;
                    }
                }
                // Object or callback ACEs: not what `install` writes.
                _ => {
                    locked = false;
                    break;
                }
            }
        }
    }
    // SAFETY: allocated by GetNamedSecurityInfoW.
    unsafe { LocalFree(descriptor) };
    locked
}

fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
}

/// Copy `from` over `to` even while `to` is running: a running exe cannot be
/// overwritten but can be renamed, so the old file is moved to `*.old` first.
fn replace_file(from: &Path, to: &Path) -> anyhow::Result<()> {
    if to.exists() {
        let old = to.with_extension("exe.old");
        // A previous .old may itself still be running; fall back to a unique name.
        let old = if fs::remove_file(&old).is_ok() || !old.exists() {
            old
        } else {
            to.with_extension(format!("exe.old.{}", std::process::id()))
        };
        fs::rename(to, &old)?;
        if let Err(error) = fs::copy(from, to) {
            let _ = fs::rename(&old, to);
            return Err(error.into());
        }
        let _ = fs::remove_file(&old); // fails while still running; removed next time
    } else {
        fs::copy(from, to)?;
    }
    Ok(())
}

fn stop_and_wait(service: &Service) -> anyhow::Result<()> {
    if service.query_status()?.current_state == ServiceState::Stopped {
        return Ok(());
    }
    let _ = service.stop();
    let start = Instant::now();
    while service.query_status()?.current_state != ServiceState::Stopped {
        if start.elapsed() > Duration::from_secs(20) {
            bail!("service did not stop within 20 s");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    Ok(())
}

/// Which accounts to manage: `--manage <user>` (repeatable) names them, `--manage-all`
/// manages every account (administrators too), neither keeps what `machine.json` says.
fn parse_manage(arguments: &[String]) -> anyhow::Result<Option<Vec<String>>> {
    let mut users = Vec::new();
    let mut all = false;
    let mut remaining = arguments.iter();
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--manage" => users.push(
                remaining
                    .next()
                    .context("--manage needs a user name")?
                    .clone(),
            ),
            "--manage-all" => all = true,
            other => bail!("unknown option '{other}'"),
        }
    }
    match (all, users.is_empty()) {
        (true, false) => bail!("use either --manage <user> or --manage-all, not both"),
        (true, true) => Ok(Some(Vec::new())),
        (false, false) => Ok(Some(users)),
        (false, true) => Ok(None),
    }
}

fn same_file(first: &Path, second: &Path) -> bool {
    match (fs::canonicalize(first), fs::canonicalize(second)) {
        (Ok(first_canonical), Ok(second_canonical)) => first_canonical == second_canonical,
        _ => false,
    }
}

fn run(command: &mut Command) -> anyhow::Result<()> {
    let status = command
        .status()
        .with_context(|| format!("run {command:?}"))?;
    if !status.success() {
        bail!("{command:?} failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_manage;

    fn arguments(list: &[&str]) -> Vec<String> {
        list.iter().map(|argument| argument.to_string()).collect()
    }

    #[test]
    fn manage_options() {
        assert_eq!(parse_manage(&arguments(&[])).unwrap(), None);
        assert_eq!(
            parse_manage(&arguments(&["--manage-all"])).unwrap(),
            Some(vec![])
        );
        assert_eq!(
            parse_manage(&arguments(&["--manage", "work", "--manage", "admin"])).unwrap(),
            Some(arguments(&["work", "admin"]))
        );
        assert!(parse_manage(&arguments(&["--manage", "work", "--manage-all"])).is_err());
        assert!(parse_manage(&arguments(&["--manage"])).is_err());
    }
}

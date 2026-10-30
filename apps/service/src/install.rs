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

pub fn install(args: &[String]) -> anyhow::Result<()> {
    let managed = parse_manage(args)?;
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
    let dir = install_dir();
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let source_dir = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .context("no exe dir")?;
    for exe in [SERVICE_EXE, FOCUS_EXE, MCP_EXE] {
        let from = source_dir.join(exe);
        let to = dir.join(exe);
        if !from.exists() {
            if exe == SERVICE_EXE {
                bail!("{} not found", from.display());
            }
            println!("warning: {} not found next to the installer, skipped", exe);
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
    let legacy = dir.join(LEGACY_ADMIN_EXE);
    if legacy.exists() {
        match fs::remove_file(&legacy) {
            Ok(()) => println!("removed {}", legacy.display()),
            Err(err) => println!("warning: could not remove {}: {err}", legacy.display()),
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
    run(Command::new("icacls").arg(&data).args([
        "/inheritance:r",
        "/grant:r",
        &format!("{SID_SYSTEM}:(OI)(CI)F"),
        "/grant:r",
        &format!("{SID_ADMINISTRATORS}:(OI)(CI)F"),
        "/T",
        "/L",
        "/Q",
    ]))
    .context("restrict data dir")?;
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
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from("Digital Wellbeing"),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: dir.join(SERVICE_EXE),
        launch_arguments: vec![OsString::from("run")],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let service = match manager.open_service(SERVICE_NAME, service_access()) {
        Ok(service) => {
            service.change_config(&info)?;
            service
        }
        Err(_) => manager.create_service(&info, service_access())?,
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
    let focus = dir.join(FOCUS_EXE);
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
    if data.exists() && !upgrade {
        let aside = data.with_file_name(format!(
            "DigitalWellbeing.untrusted-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ));
        fs::rename(data, &aside)
            .with_context(|| format!("move pre-existing {} aside", data.display()))?;
        println!("moved a pre-existing data folder to {}", aside.display());
    }
    fs::create_dir_all(machine::log_dir())?;
    Ok(())
}

fn is_reparse_point(path: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
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
        if let Err(err) = fs::copy(from, to) {
            let _ = fs::rename(&old, to);
            return Err(err.into());
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
fn parse_manage(args: &[String]) -> anyhow::Result<Option<Vec<String>>> {
    let mut users = Vec::new();
    let mut all = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manage" => users.push(it.next().context("--manage needs a user name")?.clone()),
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

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn run(cmd: &mut Command) -> anyhow::Result<()> {
    let status = cmd.status().with_context(|| format!("run {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} failed ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_manage;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn manage_options() {
        assert_eq!(parse_manage(&args(&[])).unwrap(), None);
        assert_eq!(
            parse_manage(&args(&["--manage-all"])).unwrap(),
            Some(vec![])
        );
        assert_eq!(
            parse_manage(&args(&["--manage", "work", "--manage", "admin"])).unwrap(),
            Some(args(&["work", "admin"]))
        );
        assert!(parse_manage(&args(&["--manage", "work", "--manage-all"])).is_err());
        assert!(parse_manage(&args(&["--manage"])).is_err());
    }
}

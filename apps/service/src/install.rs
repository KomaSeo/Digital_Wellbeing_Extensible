//! Responsibility: Install and uninstall the machine-wide setup: copy the binaries, lock down
//! the data folder, register the service and the Focus app autostart.
//!
//! `install` / `uninstall`, run once from an administrator terminal.
//!
//! What `install` does, in order:
//! 1. stop an already-installed service (so its binaries can be replaced);
//! 2. copy the three executables to `%ProgramFiles%\DigitalWellbeing`
//!    (standard users cannot modify files there — otherwise they could swap
//!    in their own program and have the service run it as SYSTEM);
//! 3. create `%ProgramData%\DigitalWellbeing` and restrict it to SYSTEM and
//!    Administrators (users can neither read the API key nor edit policy);
//! 4. write `machine.json` when `--manage` is given;
//! 5. register the service: automatic start, LocalSystem, restart on failure;
//! 6. register the Focus app to start at every user's logon (HKLM Run key);
//! 7. start the service.

use crate::service::SERVICE_NAME;
use crate::{ADMIN_EXE, FOCUS_EXE, SERVICE_EXE};
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
    if let Ok(existing) = manager.open_service(SERVICE_NAME, service_access()) {
        println!("stopping the running service…");
        stop_and_wait(&existing)?;
    }

    // 2. binaries
    let dir = install_dir();
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let source_dir = std::env::current_exe()?
        .parent()
        .map(Path::to_path_buf)
        .context("no exe dir")?;
    for exe in [SERVICE_EXE, FOCUS_EXE, ADMIN_EXE] {
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
        fs::copy(&from, &to)
            .with_context(|| format!("copy {} (close the app if it is running)", to.display()))?;
        println!("copied {}", to.display());
    }

    // 3. data dir, locked down
    let data = machine::data_dir();
    fs::create_dir_all(machine::log_dir())?;
    run(Command::new("icacls").arg(&data).args([
        "/inheritance:r",
        "/grant:r",
        &format!("{SID_SYSTEM}:(OI)(CI)F"),
        "/grant:r",
        &format!("{SID_ADMINISTRATORS}:(OI)(CI)F"),
        "/T",
        "/Q",
    ]))
    .context("restrict data dir")?;
    println!("locked {} to SYSTEM + Administrators", data.display());

    // 4. managed users. Always written: its presence also tells the admin UI
    //    (digital-wellbeing.exe) to edit these shared files instead of enforcing.
    let mut config = MachineConfig::load();
    if !managed.is_empty() {
        config.managed_users = managed;
    }
    config.save()?;
    if config.managed_users.is_empty() {
        println!("managed accounts: ALL (use --manage <user> to limit to your work account)");
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

fn parse_manage(args: &[String]) -> anyhow::Result<Vec<String>> {
    let mut users = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manage" => users.push(it.next().context("--manage needs a user name")?.clone()),
            other => bail!("unknown option '{other}'"),
        }
    }
    Ok(users)
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

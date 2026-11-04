//! Responsibility: Choose the role of digital-wellbeing-service.exe from its first argument
//! (install, uninstall, service, agent) and set up logging.
//!
//! Machine-wide install of Digital Wellbeing.
//!
//! One binary, four roles (first argument):
//!
//! | command     | who runs it                     | what it does                                   |
//! |-------------|---------------------------------|------------------------------------------------|
//! | `install`   | an administrator, once          | copy binaries, lock down data, register service |
//! | `uninstall [--delete-data] [--restore-safe-mode]` | an administrator | stop + remove the service; optionally the owner's data, and open safe mode again |
//! | `run`       | the Service Control Manager     | supervisor: keep one agent per managed session |
//! | `agent`     | the supervisor (as SYSTEM)      | enforce rules inside one user's session        |
//! | `self-update-guard <setup>` | the service (a copy in the data folder) | install an update while every app stays closed (`update_guard`) |
//! | `version`   | anyone                          | print the release version                      |
//! | `self-update-check` | anyone                  | say whether GitHub has a newer release (changes nothing) |
//!
//! See `docs/multi_account.md` for the full picture.

mod agent;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod spawn;
#[cfg(windows)]
mod update_guard;

use std::fs::{self, OpenOptions};
use std::sync::Mutex;

/// Executable names inside the install directory.
pub const SERVICE_EXE: &str = "digital-wellbeing-service.exe";
pub const FOCUS_EXE: &str = "digital-wellbeing-focus.exe";
/// The old admin settings window. A machine install no longer ships it (settings are edited
/// in the Focus app); `install` deletes a copy left by an older version.
pub const LEGACY_ADMIN_EXE: &str = "digital-wellbeing.exe";
/// MCP server Claude Desktop starts in the user's account (`docs/claude_desktop.md`).
pub const MCP_EXE: &str = "digital-wellbeing-mcp.exe";

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("agent") => agent::run(),
        // Run with administrator rights (the Focus app asks Windows for them): turn off a
        // protection that only an administrator may turn off.
        Some("settings-unlock") => settings_unlock(),
        #[cfg(windows)]
        Some("install") => install::install(&arguments[1..]),
        #[cfg(windows)]
        Some("uninstall") => install::uninstall(
            arguments[1..]
                .iter()
                .any(|argument| argument == "--delete-data"),
            arguments[1..]
                .iter()
                .any(|argument| argument == "--restore-safe-mode"),
        ),
        #[cfg(windows)]
        Some("run") => service::run_dispatcher(),
        #[cfg(windows)]
        Some("self-update-guard") => {
            init_logging("update.log");
            update_guard::run(&arguments[1..])
        }
        Some("self-update-check") => {
            let releases = machine::self_update::fetch_releases().map_err(anyhow::Error::msg)?;
            match machine::self_update::newest_release(
                &releases,
                machine::self_update::this_version(),
            ) {
                Some(release) => println!("newer: {} ({})", release.version, release.setup_url),
                None => println!("up to date ({})", machine::self_update::this_version()),
            }
            Ok(())
        }
        Some("version") => {
            println!("{}", machine::self_update::this_version());
            Ok(())
        }
        _ => {
            eprintln!(
                "usage (from an administrator terminal):\n  \
                 digital-wellbeing-service install [--manage <windows user>]... | [--manage-all] [--delete-data] [--block-safe-mode]\n  \
                 digital-wellbeing-service uninstall [--delete-data] [--restore-safe-mode]"
            );
            Ok(())
        }
    }
}

/// `settings-unlock`: only an administrator can read the data folder, so reading it is the
/// check that this runs with administrator rights.
fn settings_unlock() -> anyhow::Result<()> {
    let data_directory = machine::data_dir();
    if std::fs::read_dir(&data_directory).is_err() {
        anyhow::bail!("관리자 권한으로 실행해야 합니다");
    }
    machine::settings::unprotect(
        &data_directory,
        chrono::Utc::now(),
        machine::settings::Unlock::Administrator,
    )
    .map_err(anyhow::Error::msg)?;
    println!("설정 보호를 풀었습니다. 걸린 차단도 모두 풀립니다.");
    Ok(())
}

/// Log to `%ProgramData%\DigitalWellbeing\logs\<file>`; stderr if that fails.
/// Files over 10 MB are truncated at start-up (no rotation).
pub fn init_logging(file: &str) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let log_directory = machine::log_dir();
    let path = log_directory.join(file);
    let too_big = fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 10 * 1024 * 1024);
    let opened = fs::create_dir_all(&log_directory).and_then(|_| {
        OpenOptions::new()
            .create(true)
            .append(!too_big)
            .write(true)
            .truncate(too_big)
            .open(&path)
    });
    match opened {
        Ok(log_file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(log_file))
            .init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

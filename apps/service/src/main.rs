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
//! | `sync-…`    | an administrator / the SCM      | multi-device sync (see `sync_commands`)        |
//! | `self-update-guard <setup>` | the service (a copy in the data folder) | install an update while every app stays closed (`update_guard`) |
//! | `version`   | anyone                          | print the release version                      |
//! | `self-update-check` | anyone                  | say whether GitHub has a newer release (changes nothing) |
//!
//! See `docs/multi_account.md` for the full picture.

mod agent;
#[cfg(windows)]
mod install;
mod phone_work;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod spawn;
mod sync_commands;
#[cfg(windows)]
mod update_guard;

use std::fs::{self, OpenOptions};
use std::sync::Mutex;

/// Executable names inside the install directory (named once, in `app_identity`).
pub use app_identity::{FOCUS_EXE, MCP_EXE, SERVICE_EXE};
/// The old admin settings window. A machine install no longer ships it (settings are edited
/// in the Focus app); `install` deletes a copy left by an older version.
pub const LEGACY_ADMIN_EXE: &str = app_identity::LEGACY_ADMIN_EXES[0];

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    // The computer's id for the sync server, so a reinstalled PC keeps its device entry.
    if arguments
        .first()
        .is_some_and(|command| command.starts_with("sync") || command == "run")
    {
        sync::machine_identity::set(sync::machine_identity::from_system());
    }
    match arguments.first().map(String::as_str) {
        Some("agent") => agent::run(),
        // Run with administrator rights (the Focus app asks Windows for them): turn off a
        // protection that only an administrator may turn off.
        Some("settings-unlock") => settings_unlock(),
        // Also with administrator rights: open the ways into safe mode again (the Focus app's
        // "안전 모드 켜기"; shutting them needs no approval, the agent does it).
        Some("safe-mode-open") => safe_mode_open(&arguments[1..]),
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
        Some(command) if command.starts_with("sync-") => {
            sync_commands::run_command(command, &arguments[1..])
                .unwrap_or_else(|| anyhow::bail!("unknown command '{command}' (see docs/sync.md)"))
        }
        #[cfg(windows)]
        Some("self-update-guard") => {
            init_logging("update.log");
            update_guard::run(&arguments[1..])
        }
        Some("self-update-check") => {
            let releases =
                system_care::self_update::fetch_releases().map_err(anyhow::Error::msg)?;
            match system_care::self_update::newest_available(&releases) {
                Some(release) => println!("newer: {} ({})", release.version, release.setup_url),
                None => println!(
                    "up to date ({})",
                    system_care::self_update::this_version_and_build()
                ),
            }
            Ok(())
        }
        Some("version") => {
            println!("{}", system_care::self_update::this_version_and_build());
            Ok(())
        }
        _ => {
            eprintln!(
                "usage (from an administrator terminal):\n  \
                 digital-wellbeing-service install [--manage <windows user>]... | [--manage-all] [--delete-data] [--block-safe-mode]\n  \
                 digital-wellbeing-service uninstall [--delete-data] [--restore-safe-mode]\n  \
                 digital-wellbeing-service safe-mode-open (the ways into safe mode, open again)\n  \
                 digital-wellbeing-service sync-… (multi-device sync, see docs/sync.md)"
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
        machine::settings::Unlock::Administrator {
            device: machine::settings::this_device_id(&data_directory),
        },
    )
    .map_err(anyhow::Error::msg)?;
    println!("엄격 모드를 껐습니다. 걸린 차단도 모두 풀립니다.");
    Ok(())
}

/// `safe-mode-open [--result <file>]`: open the ways into safe mode again
/// ([`machine::safe_mode::open`]). Only an administrator can read the data folder, so reading
/// it is the check that this runs with administrator rights. With `--result`, the outcome is
/// also written to that file (`{"ok", "message", "outcomes"}`) for the Focus app that asked.
fn safe_mode_open(arguments: &[String]) -> anyhow::Result<()> {
    let result_path = arguments
        .iter()
        .position(|argument| argument == "--result")
        .and_then(|index| arguments.get(index + 1))
        .map(std::path::PathBuf::from);
    let (ok, message, outcomes) = if std::fs::read_dir(machine::data_dir()).is_err() {
        (
            false,
            "관리자 권한으로 실행해야 합니다".to_string(),
            Vec::new(),
        )
    } else {
        let outcomes = machine::safe_mode::open();
        let (open, message) = match machine::safe_mode::runs_in_safe_mode() {
            Some(false) => (
                true,
                "안전 모드로 들어가는 길을 다시 열었습니다. 안전 모드에서는 이 프로그램이 동작하지 \
                 않습니다.",
            ),
            Some(true) => (
                false,
                "안전 모드를 다시 열지 못했습니다. 아래 명령의 결과를 보세요.",
            ),
            None => (false, "Windows에서만 됩니다."),
        };
        (open, message.to_string(), outcomes)
    };
    println!("{message}");
    for line in machine::safe_mode::outcome_lines(&outcomes) {
        println!("{line}");
    }
    if let Some(path) = result_path {
        let result = serde_json::json!({ "ok": ok, "message": message, "outcomes": outcomes });
        std::fs::write(&path, result.to_string())?;
    }
    if ok {
        Ok(())
    } else {
        anyhow::bail!("{message}")
    }
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

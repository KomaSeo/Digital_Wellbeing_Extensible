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
//! | `uninstall` | an administrator                | stop + remove the service                      |
//! | `run`       | the Service Control Manager     | supervisor: keep one agent per managed session |
//! | `agent`     | the supervisor (as SYSTEM)      | enforce rules inside one user's session        |
//!
//! See `docs/multi_account.md` for the full picture.

mod agent;
#[cfg(windows)]
mod install;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod spawn;

use std::fs::{self, OpenOptions};
use std::sync::Mutex;

/// Executable names inside the install directory.
pub const SERVICE_EXE: &str = "digital-wellbeing-service.exe";
pub const FOCUS_EXE: &str = "digital-wellbeing-focus.exe";
/// The old admin settings window. A machine install no longer ships it (settings are edited
/// in the Focus app while unlocked); `install` deletes a copy left by an older version.
pub const LEGACY_ADMIN_EXE: &str = "digital-wellbeing.exe";
/// MCP server Claude Desktop starts in the user's account (`docs/claude_desktop.md`).
pub const MCP_EXE: &str = "digital-wellbeing-mcp.exe";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("agent") => agent::run(),
        #[cfg(windows)]
        Some("install") => install::install(&args[1..]),
        #[cfg(windows)]
        Some("uninstall") => install::uninstall(),
        #[cfg(windows)]
        Some("run") => service::run_dispatcher(),
        _ => {
            eprintln!(
                "usage (from an administrator terminal):\n  \
                 digital-wellbeing-service install [--manage <windows user>]... | [--manage-all]\n  \
                 digital-wellbeing-service uninstall"
            );
            Ok(())
        }
    }
}

/// Log to `%ProgramData%\DigitalWellbeing\logs\<file>`; stderr if that fails.
/// Files over 10 MB are truncated at start-up (no rotation).
pub fn init_logging(file: &str) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let dir = machine::log_dir();
    let path = dir.join(file);
    let too_big = fs::metadata(&path).is_ok_and(|m| m.len() > 10 * 1024 * 1024);
    let opened = fs::create_dir_all(&dir).and_then(|_| {
        OpenOptions::new()
            .create(true)
            .append(!too_big)
            .write(true)
            .truncate(too_big)
            .open(&path)
    });
    match opened {
        Ok(f) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(f))
            .init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

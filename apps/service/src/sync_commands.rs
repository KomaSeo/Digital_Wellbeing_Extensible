//! Responsibility: The service's part of multi-device sync: the administrator commands that set
//! up a server and pair devices, the server itself, and the loop that syncs this device.
//!
//! | command                              | where                     | what it does                         |
//! |--------------------------------------|---------------------------|--------------------------------------|
//! | `sync-server-setup [--port N]`       | the server, administrator | lock the folder, register and start the server service, open the port |
//! | `sync-serve [--port N]`              | the server                | serve in this terminal (Linux, tests) |
//! | `sync-add-device <name>`             | the server, administrator | pair a device; prints its code once  |
//! | `sync-devices`                       | the server, administrator | list the paired devices              |
//! | `sync-remove-device <id>`            | the server, administrator | unpair a device                      |
//! | `sync-add-user <name>`               | the server, administrator | make a login (asks for its password) or give it a new password |
//! | `sync-users` / `sync-remove-user <name>` | the server, administrator | list / remove logins          |
//! | `sync-join <server address> <code>`  | a device, administrator   | sync this device from now on         |
//! | `sync-leave`                         | a device, administrator   | stop syncing this device             |
//! | `sync-status`                        | a device                  | show the last sync                   |
//! | `sync-now`                           | a device, administrator   | sync once now                        |
//!
//! Walkthrough (Korean): `docs/sync.md`.

use anyhow::{bail, Context};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use sync::client::{self, ClientConfig, SyncClient, SyncState};
use sync::protocol::PairingCode;
use sync::server::SyncServer;
use tracing::{info, warn};

/// The server's port unless `--port` says otherwise.
pub const DEFAULT_PORT: u16 = 47820;
/// How often a device syncs.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(20);

/// The server's folder: `%ProgramData%\DigitalWellbeingSync` (`/var/lib/digital-wellbeing-sync`
/// elsewhere); `DW_SYNC_SERVER_DIR` overrides it (tests).
pub fn server_folder() -> PathBuf {
    if let Some(folder) = std::env::var_os("DW_SYNC_SERVER_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(folder);
    }
    if cfg!(windows) {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
            .join("DigitalWellbeingSync")
    } else {
        PathBuf::from("/var/lib/digital-wellbeing-sync")
    }
}

fn server() -> SyncServer {
    SyncServer::new(server_folder(), machine::TEST_EDITION)
}

fn port_option(arguments: &[String]) -> anyhow::Result<u16> {
    match arguments.iter().position(|argument| argument == "--port") {
        Some(index) => arguments
            .get(index + 1)
            .context("--port needs a number")?
            .parse()
            .context("--port needs a number"),
        None => Ok(DEFAULT_PORT),
    }
}

/// Run one of the `sync-…` commands; `None` when `command` is not one.
pub fn run_command(command: &str, arguments: &[String]) -> Option<anyhow::Result<()>> {
    let result = match command {
        "sync-serve" => serve(arguments),
        #[cfg(windows)]
        "sync-server-setup" => port_option(arguments).and_then(crate::install::install_sync_server),
        #[cfg(windows)]
        "sync-service" => crate::service::run_sync_dispatcher(),
        "sync-add-device" => add_device(arguments),
        "sync-add-user" => add_user(arguments),
        "sync-remove-user" => remove_user(arguments),
        "sync-users" => list_users(),
        "sync-devices" => list_devices(),
        "sync-remove-device" => remove_device(arguments),
        "sync-join" => join(&machine::data_dir(), arguments),
        "sync-leave" => leave(&machine::data_dir()),
        "sync-status" => status(&machine::data_dir()),
        "sync-now" => sync_now(&machine::data_dir()),
        _ => return None,
    };
    Some(result)
}

fn serve(arguments: &[String]) -> anyhow::Result<()> {
    let port = port_option(arguments)?;
    println!(
        "sync server: {} on port {port} (Ctrl+C stops it)",
        server_folder().display()
    );
    Arc::new(server()).serve(port)?;
    Ok(())
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Serve on `port` until the process ends (the Windows service calls this).
pub fn serve_forever(port: u16) {
    loop {
        if let Err(error) = Arc::new(server()).serve(port) {
            warn!("sync server stopped: {error}; starting again in 10 s");
        }
        std::thread::sleep(Duration::from_secs(10));
    }
}

fn add_device(arguments: &[String]) -> anyhow::Result<()> {
    let name = arguments.join(" ");
    if name.trim().is_empty() {
        bail!("usage: sync-add-device <a name for the device, e.g. laptop>");
    }
    let code = server().add_device(&name).map_err(anyhow::Error::msg)?;
    println!(
        "paired '{}'. On that device, from an administrator terminal:",
        code.device_id
    );
    println!(
        "  digital-wellbeing-service sync-join http://<this server's address>:{DEFAULT_PORT} {}",
        code.to_line()
    );
    println!("The code is shown only this once; anyone with it can sync as that device.");
    Ok(())
}

/// A password typed without showing it; from standard input when there is no terminal
/// (tests pipe it in).
fn read_password(prompt: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return Ok(rpassword::prompt_password(prompt)?);
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn add_user(arguments: &[String]) -> anyhow::Result<()> {
    let name = arguments
        .first()
        .context("usage: sync-add-user <login name, e.g. me>")?;
    let password = read_password("password: ")?;
    let again = read_password("the same password again: ")?;
    if password != again {
        bail!("the two passwords differ");
    }
    server()
        .set_user(name, &password)
        .map_err(anyhow::Error::msg)?;
    println!(
        "login '{}' saved. Every paired device gets it at its next sync; each Windows account \
         logs in once in the Focus app.",
        name.trim().to_lowercase()
    );
    Ok(())
}

fn remove_user(arguments: &[String]) -> anyhow::Result<()> {
    let name = arguments
        .first()
        .context("usage: sync-remove-user <login name>")?;
    server().remove_user(name).map_err(anyhow::Error::msg)?;
    println!("login '{name}' removed; accounts that used it must log in again");
    Ok(())
}

fn list_users() -> anyhow::Result<()> {
    let users = server().users();
    if users.is_empty() {
        println!("no logins (sync-add-user <name> makes one)");
    }
    for user in users {
        println!("{user}");
    }
    Ok(())
}

fn list_devices() -> anyhow::Result<()> {
    let devices = server().devices();
    if devices.is_empty() {
        println!("no paired devices");
    }
    for device in devices {
        println!(
            "{}  ({})  version {}  paired {}  last sync {}",
            device.id,
            device.name,
            if device.version.is_empty() {
                "?"
            } else {
                &device.version
            },
            device.added_at.format("%Y-%m-%d"),
            device
                .last_seen
                .map(|seen| seen.to_rfc3339())
                .unwrap_or_else(|| "never".into())
        );
    }
    Ok(())
}

fn remove_device(arguments: &[String]) -> anyhow::Result<()> {
    let id = arguments
        .first()
        .context("usage: sync-remove-device <device id>")?;
    server().remove_device(id).map_err(anyhow::Error::msg)?;
    println!("'{id}' can no longer sync");
    Ok(())
}

/// Start syncing this device: its own state is replaced by the server's at the first sync
/// (the first device to join fills the server instead).
pub fn join(data_directory: &Path, arguments: &[String]) -> anyhow::Result<()> {
    let [server_url, code] = arguments else {
        bail!("usage: sync-join <server address, e.g. http://192.168.0.10:{DEFAULT_PORT}> <pairing code>");
    };
    let code = PairingCode::from_line(code).map_err(anyhow::Error::msg)?;
    // A key that does not open is better found now than at the first sync.
    sync::envelope::seal(&code.device_id, &code.key, b"check").map_err(anyhow::Error::msg)?;
    let server_url = if server_url.starts_with("http://") || server_url.starts_with("https://") {
        server_url.trim_end_matches('/').to_string()
    } else {
        format!("http://{}", server_url.trim_end_matches('/'))
    };
    forget_sync_state(data_directory)?;
    ClientConfig {
        server_url: server_url.clone(),
        device_id: code.device_id.clone(),
        key: code.key,
    }
    .save(data_directory)
    .map_err(anyhow::Error::msg)?;
    println!(
        "this device syncs with {server_url} as '{}' (within {} seconds once the service runs)",
        code.device_id,
        SYNC_INTERVAL.as_secs()
    );
    Ok(())
}

fn forget_sync_state(data_directory: &Path) -> anyhow::Result<()> {
    let state = data_directory.join(client::STATE_FILE);
    if state.exists() {
        std::fs::remove_file(&state)?;
    }
    let base = data_directory.join(client::BASE_FOLDER);
    if base.exists() {
        std::fs::remove_dir_all(&base)?;
    }
    Ok(())
}

pub fn leave(data_directory: &Path) -> anyhow::Result<()> {
    let config = data_directory.join(client::CONFIG_FILE);
    if !config.exists() {
        println!("this device does not sync");
        return Ok(());
    }
    std::fs::remove_file(config)?;
    forget_sync_state(data_directory)?;
    println!("this device no longer syncs (its files stay as they are)");
    Ok(())
}

fn status(data_directory: &Path) -> anyhow::Result<()> {
    let Some(config) = ClientConfig::load(data_directory) else {
        println!("this device does not sync");
        return Ok(());
    };
    let state = SyncState::load(data_directory);
    println!(
        "server: {}  device: {}",
        config.server_url, config.device_id
    );
    println!(
        "last sync: {}",
        state
            .last_success
            .map(|time| time.to_rfc3339())
            .unwrap_or_else(|| "never".into())
    );
    if let Some(error) = state.last_error {
        println!("last error: {error}");
    }
    for refusal in state.refused {
        println!("refused: {} ({})", refusal.name, refusal.reason);
    }
    for (device, version) in state.outdated_devices {
        println!("needs an update before it syncs again: {device} ({version})");
    }
    Ok(())
}

fn sync_now(data_directory: &Path) -> anyhow::Result<()> {
    let config = ClientConfig::load(data_directory).context("this device does not sync")?;
    let report = SyncClient::new(data_directory.to_path_buf(), config.clone())
        .sync_once(&mut client::http_transport(&config.server_url))
        .map_err(anyhow::Error::msg)?;
    println!(
        "sent {:?}, received {:?}, merged {:?}",
        report.pushed, report.pulled, report.merged
    );
    for refusal in report.refused {
        println!("refused: {} ({})", refusal.name, refusal.reason);
    }
    Ok(())
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Sync this device every [`SYNC_INTERVAL`] while `sync.json` exists (the service runs this).
pub fn sync_loop(data_directory: PathBuf) {
    let mut last_error: Option<String> = None;
    loop {
        if let Some(config) = ClientConfig::load(&data_directory) {
            let client = SyncClient::new(data_directory.clone(), config.clone());
            match client.sync_once(&mut client::http_transport(&config.server_url)) {
                Ok(report) => {
                    if !report.pulled.is_empty() || !report.merged.is_empty() {
                        info!(
                            "sync: received {:?}, merged {:?}",
                            report.pulled, report.merged
                        );
                    }
                    for refusal in &report.refused {
                        warn!(
                            "sync: the server refused {} ({})",
                            refusal.name, refusal.reason
                        );
                    }
                    last_error = None;
                }
                Err(error) => {
                    // Logged once per kind of failure, not every 20 seconds.
                    if last_error.as_ref() != Some(&error) {
                        warn!("sync: {error}");
                    }
                    last_error = Some(error);
                }
            }
        }
        std::thread::sleep(SYNC_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joining_saves_the_address_and_forgets_an_old_sync() {
        let folder = std::env::temp_dir().join(format!("dw_sync_join_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(folder.join(client::BASE_FOLDER)).unwrap();
        std::fs::write(folder.join(client::STATE_FILE), "{}").unwrap();
        let key = sync::envelope::new_key().unwrap();
        join(
            &folder,
            &["192.168.0.10:47820/".to_string(), format!("laptop:{key}")],
        )
        .unwrap();
        let config = ClientConfig::load(&folder).unwrap();
        assert_eq!(config.server_url, "http://192.168.0.10:47820");
        assert_eq!(config.device_id, "laptop");
        assert!(!folder.join(client::STATE_FILE).exists());
        assert!(!folder.join(client::BASE_FOLDER).exists());
        assert!(join(&folder, &["x".into(), "laptop:not-a-key".into()]).is_err());
        leave(&folder).unwrap();
        assert!(ClientConfig::load(&folder).is_none());
        let _ = std::fs::remove_dir_all(&folder);
    }
}

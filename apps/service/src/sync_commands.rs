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
//! | `sync-users` / `sync-remove-user <name>` | the server, administrator | list / remove logins          |
//! | `sync-signup <server address> <name>` | a device, administrator  | make the first account of an empty server (asks for a password) and sync this device |
//! | `sync-login <server address> <name>` | a device, administrator   | log this device in to the account (asks for the password) and sync it |
//! | `sync-login --request <file>`        | a device, administrator   | the same for the Focus app (it asks Windows for administrator approval) |
//! | `sync-join <server address> <code>`  | a device, administrator   | sync this device from now on (a pairing code instead of a login) |
//! | `sync-leave`                         | a device, administrator   | stop syncing this device (not while the settings are protected) |
//! | `sync-status`                        | a device                  | show the last sync                   |
//! | `sync-now`                           | a device, administrator   | sync once now                        |
//!
//! Walkthrough (Korean): `docs/sync.md`.

use anyhow::{bail, Context};
use machine::login::LoginRequestFile;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use sync::account::{AccountCredentials, Joiner};
use sync::client::{self, ClientConfig, SyncClient, SyncReport, SyncState};
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
    SyncServer::new(server_folder())
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
        "sync-add-user" => Err(anyhow::anyhow!(
            "logins are made by signing up from a PC (Focus app or sync-signup): the server never \
             sees a password"
        )),
        "sync-remove-user" => remove_user(arguments),
        "sync-users" => list_users(),
        "sync-devices" => list_devices(),
        "sync-remove-device" => remove_device(arguments),
        "sync-join" => join(&machine::data_dir(), arguments),
        "sync-signup" => login_command(&machine::data_dir(), arguments, true),
        "sync-login" => login_command(&machine::data_dir(), arguments, false),
        "sync-leave" => leave(&machine::data_dir()),
        "sync-status" => status(&machine::data_dir()),
        "sync-now" => sync_now(&machine::data_dir()),
        _ => return None,
    };
    Some(result)
}

fn serve(arguments: &[String]) -> anyhow::Result<()> {
    let port = port_option(arguments)?;
    // To standard error, so a Linux service manager (systemd's journal) keeps the log.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .try_init();
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
    let sync_server = server();
    let code = sync_server.add_device(&name).map_err(anyhow::Error::msg)?;
    // The port the server runs on (noted when it starts), else the default.
    let port = sync_server.port().unwrap_or(DEFAULT_PORT);
    println!(
        "paired '{}'. On that device, from an administrator terminal:",
        code.device_id
    );
    println!(
        "  digital-wellbeing-service sync-join http://<this server's address>:{port} {}",
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
        println!("no logins (a PC signs up to make one)");
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
    let server_url = clean_server_url(server_url);
    forget_sync_state(data_directory)?;
    ClientConfig {
        server_url: server_url.clone(),
        device_id: code.device_id.clone(),
        key: code.key,
        account: None,
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

/// `http://` in front of an address given without it, the default port when none is given,
/// and no `/` at the end: `3.35.0.1` -> `http://3.35.0.1:47820`.
fn clean_server_url(server_url: &str) -> String {
    let server_url = server_url.trim().trim_end_matches('/');
    let (scheme, host) = match server_url.split_once("://") {
        Some((scheme, host)) => (scheme, host),
        None => ("http", server_url),
    };
    let has_port = host.rsplit_once(':').is_some_and(|(_, port)| {
        !port.is_empty() && port.chars().all(|character| character.is_ascii_digit())
    });
    if has_port {
        format!("{scheme}://{host}")
    } else {
        format!("{scheme}://{host}:{DEFAULT_PORT}")
    }
}

/// This computer's name (a new device's id comes from it).
fn computer_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "computer".into())
}

/// The value after `option` in `arguments`, and the arguments without the two.
fn take_option(arguments: &[String], option: &str) -> (Option<String>, Vec<String>) {
    match arguments.iter().position(|argument| argument == option) {
        Some(index) => {
            let value = arguments.get(index + 1).cloned();
            let rest = arguments
                .iter()
                .enumerate()
                .filter(|(position, _)| *position != index && *position != index + 1)
                .map(|(_, argument)| argument.clone())
                .collect();
            (value, rest)
        }
        None => (None, arguments.to_vec()),
    }
}

/// `sync-signup` / `sync-login`: from a terminal (the password is asked for), or from the
/// Focus app (`--request <file>`: the file is read and deleted at once, and the outcome is
/// written next to it as `<file>.result`).
fn login_command(data_directory: &Path, arguments: &[String], signup: bool) -> anyhow::Result<()> {
    let (request_path, arguments) = take_option(arguments, "--request");
    if let Some(request_path) = request_path {
        let request_path = PathBuf::from(request_path);
        let outcome = (|| -> anyhow::Result<String> {
            let text = std::fs::read_to_string(&request_path)
                .with_context(|| format!("read {}", request_path.display()))?;
            let _ = std::fs::remove_file(&request_path);
            let request: LoginRequestFile = serde_json::from_str(&text)?;
            log_in_device(data_directory, &request, &computer_name()).map_err(anyhow::Error::msg)
        })();
        let result = match &outcome {
            Ok(message) => serde_json::json!({ "ok": true, "message": message }),
            Err(error) => serde_json::json!({ "ok": false, "message": format!("{error:#}") }),
        };
        let mut result_path = request_path.into_os_string();
        result_path.push(".result");
        std::fs::write(&result_path, result.to_string())?;
        return outcome.map(|message| println!("{message}"));
    }
    let (device_name, arguments) = take_option(&arguments, "--device");
    let (windows_user, arguments) = take_option(&arguments, "--for");
    let [server_url, name] = arguments.as_slice() else {
        bail!(
            "usage: sync-{} <server address, e.g. 3.35.0.1:{DEFAULT_PORT}> <login name> [--for <Windows account>] [--device <name>]",
            if signup { "signup" } else { "login" }
        );
    };
    let password = read_password("password: ")?;
    if signup && read_password("the same password again: ")? != password {
        bail!("the two passwords differ");
    }
    let request = LoginRequestFile {
        server_url: server_url.clone(),
        name: name.clone(),
        password,
        signup,
        windows_user,
    };
    let message = log_in_device(
        data_directory,
        &request,
        &device_name.unwrap_or_else(computer_name),
    )
    .map_err(anyhow::Error::msg)?;
    println!("{message}");
    Ok(())
}

/// Sign up or log in, sync this device with the account from now on, and log the Windows
/// account in (when given). A device already in this account only checks the password.
pub fn log_in_device(
    data_directory: &Path,
    request: &LoginRequestFile,
    device_name: &str,
) -> Result<String, String> {
    let server_url = clean_server_url(&request.server_url);
    let name = sync::users::clean_name(&request.name)?;
    let current = ClientConfig::load(data_directory);
    // This PC already syncs with this account: the password is checked against the one it
    // keeps, without asking the server (another Windows account of the same PC logging in).
    let already_in = current.as_ref().is_some_and(|config| {
        config.server_url == server_url
            && config.account.as_ref().is_some_and(|account| {
                account.name == name
                    && sync::secret::unprotect(&account.password)
                        .is_ok_and(|kept| same_text(&kept, &request.password))
            })
    });
    let mut message = String::new();
    if !already_in || request.signup {
        let mut transport = sync::account::http_transport(&server_url);
        let mut joiner = Joiner {
            transport: &mut transport,
        };
        let joined = if request.signup {
            joiner.sign_up(&name, &request.password, device_name)?
        } else {
            // The same computer logging in again keeps its id on that server.
            let previous = current
                .as_ref()
                .filter(|config| config.server_url == server_url)
                .map(|config| config.device_id.clone());
            joiner.log_in(&name, &request.password, device_name, previous)?
        };
        forget_sync_state(data_directory).map_err(|error| error.to_string())?;
        ClientConfig {
            server_url: server_url.clone(),
            device_id: joined.answer.device_id.clone(),
            key: joined.answer.key,
            account: Some(AccountCredentials {
                name: name.clone(),
                password: sync::secret::protect(&request.password)?,
                vault_key: joined.vault_key,
            }),
        }
        .save(data_directory)?;
        message = format!(
            "this device syncs with {server_url} as '{}' in the account '{name}' (within {} seconds once the service runs). ",
            joined.answer.device_id,
            SYNC_INTERVAL.as_secs()
        );
    }
    if let Some(windows_user) = request
        .windows_user
        .as_deref()
        .filter(|user| !user.is_empty())
    {
        machine::login::assign(data_directory, windows_user, &name)?;
        message.push_str(&format!(
            "The Windows account '{windows_user}' uses the login '{name}'."
        ));
    }
    Ok(message.trim().to_string())
}

/// One sync; when the server no longer knows this device (it was set up again) and the device
/// logged in with an account, it logs in again by itself and syncs from the server's state.
pub fn sync_device(data_directory: &Path, config: &ClientConfig) -> Result<SyncReport, String> {
    let result = sync_with_vault(data_directory, config);
    let Err(error) = &result else {
        return result;
    };
    let Some(credentials) = config.account.as_ref() else {
        return result;
    };
    if !error.contains("unknown device") {
        return result;
    }
    let password = sync::secret::unprotect(&credentials.password)?;
    let logged_in = Joiner {
        transport: &mut sync::account::http_transport(&config.server_url),
    }
    .log_in(
        &credentials.name,
        &password,
        &config.device_id,
        Some(config.device_id.clone()),
    );
    let joined = match logged_in {
        Ok(joined) => joined,
        Err(login_error) => {
            // The password was changed (the server was set up with another one): the kept
            // password is no good, and trying it every 20 seconds would lock the name out.
            if login_error == sync::account::WRONG_LOGIN {
                ClientConfig {
                    account: None,
                    ..config.clone()
                }
                .save(data_directory)?;
            }
            return Err(format!("{error}; logging in again: {login_error}"));
        }
    };
    info!(
        "sync: the server did not know this device; logged in again as '{}'",
        joined.answer.device_id
    );
    forget_sync_state(data_directory).map_err(|error| error.to_string())?;
    let renewed = ClientConfig {
        device_id: joined.answer.device_id,
        key: joined.answer.key,
        account: Some(AccountCredentials {
            vault_key: joined.vault_key,
            ..credentials.clone()
        }),
        ..config.clone()
    };
    renewed.save(data_directory)?;
    sync_with_vault(data_directory, &renewed)
}

/// One sync round, with the API key sealed into the vault before (a key changed here goes
/// out) and taken from it after (a key changed elsewhere comes in), when the PC logged in
/// with an account.
fn sync_with_vault(data_directory: &Path, config: &ClientConfig) -> Result<SyncReport, String> {
    let vault_key = config
        .account
        .as_ref()
        .map(|account| account.vault_key.clone());
    if let Some(vault_key) = &vault_key {
        if let Err(error) = sync::vault::reconcile(data_directory, vault_key) {
            warn!("sync: the API key vault: {error}");
        }
    }
    let report = SyncClient::new(data_directory.to_path_buf(), config.clone())
        .sync_once(&mut client::http_transport(&config.server_url))?;
    if let Some(vault_key) = &vault_key {
        match sync::vault::reconcile(data_directory, vault_key) {
            Ok(Some(sync::vault::VaultChange::TookTheVaultKey)) => {
                info!("sync: took the API key the other PCs use")
            }
            Ok(_) => {}
            Err(error) => warn!("sync: the API key vault: {error}"),
        }
    }
    Ok(report)
}

/// The same text, compared in a time that does not depend on where they differ.
fn same_text(first: &str, second: &str) -> bool {
    first.len() == second.len()
        && first
            .bytes()
            .zip(second.bytes())
            .fold(0u8, |difference, (left, right)| difference | (left ^ right))
            == 0
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
    // Otherwise leaving would be a way out of the shared blocks and the shared protection.
    if machine::settings::is_protected(data_directory) {
        bail!(
            "the settings are protected: this device cannot stop syncing until the protection ends"
        );
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
    let report = sync_device(data_directory, &config).map_err(anyhow::Error::msg)?;
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
            match sync_device(&data_directory, &config) {
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
        assert_eq!(clean_server_url("3.35.0.1"), "http://3.35.0.1:47820");
        assert_eq!(
            clean_server_url("http://sync.example.com/"),
            "http://sync.example.com:47820"
        );
        assert_eq!(clean_server_url("3.35.0.1:50000"), "http://3.35.0.1:50000");
        assert_eq!(config.device_id, "laptop");
        assert!(!folder.join(client::STATE_FILE).exists());
        assert!(!folder.join(client::BASE_FOLDER).exists());
        assert!(join(&folder, &["x".into(), "laptop:not-a-key".into()]).is_err());
        leave(&folder).unwrap();
        assert!(ClientConfig::load(&folder).is_none());
        let _ = std::fs::remove_dir_all(&folder);
    }
}

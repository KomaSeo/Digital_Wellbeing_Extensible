//! The sync commands of the real program: a server in a terminal, two devices paired with the
//! codes it prints, a sync over HTTP each way, and an unpaired device refused.

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::time::{Duration, Instant};

const PROGRAM: &str = env!("CARGO_BIN_EXE_digital-wellbeing-service");

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Each data folder plays one computer: its name is the computer's id.
fn machine_of(data_folder: &Path) -> String {
    data_folder
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn run(server_folder: &Path, data_folder: &Path, arguments: &[&str]) -> Output {
    Command::new(PROGRAM)
        .args(arguments)
        .env("DW_SYNC_SERVER_DIR", server_folder)
        .env("DW_DATA_DIR", data_folder)
        .env("DW_MACHINE_ID", machine_of(data_folder))
        .output()
        .expect("run the program")
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn pair(server_folder: &Path, data_folder: &Path, name: &str, port: u16) {
    let added = run(server_folder, data_folder, &["sync-add-device", name]);
    assert!(added.status.success(), "{}", text(&added));
    let printed = text(&added);
    // The hint names the port the server runs on, not the default.
    assert!(printed.contains(&format!(":{port} ")), "{printed}");
    let code = printed
        .lines()
        .find(|line| line.contains("sync-join"))
        .and_then(|line| line.split_whitespace().last())
        .expect("a sync-join line with the code")
        .to_string();
    let joined = run(
        server_folder,
        data_folder,
        &["sync-join", &format!("127.0.0.1:{port}"), &code],
    );
    assert!(joined.status.success(), "{}", text(&joined));
}

#[test]
fn two_devices_sync_through_the_real_program() {
    let root: PathBuf = std::env::temp_dir().join(format!("dw_sync_cli_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let server_folder = root.join("server");
    let laptop = root.join("laptop");
    let desktop = root.join("desktop");
    for folder in [&server_folder, &laptop, &desktop] {
        fs::create_dir_all(folder).unwrap();
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let _server = Server(
        Command::new(PROGRAM)
            .args(["sync-serve", "--port", &port.to_string()])
            .env("DW_SYNC_SERVER_DIR", &server_folder)
            .spawn()
            .expect("start the server"),
    );
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the server did not start"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    fs::write(
        laptop.join("planner.json"),
        r#"{ "life_rules": "from the laptop" }"#,
    )
    .unwrap();
    pair(&server_folder, &laptop, "laptop", port);
    let synced = run(&server_folder, &laptop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    assert!(text(&synced).contains("planner.json"), "{}", text(&synced));

    // Logins are made by signing up from a PC, not on the server.
    let refused = run(&server_folder, &laptop, &["sync-add-user", "me"]);
    assert!(!refused.status.success());

    pair(&server_folder, &desktop, "desktop", port);
    let synced = run(&server_folder, &desktop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    let received = fs::read_to_string(desktop.join("planner.json")).unwrap();
    assert!(received.contains("from the laptop"), "{received}");

    let status = text(&run(&server_folder, &desktop, &["sync-status"]));
    assert!(status.contains("device: desktop"), "{status}");
    assert!(!status.contains("last sync: never"), "{status}");
    let devices = text(&run(&server_folder, &desktop, &["sync-devices"]));
    assert!(
        devices.contains("laptop") && devices.contains("desktop"),
        "{devices}"
    );
    // The keys never show in the list.
    let config = fs::read_to_string(desktop.join("sync.json")).unwrap();
    let key: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert!(!devices.contains(key["key"].as_str().unwrap()));

    // An unpaired device can no longer sync.
    let removed = run(&server_folder, &desktop, &["sync-remove-device", "desktop"]);
    assert!(removed.status.success(), "{}", text(&removed));
    let refused = run(&server_folder, &desktop, &["sync-now"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("unknown device"),
        "{}",
        text(&refused)
    );

    let left = run(&server_folder, &desktop, &["sync-leave"]);
    assert!(left.status.success());
    assert!(!desktop.join("sync.json").exists());
    let _ = fs::remove_dir_all(&root);
}

fn run_with_input(
    server_folder: &Path,
    data_folder: &Path,
    arguments: &[&str],
    input: &str,
) -> Output {
    use std::io::Write;
    let mut child = Command::new(PROGRAM)
        .args(arguments)
        .env("DW_SYNC_SERVER_DIR", server_folder)
        .env("DW_DATA_DIR", data_folder)
        .env("DW_MACHINE_ID", machine_of(data_folder))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run the program");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn start_server(server_folder: &Path, port: u16) -> Server {
    let server = Server(
        Command::new(PROGRAM)
            .args(["sync-serve", "--port", &port.to_string()])
            .env("DW_SYNC_SERVER_DIR", server_folder)
            .spawn()
            .expect("start the server"),
    );
    let started = Instant::now();
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the server did not start"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    server
}

#[test]
fn devices_sign_up_and_log_in_and_come_back_after_the_server_is_set_up_again() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("dw_sync_login_cli_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let server_folder = root.join("server");
    let laptop = root.join("laptop");
    let desktop = root.join("desktop");
    for folder in [&server_folder, &laptop, &desktop] {
        fs::create_dir_all(folder).unwrap();
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let mut server = start_server(&server_folder, port);

    fs::write(
        laptop.join("planner.json"),
        r#"{ "life_rules": "from the laptop" }"#,
    )
    .unwrap();
    fs::write(
        laptop.join("ai_focus.json"),
        r#"{ "api_key": "sk-ant-test-shared-key" }"#,
    )
    .unwrap();
    let signed_up = run_with_input(
        &server_folder,
        &laptop,
        &[
            "sync-signup",
            &address,
            "me",
            "--device",
            "laptop",
            "--for",
            "work",
        ],
        "long password\nlong password\n",
    );
    assert!(signed_up.status.success(), "{}", text(&signed_up));
    let synced = run(&server_folder, &laptop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    let logins = fs::read_to_string(laptop.join("logins.json")).unwrap();
    assert!(logins.contains("\"work\": \"me\""), "{logins}");

    // A wrong password is refused; the right one joins the desktop.
    let wrong = run_with_input(
        &server_folder,
        &desktop,
        &["sync-login", &address, "me", "--device", "desktop"],
        "wrong password\n",
    );
    assert!(!wrong.status.success());
    assert!(text(&wrong).contains("맞지 않습니다"), "{}", text(&wrong));
    let logged_in = run_with_input(
        &server_folder,
        &desktop,
        &["sync-login", &address, "me", "--device", "desktop"],
        "long password\n",
    );
    assert!(logged_in.status.success(), "{}", text(&logged_in));
    let synced = run(&server_folder, &desktop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    let received = fs::read_to_string(desktop.join("planner.json")).unwrap();
    assert!(received.contains("from the laptop"), "{received}");
    // One API key for every PC: the desktop took the laptop's, through the sealed vault.
    let settings = fs::read_to_string(desktop.join("ai_focus.json")).unwrap();
    assert!(settings.contains("sk-ant-test-shared-key"), "{settings}");
    // The server has neither the key nor the password, in any file.
    let mut server_files = String::new();
    let mut folders = vec![server_folder.clone()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).unwrap().flatten() {
            if entry.path().is_dir() {
                folders.push(entry.path());
            } else {
                server_files.push_str(&String::from_utf8_lossy(&fs::read(entry.path()).unwrap()));
            }
        }
    }
    assert!(!server_files.contains("sk-ant-test-shared-key"));
    assert!(!server_files.contains("long password"));
    // The password is kept on the device only sealed.
    let config = fs::read_to_string(desktop.join("sync.json")).unwrap();
    assert!(!config.contains("long password"), "{config}");

    // The desktop is installed again and its data deleted: logging in again on the same
    // computer gives it its old device entry back, not a second one.
    let device_of = |folder: &Path| -> String {
        let config: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(folder.join("sync.json")).unwrap()).unwrap();
        config["device_id"].as_str().unwrap().to_string()
    };
    let desktop_device = device_of(&desktop);
    for file in ["sync.json", "sync_state.json"] {
        let _ = fs::remove_file(desktop.join(file));
    }
    let again = run_with_input(
        &server_folder,
        &desktop,
        &["sync-login", &address, "me", "--device", "desktop"],
        "long password\n",
    );
    assert!(again.status.success(), "{}", text(&again));
    assert_eq!(device_of(&desktop), desktop_device);
    let devices = text(&run(&server_folder, &desktop, &["sync-devices"]));
    assert_eq!(
        devices
            .lines()
            .filter(|line| line.contains("(desktop)"))
            .count(),
        1,
        "{devices}"
    );
    let synced = run(&server_folder, &desktop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));

    // The Focus app's way: a request file, read and deleted, with the outcome next to it.
    let request = root.join("request.json");
    fs::write(
        &request,
        serde_json::json!({
            "server_url": address, "name": "me", "password": "long password",
            "windows_user": "other"
        })
        .to_string(),
    )
    .unwrap();
    let from_app = run(
        &server_folder,
        &desktop,
        &["sync-login", "--request", request.to_str().unwrap()],
    );
    assert!(from_app.status.success(), "{}", text(&from_app));
    assert!(!request.exists());
    let result = fs::read_to_string(root.join("request.json.result")).unwrap();
    assert!(result.contains("\"ok\":true"), "{result}");
    let logins = fs::read_to_string(desktop.join("logins.json")).unwrap();
    assert!(logins.contains("\"other\": \"me\""), "{logins}");

    // The server is set up again from nothing: the laptop signs up again, and the desktop
    // logs in again by itself at its next sync.
    drop(server);
    fs::remove_dir_all(&server_folder).unwrap();
    fs::create_dir_all(&server_folder).unwrap();
    server = start_server(&server_folder, port);
    let signed_up = run_with_input(
        &server_folder,
        &laptop,
        &["sync-signup", &address, "me", "--device", "laptop"],
        "long password\nlong password\n",
    );
    assert!(signed_up.status.success(), "{}", text(&signed_up));
    let synced = run(&server_folder, &laptop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    let synced = run(&server_folder, &desktop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    let devices = text(&run(&server_folder, &desktop, &["sync-devices"]));
    assert!(
        devices.contains("laptop") && devices.contains("desktop"),
        "{devices}"
    );

    // Set up again with another password: the desktop's kept key no longer logs in, and it
    // stops trying (so the name is not locked out for everyone).
    drop(server);
    fs::remove_dir_all(&server_folder).unwrap();
    fs::create_dir_all(&server_folder).unwrap();
    server = start_server(&server_folder, port);
    let signed_up = run_with_input(
        &server_folder,
        &laptop,
        &["sync-signup", &address, "me", "--device", "laptop"],
        "other password\nother password\n",
    );
    assert!(signed_up.status.success(), "{}", text(&signed_up));
    let refused = run(&server_folder, &desktop, &["sync-now"]);
    assert!(!refused.status.success());
    let config = fs::read_to_string(desktop.join("sync.json")).unwrap();
    assert!(!config.contains("\"account\""), "{config}");

    // While the settings are protected, the device cannot stop syncing.
    fs::write(
        desktop.join("settings_protection.json"),
        r#"{"protected_since":"2026-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let left = run(&server_folder, &desktop, &["sync-leave"]);
    assert!(!left.status.success());
    assert!(desktop.join("sync.json").exists());
    drop(server);
    let _ = fs::remove_dir_all(&root);
}

/// Sync `data_folder` now and return what the program printed.
fn sync_now(server_folder: &Path, data_folder: &Path) -> String {
    let synced = run(server_folder, data_folder, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    text(&synced)
}

/// An older administrator protection (any computer) is tied to the computers registered
/// then, and taking a computer off the list ends on every computer: both make it stricter,
/// so the server takes them while the strict mode is on.
#[test]
fn the_unlock_list_is_pinned_and_shrinks_on_every_computer() {
    use chrono::Utc;
    use machine::settings::{self, ProtectionChoice, Unlock};

    let root: PathBuf =
        std::env::temp_dir().join(format!("dw_sync_unlock_list_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let server_folder = root.join("server");
    let laptop = root.join("laptop");
    let desktop = root.join("desktop");
    for folder in [&server_folder, &laptop, &desktop] {
        fs::create_dir_all(folder).unwrap();
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let _server = start_server(&server_folder, port);
    let signed_up = run_with_input(
        &server_folder,
        &laptop,
        &["sync-signup", &address, "me", "--device", "laptop"],
        "long password\nlong password\n",
    );
    assert!(signed_up.status.success(), "{}", text(&signed_up));
    let logged_in = run_with_input(
        &server_folder,
        &desktop,
        &["sync-login", &address, "me", "--device", "desktop"],
        "long password\n",
    );
    assert!(logged_in.status.success(), "{}", text(&logged_in));
    let laptop_id = settings::this_device_id(&laptop).unwrap();
    let desktop_id = settings::this_device_id(&desktop).unwrap();

    // As an older version left it: any computer's administrator may end it.
    let now = Utc::now();
    settings::protect(
        &laptop,
        now,
        ProtectionChoice::Administrator {
            device: None,
            devices: Vec::new(),
        },
    )
    .unwrap();
    sync_now(&server_folder, &laptop);
    sync_now(&server_folder, &desktop);
    assert!(settings::is_protected(&desktop));

    // The service pins it to the computers registered now; the other computer gets the list.
    assert!(settings::pin_administrator_devices(
        &desktop,
        &[laptop_id.clone(), desktop_id.clone()]
    ));
    let printed = sync_now(&server_folder, &desktop);
    assert!(!printed.contains("refused"), "{printed}");
    sync_now(&server_folder, &laptop);
    let on_laptop = settings::protection(&laptop)
        .unwrap()
        .mode
        .administrator_devices();
    assert_eq!(on_laptop.len(), 2, "{on_laptop:?}");

    // The desktop is taken off the list there; the laptop gets that too, and the desktop's
    // administrator can no longer end it.
    settings::remove_administrator_device(&laptop, &desktop_id).unwrap();
    let printed = sync_now(&server_folder, &laptop);
    assert!(!printed.contains("refused"), "{printed}");
    sync_now(&server_folder, &desktop);
    let on_desktop = settings::protection(&desktop)
        .unwrap()
        .mode
        .administrator_devices();
    assert_eq!(on_desktop, [laptop_id.clone()]);
    assert!(settings::unprotect(
        &desktop,
        now,
        Unlock::Administrator {
            device: Some(desktop_id.clone())
        }
    )
    .is_err());

    // The laptop's administrator ends it everywhere.
    settings::unprotect(
        &laptop,
        now,
        Unlock::Administrator {
            device: Some(laptop_id.clone()),
        },
    )
    .unwrap();
    let printed = sync_now(&server_folder, &laptop);
    assert!(!printed.contains("refused"), "{printed}");
    sync_now(&server_folder, &desktop);
    assert!(!settings::is_protected(&desktop));
    let _ = fs::remove_dir_all(&root);
}

/// The way around the wait: a computer that is not on the list writes itself onto it (its
/// administrator can write any file there) and then ends the strict mode.
#[test]
fn a_computer_cannot_write_itself_onto_the_unlock_list_without_the_confirmations() {
    use chrono::Utc;
    use machine::settings::{self, ProtectionChoice};

    let root: PathBuf =
        std::env::temp_dir().join(format!("dw_sync_unlock_forged_{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let server_folder = root.join("server");
    let laptop = root.join("laptop");
    let new_pc = root.join("newpc");
    for folder in [&server_folder, &laptop, &new_pc] {
        fs::create_dir_all(folder).unwrap();
    }
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let address = format!("127.0.0.1:{port}");
    let _server = start_server(&server_folder, port);
    let signed_up = run_with_input(
        &server_folder,
        &laptop,
        &["sync-signup", &address, "me", "--device", "laptop"],
        "long password\nlong password\n",
    );
    assert!(signed_up.status.success(), "{}", text(&signed_up));
    let logged_in = run_with_input(
        &server_folder,
        &new_pc,
        &["sync-login", &address, "me", "--device", "newpc"],
        "long password\n",
    );
    assert!(logged_in.status.success(), "{}", text(&logged_in));
    let laptop_id = settings::this_device_id(&laptop).unwrap();
    let new_pc_id = settings::this_device_id(&new_pc).unwrap();
    settings::protect(
        &laptop,
        Utc::now(),
        ProtectionChoice::Administrator {
            device: None,
            devices: vec![laptop_id.clone()],
        },
    )
    .unwrap();
    sync_now(&server_folder, &laptop);
    sync_now(&server_folder, &new_pc);
    assert!(settings::is_protected(&new_pc));

    // Written by hand: still on, with the new PC on the list.
    let path = new_pc.join("settings_protection.json");
    let mut forged: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    forged["mode"]["devices"] = serde_json::json!([laptop_id, new_pc_id]);
    fs::write(&path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();
    let printed = sync_now(&server_folder, &new_pc);
    println!("{printed}");
    sync_now(&server_folder, &laptop);
    let on_laptop = settings::protection(&laptop)
        .unwrap()
        .mode
        .administrator_devices();
    assert!(
        !on_laptop.contains(&new_pc_id),
        "the forged list reached the laptop: {on_laptop:?}"
    );
    let _ = fs::remove_dir_all(&root);
}

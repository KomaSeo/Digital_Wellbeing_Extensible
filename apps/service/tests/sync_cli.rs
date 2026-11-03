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

fn run(server_folder: &Path, data_folder: &Path, arguments: &[&str]) -> Output {
    Command::new(PROGRAM)
        .args(arguments)
        .env("DW_SYNC_SERVER_DIR", server_folder)
        .env("DW_DATA_DIR", data_folder)
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

    // A login made on the server reaches the devices (with its password hashed).
    let mut adding = Command::new(PROGRAM)
        .args(["sync-add-user", "Me"])
        .env("DW_SYNC_SERVER_DIR", &server_folder)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        let mut input = adding.stdin.take().unwrap();
        input.write_all(b"secret pass\nsecret pass\n").unwrap();
    }
    let added = adding.wait_with_output().unwrap();
    assert!(added.status.success(), "{}", text(&added));
    let users = text(&run(&server_folder, &laptop, &["sync-users"]));
    assert!(users.contains("me"), "{users}");

    pair(&server_folder, &desktop, "desktop", port);
    let synced = run(&server_folder, &desktop, &["sync-now"]);
    assert!(synced.status.success(), "{}", text(&synced));
    let received = fs::read_to_string(desktop.join("planner.json")).unwrap();
    assert!(received.contains("from the laptop"), "{received}");
    let logins = fs::read_to_string(desktop.join("users.json")).unwrap();
    assert!(
        logins.contains("\"me\"") && !logins.contains("secret"),
        "{logins}"
    );

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

//! Joining with a login name and password: the first sign-up owns an empty server, other
//! devices log in with the same password, and wrong passwords are refused and slowed down.

use std::fs;
use std::path::PathBuf;
use sync::account::{AccountCredentials, JoinAnswer, Joiner};
use sync::client::{ClientConfig, SyncClient};
use sync::envelope::Envelope;
use sync::server::{SyncServer, MAX_FAILED_LOGINS};

fn temporary_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("dw_account_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn sign_up(
    server: &SyncServer,
    name: &str,
    password: &str,
    device: &str,
) -> Result<(JoinAnswer, AccountCredentials), String> {
    let public_key = server.public_key().unwrap();
    let mut transport = |sealed: &Envelope| server.handle(sealed);
    Joiner {
        transport: &mut transport,
    }
    .sign_up(&public_key, name, password, device)
}

fn log_in(
    server: &SyncServer,
    credentials: &AccountCredentials,
    device: &str,
    previous: Option<String>,
) -> Result<JoinAnswer, String> {
    let mut transport = |sealed: &Envelope| server.handle(sealed);
    Joiner {
        transport: &mut transport,
    }
    .log_in(credentials, device, previous)
}

fn client(folder: PathBuf, answer: &JoinAnswer) -> SyncClient {
    fs::create_dir_all(&folder).unwrap();
    SyncClient::new(
        folder,
        ClientConfig {
            server_url: "http://unused".into(),
            device_id: answer.device_id.clone(),
            key: answer.key.clone(),
            account: None,
        },
    )
}

#[test]
fn the_first_sign_up_owns_the_server_and_other_devices_log_in() {
    let root = temporary_folder("signup");
    let server = SyncServer::new(root.join("server"));

    // Nobody can log in before someone signed up.
    let credentials = AccountCredentials {
        name: "me".into(),
        key: sync::account::account_key("me", "long password").unwrap(),
    };
    let error = log_in(&server, &credentials, "Desktop", None).unwrap_err();
    assert!(error.contains("계정이 없습니다"), "{error}");
    // A short password is refused before anything is sent.
    assert!(sign_up(&server, "me", "short", "Laptop").is_err());

    let (laptop_answer, laptop_credentials) =
        sign_up(&server, "Me", "long password", "Laptop").unwrap();
    assert_eq!(laptop_answer.account, "me");
    assert_eq!(laptop_answer.device_id, "laptop");
    assert_eq!(laptop_credentials, credentials);
    assert_eq!(server.users(), vec!["me".to_string()]);
    // The account key never reaches the documents every device gets.
    let users = fs::read_to_string(root.join("server/documents/users.json")).unwrap();
    assert!(!users.contains(&credentials.key));

    // A second sign-up is refused, whoever sends it.
    let error = sign_up(&server, "other", "long password", "Stranger").unwrap_err();
    assert!(error.contains("이미 계정이 있습니다"), "{error}");

    // Another device logs in with the same password and syncs.
    let desktop_answer = log_in(&server, &credentials, "Desktop", None).unwrap();
    assert_eq!(desktop_answer.device_id, "desktop");
    let laptop_folder = root.join("laptop");
    let laptop = client(laptop_folder.clone(), &laptop_answer);
    fs::write(
        laptop_folder.join("planner.json"),
        r#"{ "life_rules": "shared" }"#,
    )
    .unwrap();
    laptop
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap();
    let desktop_folder = root.join("desktop");
    let desktop = client(desktop_folder.clone(), &desktop_answer);
    desktop
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap();
    assert!(fs::read_to_string(desktop_folder.join("planner.json"))
        .unwrap()
        .contains("shared"));
    assert!(desktop_folder.join("users.json").exists());

    // Logging in again from the same computer keeps its id and gives it a new key: the old
    // key no longer opens.
    let again = log_in(&server, &credentials, "Desktop", Some("desktop".into())).unwrap();
    assert_eq!(again.device_id, "desktop");
    assert_ne!(again.key, desktop_answer.key);
    let error = desktop
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap_err();
    assert!(!error.is_empty());
    let renewed = client(root.join("desktop2"), &again);
    renewed
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap();
    assert_eq!(server.devices().len(), 2);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn wrong_passwords_are_refused_and_then_the_name_waits() {
    let root = temporary_folder("wrong");
    let server = SyncServer::new(root.join("server"));
    sign_up(&server, "me", "long password", "Laptop").unwrap();
    let wrong = AccountCredentials {
        name: "me".into(),
        key: sync::account::account_key("me", "guessed password").unwrap(),
    };
    for _ in 0..MAX_FAILED_LOGINS {
        let error = log_in(&server, &wrong, "Thief", None).unwrap_err();
        assert!(error.contains("맞지 않습니다"), "{error}");
    }
    // Now even the right password waits, so guessing cannot go on.
    let right = AccountCredentials {
        name: "me".into(),
        key: sync::account::account_key("me", "long password").unwrap(),
    };
    let error = log_in(&server, &right, "Desktop", None).unwrap_err();
    assert!(error.contains("너무 많이"), "{error}");
    assert_eq!(server.devices().len(), 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_login_request_cannot_be_sent_again() {
    let root = temporary_folder("replay");
    let server = SyncServer::new(root.join("server"));
    let (_, credentials) = sign_up(&server, "me", "long password", "Laptop").unwrap();
    let mut captured: Option<Envelope> = None;
    let mut transport = |sealed: &Envelope| {
        captured = Some(sealed.clone());
        server.handle(sealed)
    };
    Joiner {
        transport: &mut transport,
    }
    .log_in(&credentials, "Desktop", None)
    .unwrap();
    let error = server.handle(&captured.unwrap()).unwrap_err();
    assert!(error.contains("already used"), "{error}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_password_set_on_the_server_also_logs_in() {
    let root = temporary_folder("server_password");
    let server = SyncServer::new(root.join("server"));
    server.set_user("me", "server password").unwrap();
    let credentials = AccountCredentials {
        name: "me".into(),
        key: sync::account::account_key("me", "server password").unwrap(),
    };
    log_in(&server, &credentials, "Laptop", None).unwrap();
    // A new password: the old key no longer logs in.
    server.set_user("me", "newer password").unwrap();
    let error = log_in(&server, &credentials, "Laptop", Some("laptop".into())).unwrap_err();
    assert!(error.contains("맞지 않습니다"), "{error}");
    let _ = fs::remove_dir_all(&root);
}

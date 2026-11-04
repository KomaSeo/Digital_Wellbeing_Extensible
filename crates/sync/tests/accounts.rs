//! Joining with a login name and password (OPAQUE): the first sign-up owns an empty server,
//! other devices log in with the same password, the server keeps nothing a password could be
//! read from, and guessing is cut off.

use std::fs;
use std::path::PathBuf;
use sync::account::{AccountRequest, Joined, Joiner, WRONG_LOGIN};
use sync::client::{ClientConfig, SyncClient};
use sync::envelope::Envelope;
use sync::server::{SyncServer, MAX_UNFINISHED_LOGINS};

const PASSWORD: &str = "correct horse battery";

fn temporary_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("dw_account_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn joiner_call<T>(
    server: &SyncServer,
    action: impl FnOnce(&mut Joiner) -> Result<T, String>,
) -> Result<T, String> {
    let mut transport = |request: &AccountRequest| server.handle_account(request);
    action(&mut Joiner {
        transport: &mut transport,
    })
}

fn sign_up(
    server: &SyncServer,
    name: &str,
    password: &str,
    device: &str,
) -> Result<Joined, String> {
    joiner_call(server, |joiner| joiner.sign_up(name, password, device))
}

fn log_in(
    server: &SyncServer,
    name: &str,
    password: &str,
    device: &str,
    previous: Option<String>,
) -> Result<Joined, String> {
    joiner_call(server, |joiner| {
        joiner.log_in(name, password, device, previous)
    })
}

fn client(folder: PathBuf, joined: &Joined) -> SyncClient {
    fs::create_dir_all(&folder).unwrap();
    SyncClient::new(
        folder,
        ClientConfig {
            server_url: "http://unused".into(),
            device_id: joined.answer.device_id.clone(),
            key: joined.answer.key.clone(),
            account: None,
        },
    )
}

fn server_files_text(root: &std::path::Path) -> String {
    let mut text = String::new();
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(folder).unwrap().flatten() {
            if entry.path().is_dir() {
                folders.push(entry.path());
            } else {
                text.push_str(&String::from_utf8_lossy(&fs::read(entry.path()).unwrap()));
            }
        }
    }
    text
}

#[test]
fn the_first_sign_up_owns_the_server_and_other_devices_log_in() {
    let root = temporary_folder("signup");
    let server = SyncServer::new(root.join("server"));

    // Nobody can log in before someone signed up.
    let error = log_in(&server, "me", PASSWORD, "Desktop", None).unwrap_err();
    assert!(error.contains("계정이 없습니다"), "{error}");
    // A short password is refused before anything is sent.
    assert!(sign_up(&server, "me", "short", "Laptop").is_err());

    let laptop = sign_up(&server, "Me", PASSWORD, "Laptop").unwrap();
    assert_eq!(laptop.answer.account, "me");
    assert_eq!(laptop.answer.device_id, "laptop");
    assert_eq!(server.users(), vec!["me".to_string()]);
    // Neither the password nor anything made only from it is on the server.
    let stored = server_files_text(&root.join("server"));
    assert!(!stored.contains(PASSWORD));
    assert!(!stored.contains(&laptop.vault_key));

    // A second sign-up is refused, whoever sends it.
    let error = sign_up(&server, "other", PASSWORD, "Stranger").unwrap_err();
    assert!(error.contains("이미 계정이 있습니다"), "{error}");

    // Another device logs in with the same password, gets the same vault key, and syncs.
    let desktop = log_in(&server, "me", PASSWORD, "Desktop", None).unwrap();
    assert_eq!(desktop.answer.device_id, "desktop");
    assert_eq!(desktop.vault_key, laptop.vault_key);
    let laptop_folder = root.join("laptop");
    let laptop_client = client(laptop_folder.clone(), &laptop);
    fs::write(
        laptop_folder.join("planner.json"),
        r#"{ "life_rules": "shared" }"#,
    )
    .unwrap();
    laptop_client
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap();
    let desktop_folder = root.join("desktop");
    let desktop_client = client(desktop_folder.clone(), &desktop);
    desktop_client
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap();
    assert!(fs::read_to_string(desktop_folder.join("planner.json"))
        .unwrap()
        .contains("shared"));
    assert!(desktop_folder.join("users.json").exists());

    // Logging in again from the same computer keeps its id and gives it a new key.
    let again = log_in(&server, "me", PASSWORD, "Desktop", Some("desktop".into())).unwrap();
    assert_eq!(again.answer.device_id, "desktop");
    assert_ne!(again.answer.key, desktop.answer.key);
    assert!(desktop_client
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .is_err());
    assert_eq!(server.devices().len(), 2);

    // A device taken out may not come back by itself under its id.
    server.remove_device("desktop").unwrap();
    let error = log_in(&server, "me", PASSWORD, "Desktop", Some("desktop".into())).unwrap_err();
    assert!(error.contains("빠졌습니다"), "{error}");
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn wrong_passwords_are_refused_and_then_the_name_waits() {
    let root = temporary_folder("wrong");
    let server = SyncServer::new(root.join("server"));
    sign_up(&server, "me", PASSWORD, "Laptop").unwrap();
    // An unknown name looks the same as a wrong password.
    assert_eq!(
        log_in(&server, "nobody", PASSWORD, "Thief", None).unwrap_err(),
        WRONG_LOGIN
    );
    for _ in 0..MAX_UNFINISHED_LOGINS {
        assert_eq!(
            log_in(&server, "me", "guessed password", "Thief", None).unwrap_err(),
            WRONG_LOGIN
        );
    }
    // Now even the right password waits, so guessing cannot go on.
    let error = log_in(&server, "me", PASSWORD, "Desktop", None).unwrap_err();
    assert!(error.contains("너무 많이"), "{error}");
    assert_eq!(server.devices().len(), 1);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_login_cannot_be_finished_twice_or_by_someone_else() {
    let root = temporary_folder("replay");
    let server = SyncServer::new(root.join("server"));
    sign_up(&server, "me", PASSWORD, "Laptop").unwrap();
    let mut finish: Option<AccountRequest> = None;
    let mut transport = |request: &AccountRequest| {
        if matches!(request, AccountRequest::LoginFinish { .. }) {
            finish = Some(request.clone());
        }
        server.handle_account(request)
    };
    Joiner {
        transport: &mut transport,
    }
    .log_in("me", PASSWORD, "Desktop", None)
    .unwrap();
    // The same last step again: the login is over.
    let error = server.handle_account(&finish.unwrap()).unwrap_err();
    assert!(error.contains("log in again"), "{error}");
    // A last step for a login that was never started.
    let error = server
        .handle_account(&AccountRequest::LoginFinish {
            login_id: "00".into(),
            finalization: "AAAA".into(),
            sealed: sync::envelope::seal("login:00", &sync::envelope::new_key().unwrap(), b"x")
                .unwrap(),
        })
        .unwrap_err();
    assert!(error.contains("log in again"), "{error}");
    // A sign-up on a server that has an account.
    assert!(server
        .handle_account(&AccountRequest::SignupStart {
            name: "x".into(),
            request: "AAAA".into()
        })
        .is_err());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_new_password_after_the_server_is_set_up_again() {
    let root = temporary_folder("again");
    let server = SyncServer::new(root.join("server"));
    let first = sign_up(&server, "me", PASSWORD, "Laptop").unwrap();
    // Set up again from nothing: the same password gives a new vault key (the server's OPRF
    // key is new), and the old password no longer works once another one is chosen.
    let _ = fs::remove_dir_all(root.join("server"));
    let server = SyncServer::new(root.join("server"));
    let second = sign_up(&server, "me", PASSWORD, "Laptop").unwrap();
    assert_ne!(first.vault_key, second.vault_key);
    let _ = fs::remove_dir_all(root.join("server"));
    let server = SyncServer::new(root.join("server"));
    sign_up(&server, "me", "another long password", "Laptop").unwrap();
    assert_eq!(
        log_in(&server, "me", PASSWORD, "Desktop", None).unwrap_err(),
        WRONG_LOGIN
    );
    let _ = fs::remove_dir_all(&root);
}

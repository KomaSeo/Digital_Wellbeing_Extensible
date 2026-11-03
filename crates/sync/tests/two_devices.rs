//! Two devices and one server, all in temporary folders: what one device changes reaches the
//! other, changes on both sides are merged, and the server's rules hold.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use sync::client::{ClientConfig, SyncClient, SyncState};
use sync::envelope::{self, Envelope};
use sync::protocol::{Push, SyncRequest, SyncResponse};
use sync::server::SyncServer;

fn temporary_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("dw_sync_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

fn write(folder: &Path, name: &str, value: &Value) {
    let path = folder.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

fn read(folder: &Path, name: &str) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(folder.join(name)).ok()?).ok()
}

struct Setup {
    server: Arc<SyncServer>,
    laptop: (PathBuf, SyncClient),
    desktop: (PathBuf, SyncClient),
}

/// The same as [`setup`], for a test whose `setup` variable hides that function.
fn setup_named(name: &str) -> Setup {
    setup(name)
}

fn setup(name: &str) -> Setup {
    let root = temporary_folder(name);
    let server = Arc::new(SyncServer::new(root.join("server")));
    let device = |device_name: &str| {
        let code = server.add_device(device_name).unwrap();
        let folder = root.join(device_name);
        fs::create_dir_all(&folder).unwrap();
        let config = ClientConfig {
            server_url: "http://unused".into(),
            device_id: code.device_id,
            key: code.key,
            account: None,
        };
        (folder.clone(), SyncClient::new(folder, config))
    };
    let laptop = device("Laptop");
    let desktop = device("Desktop");
    Setup {
        server,
        laptop,
        desktop,
    }
}

fn sync(server: &SyncServer, client: &SyncClient) -> sync::client::SyncReport {
    client
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap()
}

#[test]
fn the_first_device_fills_the_server_and_a_joining_device_takes_its_state() {
    let setup = setup("join");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    write(
        laptop_folder,
        "planner.json",
        &json!({ "life_rules": "sleep at 23" }),
    );
    write(
        laptop_folder,
        "planner_state.json",
        &json!({ "assignment": { "todo_id": "a" } }),
    );
    write(
        desktop_folder,
        "planner.json",
        &json!({ "life_rules": "old desktop rules" }),
    );
    write(desktop_folder, "grants/me.json", &json!({ "grants": [] }));

    let report = sync(&setup.server, laptop);
    assert!(report.pushed.contains(&"planner.json".to_string()));
    assert!(report.pushed.contains(&"planner_state.json".to_string()));

    sync(&setup.server, desktop);
    // The joining device's own settings are replaced by the shared ones.
    assert_eq!(
        read(desktop_folder, "planner.json").unwrap(),
        json!({ "life_rules": "sleep at 23" })
    );
    assert_eq!(
        read(desktop_folder, "planner_state.json").unwrap(),
        json!({ "assignment": { "todo_id": "a" } })
    );
    // What the server did not have yet is sent, and reaches the first device.
    sync(&setup.server, laptop);
    assert_eq!(
        read(laptop_folder, "grants/me.json").unwrap(),
        json!({ "grants": [] })
    );
    // Nothing left to do: another sync changes nothing.
    let quiet = sync(&setup.server, laptop);
    assert!(quiet.pushed.is_empty() && quiet.pulled.is_empty());
}

#[test]
fn changes_on_both_devices_are_merged() {
    let setup = setup("merge");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    write(
        laptop_folder,
        "planner.json",
        &json!({ "todos": [{ "id": "a", "title": "A" }], "life_rules": "x" }),
    );
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);

    // Both change the same file while apart.
    write(
        laptop_folder,
        "planner.json",
        &json!({ "todos": [{ "id": "a", "title": "A" }, { "id": "b", "title": "B" }], "life_rules": "x" }),
    );
    write(
        desktop_folder,
        "planner.json",
        &json!({ "todos": [{ "id": "a", "title": "A edited" }], "life_rules": "y" }),
    );
    sync(&setup.server, laptop);
    let report = sync(&setup.server, desktop);
    assert!(report.merged.contains(&"planner.json".to_string()));
    sync(&setup.server, laptop);

    let expected = json!({
        "todos": [{ "id": "a", "title": "A edited" }, { "id": "b", "title": "B" }],
        "life_rules": "y"
    });
    assert_eq!(read(laptop_folder, "planner.json").unwrap(), expected);
    assert_eq!(read(desktop_folder, "planner.json").unwrap(), expected);
}

#[test]
fn two_devices_assigning_at_once_end_with_one_whole_assignment() {
    let setup = setup("assignment");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    write(
        laptop_folder,
        "planner_state.json",
        &json!({ "assignment": null, "done": [] }),
    );
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);

    write(
        laptop_folder,
        "planner_state.json",
        &json!({ "assignment": { "todo_id": "a", "until": "10:00" }, "done": [{ "todo_id": "x" }] }),
    );
    write(
        desktop_folder,
        "planner_state.json",
        &json!({ "assignment": { "todo_id": "b", "until": "11:00" }, "done": [{ "todo_id": "y" }] }),
    );
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    sync(&setup.server, laptop);

    let laptop_state = read(laptop_folder, "planner_state.json").unwrap();
    let desktop_state = read(desktop_folder, "planner_state.json").unwrap();
    assert_eq!(laptop_state, desktop_state);
    // The first to reach the server wins the whole assignment; both done records are kept.
    assert_eq!(
        laptop_state["assignment"],
        json!({ "todo_id": "a", "until": "10:00" })
    );
    assert_eq!(
        laptop_state["done"],
        json!([{ "todo_id": "x" }, { "todo_id": "y" }])
    );
}

#[test]
fn every_device_sees_every_devices_usage_but_sends_only_its_own() {
    let setup = setup("usage");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    write(laptop_folder, "usage/me.json", &json!({ "calls": 1 }));
    write(desktop_folder, "usage/me.json", &json!({ "calls": 2 }));
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    sync(&setup.server, laptop);

    assert_eq!(
        read(laptop_folder, "usage/me.json").unwrap(),
        json!({ "calls": 1 })
    );
    assert_eq!(
        read(laptop_folder, "usage/desktop~me.json").unwrap(),
        json!({ "calls": 2 })
    );
    assert_eq!(
        read(desktop_folder, "usage/me.json").unwrap(),
        json!({ "calls": 2 })
    );
    assert_eq!(
        read(desktop_folder, "usage/laptop~me.json").unwrap(),
        json!({ "calls": 1 })
    );

    // A later count on one device reaches the other.
    write(laptop_folder, "usage/me.json", &json!({ "calls": 5 }));
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    assert_eq!(
        read(desktop_folder, "usage/laptop~me.json").unwrap(),
        json!({ "calls": 5 })
    );
    assert_eq!(
        read(desktop_folder, "usage/me.json").unwrap(),
        json!({ "calls": 2 })
    );
}

#[test]
fn protection_cannot_be_turned_off_through_sync() {
    let setup = setup("protection");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    let protected = json!({ "protected_since": "2026-11-01T00:00:00Z" });
    write(laptop_folder, "settings_protection.json", &protected);
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    assert_eq!(
        read(desktop_folder, "settings_protection.json").unwrap(),
        protected
    );

    write(
        desktop_folder,
        "settings_protection.json",
        &json!({ "protected_since": null }),
    );
    let report = sync(&setup.server, desktop);
    assert_eq!(report.refused.len(), 1);
    // The device takes the server's (protected) version back.
    assert_eq!(
        read(desktop_folder, "settings_protection.json").unwrap(),
        protected
    );
    assert_eq!(SyncState::load(desktop_folder).refused.len(), 1);
}

fn send_raw(
    server: &SyncServer,
    device_id: &str,
    key: &str,
    request: &SyncRequest,
) -> Result<SyncResponse, String> {
    let sealed = envelope::seal(device_id, key, &serde_json::to_vec(request).unwrap()).unwrap();
    let answer = server.handle(&sealed)?;
    Ok(serde_json::from_slice(&envelope::open(&answer, key).unwrap()).unwrap())
}

#[test]
fn the_server_refuses_strangers_replays_old_requests_and_foreign_files() {
    let root = temporary_folder("rules");
    let server = SyncServer::new(root.join("server"));
    let code = server.add_device("laptop").unwrap();
    let request = |counter: u64, pushes: Vec<Push>| SyncRequest {
        sent_at: chrono::Utc::now(),
        counter,
        pushes,
        known: Default::default(),
        app_version: sync::protocol::app_version().to_string(),
    };

    // A wrong key, or an unknown device.
    let stranger_key = envelope::new_key().unwrap();
    assert!(send_raw(&server, &code.device_id, &stranger_key, &request(1, vec![])).is_err());
    assert!(send_raw(&server, "nobody", &code.key, &request(1, vec![])).is_err());

    // The same sealed request twice.
    let sealed = envelope::seal(
        &code.device_id,
        &code.key,
        &serde_json::to_vec(&request(1, vec![])).unwrap(),
    )
    .unwrap();
    assert!(server.handle(&sealed).is_ok());
    assert!(server.handle(&sealed).is_err());

    // An old request.
    let mut old = request(2, vec![]);
    old.sent_at = chrono::Utc::now() - chrono::Duration::hours(1);
    assert!(send_raw(&server, &code.device_id, &code.key, &old).is_err());

    // Another device's usage, a file that is not shared, and a path outside the folder.
    let push = |name: &str| Push {
        name: name.into(),
        base_revision: 0,
        content: json!({}),
    };
    let response = send_raw(
        &server,
        &code.device_id,
        &code.key,
        &request(
            3,
            vec![
                push("usage/desktop/me.json"),
                push("machine.json"),
                push("grants/../policy.json"),
                push("usage/laptop/me.json"),
            ],
        ),
    )
    .unwrap();
    assert_eq!(response.rejected.len(), 3);
    assert_eq!(response.documents.len(), 1);
    assert_eq!(response.documents[0].name, "usage/laptop/me.json");

    // A removed device can no longer sync.
    server.remove_device(&code.device_id).unwrap();
    assert!(send_raw(&server, &code.device_id, &code.key, &request(4, vec![])).is_err());
}

#[test]
fn a_sync_round_works_over_http() {
    let root = temporary_folder("http");
    let server = Arc::new(SyncServer::new(root.join("server")));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let serving = Arc::clone(&server);
    std::thread::spawn(move || serving.serve(port));
    std::thread::sleep(std::time::Duration::from_millis(200));

    let code = server.add_device("laptop").unwrap();
    let folder = root.join("laptop");
    write(
        &folder,
        "planner.json",
        &json!({ "life_rules": "over http" }),
    );
    let config = ClientConfig {
        server_url: format!("http://127.0.0.1:{port}"),
        device_id: code.device_id,
        key: code.key,
        account: None,
    };
    let client = SyncClient::new(folder, config.clone());
    let report = client
        .sync_once(&mut sync::client::http_transport(&config.server_url))
        .unwrap();
    assert_eq!(report.pushed, vec!["planner.json".to_string()]);

    // A request line with no end is cut off, not read forever.
    {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let _ = stream.write_all(&vec![b'a'; 64 * 1024]);
        let mut answer = Vec::new();
        let _ = stream.read_to_end(&mut answer);
        assert!(answer.len() < 1024, "{}", String::from_utf8_lossy(&answer));
    }
    assert_eq!(server.port(), Some(port));

    // A wrong key is refused with the server's reason.
    let stranger = SyncClient::new(
        root.join("stranger"),
        ClientConfig {
            key: envelope::new_key().unwrap(),
            ..config.clone()
        },
    );
    let error = stranger
        .sync_once(&mut sync::client::http_transport(&config.server_url))
        .unwrap_err();
    assert!(error.contains("refused"), "{error}");
}

#[test]
fn each_device_keeps_its_own_api_key() {
    let setup = setup("api_key");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    write(
        laptop_folder,
        "ai_focus.json",
        &json!({ "api_key": "sk-laptop", "model": "a" }),
    );
    write(
        desktop_folder,
        "ai_focus.json",
        &json!({ "api_key": "sk-desktop", "model": "b" }),
    );
    sync(&setup.server, laptop);
    // Joining takes the server's settings, but not another device's key.
    sync(&setup.server, desktop);
    assert_eq!(
        read(desktop_folder, "ai_focus.json").unwrap(),
        json!({ "api_key": "sk-desktop", "model": "a" })
    );

    // A changed key is never sent; a changed model is.
    write(
        desktop_folder,
        "ai_focus.json",
        &json!({ "api_key": "sk-desktop-2", "model": "c" }),
    );
    sync(&setup.server, desktop);
    sync(&setup.server, laptop);
    assert_eq!(
        read(laptop_folder, "ai_focus.json").unwrap(),
        json!({ "api_key": "sk-laptop", "model": "c" })
    );
    let stored = fs::read_to_string(
        laptop_folder
            .parent()
            .unwrap()
            .join("server/documents/ai_focus.json"),
    )
    .unwrap();
    assert!(!stored.contains("sk-"), "{stored}");
}

#[test]
fn a_device_with_an_older_version_stops_syncing_until_it_is_updated() {
    let root = temporary_folder("versions");
    let server = SyncServer::new(root.join("server"));
    let newer = server.add_device("laptop").unwrap();
    let older = server.add_device("desktop").unwrap();
    let request = |counter: u64, version: &str| SyncRequest {
        sent_at: chrono::Utc::now(),
        counter,
        pushes: vec![],
        known: Default::default(),
        app_version: version.to_string(),
    };
    send_raw(
        &server,
        &older.device_id,
        &older.key,
        &request(1, "0.1.0-rc.30"),
    )
    .unwrap();
    // The laptop is updated first.
    let answer = send_raw(
        &server,
        &newer.device_id,
        &newer.key,
        &request(1, "0.1.0-rc.31"),
    )
    .unwrap();
    assert_eq!(
        answer.outdated_devices,
        vec![("desktop".to_string(), "0.1.0-rc.30".to_string())]
    );
    let refused = send_raw(
        &server,
        &older.device_id,
        &older.key,
        &request(2, "0.1.0-rc.30"),
    )
    .unwrap_err();
    assert!(refused.contains("0.1.0-rc.31"), "{refused}");
    // Updated: it syncs again, and nobody is behind.
    let answer = send_raw(
        &server,
        &older.device_id,
        &older.key,
        &request(3, "0.1.0-rc.31"),
    )
    .unwrap();
    assert!(answer.outdated_devices.is_empty());
}

#[test]
fn an_old_answer_sent_again_is_refused_and_protection_stays() {
    let setup = setup("replay");
    let (laptop_folder, laptop) = &setup.laptop;
    // The server is unprotected at first; the laptop records that answer.
    write(
        laptop_folder,
        "settings_protection.json",
        &json!({ "protected_since": null }),
    );
    let mut recorded: Option<Envelope> = None;
    laptop
        .sync_once(&mut |sealed: &Envelope| {
            let answer = setup.server.handle(sealed)?;
            recorded.get_or_insert(answer.clone());
            Ok(answer)
        })
        .unwrap();
    // Protection goes on and is synced.
    let protected = json!({ "protected_since": "2026-11-01T00:00:00Z" });
    write(laptop_folder, "settings_protection.json", &protected);
    sync(&setup.server, laptop);

    // Someone sends the old answer back in place of the server's.
    let old = recorded.unwrap();
    let error = laptop
        .sync_once(&mut |_sealed: &Envelope| Ok(old.clone()))
        .unwrap_err();
    assert!(error.contains("does not belong"), "{error}");
    assert_eq!(
        read(laptop_folder, "settings_protection.json").unwrap(),
        protected
    );
}

#[test]
fn a_protection_ends_on_every_device_only_the_way_it_allows() {
    let setup = setup("protection_modes");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    let off = |mode: Value| json!({ "protected_since": null, "mode": mode });
    let on = |mode: Value| json!({ "protected_since": "2026-11-01T00:00:00Z", "mode": mode });

    // A password protection ended on the laptop (its service checked the password) ends on
    // the desktop too.
    let password =
        json!({ "kind": "password", "password": { "salt": "", "hash": "", "rounds": 1 } });
    write(
        laptop_folder,
        "settings_protection.json",
        &on(password.clone()),
    );
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    write(
        laptop_folder,
        "settings_protection.json",
        &off(password.clone()),
    );
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    assert_eq!(
        read(desktop_folder, "settings_protection.json").unwrap(),
        off(password)
    );

    // A time protection does not end before its time: the server refuses, and the laptop
    // takes the protected version back.
    let later = json!({ "kind": "until", "until": "2099-01-01T00:00:00Z" });
    write(
        laptop_folder,
        "settings_protection.json",
        &on(later.clone()),
    );
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    write(
        laptop_folder,
        "settings_protection.json",
        &off(later.clone()),
    );
    let report = sync(&setup.server, laptop);
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert_eq!(
        read(laptop_folder, "settings_protection.json").unwrap(),
        on(later.clone())
    );
    assert_eq!(
        read(desktop_folder, "settings_protection.json").unwrap(),
        on(later)
    );

    // How it ends cannot be changed while it is on.
    let permanent = json!({ "kind": "permanent" });
    write(laptop_folder, "settings_protection.json", &on(permanent));
    let report = sync(&setup.server, laptop);
    assert_eq!(report.refused.len(), 1, "{report:?}");

    // One whose time is over may end.
    let setup = setup_named("protection_over");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    let over = json!({ "kind": "until", "until": "2020-01-01T00:00:00Z" });
    write(laptop_folder, "settings_protection.json", &on(over.clone()));
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    write(
        laptop_folder,
        "settings_protection.json",
        &off(over.clone()),
    );
    let report = sync(&setup.server, laptop);
    assert!(report.refused.is_empty(), "{report:?}");
    sync(&setup.server, desktop);
    assert_eq!(
        read(desktop_folder, "settings_protection.json").unwrap(),
        off(over)
    );
}

#[test]
fn files_with_dots_in_their_names_are_synced() {
    let setup = setup("dots");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    write(
        laptop_folder,
        "grants/john.doe.json",
        &json!({ "grants": [1] }),
    );
    // Temporary and corrupt files are not.
    write(laptop_folder, "grants/john.doe.json.123.4.tmp", &json!({}));
    write(laptop_folder, "grants/john.doe.json.corrupt", &json!({}));
    sync(&setup.server, laptop);
    sync(&setup.server, desktop);
    assert_eq!(
        read(desktop_folder, "grants/john.doe.json").unwrap(),
        json!({ "grants": [1] })
    );
    assert!(!desktop_folder.join("grants/john.doe.json.corrupt").exists());
}

#[test]
fn only_one_sync_of_a_device_runs_at_a_time() {
    let setup = setup("lock");
    let (laptop_folder, laptop) = &setup.laptop;
    fs::write(laptop_folder.join("sync.lock"), "").unwrap();
    let error = laptop
        .sync_once(&mut |sealed: &Envelope| setup.server.handle(sealed))
        .unwrap_err();
    assert!(error.contains("another sync"), "{error}");
    fs::remove_file(laptop_folder.join("sync.lock")).unwrap();
    sync(&setup.server, laptop);
    assert!(!laptop_folder.join("sync.lock").exists());
}

#[test]
fn an_administrator_protection_tied_to_one_computer_ends_only_from_there() {
    let setup = setup("protection_device");
    let (laptop_folder, laptop) = &setup.laptop;
    let (desktop_folder, desktop) = &setup.desktop;
    // Only the desktop's administrator may end it.
    let mode = json!({ "kind": "administrator", "device": "desktop" });
    let on = json!({ "protected_since": "2026-11-01T00:00:00Z", "mode": mode });
    let off = json!({ "protected_since": null, "mode": mode });
    write(laptop_folder, "settings_protection.json", &on);
    sync(&setup.server, laptop);
    let report = sync(&setup.server, desktop);
    // Every device learns the paired devices (for choosing whose administrator).
    assert_eq!(
        report.devices,
        vec![
            ("laptop".to_string(), "Laptop".to_string()),
            ("desktop".to_string(), "Desktop".to_string())
        ]
    );

    // From the laptop: refused, and the laptop is protected again.
    write(laptop_folder, "settings_protection.json", &off);
    let report = sync(&setup.server, laptop);
    assert_eq!(report.refused.len(), 1, "{report:?}");
    assert_eq!(read(laptop_folder, "settings_protection.json").unwrap(), on);

    // From the desktop: taken, and it reaches the laptop.
    write(desktop_folder, "settings_protection.json", &off);
    let report = sync(&setup.server, desktop);
    assert!(report.refused.is_empty(), "{report:?}");
    sync(&setup.server, laptop);
    assert_eq!(
        read(laptop_folder, "settings_protection.json").unwrap(),
        off
    );
}

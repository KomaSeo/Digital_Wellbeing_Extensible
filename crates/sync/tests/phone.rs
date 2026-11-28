//! A phone, a server and two PCs in temporary folders: the phone joins with a code one PC made,
//! sees the to-do list, and leaves changes that exactly one PC with a running agent handles.

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use sync::client::{ClientConfig, SyncClient};
use sync::envelope::Envelope;
use sync::phone::{self, PhoneOutcome};
use sync::server::SyncServer;

fn temporary_folder(name: &str) -> PathBuf {
    let folder = std::env::temp_dir().join(format!("dw_phone_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    folder
}

/// A PC's folder and its sync client, ready or not to take phone requests.
fn pc(server: &SyncServer, root: &Path, name: &str) -> (PathBuf, ClientConfig) {
    let code = server.add_device(name).unwrap();
    let folder = root.join(name);
    fs::create_dir_all(&folder).unwrap();
    (
        folder,
        ClientConfig {
            server_url: "http://unused".into(),
            device_id: code.device_id,
            key: code.key,
            account: None,
        },
    )
}

fn sync(server: &SyncServer, folder: &Path, config: &ClientConfig, ready: bool) {
    SyncClient::new(folder.to_path_buf(), config.clone())
        .phone_ready(ready)
        .sync_once(&mut |sealed: &Envelope| server.handle(sealed))
        .unwrap();
}

fn server_with_view(root: &Path) -> SyncServer {
    SyncServer::new(root.join("server")).with_phone_view(std::sync::Arc::new(
        |planner: &Value, _state: &Value| json!({ "todos": planner["todos"].clone() }),
    ))
}

#[test]
fn a_phone_joins_once_with_a_pc_code_and_one_ready_pc_handles_its_requests() {
    let root = temporary_folder("flow");
    let server = server_with_view(&root);
    let (desktop, desktop_config) = pc(&server, &root, "Desktop");
    let (laptop, laptop_config) = pc(&server, &root, "Laptop");
    fs::write(
        desktop.join("planner.json"),
        json!({ "todos": [{ "id": "milk", "title": "Buy milk" }] }).to_string(),
    )
    .unwrap();

    // A code is good only once it reached the server, and only once.
    let code = phone::new_code(&desktop, chrono::Utc::now()).unwrap();
    let pair = |code: &str| {
        server.phone_api(&json!({ "action": "pair", "code": code, "name": "My phone" }))
    };
    assert!(pair(&code)["error"].is_string());
    sync(&server, &desktop, &desktop_config, false);
    let joined = pair(&code.to_lowercase().replace('-', " "));
    let token = joined["token"].as_str().expect("joined").to_string();
    assert!(pair(&code)["error"].is_string());
    assert!(
        server.phone_api(&json!({ "action": "list", "token": "wrong" }))["unpaired"]
            .as_bool()
            .unwrap()
    );

    // The list comes from the stored planner file; no PC is ready yet.
    let list = server.phone_api(&json!({ "action": "list", "token": token }));
    assert_eq!(list["list"]["todos"][0]["title"], "Buy milk");
    assert_eq!(list["pc_ready"], false);
    assert_eq!(list["phone"]["name"], "My phone");

    // Only to-do changes are taken.
    let refused = server.phone_api(&json!({
        "action": "request", "token": token, "change": { "kind": "split", "todo_id": "milk" },
    }));
    assert!(refused["error"].is_string());
    let sent = server.phone_api(&json!({
        "action": "request", "token": token,
        "change": { "kind": "add", "todo": { "title": "Call the bank" } }, "reason": "before Friday",
    }));
    let request_id = sent["id"].as_str().unwrap().to_string();
    let status = |token: &str| {
        server.phone_api(&json!({ "action": "list", "token": token }))["requests"][0]["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(status(&token), "waiting");

    // A PC without an agent takes nothing; the first ready PC gets it, the other does not.
    sync(&server, &laptop, &laptop_config, false);
    assert!(phone::requests_for(&laptop, &laptop_config.device_id).is_empty());
    sync(&server, &desktop, &desktop_config, true);
    sync(&server, &laptop, &laptop_config, true);
    let mine = phone::requests_for(&desktop, &desktop_config.device_id);
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].reason, "before Friday");
    assert!(phone::requests_for(&laptop, &laptop_config.device_id).is_empty());
    assert_eq!(status(&token), "handling");
    assert_eq!(
        server.phone_api(&json!({ "action": "list", "token": token }))["pc_ready"],
        true
    );

    // The desktop's agent decided: the outcome goes up with the next round and is dropped here.
    phone::save_outcome(
        &desktop,
        PhoneOutcome {
            request_id: request_id.clone(),
            at: chrono::Utc::now(),
            approved: true,
            reply: "Fine.".into(),
        },
    )
    .unwrap();
    assert!(phone::requests_for(&desktop, &desktop_config.device_id).is_empty());
    sync(&server, &desktop, &desktop_config, true);
    assert!(phone::pending_outcomes(&desktop).is_empty());
    let list = server.phone_api(&json!({ "action": "list", "token": token }));
    assert_eq!(list["requests"][0]["status"], "done");
    assert_eq!(list["requests"][0]["outcome"]["reply"], "Fine.");

    // A PC takes the phone off: its token stops working and the PCs no longer list it.
    let phone_id = joined["phone"]["id"].as_str().unwrap();
    phone::remove_phone(&laptop, phone_id).unwrap();
    sync(&server, &laptop, &laptop_config, true);
    assert!(
        server.phone_api(&json!({ "action": "list", "token": token }))["unpaired"]
            .as_bool()
            .unwrap()
    );
    sync(&server, &desktop, &desktop_config, true);
    assert!(phone::phones(&desktop).phones.is_empty());
}

#[test]
fn wrong_codes_make_the_server_wait() {
    let root = temporary_folder("guess");
    let server = server_with_view(&root);
    for _ in 0..10 {
        let answer = server.phone_api(&json!({ "action": "pair", "code": "AAAA-AAAA" }));
        assert!(answer["error"].as_str().unwrap().contains("맞지 않거나"));
    }
    let answer = server.phone_api(&json!({ "action": "pair", "code": "AAAA-AAAA" }));
    assert!(answer["error"].as_str().unwrap().contains("15분"));
}

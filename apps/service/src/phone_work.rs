//! Responsibility: Hand the phone requests the sync server gave this PC to an agent of this PC,
//! which reviews and applies each one like a to-do change from the Focus app (`todo_change`,
//! via "phone": in strict mode the AI decides), and keep each outcome until the next sync
//! round takes it to the server (`sync::phone`).

use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{info, warn};

/// The source the agent records for these changes (the Focus app's log shows "휴대폰").
pub const VIA: &str = "phone";

/// The sessions whose agent may run on this PC.
#[cfg(windows)]
fn candidate_sessions() -> Vec<u32> {
    crate::spawn::user_sessions()
        .into_iter()
        .map(|session| session.id)
        .collect()
}

#[cfg(not(windows))]
fn candidate_sessions() -> Vec<u32> {
    vec![machine::session::current_session_id()]
}

/// A session whose agent answers now: this PC can take phone requests.
pub fn agent_session() -> Option<u32> {
    candidate_sessions()
        .into_iter()
        .find(|session| agent_api::user_api::call_value(*session, "last_block", json!({})).is_ok())
}

/// Review one change with the agent of `session`: whether it was applied, and the AI's answer
/// (or why it was not applied).
pub fn review_with_agent(
    session: u32,
    change: &Value,
    reason: &str,
) -> Result<(bool, String), String> {
    let status = agent_api::user_api::call(
        session,
        "todo_change",
        json!({ "change": change, "reason": reason, "via": VIA }),
    )?;
    let record = status
        .last_todo_change
        .ok_or("the agent did not say what it decided")?;
    let reply = if record.reply.trim().is_empty() {
        if record.approved {
            "적용했습니다.".to_string()
        } else {
            "적용하지 않았습니다.".to_string()
        }
    } else {
        record.reply
    };
    Ok((record.approved, reply))
}

/// Decide every request the server gave `device_id` with `review`, and keep the outcomes for
/// the next round. A change the PC could not handle is refused with the reason, so the phone
/// learns of it. Returns how many were decided.
pub fn handle_requests(
    data_directory: &Path,
    device_id: &str,
    review: &mut dyn FnMut(&Value, &str) -> Result<(bool, String), String>,
) -> Result<usize, String> {
    let requests = sync::phone::requests_for(data_directory, device_id);
    for request in &requests {
        let (approved, reply) = review(&request.change, &request.reason).unwrap_or_else(|error| {
            (
                false,
                format!("PC가 이 변경을 처리하지 못했습니다: {error}"),
            )
        });
        info!(
            "phone request {}: {}",
            request.id,
            if approved { "applied" } else { "not applied" }
        );
        sync::phone::save_outcome(
            data_directory,
            sync::phone::PhoneOutcome {
                request_id: request.id.clone(),
                at: chrono::Utc::now(),
                approved,
                reply,
            },
        )?;
    }
    Ok(requests.len())
}

/// Handle the requests given to this PC with the agent of `session` on another thread (an AI
/// review takes seconds), then sync at once so the phone sees the outcomes. One at a time.
pub fn handle_in_background(data_directory: &Path, session: u32) {
    static RUNNING: AtomicBool = AtomicBool::new(false);
    let Some(config) = sync::client::ClientConfig::load(data_directory) else {
        return;
    };
    if sync::phone::requests_for(data_directory, &config.device_id).is_empty()
        || RUNNING.swap(true, Ordering::SeqCst)
    {
        return;
    }
    let data_directory = data_directory.to_path_buf();
    std::thread::spawn(move || {
        let handled = handle_requests(&data_directory, &config.device_id, &mut |change, reason| {
            review_with_agent(session, change, reason)
        });
        match handled {
            Ok(count) if count > 0 => {
                if let Err(error) =
                    crate::sync_commands::sync_device(&data_directory, &config, true)
                {
                    // The next round of the loop sends them.
                    warn!("sync after phone requests: {error}");
                }
            }
            Ok(_) => {}
            Err(error) => warn!("phone requests: {error}"),
        }
        RUNNING.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_request_given_to_this_pc_gets_an_outcome_and_failures_are_told() {
        let folder = std::env::temp_dir().join(format!("dw_phone_work_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let request = |id: &str, to: &str, title: &str| {
            json!({
                "id": id, "phone_id": "phone", "at": chrono::Utc::now(),
                "change": { "kind": "add", "todo": { "title": title } }, "reason": "",
                "assigned_to": to, "assigned_at": chrono::Utc::now(),
            })
        };
        std::fs::write(
            folder.join(sync::phone::PHONES_DOCUMENT),
            json!({ "phones": [], "requests": [
                request("one", "desktop", "Milk"),
                request("two", "desktop", "Bad"),
                request("three", "laptop", "Not mine"),
            ] })
            .to_string(),
        )
        .unwrap();
        let mut seen = Vec::new();
        let handled = handle_requests(&folder, "desktop", &mut |change, _reason| {
            let title = change["todo"]["title"].as_str().unwrap().to_string();
            seen.push(title.clone());
            if title == "Bad" {
                Err("params.change: missing field".into())
            } else {
                Ok((true, "Fine.".into()))
            }
        })
        .unwrap();
        assert_eq!(handled, 2);
        assert_eq!(seen, ["Milk", "Bad"]);
        let outcomes = sync::phone::pending_outcomes(&folder);
        assert!(outcomes[0].approved && outcomes[0].reply == "Fine.");
        assert!(!outcomes[1].approved && outcomes[1].reply.contains("missing field"));
        // Decided once: not handed over again.
        assert_eq!(
            handle_requests(&folder, "desktop", &mut |_, _| unreachable!()).unwrap(),
            0
        );
    }
}

//! The per-session agent.
//!
//! Started by the service *inside* a user's session but with the service's
//! SYSTEM token. Consequences:
//! - it sees that session's desktop (foreground window, screenshots, Ctrl+W);
//! - the user cannot end it (Task Manager: "Access is denied");
//! - it can terminate any process, so the enforcer is restricted to processes
//!   of its own session.
//!
//! It reads config from `%ProgramData%\DigitalWellbeing` and never writes it;
//! the admin UI writes, the agent reloads within a few seconds.

use ai_focus_rule::{native_monitor, AiFocusRuleHandle, AiFocusStore};
use enforcer::Enforcer;
use list_based_rule::{FileListPersistence, ListBasedRuleHandle, ListStore};
use machine::session::{current_session_id, process_session_id};
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime};
use tracing::{info, warn};

const RELOAD_INTERVAL: Duration = Duration::from_secs(3);

pub fn run() -> anyhow::Result<()> {
    let session = current_session_id();
    crate::init_logging(&format!("agent-session{session}.log"));
    info!("agent starting in session {session}");

    let list_store = ListStore::with_persistence(Arc::new(FileListPersistence::new(
        machine::policy_path(),
    )))?;
    let list_rule = ListBasedRuleHandle::new(list_store.clone());

    let ai_store = AiFocusStore::open_read_only(machine::ai_focus_path())?;
    let monitor = native_monitor(ai_store.clone());
    let ai_rule = AiFocusRuleHandle::new(Arc::clone(&monitor));

    let enforcer = Arc::new(Enforcer::new());
    enforcer.set_process_filter(Arc::new(move |pid| process_session_id(pid) == Some(session)));
    enforcer.register_rule(list_rule.rule());
    enforcer.register_rule(ai_rule.rule());

    let enforce = enforcer.start_background(Duration::from_millis(800));
    let _monitor = monitor.start_background();

    let api_monitor = Arc::clone(&monitor);
    thread::spawn(move || {
        if let Err(err) = machine::user_api::serve(api_monitor, session) {
            warn!("user api failed: {err}");
        }
    });

    thread::spawn(move || reload_loop(list_store, ai_store));

    enforce.join().ok();
    Ok(())
}

/// Pick up edits the admin UI makes to the config files.
fn reload_loop(list_store: ListStore, ai_store: AiFocusStore) {
    let policy = machine::policy_path();
    let ai = machine::ai_focus_path();
    let mut seen = (modified(&policy), modified(&ai));
    loop {
        thread::sleep(RELOAD_INTERVAL);
        let now = (modified(&policy), modified(&ai));
        if now.0 != seen.0 {
            match list_store.reload() {
                Ok(()) => info!("reloaded {}", policy.display()),
                Err(err) => warn!("reload {}: {err}", policy.display()),
            }
        }
        if now.1 != seen.1 {
            match ai_store.reload_keeping_session() {
                Ok(()) => info!("reloaded {}", ai.display()),
                Err(err) => warn!("reload {}: {err}", ai.display()),
            }
        }
        seen = now;
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata().and_then(|m| m.modified()).ok()
}

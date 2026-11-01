//! Responsibility: Run the rule engine (list rule, AI focus monitor, task planner, access
//! grants) inside one user session, and reload the admin's settings when they change.
//!
//! Started by the service *inside* a user's session but with the service's
//! SYSTEM token. Consequences:
//! - it sees that session's desktop (foreground window, screenshots, Ctrl+W);
//! - the user cannot end it (Task Manager: "Access is denied");
//! - it can terminate any process, so the enforcer is restricted to processes
//!   of its own session.
//!
//! It reads config from `%ProgramData%\DigitalWellbeing` and writes it only
//! for the Focus app's Settings tab and Claude Desktop (`settings_put`: at once
//! during setup; once protected, after three confirmations, see
//! `machine::settings`); every agent reloads changed files within a few seconds. The files it
//! writes are `planner_state.json` (task assignment, change requests),
//! `grants\<user>.json` (this user's access grants) and `blocks\<user>.json`
//! (blocks in force, so logging out and back in does not lift them).

use ai_focus_rule::claude::ClaudeClient;
use ai_focus_rule::connection::{self, Connection, KeyProblem};
use ai_focus_rule::grants::GrantState;
use ai_focus_rule::usage::UsageRecorder;
use ai_focus_rule::JudgeError;
use ai_focus_rule::{
    claude_judge_provider, platform, AiFocusRuleHandle, AiFocusStore, FocusMonitor, SavedBlocks,
};
use enforcer::Enforcer;
use list_based_rule::{FileListPersistence, ListBasedRuleHandle, ListStore};
use machine::lockdown::LockdownRule;
use machine::session::{current_session_id, process_session_id};
use machine::user_api::AgentApi;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use task_planner::{claude_advisor_provider, JsonStore, PlannerConfig, PlannerState, TaskPlanner};
use tracing::{info, warn};

const RELOAD_INTERVAL: Duration = Duration::from_secs(3);
/// How often the planner loop looks for a missing session or an assignment whose time is
/// up, and how long it waits after a failed AI call before trying again.
const PLANNER_CHECK_INTERVAL: Duration = Duration::from_secs(15);
/// How often the AI connection is checked while it works, and while it is broken.
const PROBE_WHILE_WORKING: Duration = Duration::from_secs(300);
const PROBE_WHILE_BROKEN: Duration = Duration::from_secs(30);

pub fn run() -> anyhow::Result<()> {
    let session = current_session_id();
    crate::init_logging(&format!("agent-session{session}.log"));
    info!(
        "agent starting in session {session} (version {}, user API port {})",
        env!("CARGO_PKG_VERSION"),
        machine::user_api::port_for_session(session)
    );
    // Nobody sees the agent's stderr: put a panic or a start-up error in its log.
    std::panic::set_hook(Box::new(|panic| {
        tracing::error!("agent panicked: {panic}");
    }));
    let result = run_agent(session);
    if let Err(error) = &result {
        tracing::error!("agent stopped: {error:#}");
    }
    result
}

fn run_agent(session: u32) -> anyhow::Result<()> {
    let list_store =
        ListStore::with_persistence(Arc::new(FileListPersistence::new(machine::policy_path())))?;
    let list_rule = ListBasedRuleHandle::new(list_store.clone());

    let ai_store = AiFocusStore::open_read_only(machine::ai_focus_path())?;
    let user =
        machine::session::session_user(session).unwrap_or_else(|| format!("session{session}"));
    let grants = JsonStore::<GrantState>::open(machine::grants_path_for(&user), true)?;
    let blocks = JsonStore::<SavedBlocks>::open(machine::blocks_path_for(&user), true)?;
    // Every Claude call of this agent (focus checks, planner, access reviews) is counted;
    // the daily budget adds up every user's file.
    let usage = Arc::new(UsageRecorder::new(
        JsonStore::open(machine::usage_path_for(&user), true)?,
        Some(machine::usage_dir()),
    ));
    ai_focus_rule::usage::install(Arc::clone(&usage));
    // The planner is made after the monitor (it drives it); the lockdown asks it whether a
    // task is assigned.
    let planner_cell: Arc<OnceLock<Arc<TaskPlanner>>> = Arc::new(OnceLock::new());
    let lockdown_planner = Arc::clone(&planner_cell);
    let lockdown_now: Arc<dyn Fn() -> Option<machine::lockdown::Lockdown> + Send + Sync> =
        Arc::new(move || {
            machine::user_api::lockdown_now(lockdown_planner.get().map(|planner| planner.as_ref()))
        });
    let monitor = Arc::new(
        FocusMonitor::new(
            ai_store.clone(),
            platform::native(),
            claude_judge_provider(),
        )
        .with_grants(grants)
        .with_block_store(blocks)
        .with_usage(Arc::clone(&usage))
        .with_lockdown({
            let lockdown_now = Arc::clone(&lockdown_now);
            Arc::new(move || lockdown_now().is_some_and(|lockdown| lockdown.browsers_allowed))
        }),
    );
    let ai_rule = AiFocusRuleHandle::new(Arc::clone(&monitor));

    let enforcer = Arc::new(Enforcer::new());
    enforcer.set_process_filter(Arc::new(move |process_id| {
        process_session_id(process_id) == Some(session)
    }));
    enforcer.register_rule(list_rule.rule());
    enforcer.register_rule(ai_rule.rule());
    // Protected settings and a broken AI connection: every app but the browsers (Claude pages
    // only, see the monitor) and Claude Desktop is closed until the key works again.
    enforcer.register_rule(Arc::new(LockdownRule {
        active: Arc::clone(&lockdown_now),
        settings: ai_store.clone(),
        closes_programs: cfg!(windows),
    }));
    let probe_store = ai_store.clone();
    thread::spawn(move || connection_probe_loop(probe_store));

    let enforce = enforcer.start_background(Duration::from_millis(800));
    let _monitor = monitor.start_background();

    let planner_config = JsonStore::<PlannerConfig>::open(machine::planner_path(), false)?;
    let planner_state = JsonStore::<PlannerState>::open(machine::planner_state_path(), true)?;
    let planner = Arc::new(TaskPlanner::new(
        planner_config.clone(),
        planner_state,
        Arc::clone(&monitor),
        claude_advisor_provider(),
    ));

    let _ = planner_cell.set(Arc::clone(&planner));
    let auto_planner = Arc::clone(&planner);
    thread::spawn(move || planner_loop(auto_planner));

    let api = Arc::new(AgentApi {
        monitor: Arc::clone(&monitor),
        planner: Some(planner),
        assistant: Default::default(),
    });
    thread::spawn(move || {
        if let Err(error) = machine::user_api::serve(api, session) {
            warn!("user api failed: {error}");
        }
    });

    thread::spawn(move || reload_loop(list_store, ai_store, planner_config));

    enforce.join().ok();
    Ok(())
}

/// After logon the assignment there is goes on (no AI call). Nothing is ever picked here:
/// the user asks for their next task ("할 일 받기"), and until then every app is locked
/// (see `machine::lockdown`). When an assignment's time is up it stays until the user
/// reports whether it is finished.
fn planner_loop(planner: Arc<TaskPlanner>) {
    loop {
        if let Err(error) = planner.resume() {
            warn!("planner: {error}");
        }
        thread::sleep(PLANNER_CHECK_INTERVAL);
    }
}

/// Keep [`connection`] current even when no AI call happens: check the key and the model
/// (the Models API, free) every few minutes, at once when either changes, and every 30
/// seconds while the connection is broken. An account without credit is checked with the
/// smallest message, since the Models API answers without credit.
fn connection_probe_loop(ai_store: AiFocusStore) {
    let mut last_checked: Option<(String, String)> = None;
    let mut next_check = Instant::now();
    loop {
        let config = ai_store.get();
        let key = config.resolve_api_key();
        let checked = (key.clone().unwrap_or_default(), config.model.clone());
        if last_checked.as_ref() != Some(&checked) || Instant::now() >= next_check {
            last_checked = Some(checked);
            match key {
                None => connection::report_error(&JudgeError::NoApiKey),
                Some(key) => {
                    let client = ClaudeClient::new(key, config.model.clone());
                    let billing = matches!(
                        connection::current().0,
                        Connection::Broken {
                            problem: KeyProblem::Billing,
                            ..
                        }
                    );
                    let result = if billing {
                        client.ping()
                    } else {
                        client.check()
                    };
                    if let Err(error) = result {
                        tracing::debug!("connection check: {error}");
                    }
                }
            }
            let broken = matches!(connection::current().0, Connection::Broken { .. });
            // Tests check every few seconds (DW_CONNECTION_CHECK_SECONDS); on Windows only an
            // administrator can set the service's environment.
            let test_interval = std::env::var("DW_CONNECTION_CHECK_SECONDS")
                .ok()
                .and_then(|seconds| seconds.parse().ok())
                .map(Duration::from_secs);
            next_check = Instant::now()
                + test_interval.unwrap_or(if broken {
                    PROBE_WHILE_BROKEN
                } else {
                    PROBE_WHILE_WORKING
                });
        }
        thread::sleep(RELOAD_INTERVAL);
    }
}

/// Pick up edits to the config files (made through any agent's `settings_put`).
fn reload_loop(
    list_store: ListStore,
    ai_store: AiFocusStore,
    planner_config: JsonStore<PlannerConfig>,
) {
    let policy_path = machine::policy_path();
    let ai_focus_path = machine::ai_focus_path();
    let planner_path = machine::planner_path();
    let mut seen = (
        modified(&policy_path),
        modified(&ai_focus_path),
        modified(&planner_path),
    );
    loop {
        thread::sleep(RELOAD_INTERVAL);
        let now = (
            modified(&policy_path),
            modified(&ai_focus_path),
            modified(&planner_path),
        );
        // A change counts as seen only once its reload worked; a file that was
        // locked or half-written (e.g. saved in place by an editor) is retried.
        if now.0 != seen.0 {
            match list_store.reload() {
                Ok(()) => {
                    info!("reloaded {}", policy_path.display());
                    seen.0 = now.0;
                }
                Err(error) => warn!("reload {}: {error}; retrying", policy_path.display()),
            }
        }
        if now.1 != seen.1 {
            match ai_store.reload_keeping_session() {
                Ok(()) => {
                    info!("reloaded {}", ai_focus_path.display());
                    seen.1 = now.1;
                }
                Err(error) => warn!("reload {}: {error}; retrying", ai_focus_path.display()),
            }
        }
        if now.2 != seen.2 {
            match planner_config.reload() {
                Ok(()) => {
                    info!("reloaded {}", planner_path.display());
                    seen.2 = now.2;
                }
                Err(error) => warn!("reload {}: {error}; retrying", planner_path.display()),
            }
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    path.metadata()
        .and_then(|metadata| metadata.modified())
        .ok()
}

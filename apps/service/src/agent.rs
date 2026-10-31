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

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::usage::UsageRecorder;
use ai_focus_rule::{
    claude_judge_provider, platform, AiFocusRuleHandle, AiFocusStore, FocusMonitor, SavedBlocks,
};
use enforcer::Enforcer;
use list_based_rule::{FileListPersistence, ListBasedRuleHandle, ListStore};
use machine::session::{current_session_id, process_session_id};
use machine::user_api::AgentApi;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use task_planner::{claude_advisor_provider, JsonStore, PlannerConfig, PlannerState, TaskPlanner};
use tracing::{info, warn};

const RELOAD_INTERVAL: Duration = Duration::from_secs(3);
/// How often the planner loop looks for a missing session or an assignment whose time is
/// up, and how long it waits after a failed AI call before trying again.
const PLANNER_CHECK_INTERVAL: Duration = Duration::from_secs(15);
const PLANNER_RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

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
    let monitor = Arc::new(
        FocusMonitor::new(
            ai_store.clone(),
            platform::native(),
            claude_judge_provider(),
        )
        .with_grants(grants)
        .with_block_store(blocks)
        .with_usage(Arc::clone(&usage)),
    );
    let ai_rule = AiFocusRuleHandle::new(Arc::clone(&monitor));

    let enforcer = Arc::new(Enforcer::new());
    enforcer.set_process_filter(Arc::new(move |process_id| {
        process_session_id(process_id) == Some(session)
    }));
    enforcer.register_rule(list_rule.rule());
    enforcer.register_rule(ai_rule.rule());

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

    let auto_planner = Arc::clone(&planner);
    let auto_monitor = Arc::clone(&monitor);
    thread::spawn(move || planner_loop(auto_planner, auto_monitor));

    let api = Arc::new(AgentApi {
        monitor: Arc::clone(&monitor),
        planner: Some(planner),
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

/// While the planner is on, a focus session is not optional: if none is
/// running (after logon, or when the user simply never asks for a task),
/// start one with the assigned task. A recent assignment is resumed without
/// an AI call. When the current assignment's time is up, the AI picks the next
/// one (unless the user got "notify only" approved). After a failed AI call
/// (e.g. no network) wait a minute before trying again.
fn planner_loop(planner: Arc<TaskPlanner>, monitor: Arc<FocusMonitor>) {
    let mut next_attempt = Instant::now();
    loop {
        thread::sleep(PLANNER_CHECK_INTERVAL);
        if !planner.is_active() || Instant::now() < next_attempt {
            continue;
        }
        let now = chrono::Utc::now();
        let result = if monitor.store().get().session_active {
            planner.reassign_if_time_is_up(now).map(|assignment| {
                if let Some(assignment) = assignment {
                    info!("time was up; next assignment: {}", assignment.task);
                }
            })
        } else {
            planner
                .assign(now)
                .map(|assignment| info!("started focus session automatically: {}", assignment.task))
        };
        if let Err(error) = result {
            warn!("planner: {error}");
            next_attempt = Instant::now() + PLANNER_RETRY_AFTER_FAILURE;
        }
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

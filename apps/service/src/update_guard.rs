//! Responsibility: Install an update of this program without a moment in which the rules lapse.
//!
//! Installing stops the service (and with it every agent) for a few seconds. So the service
//! does not run the setup program itself: it copies its own program into the data folder and
//! starts that copy as the **update guard** (`self-update-guard <setup program>`), a process of
//! its own that the stopping service does not take down. The guard
//! 1. closes every app in the users' sessions (browsers too) but Windows' own programs and the
//!    always-allowed ones, every half second, like the "lock every app" lockdown;
//! 2. runs the setup program silently (it keeps the owner's data), with this program's own
//!    windows closed too: the Focus app (where the update was asked for) and the MCP server
//!    Claude Desktop runs. The enforcer never closes them, and a program file in use makes
//!    the silent setup give up (exit code 5). The setup's log goes to `logs\setup.log`;
//! 3. starts the service if the setup left it stopped, waits until the new agents run, and
//!    only then stops closing apps and records the outcome (`self_update.json`).
//!
//! See `machine::self_update` for the request and the check.

use anyhow::Context;
use enforcer::{BlockEvent, BlockRule, Enforcer, ProcessSnapshot};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
/// The longest the setup program may take.
const SETUP_TIME_LIMIT: Duration = Duration::from_secs(15 * 60);
/// How long the guard keeps closing apps after the new service runs (its agents start
/// within a few seconds and take over).
const AGENTS_START_TIME: Duration = Duration::from_secs(20);

/// The service's side: copy this program into the data folder and start it as the guard.
pub fn start(setup: &Path) -> Result<(), String> {
    let folder = machine::data_dir().join(machine::self_update::WORK_FOLDER);
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    let guard = folder.join("update-guard.exe");
    let this_program = std::env::current_exe().map_err(|error| error.to_string())?;
    std::fs::copy(&this_program, &guard).map_err(|error| format!("copy the guard: {error}"))?;
    Command::new(&guard)
        .arg("self-update-guard")
        .arg(setup)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|error| format!("start the guard: {error}"))?;
    Ok(())
}

/// Responsibility: Close every app in the users' sessions while the update installs.
struct GuardRule {
    settings: ai_focus_rule::AiFocusConfig,
}

impl BlockRule for GuardRule {
    fn id(&self) -> &str {
        "update_guard"
    }

    fn name(&self) -> &str {
        "update guard"
    }

    fn description(&self) -> &str {
        "While this program updates, every app but Windows' own programs is closed."
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        processes
            .iter()
            .filter(|process| {
                !machine::lockdown::spared(process, |name| self.settings.is_always_allowed(name))
            })
            .map(|process| BlockEvent::new(process, "업데이트하는 동안 모든 앱을 닫는 중"))
            .collect()
    }

    fn keeps(&self, process: &ProcessSnapshot) -> bool {
        machine::lockdown::spared(process, |name| self.settings.is_always_allowed(name))
    }
}

/// `self-update-guard <setup program>` (run by the service, see the module comment).
pub fn run(arguments: &[String]) -> anyhow::Result<()> {
    let setup = PathBuf::from(
        arguments
            .first()
            .context("usage: self-update-guard <setup>")?,
    );
    let data_directory = machine::data_dir();
    let from = machine::self_update::this_version().to_string();
    let started_at = chrono::Utc::now();
    let settings: ai_focus_rule::AiFocusConfig = std::fs::read_to_string(machine::ai_focus_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();

    let enforcer = Arc::new(Enforcer::new());
    // Only the users' sessions: session 0 holds the services and the setup program itself.
    enforcer.set_process_filter(Arc::new(|process_id| {
        machine::session::process_session_id(process_id).is_some_and(|session| session != 0)
    }));
    enforcer.register_rule(Arc::new(GuardRule { settings }));
    let closing = enforcer.start_background(Duration::from_millis(500));
    info!(
        "update guard: closing every app; running {}",
        setup.display()
    );

    let outcome = run_setup(&setup).and_then(|()| wait_for_the_new_service());
    enforcer.stop();
    let _ = closing.join();
    let _ = std::fs::remove_file(&setup);

    let to = installed_version().unwrap_or_default();
    let (succeeded, message) = match &outcome {
        Ok(()) => (true, format!("{from}에서 {to}로 업데이트했습니다")),
        Err(error) => (false, format!("업데이트하지 못했습니다: {error:#}")),
    };
    info!("update guard: {message}");
    machine::self_update::change_state(&data_directory, |state| {
        state.installing_since = None;
        if succeeded {
            state.available = None;
        }
        state.last_install = Some(machine::self_update::InstallRun {
            started_at,
            finished_at: chrono::Utc::now(),
            from: from.clone(),
            to: to.clone(),
            succeeded,
            message: message.clone(),
        });
    });
    outcome
}

/// Run the setup program silently (a silent setup keeps the owner's data).
/// This program's own programs that run in the users' sessions and hold files the setup
/// replaces (the enforcer never closes them).
const OWN_USER_PROGRAMS: [&str; 3] = [
    "digital-wellbeing-focus.exe",
    "digital-wellbeing-mcp.exe",
    "digital-wellbeing.exe",
];

/// Close this program's own windows and servers in every session.
fn close_own_programs() {
    let mut command = Command::new("taskkill.exe");
    command.arg("/F");
    for program in OWN_USER_PROGRAMS {
        command.args(["/IM", program]);
    }
    // "Not found" is fine: nothing of ours was running.
    let _ = command.creation_flags(CREATE_NO_WINDOW).output();
}

fn run_setup(setup: &Path) -> anyhow::Result<()> {
    close_own_programs();
    let log = machine::log_dir().join("setup.log");
    let mut child = Command::new(setup)
        .args([
            "/VERYSILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/SP-",
            "/CLOSEAPPLICATIONS",
            "/FORCECLOSEAPPLICATIONS",
        ])
        .arg(format!("/LOG={}", log.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("start {}", setup.display()))?;
    let started = Instant::now();
    let mut closed_at = Instant::now();
    loop {
        // Opened again while the setup runs (the Focus app starts at logon): closed again.
        if closed_at.elapsed() > Duration::from_secs(2) {
            close_own_programs();
            closed_at = Instant::now();
        }
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            // The service may be left stopped: start it again, the old version or the new.
            let _ = start_service();
            anyhow::bail!(
                "the setup program ended with {status} (its log: {})",
                log.display()
            );
        }
        if started.elapsed() > SETUP_TIME_LIMIT {
            let _ = child.kill();
            let _ = start_service();
            anyhow::bail!("the setup program took more than 15 minutes");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn start_service() -> anyhow::Result<()> {
    let output = Command::new("sc.exe")
        .args(["start", crate::service::SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()?;
    if !output.status.success() {
        warn!(
            "sc start: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    Ok(())
}

fn service_running() -> bool {
    Command::new("sc.exe")
        .args(["query", crate::service::SERVICE_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("RUNNING"))
}

/// The new service runs (started if the setup did not), and its agents had time to start.
fn wait_for_the_new_service() -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(120);
    while !service_running() {
        if Instant::now() > deadline {
            anyhow::bail!("the service did not start after the update");
        }
        let _ = start_service();
        std::thread::sleep(Duration::from_secs(2));
    }
    std::thread::sleep(AGENTS_START_TIME);
    Ok(())
}

/// The version of the program the setup put in place (`digital-wellbeing-service.exe
/// version`).
fn installed_version() -> Option<String> {
    let program = crate::install::install_dir().join(crate::SERVICE_EXE);
    let output = Command::new(program)
        .arg("version")
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!version.is_empty()).then_some(version)
}

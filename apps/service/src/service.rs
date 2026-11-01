//! Responsibility: Run as the Windows service and keep exactly one agent alive in every managed
//! user session.

use crate::spawn::{spawn_in_session, user_sessions, Child};
use machine::MachineConfig;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;
use tracing::{error, info, warn};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "DigitalWellbeing";
/// How often sessions are re-scanned and dead agents restarted.
const TICK: Duration = Duration::from_secs(2);

define_windows_service!(ffi_service_main, service_main);

/// Entry point when the SCM starts us with `run`.
pub fn run_dispatcher() -> anyhow::Result<()> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_arguments: Vec<OsString>) {
    crate::init_logging("service.log");
    if let Err(error) = run_service() {
        error!("service failed: {error:#}");
    }
}

fn run_service() -> anyhow::Result<()> {
    let (stop_sender, stop_receiver) = mpsc::channel();
    // Only administrators may send Stop: the default service ACL gives
    // standard users query rights only.
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_sender.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let report = |state, accept| ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: accept,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    };
    status.set_service_status(report(
        ServiceState::Running,
        ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
    ))?;
    info!("service running");

    thread::spawn(|| care_loop(CareJob::TimeZone));
    thread::spawn(|| care_loop(CareJob::Updates));
    let mut supervisor = Supervisor::new(std::env::current_exe()?);
    loop {
        supervisor.tick();
        match stop_receiver.recv_timeout(TICK) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => break,
        }
    }
    supervisor.stop_all();
    info!("service stopped");
    status.set_service_status(report(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

/// Responsibility: Name the administrator jobs the service does for the user.
#[derive(Clone, Copy)]
enum CareJob {
    /// Put the time zone of the settings back when it changed.
    TimeZone,
    /// Run requested and daily program updates (each may take minutes).
    Updates,
}

/// How often each care job looks again.
const CARE_INTERVAL: Duration = Duration::from_secs(5);

/// Do `job` forever (see `machine::system_care`); the jobs run on their own threads so a long
/// update does not delay putting the time zone back.
fn care_loop(job: CareJob) {
    use machine::system_care;
    let data_directory = machine::data_dir();
    loop {
        let config: ai_focus_rule::AiFocusConfig =
            std::fs::read_to_string(machine::ai_focus_path())
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
        match job {
            CareJob::TimeZone => {
                system_care::guard_time_zone(
                    &data_directory,
                    &config,
                    system_care::current_time_zone(),
                    &system_care::set_time_zone,
                );
            }
            CareJob::Updates => system_care::run_updates(
                &data_directory,
                &config,
                chrono::Local::now(),
                &system_care::winget_upgrade,
            ),
        }
        thread::sleep(CARE_INTERVAL);
    }
}

/// Responsibility: Track the agent of each session: start missing ones, restart dead ones, stop
/// those whose session is no longer managed.
///
/// Session id -> running agent.
struct Supervisor {
    executable: PathBuf,
    agents: HashMap<u32, Child>,
}

impl Supervisor {
    fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            agents: HashMap::new(),
        }
    }

    fn tick(&mut self) {
        let config = MachineConfig::load();
        let wanted: Vec<_> = user_sessions()
            .into_iter()
            .filter(|session| config.manages(&session.user))
            .collect();

        // Drop agents whose session ended or is no longer managed.
        self.agents.retain(|id, child| {
            let keep = wanted.iter().any(|session| session.id == *id);
            if !keep && child.is_running() {
                info!("stopping agent for session {id}");
                child.kill();
            }
            keep
        });

        for session in &wanted {
            let alive = self.agents.get(&session.id).is_some_and(Child::is_running);
            if alive {
                continue;
            }
            // It ran and ended: say how, or a crash loop would look like a
            // stream of "started" lines only (the agent logs its own reason).
            if let Some(code) = self.agents.get(&session.id).and_then(Child::exit_code) {
                warn!(
                    "agent for {} (session {}) exited with code {code:#x}; see \
                     agent-session{}.log; starting it again",
                    session.user, session.id, session.id
                );
            }
            match spawn_in_session(session.id, &self.executable, "agent") {
                Ok(child) => {
                    info!(
                        "started agent pid {} for {} (session {})",
                        child.pid, session.user, session.id
                    );
                    self.agents.insert(session.id, child);
                }
                Err(error) => warn!("could not start agent for session {}: {error}", session.id),
            }
        }
    }

    fn stop_all(&mut self) {
        for (_, child) in self.agents.drain() {
            child.kill();
        }
    }
}

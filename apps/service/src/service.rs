//! The Windows service: runs as LocalSystem from boot and keeps exactly one
//! agent alive in every managed user session.

use crate::spawn::{spawn_in_session, user_sessions, Child};
use machine::MachineConfig;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError};
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

fn service_main(_args: Vec<OsString>) {
    crate::init_logging("service.log");
    if let Err(err) = run_service() {
        error!("service failed: {err:#}");
    }
}

fn run_service() -> anyhow::Result<()> {
    let (stop_tx, stop_rx) = mpsc::channel();
    // Only administrators may send Stop: the default service ACL gives
    // standard users query rights only.
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
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

    let mut supervisor = Supervisor::new(std::env::current_exe()?);
    loop {
        supervisor.tick();
        match stop_rx.recv_timeout(TICK) {
            Err(RecvTimeoutError::Timeout) => {}
            _ => break,
        }
    }
    supervisor.stop_all();
    info!("service stopped");
    status.set_service_status(report(ServiceState::Stopped, ServiceControlAccept::empty()))?;
    Ok(())
}

/// Session id -> running agent.
struct Supervisor {
    exe: PathBuf,
    agents: HashMap<u32, Child>,
}

impl Supervisor {
    fn new(exe: PathBuf) -> Self {
        Self {
            exe,
            agents: HashMap::new(),
        }
    }

    fn tick(&mut self) {
        let config = MachineConfig::load();
        let wanted: Vec<_> = user_sessions()
            .into_iter()
            .filter(|s| config.manages(&s.user))
            .collect();

        // Drop agents whose session ended or is no longer managed.
        self.agents.retain(|id, child| {
            let keep = wanted.iter().any(|s| s.id == *id);
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
            match spawn_in_session(session.id, &self.exe, "agent") {
                Ok(child) => {
                    info!(
                        "started agent pid {} for {} (session {})",
                        child.pid, session.user, session.id
                    );
                    self.agents.insert(session.id, child);
                }
                Err(err) => warn!("could not start agent for session {}: {err}", session.id),
            }
        }
    }

    fn stop_all(&mut self) {
        for (_, child) in self.agents.drain() {
            child.kill();
        }
    }
}

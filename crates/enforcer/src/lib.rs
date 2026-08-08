use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use policy::PolicyStore;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use sysinfo::{ProcessesToUpdate, System};
use tracing::{debug, info, warn};

const LOG_CAPACITY: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockEvent {
    pub process_name: String,
    pub pid: u32,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
}

#[derive(Clone)]
pub struct Enforcer {
    store: PolicyStore,
    logs: Arc<Mutex<VecDeque<BlockEvent>>>,
    running: Arc<Mutex<bool>>,
}

impl Enforcer {
    pub fn new(store: PolicyStore) -> Self {
        Self {
            store,
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAPACITY))),
            running: Arc::new(Mutex::new(false)),
        }
    }

    pub fn store(&self) -> &PolicyStore {
        &self.store
    }

    pub fn start_background(self: &Arc<Self>, interval: Duration) -> JoinHandle<()> {
        let this = Arc::clone(self);
        *this.running.lock() = true;
        thread::spawn(move || {
            info!("enforcer loop started");
            let mut system = System::new();
            while *this.running.lock() {
                if let Err(err) = this.reconcile(&mut system) {
                    warn!("reconcile error: {err:#}");
                }
                thread::sleep(interval);
            }
            info!("enforcer loop stopped");
        })
    }

    pub fn stop(&self) {
        *self.running.lock() = false;
    }

    pub fn recent_blocks(&self, limit: usize) -> Vec<BlockEvent> {
        self.logs
            .lock()
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn list_processes(&self) -> Vec<ProcessInfo> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);
        let mut list: Vec<ProcessInfo> = system
            .processes()
            .iter()
            .map(|(pid, proc)| ProcessInfo {
                pid: pid.as_u32(),
                name: proc.name().to_string_lossy().to_string(),
                exe: proc.exe().map(|p| p.to_string_lossy().to_string()),
            })
            .collect();
        list.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.pid.cmp(&b.pid))
        });
        list
    }

    /// Terminate every running process that the current policy does not allow.
    pub fn reconcile(&self, system: &mut System) -> anyhow::Result<usize> {
        system.refresh_processes(ProcessesToUpdate::All, true);
        let policy = self.store.get();
        let mut killed = 0usize;
        let self_pid = std::process::id();

        for (pid, proc) in system.processes() {
            let pid_u32 = pid.as_u32();
            if pid_u32 == self_pid {
                continue;
            }
            let name = proc.name().to_string_lossy().to_string();
            if !policy.should_block(&name) {
                continue;
            }

            if proc.kill() {
                killed += 1;
                self.push_log(BlockEvent {
                    process_name: name.clone(),
                    pid: pid_u32,
                    at: Utc::now(),
                });
                info!("terminated {name} (pid={pid_u32})");
            } else {
                debug!("failed to terminate {name} (pid={pid_u32})");
            }
        }
        Ok(killed)
    }

    fn push_log(&self, event: BlockEvent) {
        let mut logs = self.logs.lock();
        if logs.len() >= LOG_CAPACITY {
            logs.pop_front();
        }
        logs.push_back(event);
    }
}

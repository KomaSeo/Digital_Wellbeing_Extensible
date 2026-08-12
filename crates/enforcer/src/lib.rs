mod rule;

pub use rule::{
    BlockDecision, BlockRule, ListPolicyHandle, ListPolicyRule, ProcessSnapshot, RuleInfo,
};

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use policy::Policy;
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
    pub reason: String,
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
    rules: Arc<Mutex<Vec<Arc<dyn BlockRule>>>>,
    logs: Arc<Mutex<VecDeque<BlockEvent>>>,
    running: Arc<Mutex<bool>>,
}

impl Enforcer {
    pub fn new() -> Self {
        Self {
            rules: Arc::new(Mutex::new(Vec::new())),
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAPACITY))),
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Register a rule. Later rules are consulted after earlier ones; any [`BlockDecision::Block`] wins.
    pub fn register_rule(&self, rule: Arc<dyn BlockRule>) {
        let id = rule.id().to_string();
        let mut rules = self.rules.lock();
        if let Some(pos) = rules.iter().position(|r| r.id() == id) {
            rules[pos] = rule;
            info!("replaced block rule '{id}'");
        } else {
            rules.push(rule);
            info!("registered block rule '{id}'");
        }
    }

    pub fn unregister_rule(&self, id: &str) -> bool {
        let mut rules = self.rules.lock();
        let before = rules.len();
        rules.retain(|r| r.id() != id);
        before != rules.len()
    }

    pub fn list_rules(&self) -> Vec<RuleInfo> {
        self.rules
            .lock()
            .iter()
            .map(|r| RuleInfo {
                id: r.id().to_string(),
            })
            .collect()
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

    /// Terminate every running process that any registered rule decides to block.
    pub fn reconcile(&self, system: &mut System) -> anyhow::Result<usize> {
        system.refresh_processes(ProcessesToUpdate::All, true);
        let rules = self.rules.lock().clone();
        let mut killed = 0usize;
        let self_pid = std::process::id();

        for (pid, proc) in system.processes() {
            let pid_u32 = pid.as_u32();
            if pid_u32 == self_pid {
                continue;
            }
            let name = proc.name().to_string_lossy().to_string();
            if Policy::is_protected(&name) {
                continue;
            }

            let snapshot = ProcessSnapshot {
                pid: pid_u32,
                name: name.clone(),
                exe: proc.exe().map(|p| p.to_string_lossy().to_string()),
            };

            let Some(reason) = decide_block(&rules, &snapshot) else {
                continue;
            };

            if proc.kill() {
                killed += 1;
                self.push_log(BlockEvent {
                    process_name: name.clone(),
                    pid: pid_u32,
                    reason,
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

impl Default for Enforcer {
    fn default() -> Self {
        Self::new()
    }
}

fn decide_block(rules: &[Arc<dyn BlockRule>], snapshot: &ProcessSnapshot) -> Option<String> {
    for rule in rules {
        match rule.evaluate(snapshot) {
            BlockDecision::Allow => {}
            BlockDecision::Block { reason } => {
                return Some(format!("{}: {reason}", rule.id()));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FlagRule {
        id: &'static str,
        block: Arc<AtomicBool>,
    }

    impl BlockRule for FlagRule {
        fn id(&self) -> &str {
            self.id
        }

        fn evaluate(&self, process: &ProcessSnapshot) -> BlockDecision {
            if self.block.load(Ordering::SeqCst) && process.name.eq_ignore_ascii_case("evil.exe") {
                BlockDecision::block("flagged")
            } else {
                BlockDecision::Allow
            }
        }
    }

    #[test]
    fn any_rule_block_wins() {
        let enforcer = Enforcer::new();
        let flag = Arc::new(AtomicBool::new(false));
        enforcer.register_rule(Arc::new(FlagRule {
            id: "flag",
            block: Arc::clone(&flag),
        }));

        let snap = ProcessSnapshot {
            pid: 1,
            name: "evil.exe".into(),
            exe: None,
        };
        let rules = enforcer.rules.lock().clone();
        assert!(decide_block(&rules, &snap).is_none());

        flag.store(true, Ordering::SeqCst);
        let reason = decide_block(&rules, &snap).expect("should block");
        assert!(reason.contains("flag"));
    }

    #[test]
    fn replace_rule_same_id() {
        let enforcer = Enforcer::new();
        enforcer.register_rule(Arc::new(FlagRule {
            id: "flag",
            block: Arc::new(AtomicBool::new(false)),
        }));
        enforcer.register_rule(Arc::new(FlagRule {
            id: "flag",
            block: Arc::new(AtomicBool::new(true)),
        }));
        assert_eq!(enforcer.list_rules().len(), 1);
    }
}

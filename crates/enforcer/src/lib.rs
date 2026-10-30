//! Responsibility: Run the enforcement loop: snapshot processes, ask every registered
//! BlockRule, and terminate what they block (never OS-critical or own processes).

mod rule;

pub use rule::{BlockEvent, BlockRule, ProcessSnapshot, RuleInfo};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use sysinfo::{ProcessesToUpdate, System};
use tracing::{debug, info, warn};

const LOG_CAPACITY: usize = 200;
/// How long to leave a process alone after it refused to die.
const KILL_RETRY: Duration = Duration::from_secs(30);

/// OS / self processes the enforcer will never terminate, regardless of rules.
pub fn never_terminate() -> &'static [&'static str] {
    // Every entry is already in `normalize_process_name` form (lower case,
    // `.exe`), or it would never match; a test checks this.
    &[
        // kernel and pseudo processes
        "system.exe",
        "system idle process.exe",
        "idle.exe",
        "registry.exe",
        "memory compression.exe",
        "secure system.exe",
        // core OS
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "services.exe",
        "lsass.exe",
        "lsaiso.exe",
        "smss.exe",
        "svchost.exe",
        "spoolsv.exe",
        "msmpeng.exe",
        // a usable desktop: shell, typing/IME, sound, drivers, UAC, lock screen
        "explorer.exe",
        "dwm.exe",
        "fontdrvhost.exe",
        "sihost.exe",
        "taskhostw.exe",
        "runtimebroker.exe",
        "searchhost.exe",
        "startmenuexperiencehost.exe",
        "shellexperiencehost.exe",
        "applicationframehost.exe",
        "conhost.exe",
        "ctfmon.exe",
        "textinputhost.exe",
        "audiodg.exe",
        "wudfhost.exe",
        "dllhost.exe",
        "consent.exe",
        "logonui.exe",
        "lockapp.exe",
        "userinit.exe",
        // this app
        "digital-wellbeing.exe",
        "digital_wellbeing.exe",
        "digital-wellbeing-service.exe",
        "digital-wellbeing-focus.exe",
        "digital-wellbeing-mcp.exe",
    ]
}

/// Lower-case file name with `.exe` appended when missing (`C:\\X\\Steam` -> `steam.exe`).
pub fn normalize_process_name(name: &str) -> String {
    let trimmed = name.trim().to_lowercase();
    let file_name = Path::new(&trimmed)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&trimmed);
    if file_name.ends_with(".exe") {
        file_name.to_string()
    } else if file_name.is_empty() {
        trimmed
    } else {
        format!("{file_name}.exe")
    }
}

/// True for processes in [`never_terminate`].
pub fn is_never_terminate(name: &str) -> bool {
    let n = normalize_process_name(name);
    never_terminate().iter().any(|p| *p == n)
}

/// Responsibility: Describe a running process for UIs and the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub exe: Option<String>,
}

/// Decides which pids the enforcer may see and terminate (e.g. "only my session").
pub type ProcessFilter = Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// Responsibility: Own the registered rules, the process filter and the block log, and run the
/// enforcement loop.
#[derive(Clone)]
pub struct Enforcer {
    rules: Arc<Mutex<Vec<Arc<dyn BlockRule>>>>,
    logs: Arc<Mutex<VecDeque<BlockEvent>>>,
    /// Bumped by every start and stop; a loop runs only while it holds the
    /// current value, so a second start never leaves two loops running.
    generation: Arc<AtomicU64>,
    filter: Arc<Mutex<Option<ProcessFilter>>>,
    /// Processes that would not die (pid, start time) -> when to try again.
    /// Without this, protected processes would be retried every tick forever.
    failed_kills: Arc<Mutex<HashMap<(u32, u64), Instant>>>,
}

impl Enforcer {
    pub fn new() -> Self {
        Self {
            rules: Arc::new(Mutex::new(Vec::new())),
            logs: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_CAPACITY))),
            generation: Arc::new(AtomicU64::new(0)),
            filter: Arc::new(Mutex::new(None)),
            failed_kills: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Restrict enforcement (and [`Self::list_processes`]) to pids the filter accepts.
    /// Without a filter every process on the machine is in scope.
    pub fn set_process_filter(&self, filter: ProcessFilter) {
        *self.filter.lock() = Some(filter);
    }

    fn in_scope(&self, pid: u32) -> bool {
        self.filter.lock().as_ref().is_none_or(|f| f(pid))
    }

    /// Register a rule. Later rules run after earlier ones; the first event for a pid wins.
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
                name: r.name().to_string(),
                description: r.description().to_string(),
            })
            .collect()
    }

    /// Start the loop. Starting again replaces the previous loop.
    pub fn start_background(self: &Arc<Self>, interval: Duration) -> JoinHandle<()> {
        let this = Arc::clone(self);
        let generation = this.generation.fetch_add(1, Ordering::SeqCst) + 1;
        thread::spawn(move || {
            info!("enforcer loop started");
            let mut system = System::new();
            while this.generation.load(Ordering::SeqCst) == generation {
                if let Err(err) = this.reconcile(&mut system) {
                    warn!("reconcile error: {err:#}");
                }
                thread::sleep(interval);
            }
            info!("enforcer loop stopped");
        })
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
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
            .filter(|(pid, _)| self.in_scope(pid.as_u32()))
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
        let self_pid = std::process::id();

        let snapshots: Vec<ProcessSnapshot> = system
            .processes()
            .iter()
            .filter_map(|(pid, proc)| {
                let pid_u32 = pid.as_u32();
                if pid_u32 == self_pid || !self.in_scope(pid_u32) {
                    return None;
                }
                let name = proc.name().to_string_lossy().to_string();
                if is_never_terminate(&name) {
                    return None;
                }
                Some(ProcessSnapshot {
                    pid: pid_u32,
                    name,
                    exe: proc.exe().map(|p| p.to_string_lossy().to_string()),
                })
            })
            .collect();

        let blocks = collect_blocks(&rules, &snapshots);
        let mut killed = 0usize;
        let now = Instant::now();
        let mut failed = self.failed_kills.lock();
        failed.retain(|(pid, start), _| {
            system
                .process(sysinfo::Pid::from_u32(*pid))
                .is_some_and(|p| p.start_time() == *start)
        });

        for event in blocks {
            let Some(proc) = system.process(sysinfo::Pid::from_u32(event.pid)) else {
                continue;
            };
            let key = (event.pid, proc.start_time());
            if failed.get(&key).is_some_and(|retry| now < *retry) {
                continue;
            }
            if terminate(proc) {
                failed.remove(&key);
                killed += 1;
                info!(
                    "terminated {} (pid={}) [{}]",
                    event.process_name, event.pid, event.reason
                );
                self.push_log(event);
            } else {
                debug!(
                    "failed to terminate {} (pid={}); retrying in {}s",
                    event.process_name,
                    event.pid,
                    KILL_RETRY.as_secs()
                );
                failed.insert(key, now + KILL_RETRY);
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

/// Terminate directly. (sysinfo's `kill()` starts `taskkill.exe` on Windows: one
/// child process per kill, found via the exe's own folder before System32.)
#[cfg(windows)]
fn terminate(proc: &sysinfo::Process) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    // SAFETY: the handle is checked for null and closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, proc.pid().as_u32());
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ok
    }
}

#[cfg(not(windows))]
fn terminate(proc: &sysinfo::Process) -> bool {
    proc.kill()
}

/// Merge rule outputs: first event for each pid wins; reasons are prefixed with rule id.
fn collect_blocks(rules: &[Arc<dyn BlockRule>], processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for rule in rules {
        for mut event in rule.evaluate(processes) {
            if !seen.insert(event.pid) {
                continue;
            }
            event.reason = format!("{}: {}", rule.id(), event.reason);
            out.push(event);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Responsibility: Test rule: block evil.exe while its flag is set.
    struct FlagRule {
        id: &'static str,
        block: Arc<AtomicBool>,
    }

    impl BlockRule for FlagRule {
        fn id(&self) -> &str {
            self.id
        }

        fn name(&self) -> &str {
            "Flag rule"
        }

        fn description(&self) -> &str {
            "Test rule that blocks evil.exe when armed"
        }

        fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
            if !self.block.load(Ordering::SeqCst) {
                return Vec::new();
            }
            processes
                .iter()
                .filter(|p| p.name.eq_ignore_ascii_case("evil.exe"))
                .map(|p| BlockEvent::new(p, "flagged"))
                .collect()
        }
    }

    #[test]
    fn protected_names_are_in_normalized_form() {
        for name in never_terminate() {
            assert_eq!(normalize_process_name(name), *name);
        }
        assert!(is_never_terminate("System"));
        assert!(is_never_terminate("Registry"));
        assert!(is_never_terminate("CTFMON.EXE"));
    }

    #[test]
    fn restarting_the_loop_retires_the_old_one() {
        let enforcer = Arc::new(Enforcer::new());
        let first = enforcer.start_background(Duration::from_millis(5));
        let _second = enforcer.start_background(Duration::from_millis(5));
        first.join().unwrap(); // returns only because the first loop stopped
        enforcer.stop();
    }

    #[test]
    fn works_with_zero_rules() {
        let enforcer = Enforcer::new();
        assert!(enforcer.list_rules().is_empty());
        let snaps = [ProcessSnapshot {
            pid: 1,
            name: "evil.exe".into(),
            exe: None,
        }];
        assert!(collect_blocks(&[], &snaps).is_empty());
    }

    #[test]
    fn any_rule_block_wins() {
        let enforcer = Enforcer::new();
        let flag = Arc::new(AtomicBool::new(false));
        enforcer.register_rule(Arc::new(FlagRule {
            id: "flag",
            block: Arc::clone(&flag),
        }));

        let snaps = [ProcessSnapshot {
            pid: 1,
            name: "evil.exe".into(),
            exe: None,
        }];
        let rules = enforcer.rules.lock().clone();
        assert!(collect_blocks(&rules, &snaps).is_empty());

        flag.store(true, Ordering::SeqCst);
        let blocks = collect_blocks(&rules, &snaps);
        assert_eq!(blocks.len(), 1);
        assert!(blocks[0].reason.contains("flag"));
        assert_eq!(blocks[0].pid, 1);
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

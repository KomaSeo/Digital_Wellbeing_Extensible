//! Responsibility: Lock every app (and so every website) while the settings are protected and
//! the AI connection is broken, and say which settings may be fixed at once meanwhile.
//!
//! Without a working key the AI cannot check anything, so a user who wants to escape the
//! checks could break the key: change it, or revoke it or empty its credit on the Anthropic
//! website. Once the settings are protected, a broken connection therefore closes every
//! program of the user's session except the Windows desktop, this app and the programs
//! the settings never check (`always_allow`), until a call with the key works again. Fixing
//! the key is the way out, so while locked an edit of the API key (and of the model, when the
//! model is the problem) takes effect at once instead of being scheduled.
//!
//! Network errors do not lock: they say nothing about the key.
//!
//! The same lock holds while there are to-dos but none is assigned (after a report, or
//! before the first task of the day): the user starts the next task by asking for it, so
//! the time the AI gives it is not spent away from the computer. Then browsers close too.
//! And while the task is done away from the computer (sleep, exercise, an appointment out of
//! the house), everything locks the same way until the user reports or the appointment ends.
//! Walkthrough (Korean): `docs/ai_focus_rule.md` 5.13.

use ai_focus_rule::connection::{Connection, KeyProblem};
use ai_focus_rule::AiFocusStore;
use enforcer::{normalize_process_name, BlockEvent, BlockRule, ProcessSnapshot};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Responsibility: Name why every app is locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LockdownCause {
    /// The AI connection is broken.
    Connection { problem: KeyProblem },
    /// There are to-dos but none is assigned: the user asks for their task ("할 일 받기").
    NoTask,
    /// The task is done away from the computer (sleep, exercise, an appointment).
    AwayFromComputer,
}

/// Responsibility: Say why the planner wants every app locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskLock {
    /// There are to-dos but none is assigned.
    WaitingForTask,
    /// The task (named) is done away from the computer.
    AwayFromComputer { task: String },
}

/// Responsibility: Describe a lockdown in force for the Focus app and the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockdown {
    pub cause: LockdownCause,
    /// Why, in Korean, for the Focus app.
    pub reason: String,
    /// Settings (paths of keys in `ai_focus.json`) whose edits take effect at once while
    /// locked.
    pub fix_now: Vec<Vec<String>>,
    /// Browsers stay open with Claude and Anthropic pages only (to fix the key); otherwise
    /// they are closed like every other app.
    pub browsers_allowed: bool,
}

/// The lockdown in force, if any. Only once the settings are protected: during setup
/// nothing locks. A broken connection comes first, since fixing it needs the browser.
pub fn lockdown(
    protected: bool,
    connection: &Connection,
    task_lock: Option<TaskLock>,
) -> Option<Lockdown> {
    if !protected {
        return None;
    }
    if let Connection::Broken { problem, detail } = connection {
        let what = match problem {
            KeyProblem::NoKey => "API 키가 없습니다".to_string(),
            KeyProblem::Rejected => {
                "API가 키를 받아들이지 않습니다(틀렸거나, 취소되었거나, 꺼진 키)".to_string()
            }
            KeyProblem::Billing => "API 계정의 크레딧이 없거나 사용 한도에 닿았습니다".to_string(),
            KeyProblem::UnknownModel => "API가 설정의 모델을 모릅니다".to_string(),
        };
        let mut fix_now = vec![vec!["api_key".to_string()]];
        if *problem == KeyProblem::UnknownModel {
            fix_now.push(vec!["model".to_string()]);
        }
        return Some(Lockdown {
            cause: LockdownCause::Connection { problem: *problem },
            reason: if *problem == KeyProblem::NoKey || detail.is_empty() {
                what
            } else {
                format!("{what} ({detail})")
            },
            fix_now,
            browsers_allowed: true,
        });
    }
    let (cause, reason) = match task_lock? {
        TaskLock::WaitingForTask => (
            LockdownCause::NoTask,
            "지금 할 일이 없습니다. 준비되면 \"할 일 받기\"를 누르세요".to_string(),
        ),
        TaskLock::AwayFromComputer { task } => (
            LockdownCause::AwayFromComputer,
            format!(
                "지금 할 일은 컴퓨터 없이 하는 일입니다({}). 끝나면 할 일 탭에서 보고하세요",
                task.chars().take(80).collect::<String>()
            ),
        ),
    };
    Some(Lockdown {
        cause,
        reason,
        fix_now: Vec::new(),
        browsers_allowed: false,
    })
}

/// Programs a lockdown leaves running besides the enforcer's own never-terminate list: Claude
/// Desktop, the Windows settings (network), the web view the Start menu and widgets use, and
/// security helpers. Browsers stay too, but the monitor closes every tab except Claude and
/// Anthropic pages (`ai_focus_rule::lockdown_allows_page`).
pub const LOCKDOWN_KEEPS: [&str; 8] = [
    // Claude Desktop, to get help fixing the connection (the owner's exception)
    "claude.exe",
    "systemsettings.exe",
    "shellhost.exe",
    "msedgewebview2.exe",
    "securityhealthsystray.exe",
    "smartscreen.exe",
    "taskmgr.exe",
    "useroobebroker.exe",
];

/// Whether a lockdown leaves `process` running: the programs of [`LOCKDOWN_KEEPS`], the
/// programs `kept` names (always allowed programs and browsers), and Windows' own programs
/// (under the Windows folder).
pub fn spared(process: &ProcessSnapshot, kept: impl Fn(&str) -> bool) -> bool {
    let name = normalize_process_name(&process.name);
    if LOCKDOWN_KEEPS.contains(&name.as_str()) || kept(&process.name) {
        return true;
    }
    let windows_folder = std::env::var("SystemRoot")
        .unwrap_or_else(|_| r"C:\Windows".to_string())
        .to_lowercase();
    process.exe.as_deref().is_some_and(|exe| {
        let exe = exe.to_lowercase();
        exe.starts_with(&format!("{windows_folder}\\"))
    })
}

/// Responsibility: Ask the enforcer to close every program a lockdown does not spare.
pub struct LockdownRule {
    /// The lockdown in force now.
    pub active: Arc<dyn Fn() -> Option<Lockdown> + Send + Sync>,
    pub settings: AiFocusStore,
    /// Close programs. Off on Linux, where the agent's "session" is every process of the
    /// user, the developer's tools included.
    pub closes_programs: bool,
}

impl BlockRule for LockdownRule {
    fn id(&self) -> &str {
        "lockdown"
    }

    fn name(&self) -> &str {
        "AI connection lockdown"
    }

    fn description(&self) -> &str {
        "While the settings are protected and the AI key does not work, every app but the browsers \
         (Claude pages only) and Claude Desktop is closed."
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        if !self.closes_programs {
            return Vec::new();
        }
        let Some(lockdown) = (self.active)() else {
            return Vec::new();
        };
        let config = self.settings.get();
        processes
            .iter()
            .filter(|process| {
                !spared(process, |name| {
                    config.is_always_allowed(name)
                        || (lockdown.browsers_allowed && config.is_browser(name))
                })
            })
            .map(|process| {
                BlockEvent::new(process, format!("모든 앱을 막는 중: {}", lockdown.reason))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_focus_rule::AiFocusConfig;

    fn process(name: &str, exe: &str) -> ProcessSnapshot {
        ProcessSnapshot {
            pid: 1,
            name: name.into(),
            exe: Some(exe.into()),
        }
    }

    #[test]
    fn only_a_broken_key_with_protected_settings_locks() {
        let broken = Connection::Broken {
            problem: KeyProblem::Rejected,
            detail: "401".into(),
        };
        assert!(
            lockdown(false, &broken, Some(TaskLock::WaitingForTask)).is_none(),
            "setup never locks"
        );
        assert!(lockdown(true, &Connection::Working, None).is_none());
        assert!(lockdown(true, &Connection::Unknown, None).is_none());
        let locked = lockdown(true, &broken, Some(TaskLock::WaitingForTask)).unwrap();
        assert_eq!(locked.fix_now, vec![vec!["api_key".to_string()]]);
        assert!(locked.browsers_allowed, "the connection comes first");
        let model = lockdown(
            true,
            &Connection::Broken {
                problem: KeyProblem::UnknownModel,
                detail: "404".into(),
            },
            None,
        )
        .unwrap();
        assert_eq!(model.fix_now.len(), 2);
        let waiting = lockdown(true, &Connection::Working, Some(TaskLock::WaitingForTask)).unwrap();
        assert_eq!(waiting.cause, LockdownCause::NoTask);
        assert!(!waiting.browsers_allowed);
        let away = lockdown(
            true,
            &Connection::Working,
            Some(TaskLock::AwayFromComputer {
                task: "잠자기".into(),
            }),
        )
        .unwrap();
        assert_eq!(away.cause, LockdownCause::AwayFromComputer);
        assert!(away.reason.contains("잠자기"));
        assert!(!away.browsers_allowed);
    }

    #[test]
    fn a_lockdown_closes_everything_but_windows_claude_browsers_and_always_allowed_programs() {
        let mut config = AiFocusConfig::default();
        config.always_allow.insert("keepass.exe".into());
        let locked = lockdown(
            true,
            &Connection::Broken {
                problem: KeyProblem::NoKey,
                detail: String::new(),
            },
            None,
        );
        let rule = LockdownRule {
            active: Arc::new(move || locked.clone()),
            settings: AiFocusStore::in_memory(config),
            closes_programs: true,
        };
        let processes = [
            process("steam.exe", r"C:\Program Files (x86)\Steam\steam.exe"),
            process(
                "chrome.exe",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            ),
            process(
                "Claude.exe",
                r"C:\Users\me\AppData\Local\AnthropicClaude\claude.exe",
            ),
            process(
                "KakaoTalk.exe",
                r"C:\Program Files (x86)\Kakao\KakaoTalk\KakaoTalk.exe",
            ),
            process("notepad.exe", r"C:\Windows\System32\notepad.exe"),
            process(
                "SystemSettings.exe",
                r"C:\Windows\ImmersiveControlPanel\SystemSettings.exe",
            ),
            process(
                "msedgewebview2.exe",
                r"C:\Program Files (x86)\Microsoft\EdgeWebView\msedgewebview2.exe",
            ),
            process("KeePass.exe", r"C:\Program Files\KeePass\KeePass.exe"),
        ];
        let closed: Vec<String> = rule
            .evaluate(&processes)
            .into_iter()
            .map(|event| event.process_name)
            .collect();
        // Browsers stay; the monitor closes their tabs except Claude pages.
        assert_eq!(closed, ["steam.exe", "KakaoTalk.exe"]);

        // Waiting for a task: browsers close too.
        let waiting = lockdown(true, &Connection::Working, Some(TaskLock::WaitingForTask));
        let rule = LockdownRule {
            active: Arc::new(move || waiting.clone()),
            settings: AiFocusStore::in_memory(AiFocusConfig::default()),
            closes_programs: true,
        };
        let closed: Vec<String> = rule
            .evaluate(&processes)
            .into_iter()
            .map(|event| event.process_name)
            .collect();
        assert_eq!(
            closed,
            ["steam.exe", "chrome.exe", "KakaoTalk.exe", "KeePass.exe"]
        );

        let off = LockdownRule {
            active: Arc::new(|| None),
            settings: AiFocusStore::in_memory(AiFocusConfig::default()),
            closes_programs: true,
        };
        assert!(off.evaluate(&processes).is_empty());
    }
}

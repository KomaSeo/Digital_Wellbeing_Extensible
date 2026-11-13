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
//! Tasks done away from the computer no longer lock (the owner: "그래 할 일에서도 없애자"):
//! the window checks judge what has to do with them.
//! A paused task locks the same way (its time stands still meanwhile), and so does a task
//! whose time is up until the user reports on it.
//! In every lock but a broken connection, nothing is closed: the Focus app covers every
//! screen, always in front ([`Lockdown::covers_screen`]); only a program kept in front of it
//! is closed, and every program when the Focus app does not run.
//! Walkthrough (Korean): `docs/ai_focus_rule.md` 5.13.

use ai_focus_rule::connection::{Connection, KeyProblem};
use ai_focus_rule::AiFocusStore;
use enforcer::{normalize_process_name, BlockEvent, BlockRule, ProcessSnapshot};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Responsibility: Name why every app is locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LockdownCause {
    /// The AI connection is broken.
    Connection { problem: KeyProblem },
    /// There are to-dos but none is assigned: the user asks for their task ("할 일 받기").
    NoTask,
    /// The user pressed "사용 중지".
    Stopped,
    /// This Windows account has not logged in to the owner's sync login yet.
    LoginRequired,
    /// The user paused the current task ("일시 정지").
    Paused,
    /// The current task's time is up and the user has not reported on it yet.
    ReportDue,
}

/// Responsibility: Say why the planner wants every app locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskLock {
    /// There are to-dos but none is assigned.
    WaitingForTask,
    /// The user stopped using the computer ("사용 중지"). Locks only in strict mode (once the
    /// settings are protected), like every other lock: the owner asked that it lock nothing
    /// outside strict mode.
    Stopped,
    /// The PC syncs and has logins, and this Windows account has not logged in
    /// (`crate::login`).
    LoginRequired,
    /// The user paused the current task: its time stands still, so the computer must not be
    /// usable meanwhile, or a pause would make the task longer for free (the owner: "일시 정지해서
    /// 지금 할 일을 늘리는 꼼수가 가능해. 그러니 일시 정지를 할 때는 화면을 똑같이 덮어서 못쓰게
    /// 하렴").
    Paused,
    /// The current task's time is up and the user has not reported whether it is finished:
    /// without the lock, not reporting would make the task last as long as the user likes (the
    /// owner: "일 하는 시간 끝났을 때 보고 안하고 넘기면 무한정 일을 연장할 수 있더라. … "일시
    /// 정지"한 것처럼 화면을 덮고 사용자가 완료 결과를 보고하게 만들어"). Not for
    /// appointments, which end on their own.
    ReportDue,
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
    /// The Focus app covers every screen, always in front, and no program is closed (the
    /// owner: "앱이나 웹사이트를 도륙내는 대신에, digital wellbeing focus앱이 전체화면으로
    /// 전환되게 만들고 항상 맨 앞으로 나오게"). Every lock but a broken connection, which
    /// needs the browser to fix the key.
    #[serde(default)]
    pub covers_screen: bool,
}

/// The lockdown in force, if any. Only once the settings are protected: during setup
/// nothing locks. A broken connection comes first, since fixing it needs the browser.
pub fn lockdown(
    protected: bool,
    connection: &Connection,
    task_lock: Option<TaskLock>,
) -> Option<Lockdown> {
    if protected && task_lock == Some(TaskLock::Stopped) {
        return Some(Lockdown {
            cause: LockdownCause::Stopped,
            reason: "사용을 멈췄습니다. 다시 쓰려면 일하기 탭에서 \"다시 사용하기\"를 누르세요"
                .to_string(),
            fix_now: Vec::new(),
            browsers_allowed: false,
            covers_screen: true,
        });
    }
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
            fix_now.push(vec!["recheck_model".to_string()]);
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
            covers_screen: false,
        });
    }
    let (cause, reason) = match task_lock? {
        TaskLock::WaitingForTask => (
            LockdownCause::NoTask,
            "지금 할 일이 없습니다. 준비되면 \"할 일 받기\"를 누르세요".to_string(),
        ),
        TaskLock::Stopped => unreachable!("handled first"),
        TaskLock::LoginRequired => (
            LockdownCause::LoginRequired,
            "이 PC는 여러 기기 동기화를 씁니다. 쓰려면 먼저 로그인하세요(Focus 앱 맨 위)"
                .to_string(),
        ),
        TaskLock::ReportDue => (
            LockdownCause::ReportDue,
            "할 일의 시간이 끝났습니다. 일하기 탭에서 끝냈는지 보고하세요(끝냈다, 연장하기, 못 끝냄 등)"
                .to_string(),
        ),
        TaskLock::Paused => (
            LockdownCause::Paused,
            "할 일을 일시 정지했습니다. 이어서 하려면 일하기 탭에서 \"다시 시작\"을 누르세요"
                .to_string(),
        ),
    };
    Some(Lockdown {
        cause,
        reason,
        fix_now: Vec::new(),
        browsers_allowed: false,
        covers_screen: true,
    })
}

/// Programs every lockdown leaves running besides the enforcer's own never-terminate list:
/// the Windows settings (network), the web view the Start menu and widgets use, and security
/// helpers. A broken connection also leaves browsers, whose tabs the monitor closes except
/// Claude and Anthropic pages (`ai_focus_rule::lockdown_allows_page`). Claude Desktop is
/// closed: it can browse the web, so it would be a way around the lock, and fixing the key
/// needs only the Claude Console in a browser (the owner's request).
pub const LOCKDOWN_KEEPS: [&str; 7] = [
    "systemsettings.exe",
    "shellhost.exe",
    "msedgewebview2.exe",
    "securityhealthsystray.exe",
    "smartscreen.exe",
    "taskmgr.exe",
    "useroobebroker.exe",
];

/// Claude Desktop's program (its helper processes have the same name). No lockdown spares it;
/// the update guard does.
pub const CLAUDE_DESKTOP: &str = "claude.exe";

/// Whether `process_name` is Claude Desktop.
pub fn is_claude_desktop(process_name: &str) -> bool {
    normalize_process_name(process_name) == CLAUDE_DESKTOP
}

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

/// The Focus app's program: while it runs, a lock that covers the screen closes nothing but
/// a program held in front of it.
pub const FOCUS_APP: &str = "digital-wellbeing-focus.exe";

/// How long a program may stay in front of the covering Focus app before it is closed.
pub const COVER_GRACE: Duration = Duration::from_secs(5);

/// The program in front: process id and name.
pub type ForegroundSource = Arc<dyn Fn() -> Option<(u32, String)> + Send + Sync>;

/// Responsibility: Ask the enforcer to close what a lockdown does not allow: while the Focus
/// app covers the screen, only a program kept in front of it for [`COVER_GRACE`]; otherwise
/// (a broken connection, or the Focus app is not running) every program the lockdown does
/// not spare.
pub struct LockdownRule {
    /// The lockdown in force now.
    active: Arc<dyn Fn() -> Option<Lockdown> + Send + Sync>,
    settings: AiFocusStore,
    /// Close programs. Off on Linux, where the agent's "session" is every process of the
    /// user, the developer's tools included.
    closes_programs: bool,
    foreground: ForegroundSource,
    /// A program in front of the covering Focus app, and since when.
    in_front_since: Mutex<Option<(u32, Instant)>>,
}

impl LockdownRule {
    pub fn new(
        active: Arc<dyn Fn() -> Option<Lockdown> + Send + Sync>,
        settings: AiFocusStore,
        closes_programs: bool,
        foreground: ForegroundSource,
    ) -> Self {
        Self {
            active,
            settings,
            closes_programs,
            foreground,
            in_front_since: Mutex::new(None),
        }
    }

    /// The last resort while the Focus app covers the screen: a program the lockdown does not
    /// spare that has stayed in front (Alt+Tab, a window kept on top) for [`COVER_GRACE`].
    fn held_in_front(
        &self,
        processes: &[ProcessSnapshot],
        config: &ai_focus_rule::AiFocusConfig,
        lockdown: &Lockdown,
    ) -> Vec<BlockEvent> {
        let mut in_front_since = self.in_front_since.lock();
        let Some((process_id, _)) = (self.foreground)() else {
            *in_front_since = None;
            return Vec::new();
        };
        let Some(process) = processes
            .iter()
            .find(|process| process.pid == process_id)
            .filter(|process| !spared(process, |name| config.is_always_allowed(name)))
        else {
            *in_front_since = None;
            return Vec::new();
        };
        match *in_front_since {
            Some((front, since)) if front == process_id => {
                if since.elapsed() >= COVER_GRACE {
                    *in_front_since = None;
                    return vec![BlockEvent::new(
                        process,
                        format!(
                            "모든 앱이 잠긴 동안 Focus 앱을 가리고 앞에 있어서 닫음: {}",
                            lockdown.reason
                        ),
                    )];
                }
            }
            _ => *in_front_since = Some((process_id, Instant::now())),
        }
        Vec::new()
    }
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
         (Claude pages only) is closed, Claude Desktop included. In the other locks (no task, \
         stopped, login) the Focus app covers the screen and only a program kept in front of it \
         is closed (every program, when the Focus app is not running)."
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        if !self.closes_programs {
            return Vec::new();
        }
        let Some(lockdown) = (self.active)() else {
            *self.in_front_since.lock() = None;
            return Vec::new();
        };
        let config = self.settings.get();
        let focus_app_runs = processes
            .iter()
            .any(|process| normalize_process_name(&process.name) == FOCUS_APP);
        if lockdown.covers_screen && focus_app_runs {
            return self.held_in_front(processes, &config, &lockdown);
        }
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

    /// What a lockdown leaves running (Windows' own programs, always-allowed programs,
    /// browsers) is never closed as another program's helper.
    fn keeps(&self, process: &ProcessSnapshot) -> bool {
        let config = self.settings.get();
        spared(process, |name| {
            config.is_always_allowed(name) || config.is_browser(name)
        })
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
        assert_eq!(model.fix_now.len(), 3, "the key and both models");
        let waiting = lockdown(true, &Connection::Working, Some(TaskLock::WaitingForTask)).unwrap();
        assert_eq!(waiting.cause, LockdownCause::NoTask);
        assert!(!waiting.browsers_allowed);
        let login = lockdown(true, &Connection::Working, Some(TaskLock::LoginRequired)).unwrap();
        assert_eq!(login.cause, LockdownCause::LoginRequired);
        assert!(!login.browsers_allowed);
        assert!(lockdown(false, &Connection::Working, Some(TaskLock::LoginRequired)).is_none());
        // Stopping locks in strict mode, before a broken connection; outside it, nothing.
        let stopped = lockdown(true, &broken, Some(TaskLock::Stopped)).unwrap();
        assert_eq!(stopped.cause, LockdownCause::Stopped);
        assert!(!stopped.browsers_allowed);
        assert!(lockdown(false, &broken, Some(TaskLock::Stopped)).is_none());
        // A paused task covers the screen in strict mode, like waiting for a task.
        let paused = lockdown(true, &Connection::Working, Some(TaskLock::Paused)).unwrap();
        assert_eq!(paused.cause, LockdownCause::Paused);
        assert!(paused.covers_screen && !paused.browsers_allowed);
        assert!(lockdown(false, &Connection::Working, Some(TaskLock::Paused)).is_none());
        // A task whose time is up covers the screen until it is reported on.
        let due = lockdown(true, &Connection::Working, Some(TaskLock::ReportDue)).unwrap();
        assert_eq!(due.cause, LockdownCause::ReportDue);
        assert!(due.covers_screen && !due.browsers_allowed);
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
        let rule = LockdownRule::new(
            Arc::new(move || locked.clone()),
            AiFocusStore::in_memory(config),
            true,
            Arc::new(|| None),
        );
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
        // Browsers stay; the monitor closes their tabs except Claude pages. Claude Desktop
        // closes (it can browse the web; the key is fixed in the Claude Console).
        assert_eq!(closed, ["steam.exe", "Claude.exe", "KakaoTalk.exe"]);
        assert!(
            !rule.keeps(&processes[2]),
            "not kept as another program's helper either"
        );

        // Waiting for a task while the Focus app does not run (nothing covers the screen):
        // everything closes as before, browsers too.
        let waiting = lockdown(true, &Connection::Working, Some(TaskLock::WaitingForTask));
        let rule = LockdownRule::new(
            Arc::new(move || waiting.clone()),
            AiFocusStore::in_memory(AiFocusConfig::default()),
            true,
            Arc::new(|| None),
        );
        let closed: Vec<String> = rule
            .evaluate(&processes)
            .into_iter()
            .map(|event| event.process_name)
            .collect();
        assert_eq!(
            closed,
            [
                "steam.exe",
                "chrome.exe",
                "Claude.exe",
                "KakaoTalk.exe",
                "KeePass.exe"
            ]
        );

        let off = LockdownRule::new(
            Arc::new(|| None),
            AiFocusStore::in_memory(AiFocusConfig::default()),
            true,
            Arc::new(|| None),
        );
        assert!(off.evaluate(&processes).is_empty());
    }

    #[test]
    fn while_the_focus_app_covers_the_screen_only_a_program_held_in_front_is_closed() {
        let waiting = lockdown(true, &Connection::Working, Some(TaskLock::WaitingForTask));
        assert!(waiting.as_ref().is_some_and(|locked| locked.covers_screen));
        let broken = lockdown(
            true,
            &Connection::Broken {
                problem: KeyProblem::NoKey,
                detail: String::new(),
            },
            None,
        );
        assert!(broken.is_some_and(|locked| !locked.covers_screen));
        let front: Arc<Mutex<Option<(u32, String)>>> = Arc::new(Mutex::new(None));
        let shown = Arc::clone(&front);
        let rule = LockdownRule::new(
            Arc::new(move || waiting.clone()),
            AiFocusStore::in_memory(AiFocusConfig::default()),
            true,
            Arc::new(move || shown.lock().clone()),
        );
        let with_id = |pid: u32, name: &str, exe: &str| ProcessSnapshot {
            pid,
            name: name.into(),
            exe: Some(exe.into()),
        };
        let processes = [
            with_id(
                10,
                "digital-wellbeing-focus.exe",
                r"C:\Program Files\DigitalWellbeing\digital-wellbeing-focus.exe",
            ),
            with_id(11, "steam.exe", r"C:\Program Files (x86)\Steam\steam.exe"),
            with_id(
                12,
                "WINWORD.EXE",
                r"C:\Program Files\Microsoft Office\WINWORD.EXE",
            ),
            with_id(13, "explorer.exe", r"C:\Windows\explorer.exe"),
        ];
        let in_front = |process_id: u32, name: &str| {
            *front.lock() = Some((process_id, name.to_string()));
        };
        let closed = |rule: &LockdownRule| -> Vec<String> {
            rule.evaluate(&processes)
                .into_iter()
                .map(|event| event.process_name)
                .collect()
        };
        // The Focus app in front: nothing is closed, Word and Steam stay open behind it.
        in_front(10, "digital-wellbeing-focus.exe");
        assert!(closed(&rule).is_empty());
        // Steam comes in front: not closed at once ...
        in_front(11, "steam.exe");
        assert!(closed(&rule).is_empty());
        // ... but once it has stayed in front for the grace time, it alone is closed.
        *rule.in_front_since.lock() = Some((11, Instant::now() - COVER_GRACE));
        assert_eq!(closed(&rule), ["steam.exe"]);
        // Another program in front starts its own grace time.
        in_front(12, "WINWORD.EXE");
        assert!(closed(&rule).is_empty());
        // Windows' own programs (the taskbar) are never closed.
        in_front(13, "explorer.exe");
        *rule.in_front_since.lock() = Some((13, Instant::now() - COVER_GRACE));
        assert!(closed(&rule).is_empty());
        // Without the Focus app nothing covers the screen: everything closes as before.
        let without_focus_app = &processes[1..];
        let events = rule.evaluate(without_focus_app);
        assert_eq!(events.len(), 2, "Steam and Word");
    }
}

use crate::config::{AiFocusConfig, AiFocusStore};
use crate::judge::{ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, Verdict};
use crate::platform::{Desktop, Foreground, Screenshot};
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use enforcer::{is_never_terminate, normalize_process_name};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use tracing::{info, warn};

/// How often the foreground window is sampled for the activity history.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
const HISTORY_CAPACITY: usize = 30;
const VERDICT_LOG_CAPACITY: usize = 100;
const MAX_TITLE_CHARS: usize = 120;
const SCREENSHOT_MAX_WIDTH: u32 = 1280;

/// One stretch of time a window stayed in front.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityEntry {
    pub process_name: String,
    pub title: String,
    pub since: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerdictLogEntry {
    pub at: DateTime<Utc>,
    pub process_name: String,
    pub title: String,
    pub verdict: Verdict,
    pub reason: String,
    /// What the monitor did about it ("", "strike 1/2", "blocked", "tab closed").
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockedEntry {
    pub reason: String,
    pub until: Option<DateTime<Utc>>,
}

/// What a judgement asks the monitor to do right away.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Process is now on the blocked list; the enforcer kills it on its next tick.
    BlockProcess(String),
    CloseTab,
}

/// Pure focus-session state; no I/O, so it is unit-testable.
#[derive(Debug, Default)]
pub struct MonitorState {
    pub task: String,
    pub history: VecDeque<ActivityEntry>,
    pub verdicts: VecDeque<VerdictLogEntry>,
    strikes: HashMap<String, u32>,
    /// Normalized process name -> block.
    pub blocked_processes: HashMap<String, BlockedEntry>,
    /// Browser "process|title" -> block.
    pub blocked_tabs: HashMap<String, BlockedEntry>,
    pub last_check: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

impl MonitorState {
    /// Start over for a new task (or a stopped session).
    pub fn reset(&mut self, task: &str) {
        *self = Self {
            task: task.to_string(),
            history: std::mem::take(&mut self.history),
            verdicts: std::mem::take(&mut self.verdicts),
            ..Default::default()
        };
    }

    pub fn record(&mut self, now: DateTime<Utc>, fg: &Foreground) {
        let title = truncate(&fg.title, MAX_TITLE_CHARS);
        if let Some(last) = self.history.back_mut() {
            if last.process_name == fg.process_name && last.title == title {
                last.last_seen = now;
                return;
            }
        }
        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(ActivityEntry {
            process_name: fg.process_name.clone(),
            title,
            since: now,
            last_seen: now,
        });
    }

    pub fn history_text(&self) -> String {
        self.history
            .iter()
            .map(|e| {
                let secs = (e.last_seen - e.since).num_seconds().max(0);
                format!(
                    "[{}] {} — \"{}\" ({}m {}s)",
                    e.since.with_timezone(&Local).format("%H:%M:%S"),
                    e.process_name,
                    e.title,
                    secs / 60,
                    secs % 60
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn check_due(&self, now: DateTime<Utc>, interval_secs: u64) -> bool {
        match self.last_check {
            None => true,
            Some(at) => now - at >= ChronoDuration::seconds(interval_secs as i64),
        }
    }

    pub fn tab_blocked(&self, now: DateTime<Utc>, fg: &Foreground) -> bool {
        self.blocked_tabs
            .get(&tab_key(fg))
            .is_some_and(|b| b.until.is_none_or(|u| now < u))
    }

    /// Currently blocked process names with their reasons (expired blocks dropped).
    pub fn active_process_blocks(&mut self, now: DateTime<Utc>) -> Vec<(String, String)> {
        self.blocked_processes
            .retain(|_, b| b.until.is_none_or(|u| now < u));
        self.blocked_tabs.retain(|_, b| b.until.is_none_or(|u| now < u));
        self.blocked_processes
            .iter()
            .map(|(name, b)| (name.clone(), b.reason.clone()))
            .collect()
    }

    pub fn apply_judgement(
        &mut self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        fg: &Foreground,
        judgement: Judgement,
    ) -> Action {
        let is_browser = config.is_browser(&fg.process_name);
        let key = if is_browser {
            tab_key(fg)
        } else {
            normalize_process_name(&fg.process_name)
        };

        let mut action = Action::None;
        let mut action_label = String::new();
        match judgement.verdict {
            Verdict::OnTask => {
                self.strikes.remove(&key);
            }
            Verdict::Unsure => {}
            Verdict::OffTask => {
                let needed = config.strikes_to_block.max(1);
                let strikes = self.strikes.entry(key.clone()).or_insert(0);
                *strikes += 1;
                if *strikes >= needed {
                    self.strikes.remove(&key);
                    let until = (config.block_minutes > 0)
                        .then(|| now + ChronoDuration::minutes(config.block_minutes as i64));
                    let entry = BlockedEntry {
                        reason: judgement.reason.clone(),
                        until,
                    };
                    if is_browser {
                        self.blocked_tabs.insert(key, entry);
                        action = Action::CloseTab;
                        action_label = "tab closed".into();
                    } else {
                        self.blocked_processes.insert(key.clone(), entry);
                        action = Action::BlockProcess(key);
                        action_label = "blocked".into();
                    }
                } else {
                    action_label = format!("strike {strikes}/{needed}");
                }
            }
        }

        if self.verdicts.len() >= VERDICT_LOG_CAPACITY {
            self.verdicts.pop_front();
        }
        self.verdicts.push_back(VerdictLogEntry {
            at: now,
            process_name: fg.process_name.clone(),
            title: truncate(&fg.title, MAX_TITLE_CHARS),
            verdict: judgement.verdict,
            reason: judgement.reason,
            action: action_label,
        });
        action
    }

    pub fn unblock(&mut self, key: &str) {
        self.blocked_processes.remove(key);
        self.blocked_tabs.remove(key);
    }
}

pub fn tab_key(fg: &Foreground) -> String {
    format!(
        "{}|{}",
        normalize_process_name(&fg.process_name),
        truncate(&fg.title, MAX_TITLE_CHARS)
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).chain(['…']).collect()
    }
}

pub type JudgeProvider =
    Arc<dyn Fn(&AiFocusConfig) -> Result<Arc<dyn Judge>, JudgeError> + Send + Sync>;

/// Claude judge built from the current config (key and model may change at runtime).
pub fn claude_judge_provider() -> JudgeProvider {
    Arc::new(|config: &AiFocusConfig| {
        let key = config.resolve_api_key().ok_or(JudgeError::NoApiKey)?;
        Ok(Arc::new(ClaudeJudge::new(key, config.model.clone())) as Arc<dyn Judge>)
    })
}

/// Background worker: samples the foreground window, asks the judge on schedule,
/// and keeps the block lists the [`crate::AiFocusRule`] reports to the enforcer.
pub struct FocusMonitor {
    store: AiFocusStore,
    desktop: Box<dyn Desktop>,
    judge: JudgeProvider,
    state: Mutex<MonitorState>,
    running: AtomicBool,
}

impl FocusMonitor {
    pub fn new(store: AiFocusStore, desktop: Box<dyn Desktop>, judge: JudgeProvider) -> Self {
        Self {
            store,
            desktop,
            judge,
            state: Mutex::new(MonitorState::default()),
            running: AtomicBool::new(false),
        }
    }

    pub fn store(&self) -> &AiFocusStore {
        &self.store
    }

    pub fn state(&self) -> parking_lot::MutexGuard<'_, MonitorState> {
        self.state.lock()
    }

    pub fn start_background(self: &Arc<Self>) -> JoinHandle<()> {
        let this = Arc::clone(self);
        this.running.store(true, Ordering::SeqCst);
        thread::spawn(move || {
            info!("ai focus monitor started");
            while this.running.load(Ordering::SeqCst) {
                this.step(Utc::now());
                thread::sleep(SAMPLE_INTERVAL);
            }
            info!("ai focus monitor stopped");
        })
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }

    /// One sample; asks the judge too when a check is due.
    pub fn step(&self, now: DateTime<Utc>) {
        let config = self.store.get();
        {
            let mut state = self.state.lock();
            let task = if config.session_active {
                config.task.trim()
            } else {
                ""
            };
            if state.task != task {
                state.reset(task);
            }
            if task.is_empty() {
                return;
            }
        }

        let Some(fg) = self.desktop.foreground() else {
            return;
        };
        let exempt = fg.pid == std::process::id()
            || is_never_terminate(&fg.process_name)
            || config.is_always_allowed(&fg.process_name);

        let due = {
            let mut state = self.state.lock();
            state.record(now, &fg);
            if !exempt && config.is_browser(&fg.process_name) && state.tab_blocked(now, &fg) {
                drop(state);
                if self.close_tab_if_unchanged(&fg) {
                    info!("closed blocked tab \"{}\"", fg.title);
                }
                return;
            }
            !exempt && state.check_due(now, config.check_interval_secs)
        };
        if !due {
            return;
        }

        self.state.lock().last_check = Some(now);
        let result = self.run_judge(&config, &fg);
        let mut state = self.state.lock();
        match result {
            Ok(judgement) => {
                state.last_error = None;
                info!(
                    "{} \"{}\": {:?} ({})",
                    fg.process_name, fg.title, judgement.verdict, judgement.reason
                );
                let action = state.apply_judgement(now, &config, &fg, judgement);
                drop(state);
                if action == Action::CloseTab && !self.close_tab_if_unchanged(&fg) {
                    warn!("did not close tab of {}: window or tab changed", fg.process_name);
                }
            }
            Err(err) => {
                warn!("focus check failed: {err}");
                state.last_error = Some(err.to_string());
            }
        }
    }

    /// Ctrl+W hits whatever tab is active *now*. The judge call can take seconds,
    /// so only close when the same window still shows the same title; otherwise
    /// the user switched tabs and we would close the wrong one. The block stays
    /// recorded, so the tab is closed the next time it comes to the front.
    fn close_tab_if_unchanged(&self, judged: &Foreground) -> bool {
        let still_same = self
            .desktop
            .foreground()
            .is_some_and(|now| now.window == judged.window && now.title == judged.title);
        still_same && self.desktop.close_active_tab(judged)
    }

    fn run_judge(&self, config: &AiFocusConfig, fg: &Foreground) -> Result<Judgement, JudgeError> {
        let judge = (self.judge)(config)?;
        let screenshot = if config.send_screenshot && !config.is_private(&fg.process_name) {
            self.desktop.screenshot(fg).and_then(|s| encode_jpeg(&s))
        } else {
            None
        };
        let history = self.state.lock().history_text();
        judge.judge(&JudgeRequest {
            task: &config.task,
            history: &history,
            process_name: &fg.process_name,
            title: &fg.title,
            screenshot: screenshot.as_deref(),
        })
    }
}

/// Downscale to at most [`SCREENSHOT_MAX_WIDTH`] wide and encode as JPEG.
fn encode_jpeg(shot: &Screenshot) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{imageops::FilterType, DynamicImage, RgbaImage};

    let rgba = RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone())?;
    let mut img = DynamicImage::ImageRgba8(rgba);
    if img.width() > SCREENSHOT_MAX_WIDTH {
        let height = img.height() * SCREENSHOT_MAX_WIDTH / img.width();
        img = img.resize(SCREENSHOT_MAX_WIDTH, height, FilterType::Triangle);
    }
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 70)
        .encode_image(&img.to_rgb8())
        .ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct FakeDesktop {
        fg: Arc<Mutex<Option<Foreground>>>,
        tabs_closed: Arc<AtomicUsize>,
    }

    impl Desktop for FakeDesktop {
        fn foreground(&self) -> Option<Foreground> {
            self.fg.lock().clone()
        }
        fn screenshot(&self, _fg: &Foreground) -> Option<Screenshot> {
            Some(Screenshot {
                width: 2560,
                height: 1440,
                rgba: vec![128; 2560 * 1440 * 4],
            })
        }
        fn close_active_tab(&self, _fg: &Foreground) -> bool {
            self.tabs_closed.fetch_add(1, Ordering::SeqCst);
            true
        }
    }

    struct FixedJudge {
        verdict: Verdict,
        /// Foreground of the fake desktop, and what to switch it to mid-call.
        fg: Arc<Mutex<Option<Foreground>>>,
        switch_to: Arc<Mutex<Option<Foreground>>>,
        calls: Arc<AtomicUsize>,
        saw_screenshot: Arc<AtomicBool>,
    }

    impl Judge for FixedJudge {
        fn judge(&self, request: &JudgeRequest) -> Result<Judgement, JudgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(next) = self.switch_to.lock().take() {
                *self.fg.lock() = Some(next);
            }
            self.saw_screenshot
                .store(request.screenshot.is_some(), Ordering::SeqCst);
            Ok(Judgement {
                verdict: self.verdict,
                reason: "test".into(),
            })
        }
    }

    struct Harness {
        monitor: FocusMonitor,
        calls: Arc<AtomicUsize>,
        tabs_closed: Arc<AtomicUsize>,
        saw_screenshot: Arc<AtomicBool>,
        switch_to: Arc<Mutex<Option<Foreground>>>,
    }

    fn fg(process: &str, title: &str) -> Foreground {
        Foreground {
            window: 1,
            pid: 4242,
            process_name: process.into(),
            title: title.into(),
        }
    }

    fn harness(verdict: Verdict, current: Foreground, config: AiFocusConfig) -> Harness {
        let calls = Arc::new(AtomicUsize::new(0));
        let tabs_closed = Arc::new(AtomicUsize::new(0));
        let saw_screenshot = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(Some(current)));
        let switch_to = Arc::new(Mutex::new(None));
        let judge: Arc<dyn Judge> = Arc::new(FixedJudge {
            verdict,
            fg: Arc::clone(&current),
            switch_to: Arc::clone(&switch_to),
            calls: Arc::clone(&calls),
            saw_screenshot: Arc::clone(&saw_screenshot),
        });
        let monitor = FocusMonitor::new(
            AiFocusStore::in_memory(config),
            Box::new(FakeDesktop {
                fg: current,
                tabs_closed: Arc::clone(&tabs_closed),
            }),
            Arc::new(move |_| Ok(Arc::clone(&judge))),
        );
        Harness {
            monitor,
            calls,
            tabs_closed,
            saw_screenshot,
            switch_to,
        }
    }

    fn session(task: &str) -> AiFocusConfig {
        AiFocusConfig {
            session_active: true,
            task: task.into(),
            check_interval_secs: 60,
            strikes_to_block: 2,
            ..Default::default()
        }
    }

    #[test]
    fn blocks_app_after_strikes() {
        let h = harness(Verdict::OffTask, fg("steam.exe", "Steam"), session("write thesis"));
        let t0 = Utc::now();
        h.monitor.step(t0);
        assert!(h.monitor.state().blocked_processes.is_empty());
        // Not due yet: sampled, but not judged again.
        h.monitor.step(t0 + ChronoDuration::seconds(5));
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        h.monitor.step(t0 + ChronoDuration::seconds(61));
        assert_eq!(h.calls.load(Ordering::SeqCst), 2);
        let blocks = h
            .monitor
            .state()
            .active_process_blocks(t0 + ChronoDuration::seconds(62));
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "steam.exe");
        // Expires after block_minutes.
        let later = t0 + ChronoDuration::minutes(31) + ChronoDuration::seconds(61);
        assert!(h.monitor.state().active_process_blocks(later).is_empty());
    }

    #[test]
    fn on_task_never_blocks_and_screenshot_is_sent() {
        let h = harness(Verdict::OnTask, fg("code.exe", "main.rs"), session("coding"));
        let t0 = Utc::now();
        for i in 0..5 {
            h.monitor.step(t0 + ChronoDuration::seconds(61 * i));
        }
        assert_eq!(h.calls.load(Ordering::SeqCst), 5);
        assert!(h.saw_screenshot.load(Ordering::SeqCst));
        assert!(h.monitor.state().active_process_blocks(t0).is_empty());
    }

    #[test]
    fn browser_tab_is_closed_and_stays_closed() {
        let mut config = session("write thesis");
        config.strikes_to_block = 1;
        let h = harness(Verdict::OffTask, fg("chrome.exe", "Funny cats - YouTube"), config);
        let t0 = Utc::now();
        h.monitor.step(t0);
        assert_eq!(h.tabs_closed.load(Ordering::SeqCst), 1);
        assert!(h.monitor.state().blocked_processes.is_empty());
        // Reopened: closed on the next sample without asking the judge.
        h.monitor.step(t0 + ChronoDuration::seconds(5));
        assert_eq!(h.tabs_closed.load(Ordering::SeqCst), 2);
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tab_is_not_closed_if_user_switched_tabs_meanwhile() {
        let mut config = session("write thesis");
        config.strikes_to_block = 1;
        let h = harness(Verdict::OffTask, fg("chrome.exe", "Funny cats - YouTube"), config);
        // While the judge is "thinking", the user switches to another tab
        // in the same browser window.
        *h.switch_to.lock() = Some(fg("chrome.exe", "Overleaf - thesis.tex"));
        h.monitor.step(Utc::now());
        assert_eq!(h.tabs_closed.load(Ordering::SeqCst), 0);
        // The block is still recorded for the next time that tab is in front.
        assert_eq!(h.monitor.state().blocked_tabs.len(), 1);
    }

    #[test]
    fn idle_without_session_and_skips_exempt_apps() {
        let h = harness(Verdict::OffTask, fg("steam.exe", "Steam"), AiFocusConfig::default());
        h.monitor.step(Utc::now());
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);

        let mut config = session("x");
        config.always_allow.insert("steam.exe".into());
        let h = harness(Verdict::OffTask, fg("steam.exe", "Steam"), config);
        h.monitor.step(Utc::now());
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
        assert_eq!(h.monitor.state().history.len(), 1);
    }

    #[test]
    fn private_apps_are_not_screenshotted() {
        let h = harness(Verdict::OnTask, fg("KeePass.exe", "db"), session("x"));
        h.monitor.step(Utc::now());
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
        assert!(!h.saw_screenshot.load(Ordering::SeqCst));
    }

    #[test]
    fn changing_task_clears_blocks() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let h = harness(Verdict::OffTask, fg("steam.exe", "Steam"), config);
        let t0 = Utc::now();
        h.monitor.step(t0);
        assert_eq!(h.monitor.state().blocked_processes.len(), 1);
        h.monitor
            .store()
            .update(|c| c.task = "play games".into())
            .unwrap();
        h.monitor.step(t0 + ChronoDuration::seconds(1));
        // Reset, then judged again under the new task (fixed judge blocks again).
        assert_eq!(h.calls.load(Ordering::SeqCst), 2);
        assert_eq!(h.monitor.state().task, "play games");
    }

    #[test]
    fn history_merges_consecutive_samples() {
        let mut state = MonitorState::default();
        let t0 = Utc::now();
        state.record(t0, &fg("code.exe", "a"));
        state.record(t0 + ChronoDuration::seconds(90), &fg("code.exe", "a"));
        state.record(t0 + ChronoDuration::seconds(95), &fg("chrome.exe", "b"));
        assert_eq!(state.history.len(), 2);
        assert!(state.history_text().contains("(1m 30s)"));
    }

    #[test]
    fn screenshot_is_downscaled_jpeg() {
        let shot = Screenshot {
            width: 2560,
            height: 1440,
            rgba: vec![200; 2560 * 1440 * 4],
        };
        let jpeg = encode_jpeg(&shot).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.width(), 1280);
        assert_eq!(decoded.height(), 720);
    }
}

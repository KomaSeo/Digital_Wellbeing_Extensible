use crate::access_review::{
    claude_reviewer_provider, granted_length, AccessRequest, ReviewerProvider,
};
use crate::config::{AiFocusConfig, AiFocusStore};
use crate::grants::{Grant, GrantDuration, GrantEvent, GrantLogEntry, GrantState, GrantTarget};
use crate::judge::{ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, Verdict};
use crate::platform::{Desktop, Foreground, Screenshot};
use crate::store::JsonStore;
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use enforcer::{is_never_terminate, normalize_process_name};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;
use thiserror::Error;
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
        self.blocked_tabs
            .retain(|_, b| b.until.is_none_or(|u| now < u));
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
        grant: Option<&Grant>,
    ) -> Action {
        let is_browser = config.is_browser(&fg.process_name);
        let block_key = if is_browser {
            tab_key(fg)
        } else {
            normalize_process_name(&fg.process_name)
        };
        // Strikes on a granted app/site count per grant, so switching videos
        // (= tab titles) on a granted site does not reset them.
        let key = match grant {
            Some(g) => format!("grant:{}", g.id),
            None => block_key.clone(),
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
                        self.blocked_tabs.insert(block_key, entry);
                        action = Action::CloseTab;
                        action_label = "tab closed".into();
                    } else {
                        self.blocked_processes.insert(block_key.clone(), entry);
                        action = Action::BlockProcess(block_key);
                        action_label = "blocked".into();
                    }
                    if grant.is_some() {
                        action_label = format!("access revoked, {action_label}");
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

    /// Lift every block and strike a new grant covers.
    pub fn unblock_matching(&mut self, target: &GrantTarget) {
        self.blocked_processes
            .retain(|key, _| !target.matches_block_key(key));
        self.blocked_tabs
            .retain(|key, _| !target.matches_block_key(key));
        self.strikes.retain(|key, _| !target.matches_block_key(key));
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

/// Shortest accepted purpose, so "ok" or "pls" never reaches the AI.
const MIN_PURPOSE_CHARS: usize = 4;

#[derive(Debug, Error)]
pub enum AccessError {
    #[error("start a focus session first")]
    NoSession,
    #[error("name the app or site")]
    EmptyTarget,
    #[error("say what you need it for (at least {MIN_PURPOSE_CHARS} characters)")]
    PurposeTooShort,
    #[error("the admin does not allow permanent access")]
    PermanentNotAllowed,
    #[error("another request is still being processed")]
    Busy,
    #[error("AI: {0}")]
    Ai(#[from] JudgeError),
    #[error("could not save: {0}")]
    Io(#[from] std::io::Error),
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
    /// Access grants (`grants.json`); see `grants.rs`.
    grants: JsonStore<GrantState>,
    reviewer: ReviewerProvider,
    /// Held for a whole access request, so two clicks cannot race.
    access_busy: Mutex<()>,
    running: AtomicBool,
}

impl FocusMonitor {
    /// Grants start in memory; use [`Self::with_grants`] to keep them in a file.
    pub fn new(store: AiFocusStore, desktop: Box<dyn Desktop>, judge: JudgeProvider) -> Self {
        Self {
            store,
            desktop,
            judge,
            state: Mutex::new(MonitorState::default()),
            grants: JsonStore::in_memory(GrantState::default()),
            reviewer: claude_reviewer_provider(),
            access_busy: Mutex::new(()),
            running: AtomicBool::new(false),
        }
    }

    pub fn with_grants(mut self, grants: JsonStore<GrantState>) -> Self {
        self.grants = grants;
        self
    }

    pub fn with_reviewer(mut self, reviewer: ReviewerProvider) -> Self {
        self.reviewer = reviewer;
        self
    }

    pub fn grants(&self) -> &JsonStore<GrantState> {
        &self.grants
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

        self.tidy_grants(now, &config);

        let Some(fg) = self.desktop.foreground() else {
            return;
        };
        let grant = self
            .grants
            .get()
            .covering(now, &config, &fg.process_name, &fg.title)
            .cloned();
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
        let result = self.run_judge(&config, &fg, grant.as_ref());
        let mut state = self.state.lock();
        match result {
            Ok(judgement) => {
                state.last_error = None;
                info!(
                    "{} \"{}\": {:?} ({})",
                    fg.process_name, fg.title, judgement.verdict, judgement.reason
                );
                let reason = judgement.reason.clone();
                let action = state.apply_judgement(now, &config, &fg, judgement, grant.as_ref());
                drop(state);
                if action != Action::None {
                    if let Some(grant) = &grant {
                        self.revoke_for_misuse(now, grant, &reason);
                    }
                }
                if action == Action::CloseTab && !self.close_tab_if_unchanged(&fg) {
                    warn!(
                        "did not close tab of {}: window or tab changed",
                        fg.process_name
                    );
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

    /// The user asks for an app or site. The AI decides; an approved request
    /// is stored and lifts matching blocks right away. Observation continues
    /// (the judge then weighs use against `purpose`).
    pub fn request_access(
        &self,
        now: DateTime<Utc>,
        target: GrantTarget,
        duration: GrantDuration,
        purpose: &str,
    ) -> Result<GrantLogEntry, AccessError> {
        let _busy = self.access_busy.try_lock().ok_or(AccessError::Busy)?;
        let config = self.store.get();
        if !config.session_active || config.task.trim().is_empty() {
            return Err(AccessError::NoSession);
        }
        let target = target.normalized();
        if target.is_empty() {
            return Err(AccessError::EmptyTarget);
        }
        let purpose = purpose.trim();
        if purpose.chars().count() < MIN_PURPOSE_CHARS {
            return Err(AccessError::PurposeTooShort);
        }
        if duration == GrantDuration::Permanent && !config.allow_permanent_grants {
            return Err(AccessError::PermanentNotAllowed);
        }

        let recent: Vec<GrantLogEntry> = self.grants.get().log.iter().rev().cloned().collect();
        let history = self.state.lock().history_text();
        let reviewer = (self.reviewer)(&config)?;
        let decision = reviewer.review(&AccessRequest {
            task: &config.task,
            target: &target,
            duration,
            purpose,
            history: &history,
            recent: &recent,
        })?;

        let length = granted_length(duration, &decision, &config);
        let entry = GrantLogEntry {
            at: now,
            target: target.clone(),
            purpose: purpose.to_string(),
            event: GrantEvent::Requested {
                duration,
                approved: length.is_some(),
                granted_minutes: length.flatten(),
            },
            message: decision.reply.clone(),
        };
        let grant = length.map(|minutes| Grant {
            id: format!("g{}", now.timestamp_millis()),
            target: target.clone(),
            purpose: purpose.to_string(),
            until: minutes.map(|m| now + ChronoDuration::minutes(m as i64)),
            granted_at: now,
            task: config.task.clone(),
        });
        self.grants.update(|s| {
            if let Some(grant) = grant.clone() {
                s.grants.push(grant);
            }
            s.push_log(entry.clone());
        })?;
        if grant.is_some() {
            info!("access granted: {} ({purpose})", target.label());
            self.state.lock().unblock_matching(&target);
        }
        Ok(entry)
    }

    /// Drop expired grants and those the admin revoked (logged).
    fn tidy_grants(&self, now: DateTime<Utc>, config: &AiFocusConfig) {
        let state = self.grants.get();
        if state.grants.iter().all(|g| g.is_active(now, config)) {
            return;
        }
        let result = self.grants.update(|s| {
            let (keep, ended): (Vec<_>, Vec<_>) = std::mem::take(&mut s.grants)
                .into_iter()
                .partition(|g| g.is_active(now, config));
            s.grants = keep;
            for g in ended {
                if config.revoked_grants.contains(&g.id) {
                    s.push_log(GrantLogEntry {
                        at: now,
                        target: g.target,
                        purpose: g.purpose,
                        event: GrantEvent::RevokedByAdmin,
                        message: String::new(),
                    });
                }
            }
        });
        if let Err(err) = result {
            warn!("could not save grants: {err}");
        }
    }

    fn revoke_for_misuse(&self, now: DateTime<Utc>, grant: &Grant, reason: &str) {
        info!("access to {} revoked: {reason}", grant.target.label());
        let result = self.grants.update(|s| {
            s.grants.retain(|g| g.id != grant.id);
            s.push_log(GrantLogEntry {
                at: now,
                target: grant.target.clone(),
                purpose: grant.purpose.clone(),
                event: GrantEvent::RevokedForMisuse,
                message: reason.to_string(),
            });
        });
        if let Err(err) = result {
            warn!("could not save grants: {err}");
        }
    }

    fn run_judge(
        &self,
        config: &AiFocusConfig,
        fg: &Foreground,
        grant: Option<&Grant>,
    ) -> Result<Judgement, JudgeError> {
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
            grant_purpose: grant.map(|g| g.purpose.as_str()),
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
        verdict: Arc<Mutex<Verdict>>,
        saw_grant: Arc<Mutex<Option<String>>>,
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
            *self.saw_grant.lock() = request.grant_purpose.map(str::to_string);
            Ok(Judgement {
                verdict: *self.verdict.lock(),
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
        verdict: Arc<Mutex<Verdict>>,
        saw_grant: Arc<Mutex<Option<String>>>,
        current: Arc<Mutex<Option<Foreground>>>,
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
        let verdict = Arc::new(Mutex::new(verdict));
        let saw_grant = Arc::new(Mutex::new(None));
        let judge: Arc<dyn Judge> = Arc::new(FixedJudge {
            verdict: Arc::clone(&verdict),
            saw_grant: Arc::clone(&saw_grant),
            fg: Arc::clone(&current),
            switch_to: Arc::clone(&switch_to),
            calls: Arc::clone(&calls),
            saw_screenshot: Arc::clone(&saw_screenshot),
        });
        let monitor = FocusMonitor::new(
            AiFocusStore::in_memory(config),
            Box::new(FakeDesktop {
                fg: Arc::clone(&current),
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
            verdict,
            saw_grant,
            current,
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
        let h = harness(
            Verdict::OffTask,
            fg("steam.exe", "Steam"),
            session("write thesis"),
        );
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
        let h = harness(
            Verdict::OnTask,
            fg("code.exe", "main.rs"),
            session("coding"),
        );
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
        let h = harness(
            Verdict::OffTask,
            fg("chrome.exe", "Funny cats - YouTube"),
            config,
        );
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
        let h = harness(
            Verdict::OffTask,
            fg("chrome.exe", "Funny cats - YouTube"),
            config,
        );
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
        let h = harness(
            Verdict::OffTask,
            fg("steam.exe", "Steam"),
            AiFocusConfig::default(),
        );
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

    // --- access grants ---------------------------------------------------------

    use crate::access_review::{AccessDecision, AccessReviewer};

    /// Approves or rejects every request; counts calls.
    struct FixedReviewer {
        approve: bool,
        minutes: u32,
        calls: Arc<AtomicUsize>,
    }

    impl AccessReviewer for FixedReviewer {
        fn review(&self, _r: &AccessRequest) -> Result<AccessDecision, JudgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(AccessDecision {
                approved: self.approve,
                minutes: self.minutes,
                reply: if self.approve { "ok" } else { "no" }.into(),
            })
        }
    }

    fn with_reviewer(h: Harness, approve: bool, minutes: u32) -> (Harness, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let reviewer: Arc<dyn AccessReviewer> = Arc::new(FixedReviewer {
            approve,
            minutes,
            calls: Arc::clone(&calls),
        });
        let Harness {
            monitor,
            calls: judge_calls,
            tabs_closed,
            saw_screenshot,
            switch_to,
            verdict,
            saw_grant,
            current,
        } = h;
        let monitor = monitor.with_reviewer(Arc::new(move |_| Ok(Arc::clone(&reviewer))));
        (
            Harness {
                monitor,
                calls: judge_calls,
                tabs_closed,
                saw_screenshot,
                switch_to,
                verdict,
                saw_grant,
                current,
            },
            calls,
        )
    }

    fn site(keyword: &str) -> GrantTarget {
        GrantTarget::Site {
            keyword: keyword.into(),
        }
    }

    fn minutes(m: u32) -> GrantDuration {
        GrantDuration::Minutes { minutes: m }
    }

    #[test]
    fn access_needs_a_session_and_a_real_purpose() {
        let h = harness(
            Verdict::OnTask,
            fg("chrome.exe", "x"),
            AiFocusConfig::default(),
        );
        let (h, calls) = with_reviewer(h, true, 0);
        let r = h
            .monitor
            .request_access(Utc::now(), site("youtube"), minutes(30), "강의 시청");
        assert!(matches!(r, Err(AccessError::NoSession)));

        let h = harness(Verdict::OnTask, fg("chrome.exe", "x"), session("thesis"));
        let (h, calls2) = with_reviewer(h, true, 0);
        let r = h
            .monitor
            .request_access(Utc::now(), site("youtube"), minutes(30), "응");
        assert!(matches!(r, Err(AccessError::PurposeTooShort)));
        let r = h
            .monitor
            .request_access(Utc::now(), site(" "), minutes(30), "강의 시청");
        assert!(matches!(r, Err(AccessError::EmptyTarget)));
        assert_eq!(
            calls.load(Ordering::SeqCst) + calls2.load(Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn rejected_request_is_logged_and_lifts_nothing() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let h = harness(Verdict::OffTask, fg("chrome.exe", "Cats - YouTube"), config);
        let t0 = Utc::now();
        h.monitor.step(t0);
        assert_eq!(h.monitor.state().blocked_tabs.len(), 1);

        let (h, _) = with_reviewer(h, false, 0);
        let entry = h
            .monitor
            .request_access(t0, site("youtube"), minutes(30), "그냥 보고 싶어서")
            .unwrap();
        assert!(matches!(
            entry.event,
            GrantEvent::Requested {
                approved: false,
                ..
            }
        ));
        assert!(h.monitor.grants().get().grants.is_empty());
        assert_eq!(h.monitor.state().blocked_tabs.len(), 1);
    }

    #[test]
    fn approved_request_lifts_the_block_and_is_judged_by_purpose() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let h = harness(
            Verdict::OffTask,
            fg("chrome.exe", "Rust async - YouTube"),
            config,
        );
        let t0 = Utc::now();
        h.monitor.step(t0);
        assert_eq!(h.monitor.state().blocked_tabs.len(), 1);

        let (h, _) = with_reviewer(h, true, 0);
        h.monitor
            .request_access(t0, site("youtube"), minutes(30), "Rust async 강의 시청")
            .unwrap();
        assert!(h.monitor.state().blocked_tabs.is_empty());

        // Still observed: the next check sees the grant's purpose.
        *h.verdict.lock() = Verdict::OnTask;
        let closed_before = h.tabs_closed.load(Ordering::SeqCst);
        h.monitor.step(t0 + ChronoDuration::seconds(61));
        assert_eq!(h.saw_grant.lock().as_deref(), Some("Rust async 강의 시청"));
        assert_eq!(h.tabs_closed.load(Ordering::SeqCst), closed_before);
        assert_eq!(h.monitor.grants().get().grants.len(), 1);
    }

    #[test]
    fn off_purpose_use_revokes_the_grant_even_across_tab_titles() {
        let config = session("thesis"); // 2 strikes
        let h = harness(
            Verdict::OnTask,
            fg("chrome.exe", "Rust async - YouTube"),
            config,
        );
        let (h, _) = with_reviewer(h, true, 0);
        let t0 = Utc::now();
        h.monitor
            .request_access(t0, site("youtube"), minutes(60), "Rust async 강의 시청")
            .unwrap();

        // Two different videos, both off-purpose.
        *h.verdict.lock() = Verdict::OffTask;
        *h.current.lock() = Some(fg("chrome.exe", "Funny cats - YouTube"));
        h.monitor.step(t0 + ChronoDuration::seconds(1));
        assert_eq!(h.monitor.grants().get().grants.len(), 1);
        *h.current.lock() = Some(fg("chrome.exe", "Game trailer - YouTube"));
        h.monitor.step(t0 + ChronoDuration::seconds(62));

        let grants = h.monitor.grants().get();
        assert!(grants.grants.is_empty());
        assert_eq!(
            grants.log.back().map(|e| e.event.clone()),
            Some(GrantEvent::RevokedForMisuse)
        );
        assert_eq!(h.tabs_closed.load(Ordering::SeqCst), 1);
        let last = h.monitor.state().verdicts.back().cloned().unwrap();
        assert!(last.action.starts_with("access revoked"));
    }

    #[test]
    fn grants_expire_and_admin_can_revoke() {
        let h = harness(
            Verdict::OnTask,
            fg("code.exe", "main.rs"),
            session("thesis"),
        );
        let (h, _) = with_reviewer(h, true, 0);
        let t0 = Utc::now();
        let app = GrantTarget::App {
            process: "Spotify".into(),
        };
        h.monitor
            .request_access(t0, app.clone(), minutes(10), "집중용 음악 재생")
            .unwrap();
        h.monitor
            .request_access(t0, app, GrantDuration::Permanent, "업무용 음악 재생")
            .unwrap();
        assert_eq!(h.monitor.grants().get().grants.len(), 2);

        // Timed one runs out.
        h.monitor.step(t0 + ChronoDuration::minutes(11));
        let grants = h.monitor.grants().get().grants;
        assert_eq!(grants.len(), 1);
        assert!(grants[0].until.is_none());

        // Admin revokes the permanent one through the config.
        let id = grants[0].id.clone();
        h.monitor
            .store()
            .update(|c| {
                c.revoked_grants.insert(id);
            })
            .unwrap();
        h.monitor.step(t0 + ChronoDuration::minutes(12));
        let state = h.monitor.grants().get();
        assert!(state.grants.is_empty());
        assert_eq!(
            state.log.back().map(|e| e.event.clone()),
            Some(GrantEvent::RevokedByAdmin)
        );
    }

    #[test]
    fn permanent_requests_are_refused_when_the_admin_disallows_them() {
        let mut config = session("thesis");
        config.allow_permanent_grants = false;
        let h = harness(Verdict::OnTask, fg("code.exe", "x"), config);
        let (h, calls) = with_reviewer(h, true, 0);
        let r = h.monitor.request_access(
            Utc::now(),
            site("docs.rs"),
            GrantDuration::Permanent,
            "Rust 문서 참고",
        );
        assert!(matches!(r, Err(AccessError::PermanentNotAllowed)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

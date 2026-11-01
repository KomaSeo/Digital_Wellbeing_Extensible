//! Responsibility: Run the AI focus check: sample the foreground window, ask the judge on
//! schedule, keep strikes, blocks and grants, and handle access requests.

use crate::access_review::{
    claude_reviewer_provider, granted_length, AccessRequest, ReviewerProvider,
};
use crate::config::{AiFocusConfig, AiFocusStore};
use crate::grants::{Grant, GrantDuration, GrantEvent, GrantLogEntry, GrantState, GrantTarget};
use crate::judge::{
    ClaudeJudge, Judge, JudgeError, JudgeRequest, Judgement, OtherVerdict, OtherWindow, Verdict,
};
use crate::platform::{Desktop, Foreground, OpenWindow, Screenshot};
use crate::store::JsonStore;
use crate::usage::UsageRecorder;
use crate::youtube;
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

/// How often the open windows are sampled. A window without a verdict is judged at the
/// sample that first sees it, so this is also how long a new app or site can go unjudged.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(3);
const HISTORY_CAPACITY: usize = 30;
const VERDICT_LOG_CAPACITY: usize = 100;
const MAX_TITLE_CHARS: usize = 120;
/// Most YouTube pages whose channel is remembered (see `MonitorState::channels`).
const CHANNEL_CACHE_CAPACITY: usize = 500;
const SCREENSHOT_MAX_WIDTH: u32 = 1280;
/// Most other open windows listed in one check (each costs a few tokens).
const MAX_OTHER_WINDOWS: usize = 30;

/// Responsibility: Record one stretch of time a window stayed in front.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityEntry {
    pub process_name: String,
    pub title: String,
    pub since: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// Responsibility: Record one judgement and what the monitor did about it, for the UI.
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

/// Responsibility: Record why something is blocked.
///
/// Blocks do not expire: a block that lifted itself after a while would only
/// make the monitor judge (and pay for) the same app or site again. A block
/// ends when the user is granted access (see `request_access`), when an
/// administrator lifts it (`AiFocusConfig::unblocked`), or when it is
/// unblocked by hand in standalone mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockedEntry {
    /// Unique id, so the Settings tab can name this block in the config file
    /// without writing the agent's blocks file. Empty in files saved before
    /// ids existed; [`FocusMonitor::with_block_store`] fills those in.
    #[serde(default)]
    pub id: String,
    pub reason: String,
    /// Full window title when it was blocked (grants are matched against it).
    #[serde(default)]
    pub title: String,
}

/// Responsibility: Hold the blocks in force, as saved to disk so that logging
/// out and back in (which restarts the agent) does not lift them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedBlocks {
    pub processes: HashMap<String, BlockedEntry>,
    pub tabs: HashMap<String, BlockedEntry>,
}

/// Responsibility: Tell the caller what to do right after a judgement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Process is now on the blocked list; the enforcer kills it on its next tick.
    BlockProcess(String),
    CloseTab,
}

/// Responsibility: Hold the session's runtime state (history, strikes, blocks, verdicts) and
/// apply the rules to it, without any I/O.
#[derive(Debug, Default)]
pub struct MonitorState {
    pub task: String,
    pub history: VecDeque<ActivityEntry>,
    pub verdicts: VecDeque<VerdictLogEntry>,
    /// Off-task verdicts in a row per app (or per grant of an app); see
    /// [`Self::apply_judgement`]. Browser tabs are blocked at the first one.
    strikes: HashMap<String, u32>,
    /// Normalized process name -> block.
    pub blocked_processes: HashMap<String, BlockedEntry>,
    /// Browser "process|site" -> block (see [`tab_key`]); on YouTube
    /// "process|youtube channel: …" or "process|youtube page: …" (see `youtube.rs`).
    pub blocked_tabs: HashMap<String, BlockedEntry>,
    /// YouTube page name -> channel, as the judge read it from a screenshot.
    /// The title names only the video, so this is how a video of a blocked
    /// channel is recognized on the next sample without asking the judge again.
    channels: HashMap<String, String>,
    /// Verdict key (see [`Self::verdict_key`]) -> when it was last judged, and the
    /// verdict. A window is judged as soon as it has no verdict for the current task,
    /// and again once its verdict is due ([`Self::needs_judging`]).
    last_verdicts: HashMap<String, (DateTime<Utc>, Verdict)>,
    /// When the judge was last asked.
    pub last_check: Option<DateTime<Utc>>,
    /// After a failed call (no key, no network, budget spent): no call before this,
    /// so a failure is not retried at every sample.
    retry_at: Option<DateTime<Utc>>,
    random_state: u64,
    pub last_error: Option<String>,
}

impl MonitorState {
    /// Start over for a new task (or a stopped session). Strikes start from
    /// zero; blocks stay, since they do not depend on the task: re-entering
    /// the task (or stopping and starting) must not lift them.
    pub fn reset(&mut self, task: &str) {
        *self = Self {
            task: task.to_string(),
            history: std::mem::take(&mut self.history),
            verdicts: std::mem::take(&mut self.verdicts),
            channels: std::mem::take(&mut self.channels),
            blocked_processes: std::mem::take(&mut self.blocked_processes),
            blocked_tabs: std::mem::take(&mut self.blocked_tabs),
            ..Default::default()
        };
    }

    pub fn record(&mut self, now: DateTime<Utc>, foreground: &Foreground) {
        let title = truncate(&foreground.title, MAX_TITLE_CHARS);
        if let Some(last) = self.history.back_mut() {
            if last.process_name == foreground.process_name && last.title == title {
                last.last_seen = now;
                return;
            }
        }
        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(ActivityEntry {
            process_name: foreground.process_name.clone(),
            title,
            since: now,
            last_seen: now,
        });
    }

    pub fn history_text(&self) -> String {
        self.history
            .iter()
            .map(|entry| {
                let seconds = (entry.last_seen - entry.since).num_seconds().max(0);
                format!(
                    "[{}] {} — \"{}\" ({}m {}s)",
                    entry.since.with_timezone(&Local).format("%H:%M:%S"),
                    entry.process_name,
                    entry.title,
                    seconds / 60,
                    seconds % 60
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// May the judge be asked now? Not right after a failed call.
    pub fn call_allowed(&self, now: DateTime<Utc>) -> bool {
        self.retry_at.is_none_or(|at| now >= at)
    }

    /// Record the outcome of a call: a failure holds back the next one for the check
    /// interval.
    fn note_call(&mut self, now: DateTime<Utc>, config: &AiFocusConfig, ok: bool) {
        self.retry_at = (!ok)
            .then(|| now + ChronoDuration::seconds(clamp_interval(config.check_interval_secs)));
    }

    /// Apply the judgement of a window that is open but not in front. Tabs are blocked at
    /// the first off_task (the tab is closed when it comes to the front); apps collect
    /// strikes like a front app. Returns true when the window should be minimized (a
    /// blocked tab still shown in a visible background window).
    pub fn apply_background(
        &mut self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        window: &OpenWindow,
        verdict: &OtherVerdict,
    ) -> bool {
        let foreground = window.as_foreground();
        let is_browser = config.is_browser(&foreground.process_name);
        let target = self.target_key(config, &foreground);
        self.remember_verdict(now, config, &foreground, verdict.verdict);
        let mut minimize = false;
        let mut action = String::new();
        if verdict.verdict == Verdict::OffTask {
            let id = self.new_block_id(now);
            let entry = BlockedEntry {
                id,
                reason: verdict.reason.clone(),
                title: foreground.title.clone(),
            };
            if is_browser {
                self.blocked_tabs.insert(target, entry);
                minimize = !window.minimized;
                action = if minimize {
                    "blocked, window minimized".into()
                } else {
                    "blocked".into()
                };
            } else {
                let needed = config.strikes_to_block.max(1);
                let strikes = self.strikes.entry(target.clone()).or_insert(0);
                *strikes += 1;
                if *strikes >= needed {
                    self.strikes.remove(&target);
                    self.blocked_processes.insert(target, entry);
                    action = "blocked".into();
                } else {
                    action = format!("strike {strikes}/{needed}");
                }
            }
        } else if verdict.verdict == Verdict::OnTask && !is_browser {
            self.strikes.remove(&target);
        }
        if self.verdicts.len() >= VERDICT_LOG_CAPACITY {
            self.verdicts.pop_front();
        }
        self.verdicts.push_back(VerdictLogEntry {
            at: now,
            process_name: foreground.process_name.clone(),
            title: truncate(
                &format!("(background) {}", foreground.title),
                MAX_TITLE_CHARS,
            ),
            verdict: verdict.verdict,
            reason: verdict.reason.clone(),
            action,
        });
        minimize
    }

    /// xorshift64; seeded from the clock on first use. Not for security, only
    /// so check times are not guessable.
    fn next_random(&mut self) -> u64 {
        if self.random_state == 0 {
            self.random_state = Utc::now().timestamp_nanos_opt().unwrap_or(1) as u64 | 1;
        }
        let mut random = self.random_state;
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        self.random_state = random;
        random
    }

    /// What a block of this window would be keyed by, and so what "the same
    /// window" means for early checks: the process for an app, the site for a
    /// browser tab, and on YouTube the channel (or, unknown, the page).
    pub fn target_key(&self, config: &AiFocusConfig, foreground: &Foreground) -> String {
        if !config.is_browser(&foreground.process_name) {
            return normalize_process_name(&foreground.process_name);
        }
        if !youtube::is_youtube(&foreground.title) {
            return tab_key(foreground);
        }
        match self.channels.get(&youtube::page_name(&foreground.title)) {
            Some(channel) => youtube::channel_key(&foreground.process_name, channel),
            None => youtube::page_key(&foreground.process_name, &foreground.title),
        }
    }

    /// What an on_task verdict vouches for: the app for an app (its titles
    /// change with every file), the page for a browser tab (one good page
    /// says nothing about the rest of the site), and on YouTube the channel
    /// or page (see [`Self::target_key`]).
    pub fn verdict_key(&self, config: &AiFocusConfig, foreground: &Foreground) -> String {
        if config.is_browser(&foreground.process_name) && !youtube::is_youtube(&foreground.title) {
            format!(
                "{}|{}",
                normalize_process_name(&foreground.process_name),
                strip_browser_noise(&foreground.title).to_lowercase()
            )
        } else {
            self.target_key(config, foreground)
        }
    }

    /// Does this window need a verdict? Yes when it has none for the current task (a new
    /// app, a new page), and when its last verdict is due again: an on_task verdict after
    /// `recheck_minutes`; unsure, or off_task on an app still collecting strikes, after
    /// `check_interval_secs`. (Blocked tabs and apps are not judged: they are closed.)
    pub fn needs_judging(
        &self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        foreground: &Foreground,
    ) -> bool {
        match self
            .last_verdicts
            .get(&self.verdict_key(config, foreground))
        {
            None => true,
            Some((at, verdict)) => now - *at >= recheck_after(config, *verdict),
        }
    }

    fn remember_verdict(
        &mut self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        foreground: &Foreground,
        verdict: Verdict,
    ) {
        let key = self.verdict_key(config, foreground);
        self.last_verdicts.retain(|_, (at, remembered_verdict)| {
            now - *at < recheck_after(config, *remembered_verdict)
        });
        self.last_verdicts.insert(key, (now, verdict));
    }

    /// A fresh block id (unique enough: time plus a random suffix).
    fn new_block_id(&mut self, now: DateTime<Utc>) -> String {
        format!(
            "b{}-{:04}",
            now.timestamp_millis(),
            self.next_random() % 10_000
        )
    }

    /// Drop the blocks an administrator lifted; true if any were dropped.
    pub fn apply_admin_unblocks(&mut self, config: &AiFocusConfig) -> bool {
        if config.unblocked.is_empty() {
            return false;
        }
        let before = self.blocked_processes.len() + self.blocked_tabs.len();
        let lifted = |key: &String, block: &mut BlockedEntry| {
            let keep = !config.unblocked.contains(&block.id);
            if !keep {
                info!("block on {key} lifted by an administrator");
            }
            keep
        };
        self.blocked_processes.retain(lifted);
        self.blocked_tabs.retain(lifted);
        before != self.blocked_processes.len() + self.blocked_tabs.len()
    }

    /// Remember which channel a YouTube page belongs to.
    fn learn_channel(&mut self, foreground: &Foreground, channel: &str) {
        if !youtube::is_youtube(&foreground.title) {
            return;
        }
        let Some(channel) = youtube::normalize_channel(channel) else {
            return;
        };
        if self.channels.len() >= CHANNEL_CACHE_CAPACITY {
            self.channels.clear();
        }
        self.channels
            .insert(youtube::page_name(&foreground.title), channel);
    }

    /// Is this browser tab blocked?
    pub fn tab_blocked(&self, config: &AiFocusConfig, foreground: &Foreground) -> bool {
        self.blocked_tabs
            .contains_key(&self.target_key(config, foreground))
    }

    /// Blocked process names with their reasons.
    pub fn process_blocks(&self) -> Vec<(String, String)> {
        self.blocked_processes
            .iter()
            .map(|(name, block)| (name.clone(), block.reason.clone()))
            .collect()
    }

    pub fn apply_judgement(
        &mut self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        foreground: &Foreground,
        judgement: Judgement,
        grant: Option<&Grant>,
    ) -> Action {
        let is_browser = config.is_browser(&foreground.process_name);
        if is_browser {
            self.learn_channel(foreground, &judgement.channel);
        }
        let target = self.target_key(config, foreground);
        self.remember_verdict(now, config, foreground, judgement.verdict);

        let mut action = Action::None;
        let mut action_label = String::new();
        if grant.is_none() && is_browser && self.blocked_tabs.contains_key(&target) {
            // Only now known to be a blocked channel (its first screenshot).
            action = Action::CloseTab;
            action_label = "tab closed (already blocked)".into();
        } else if is_browser {
            // A tab is blocked at the first off-task verdict: a wrong one costs
            // a closed tab (and an access request), while waiting for more
            // verdicts leaves the user a whole check interval of distraction.
            if judgement.verdict == Verdict::OffTask {
                let id = self.new_block_id(now);
                self.blocked_tabs.insert(
                    target,
                    BlockedEntry {
                        id,
                        reason: judgement.reason.clone(),
                        title: foreground.title.clone(),
                    },
                );
                action = Action::CloseTab;
                action_label = "tab closed".into();
            }
        } else {
            // Closing an app can lose unsaved work, so an app needs
            // `strikes_to_block` off-task verdicts in a row. Strikes on a
            // granted app count per grant.
            let key = match grant {
                Some(active_grant) => format!("grant:{}", active_grant.id),
                None => target.clone(),
            };
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
                        let id = self.new_block_id(now);
                        self.blocked_processes.insert(
                            target.clone(),
                            BlockedEntry {
                                id,
                                reason: judgement.reason.clone(),
                                title: foreground.title.clone(),
                            },
                        );
                        action = Action::BlockProcess(target);
                        action_label = "blocked".into();
                    } else {
                        action_label = format!("strike {strikes}/{needed}");
                    }
                }
            }
        }
        if grant.is_some() && action != Action::None {
            action_label = format!("access revoked, {action_label}");
        }

        if self.verdicts.len() >= VERDICT_LOG_CAPACITY {
            self.verdicts.pop_front();
        }
        self.verdicts.push_back(VerdictLogEntry {
            at: now,
            process_name: foreground.process_name.clone(),
            title: truncate(&foreground.title, MAX_TITLE_CHARS),
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
        let covered = |key: &str, entry: &BlockedEntry| {
            target.matches_block_key(key) || target.matches_block_key(&format!("x|{}", entry.title))
        };
        self.blocked_processes
            .retain(|key, block| !covered(key, block));
        self.blocked_tabs.retain(|key, block| !covered(key, block));
        self.strikes.retain(|key, _| !target.matches_block_key(key));
    }
}

/// Block key of a browser window: `process|site`. A block per exact title
/// would let the user dodge it by moving to the next video or post; a block
/// per site does not. (YouTube is blocked per channel instead: `youtube.rs`.)
pub fn tab_key(foreground: &Foreground) -> String {
    format!(
        "{}|{}",
        normalize_process_name(&foreground.process_name),
        site_of(&foreground.title)
    )
}

/// Best guess at the site from a browser window title, lower case:
/// `(3) Funny cats - YouTube - Google Chrome` -> `youtube`. Without a
/// separator the whole title is used.
pub fn site_of(title: &str) -> String {
    let stripped = strip_browser_noise(title);
    let site = [" - ", " | ", " — ", " · "]
        .iter()
        .filter_map(|separator| {
            stripped
                .rfind(separator)
                .map(|index| &stripped[index + separator.len()..])
        })
        .min_by_key(|candidate| candidate.len())
        .unwrap_or(stripped);
    truncate(site.trim(), 60).to_lowercase()
}

/// A browser title without the browser's name, Edge's "and 3 more pages" and
/// unread counters such as "(3) ", which change constantly:
/// `(3) Funny cats - YouTube - Google Chrome` -> `Funny cats - YouTube`.
pub fn strip_browser_noise(title: &str) -> &str {
    const BROWSER_SUFFIXES: &[&str] = &[
        " - Google Chrome",
        " - Microsoft\u{200b} Edge",
        " - Microsoft Edge",
        " — Mozilla Firefox",
        " - Mozilla Firefox",
        " - Whale",
        " - Brave",
        " - Opera",
    ];
    let mut trimmed = title.trim();
    for suffix in BROWSER_SUFFIXES {
        if let Some(rest) = trimmed.strip_suffix(suffix) {
            trimmed = rest;
            break;
        }
    }
    // Edge: "Title and 3 more pages"
    if let Some(index) = trimmed
        .rfind(" and ")
        .filter(|_| trimmed.ends_with(" more pages"))
    {
        trimmed = &trimmed[..index];
    }
    if let Some(rest) = trimmed.strip_prefix('(') {
        if let Some((count, after)) = rest.split_once(") ") {
            if count
                .chars()
                .all(|character| character.is_ascii_digit() || character == '+')
            {
                trimmed = after;
            }
        }
    }
    trimmed.trim()
}

/// How long a verdict stands before the window is judged again.
fn recheck_after(config: &AiFocusConfig, verdict: Verdict) -> ChronoDuration {
    if verdict == Verdict::OnTask && config.recheck_minutes > 0 {
        ChronoDuration::minutes(clamp_recheck_minutes(config.recheck_minutes))
    } else {
        ChronoDuration::seconds(clamp_interval(config.check_interval_secs))
    }
}

/// Admin-entered values are used in date arithmetic; keep them sane so an
/// absurd number cannot overflow (and panic) the monitor thread.
fn clamp_recheck_minutes(minutes: u64) -> i64 {
    minutes.min(24 * 60) as i64
}

fn clamp_interval(seconds: u64) -> i64 {
    seconds.clamp(10, 24 * 3600) as i64
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).chain(['…']).collect()
    }
}

/// Shortest accepted purpose, so "ok" or "pls" never reaches the AI.
const MIN_PURPOSE_CHARS: usize = 4;

/// Responsibility: Report why an access request was refused before or during the AI review.
#[derive(Debug, Error)]
pub enum AccessError {
    #[error("먼저 데일리 웰빙(AI 검사)을 시작하세요")]
    NoSession,
    #[error("앱이나 사이트의 이름을 적으세요")]
    EmptyTarget,
    #[error("무엇에 쓸지 적으세요({MIN_PURPOSE_CHARS}글자 이상)")]
    PurposeTooShort,
    #[error("설정에서 취소할 때까지 가는 허용을 막아 두었습니다")]
    PermanentNotAllowed,
    #[error("다른 요청을 아직 처리하고 있습니다")]
    Busy,
    #[error("AI: {0}")]
    Ai(#[from] JudgeError),
    #[error("저장하지 못했습니다: {0}")]
    Io(#[from] std::io::Error),
}

pub type JudgeProvider =
    Arc<dyn Fn(&AiFocusConfig) -> Result<Arc<dyn Judge>, JudgeError> + Send + Sync>;

/// Claude judge for the current config. The judge (and its HTTP connection)
/// is reused until the key or the model changes.
pub fn claude_judge_provider() -> JudgeProvider {
    type Cached = Option<((String, String), Arc<dyn Judge>)>;
    let cache: Mutex<Cached> = Mutex::new(None);
    Arc::new(move |config: &AiFocusConfig| {
        let key = config.resolve_api_key().ok_or(JudgeError::NoApiKey)?;
        let id = (key, config.model.clone());
        let mut cache = cache.lock();
        if let Some((cached_id, judge)) = cache.as_ref() {
            if *cached_id == id {
                return Ok(Arc::clone(judge));
            }
        }
        let judge: Arc<dyn Judge> = Arc::new(ClaudeJudge::new(id.0.clone(), id.1.clone()));
        *cache = Some((id, Arc::clone(&judge)));
        Ok(judge)
    })
}

/// Responsibility: Drive the check loop and access requests by combining MonitorState with the
/// desktop, the judge, the reviewer and the grant store.
///
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
    /// Where blocks are saved (in memory unless [`Self::with_block_store`]).
    blocks: JsonStore<SavedBlocks>,
    /// Token usage of this process's Claude calls (see `usage.rs`); with a daily
    /// budget, focus checks stop once it is spent.
    usage: Option<Arc<UsageRecorder>>,
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
            blocks: JsonStore::in_memory(SavedBlocks::default()),
            usage: None,
            running: AtomicBool::new(false),
        }
    }

    /// Keep blocks in `store` and resume the ones saved there.
    pub fn with_block_store(mut self, store: JsonStore<SavedBlocks>) -> Self {
        let saved = store.get();
        {
            let state = self.state.get_mut();
            state.blocked_processes = saved.processes;
            state.blocked_tabs = saved.tabs;
            let now = Utc::now();
            let mut missing: Vec<&mut BlockedEntry> = state
                .blocked_processes
                .values_mut()
                .chain(state.blocked_tabs.values_mut())
                .filter(|block| block.id.is_empty())
                .collect();
            let ids: Vec<String> = (0..missing.len())
                .map(|index| format!("b{}-{index:04}", now.timestamp_millis()))
                .collect();
            for (block, id) in missing.iter_mut().zip(ids) {
                block.id = id;
            }
        }
        self.blocks = store;
        self
    }

    /// Save the blocks if they changed since the last save.
    fn save_blocks(&self) {
        let current = {
            let state = self.state.lock();
            SavedBlocks {
                processes: state.blocked_processes.clone(),
                tabs: state.blocked_tabs.clone(),
            }
        };
        if current != self.blocks.get() {
            if let Err(error) = self.blocks.update(|saved| *saved = current) {
                warn!("could not save blocks: {error}");
            }
        }
    }

    pub fn with_grants(mut self, grants: JsonStore<GrantState>) -> Self {
        self.grants = grants;
        self
    }

    /// Count token usage in `usage` and honour the daily budget. The same recorder
    /// is usually also `usage::install`ed, so that every Claude call reaches it.
    pub fn with_usage(mut self, usage: Arc<UsageRecorder>) -> Self {
        self.usage = Some(usage);
        self
    }

    pub fn usage(&self) -> Option<&Arc<UsageRecorder>> {
        self.usage.as_ref()
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
                // One bad sample must not end observation for the whole session.
                let step = std::panic::AssertUnwindSafe(|| {
                    this.step(Utc::now());
                    this.save_blocks();
                });
                if std::panic::catch_unwind(step).is_err() {
                    warn!("focus check panicked; continuing");
                }
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
        // Always-blocked websites: closed whether or not daily wellbeing is on, without the AI.
        if self.close_always_blocked_sites(now, &config) {
            return;
        }
        {
            let mut state = self.state.lock();
            // Before the session check: blocks apply without a session too.
            state.apply_admin_unblocks(&config);
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

        let Some(foreground) = self.desktop.foreground() else {
            return;
        };
        let grant = self
            .grants
            .get()
            .covering(now, &config, &foreground.process_name, &foreground.title)
            .cloned();
        let exempt = self.exempt(&config, &foreground);
        let open = self.desktop.open_windows();

        let (due, judge_front, others) = {
            let mut state = self.state.lock();
            state.record(now, &foreground);
            if !exempt
                && grant.is_none()
                && config.is_browser(&foreground.process_name)
                && state.tab_blocked(&config, &foreground)
            {
                drop(state);
                if self.close_tab_if_unchanged(&foreground) {
                    info!("closed blocked tab \"{}\"", foreground.title);
                }
                return;
            }
            let hide: Vec<isize> = open
                .iter()
                .filter(|open_window| {
                    open_window.window != foreground.window && !open_window.minimized
                })
                .filter(|open_window| config.is_browser(&open_window.process_name))
                .filter(|open_window| state.tab_blocked(&config, &open_window.as_foreground()))
                .filter(|open_window| self.covering_grant(now, &config, open_window).is_none())
                .map(|open_window| open_window.window)
                .collect();
            let others = self.others_to_judge(now, &config, &state, &foreground, &open);
            let judge_front = !exempt && state.needs_judging(now, &config, &foreground);
            let due = (judge_front || !others.is_empty()) && state.call_allowed(now);
            drop(state);
            // A blocked tab shown in a background window (say, a video on a second
            // monitor) is hidden; it is closed once it comes to the front.
            for window in hide {
                self.desktop.minimize(window);
            }
            (due, judge_front, others)
        };
        if !due {
            return;
        }

        self.state.lock().last_check = Some(now);
        let listed: Vec<OtherWindow> = others
            .iter()
            .enumerate()
            .map(|(id, other)| OtherWindow {
                id,
                is_browser: config.is_browser(&other.process_name),
                process_name: other.process_name.clone(),
                title: truncate(&other.title, MAX_TITLE_CHARS),
            })
            .collect();
        let result = self.run_judge(&config, &foreground, grant.as_ref(), &listed);
        // The judge call can take a while; if access to this window was granted
        // (or revoked) meanwhile, the verdict was made under the old terms.
        let grant_now = self
            .grants
            .get()
            .covering(
                now,
                &self.store.get(),
                &foreground.process_name,
                &foreground.title,
            )
            .map(|current_grant| current_grant.id.clone());
        if result.is_ok()
            && grant_now
                != grant
                    .as_ref()
                    .map(|original_grant| original_grant.id.clone())
        {
            info!(
                "discarding verdict for {}: access changed during the check",
                foreground.process_name
            );
            return;
        }
        let mut state = self.state.lock();
        state.note_call(now, &config, result.is_ok());
        match result {
            Ok(mut judgement) => {
                state.last_error = None;
                let mut minimize = Vec::new();
                for verdict in std::mem::take(&mut judgement.others) {
                    if let Some(window) = others.get(verdict.id) {
                        if state.apply_background(now, &config, window, &verdict) {
                            minimize.push(window.window);
                        }
                    }
                }
                if !judge_front {
                    drop(state);
                    for window in minimize {
                        self.desktop.minimize(window);
                    }
                    return;
                }
                info!(
                    "{} \"{}\": {:?} ({})",
                    foreground.process_name, foreground.title, judgement.verdict, judgement.reason
                );
                let reason = judgement.reason.clone();
                let action =
                    state.apply_judgement(now, &config, &foreground, judgement, grant.as_ref());
                drop(state);
                for window in minimize {
                    self.desktop.minimize(window);
                }
                if action != Action::None {
                    if let Some(grant) = &grant {
                        self.revoke_for_misuse(now, grant, &reason);
                    }
                }
                if action == Action::CloseTab && !self.close_tab_if_unchanged(&foreground) {
                    warn!(
                        "did not close tab of {}: window or tab changed",
                        foreground.process_name
                    );
                }
            }
            Err(error) => {
                warn!("focus check failed: {error}");
                state.last_error = Some(error.to_string());
            }
        }
    }

    /// Close the front tab when its title matches an always-blocked website, and hide other
    /// browser windows that show one (they are closed once they come to the front). Returns
    /// whether a tab was closed.
    fn close_always_blocked_sites(&self, now: DateTime<Utc>, config: &AiFocusConfig) -> bool {
        if config.always_blocked_sites.is_empty() {
            return false;
        }
        let foreground = self.desktop.foreground();
        let shows_blocked = |process_name: &str, title: &str| {
            config.is_browser(process_name) && config.always_blocked_site(title).is_some()
        };
        for open_window in self.desktop.open_windows() {
            let in_front = foreground
                .as_ref()
                .is_some_and(|front| front.window == open_window.window);
            if !in_front
                && !open_window.minimized
                && shows_blocked(&open_window.process_name, &open_window.title)
            {
                self.desktop.minimize(open_window.window);
            }
        }
        let Some(foreground) = foreground else {
            return false;
        };
        let Some(pattern) = config.always_blocked_site(&foreground.title) else {
            return false;
        };
        if !config.is_browser(&foreground.process_name) {
            return false;
        }
        let closed = self.desktop.close_active_tab(&foreground);
        info!(
            "always-blocked website \"{pattern}\": {} tab \"{}\"",
            if closed { "closed" } else { "could not close" },
            foreground.title
        );
        if closed {
            let mut state = self.state.lock();
            if state.verdicts.len() >= VERDICT_LOG_CAPACITY {
                state.verdicts.pop_front();
            }
            state.verdicts.push_back(VerdictLogEntry {
                at: now,
                process_name: foreground.process_name.clone(),
                title: truncate(&foreground.title, MAX_TITLE_CHARS),
                verdict: Verdict::OffTask,
                reason: format!("항상 막을 웹사이트(\"{pattern}\")"),
                action: "tab closed".to_string(),
            });
        }
        closed
    }

    /// Never judged: this program, Windows essentials, the admin's always-allow list.
    fn exempt(&self, config: &AiFocusConfig, window: &Foreground) -> bool {
        window.pid == std::process::id()
            || is_never_terminate(&window.process_name)
            || config.is_always_allowed(&window.process_name)
    }

    fn covering_grant(
        &self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        window: &OpenWindow,
    ) -> Option<Grant> {
        self.grants
            .get()
            .covering(now, config, &window.process_name, &window.title)
            .cloned()
    }

    /// Open windows other than `foreground` that need a verdict: not exempt, not granted, not
    /// already blocked, without a verdict that still stands; one per verdict key
    /// (the same app's other windows, the same page in two windows), at most
    /// [`MAX_OTHER_WINDOWS`].
    fn others_to_judge(
        &self,
        now: DateTime<Utc>,
        config: &AiFocusConfig,
        state: &MonitorState,
        foreground: &Foreground,
        open: &[OpenWindow],
    ) -> Vec<OpenWindow> {
        let front_key = state.verdict_key(config, foreground);
        let mut seen = std::collections::HashSet::from([front_key]);
        open.iter()
            .filter(|window| window.window != foreground.window)
            .filter(|window| {
                let window_as_foreground = window.as_foreground();
                let blocked = if config.is_browser(&window.process_name) {
                    state.tab_blocked(config, &window_as_foreground)
                } else {
                    state
                        .blocked_processes
                        .contains_key(&normalize_process_name(&window.process_name))
                };
                !self.exempt(config, &window_as_foreground)
                    && !blocked
                    && state.needs_judging(now, config, &window_as_foreground)
                    && self.covering_grant(now, config, window).is_none()
                    && seen.insert(state.verdict_key(config, &window_as_foreground))
            })
            .take(MAX_OTHER_WINDOWS)
            .cloned()
            .collect()
    }

    /// Ctrl+W hits whatever tab is active *now*. The judge call can take seconds,
    /// so only close when the same window still shows the same title; otherwise
    /// the user switched tabs and we would close the wrong one. The block stays
    /// recorded, so the tab is closed the next time it comes to the front.
    fn close_tab_if_unchanged(&self, judged: &Foreground) -> bool {
        let still_same = self.desktop.foreground().is_some_and(|current| {
            current.window == judged.window && current.title == judged.title
        });
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
            until: minutes.map(|minute_count| now + ChronoDuration::minutes(minute_count as i64)),
            granted_at: now,
            task: config.task.clone(),
        });
        self.grants.update(|grant_state| {
            if let Some(grant) = grant.clone() {
                grant_state.grants.push(grant);
            }
            grant_state.push_log(entry.clone());
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
        if state
            .grants
            .iter()
            .all(|grant| grant.is_active(now, config))
        {
            return;
        }
        let result = self.grants.update(|grant_state| {
            let (keep, ended): (Vec<_>, Vec<_>) = std::mem::take(&mut grant_state.grants)
                .into_iter()
                .partition(|grant| grant.is_active(now, config));
            grant_state.grants = keep;
            for grant in ended {
                if config.revoked_grants.contains(&grant.id) {
                    grant_state.push_log(GrantLogEntry {
                        at: now,
                        target: grant.target,
                        purpose: grant.purpose,
                        event: GrantEvent::RevokedByAdmin,
                        message: String::new(),
                    });
                }
            }
        });
        if let Err(error) = result {
            warn!("could not save grants: {error}");
        }
    }

    fn revoke_for_misuse(&self, now: DateTime<Utc>, grant: &Grant, reason: &str) {
        info!("access to {} revoked: {reason}", grant.target.label());
        let result = self.grants.update(|grant_state| {
            grant_state
                .grants
                .retain(|existing| existing.id != grant.id);
            grant_state.push_log(GrantLogEntry {
                at: now,
                target: grant.target.clone(),
                purpose: grant.purpose.clone(),
                event: GrantEvent::RevokedForMisuse,
                message: reason.to_string(),
            });
        });
        if let Err(error) = result {
            warn!("could not save grants: {error}");
        }
    }

    /// A screenshot of the foreground window offered as proof that a to-do is finished.
    /// Refused, with the reason, when nothing is in front or a private app is on screen.
    pub fn capture_proof(&self) -> Result<Vec<u8>, String> {
        let config = self.store.get();
        let foreground = self
            .desktop
            .foreground()
            .ok_or("맨 앞에 창이 없어서 증거 화면을 찍지 못했습니다")?;
        let private_on_screen = config.is_private(&foreground.process_name)
            || (!config.private_apps.is_empty()
                && self
                    .desktop
                    .visible_processes()
                    .iter()
                    .any(|name| config.is_private(name)));
        if private_on_screen {
            return Err(
                "비공개 앱이 화면에 보여서 증거 화면을 찍지 않았습니다. 그 앱을 닫거나 화면 캡처 \
                        없이 보내세요"
                    .into(),
            );
        }
        self.desktop
            .screenshot(&foreground)
            .and_then(encode_jpeg)
            .ok_or_else(|| "증거 화면을 찍지 못했습니다".into())
    }

    fn run_judge(
        &self,
        config: &AiFocusConfig,
        foreground: &Foreground,
        grant: Option<&Grant>,
        others: &[OtherWindow],
    ) -> Result<Judgement, JudgeError> {
        if let Some((spent, limit)) = self
            .usage
            .as_ref()
            .and_then(|usage| usage.over_budget(config))
        {
            return Err(JudgeError::BudgetReached { spent, limit });
        }
        let judge = (self.judge)(config)?;
        let private_on_screen = config.is_private(&foreground.process_name)
            || (!config.private_apps.is_empty()
                && self
                    .desktop
                    .visible_processes()
                    .iter()
                    .any(|name| config.is_private(name)));
        let screenshot = if config.send_screenshot && !private_on_screen {
            self.desktop.screenshot(foreground).and_then(encode_jpeg)
        } else {
            None
        };
        let history = self.state.lock().history_text();
        judge.judge(&JudgeRequest {
            task: &config.task,
            history: &history,
            process_name: &foreground.process_name,
            title: &foreground.title,
            youtube: config.is_browser(&foreground.process_name)
                && youtube::is_youtube(&foreground.title),
            screenshot: screenshot.as_deref(),
            grant_purpose: grant.map(|active_grant| active_grant.purpose.as_str()),
            others,
        })
    }
}

/// Downscale to at most [`SCREENSHOT_MAX_WIDTH`] wide and encode as JPEG.
fn encode_jpeg(screenshot: Screenshot) -> Option<Vec<u8>> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{imageops::FilterType, DynamicImage, RgbaImage};

    // Takes the pixels by value: a 4K capture is ~33 MB, not worth copying.
    let rgba = RgbaImage::from_raw(screenshot.width, screenshot.height, screenshot.rgba)?;
    let mut dynamic_image = DynamicImage::ImageRgba8(rgba);
    if dynamic_image.width() > SCREENSHOT_MAX_WIDTH {
        let height = dynamic_image.height() * SCREENSHOT_MAX_WIDTH / dynamic_image.width();
        dynamic_image = dynamic_image.resize(SCREENSHOT_MAX_WIDTH, height, FilterType::Triangle);
    }
    let mut jpeg_bytes = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg_bytes, 70)
        .encode_image(&dynamic_image.to_rgb8())
        .ok()?;
    Some(jpeg_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Responsibility: Test double for Desktop: a settable foreground window, a gray
    /// screenshot, counted tab closes.
    struct FakeDesktop {
        foreground: Arc<Mutex<Option<Foreground>>>,
        tabs_closed: Arc<AtomicUsize>,
        visible: Arc<Mutex<Vec<String>>>,
        open: Arc<Mutex<Vec<OpenWindow>>>,
        minimized: Arc<Mutex<Vec<isize>>>,
    }

    impl Desktop for FakeDesktop {
        fn foreground(&self) -> Option<Foreground> {
            self.foreground.lock().clone()
        }
        fn screenshot(&self, _foreground: &Foreground) -> Option<Screenshot> {
            Some(Screenshot {
                width: 2560,
                height: 1440,
                rgba: vec![128; 2560 * 1440 * 4],
            })
        }
        fn visible_processes(&self) -> Vec<String> {
            self.visible.lock().clone()
        }
        fn close_active_tab(&self, _foreground: &Foreground) -> bool {
            self.tabs_closed.fetch_add(1, Ordering::SeqCst);
            true
        }
        fn open_windows(&self) -> Vec<OpenWindow> {
            self.open.lock().clone()
        }
        fn minimize(&self, window: isize) -> bool {
            self.minimized.lock().push(window);
            true
        }
    }

    /// Responsibility: Test double for Judge: return a settable verdict and record what it was
    /// asked.
    type Hook = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

    struct FixedJudge {
        verdict: Arc<Mutex<Verdict>>,
        /// Channel the judge "reads" from the screenshot.
        channel: Arc<Mutex<String>>,
        /// Verdict for every listed other window, and the last list it saw.
        others_verdict: Arc<Mutex<Verdict>>,
        saw_others: Arc<Mutex<Vec<OtherWindow>>>,
        /// Runs once inside the next judge call (simulates something happening meanwhile).
        during_call: Hook,
        saw_grant: Arc<Mutex<Option<String>>>,
        /// Foreground of the fake desktop, and what to switch it to mid-call.
        foreground: Arc<Mutex<Option<Foreground>>>,
        switch_to: Arc<Mutex<Option<Foreground>>>,
        calls: Arc<AtomicUsize>,
        saw_screenshot: Arc<AtomicBool>,
    }

    impl Judge for FixedJudge {
        fn judge(&self, request: &JudgeRequest) -> Result<Judgement, JudgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(hook) = self.during_call.lock().take() {
                hook();
            }
            if let Some(next) = self.switch_to.lock().take() {
                *self.foreground.lock() = Some(next);
            }
            self.saw_screenshot
                .store(request.screenshot.is_some(), Ordering::SeqCst);
            *self.saw_grant.lock() = request.grant_purpose.map(str::to_string);
            *self.saw_others.lock() = request.others.to_vec();
            let other_verdict = *self.others_verdict.lock();
            Ok(Judgement {
                verdict: *self.verdict.lock(),
                reason: "test".into(),
                channel: self.channel.lock().clone(),
                others: request
                    .others
                    .iter()
                    .map(|other_window| OtherVerdict {
                        id: other_window.id,
                        verdict: other_verdict,
                        reason: "bg".into(),
                    })
                    .collect(),
            })
        }
    }

    /// Responsibility: Bundle a monitor under test with handles to its fakes.
    struct Harness {
        monitor: FocusMonitor,
        calls: Arc<AtomicUsize>,
        tabs_closed: Arc<AtomicUsize>,
        saw_screenshot: Arc<AtomicBool>,
        switch_to: Arc<Mutex<Option<Foreground>>>,
        verdict: Arc<Mutex<Verdict>>,
        channel: Arc<Mutex<String>>,
        others_verdict: Arc<Mutex<Verdict>>,
        saw_others: Arc<Mutex<Vec<OtherWindow>>>,
        open: Arc<Mutex<Vec<OpenWindow>>>,
        minimized: Arc<Mutex<Vec<isize>>>,
        saw_grant: Arc<Mutex<Option<String>>>,
        current: Arc<Mutex<Option<Foreground>>>,
        during_call: Hook,
        visible: Arc<Mutex<Vec<String>>>,
    }

    fn foreground(process: &str, title: &str) -> Foreground {
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
        let channel = Arc::new(Mutex::new(String::new()));
        let others_verdict = Arc::new(Mutex::new(Verdict::OnTask));
        let saw_others = Arc::new(Mutex::new(Vec::new()));
        let open = Arc::new(Mutex::new(Vec::new()));
        let minimized = Arc::new(Mutex::new(Vec::new()));
        let saw_grant = Arc::new(Mutex::new(None));
        let during_call: Hook = Arc::new(Mutex::new(None));
        let visible = Arc::new(Mutex::new(Vec::new()));
        let judge: Arc<dyn Judge> = Arc::new(FixedJudge {
            during_call: Arc::clone(&during_call),
            verdict: Arc::clone(&verdict),
            channel: Arc::clone(&channel),
            others_verdict: Arc::clone(&others_verdict),
            saw_others: Arc::clone(&saw_others),
            saw_grant: Arc::clone(&saw_grant),
            foreground: Arc::clone(&current),
            switch_to: Arc::clone(&switch_to),
            calls: Arc::clone(&calls),
            saw_screenshot: Arc::clone(&saw_screenshot),
        });
        let monitor = FocusMonitor::new(
            AiFocusStore::in_memory(config),
            Box::new(FakeDesktop {
                foreground: Arc::clone(&current),
                tabs_closed: Arc::clone(&tabs_closed),
                visible: Arc::clone(&visible),
                open: Arc::clone(&open),
                minimized: Arc::clone(&minimized),
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
            channel,
            others_verdict,
            saw_others,
            open,
            minimized,
            saw_grant,
            current,
            during_call,
            visible,
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
    fn always_blocked_websites_are_closed_without_the_ai_and_without_daily_wellbeing() {
        let config = AiFocusConfig {
            always_blocked_sites: ["youtube".to_string(), "reddit|twitter".to_string()]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        // Daily wellbeing is off (no session).
        let fixture = harness(
            Verdict::OnTask,
            foreground("chrome.exe", "Funny cats - YouTube - Google Chrome"),
            config,
        );
        fixture.open.lock().push(OpenWindow {
            window: 7,
            pid: 99,
            process_name: "msedge.exe".into(),
            title: "Home / Twitter - Microsoft Edge".into(),
            minimized: false,
        });
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            0,
            "the AI is not asked"
        );
        assert_eq!(
            *fixture.minimized.lock(),
            [7],
            "a background window is hidden"
        );
        assert!(fixture
            .monitor
            .state()
            .verdicts
            .back()
            .is_some_and(|entry| entry.action == "tab closed" && entry.reason.contains("youtube")));
        // The browser's own name does not count: "google" does not match every Chrome tab.
        *fixture.current.lock() = Some(foreground("chrome.exe", "Rust docs - Google Chrome"));
        fixture
            .monitor
            .store()
            .update(|stored| {
                stored.always_blocked_sites.insert("google".into());
            })
            .unwrap();
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        // Not a browser: never closed as a website.
        *fixture.current.lock() = Some(foreground("code.exe", "youtube.rs - Visual Studio Code"));
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn blocks_app_after_strikes() {
        let fixture = harness(
            Verdict::OffTask,
            foreground("steam.exe", "Steam"),
            session("write thesis"),
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert!(fixture.monitor.state().blocked_processes.is_empty());
        // Not due yet: sampled, but not judged again.
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(61));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        let blocks = fixture.monitor.state().process_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "steam.exe");
        // Blocks do not expire: a day later steam is still blocked.
        *fixture.current.lock() = Some(foreground("code.exe", "main.rs"));
        fixture.monitor.step(start_time + ChronoDuration::days(1));
        assert_eq!(fixture.monitor.state().process_blocks().len(), 1);
    }

    #[test]
    fn on_task_never_blocks_and_screenshot_is_sent() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("coding"),
        );
        let mut config = fixture.monitor.store().get();
        config.recheck_minutes = 0; // judge at every check
        fixture
            .monitor
            .store()
            .update(|stored_config| *stored_config = config)
            .unwrap();
        let start_time = Utc::now();
        for check_index in 0..5 {
            fixture
                .monitor
                .step(start_time + ChronoDuration::seconds(61 * check_index));
        }
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 5);
        assert!(fixture.saw_screenshot.load(Ordering::SeqCst));
        assert!(fixture.monitor.state().process_blocks().is_empty());
    }

    #[test]
    fn an_on_task_app_is_not_rechecked_for_recheck_minutes() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("coding"), // recheck_minutes = 15 (default)
        );
        let start_time = Utc::now();
        for check_index in 0..5 {
            fixture
                .monitor
                .step(start_time + ChronoDuration::seconds(61 * check_index));
        }
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        // Another file of the same app is vouched for too.
        *fixture.current.lock() = Some(foreground("code.exe", "lib.rs"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::minutes(10));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        fixture
            .monitor
            .step(start_time + ChronoDuration::minutes(16));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        // A new task forgets the verdict: judged at once.
        fixture
            .monitor
            .store()
            .update(|stored_config| stored_config.task = "write docs".into())
            .unwrap();
        fixture
            .monitor
            .step(start_time + ChronoDuration::minutes(17));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn an_on_task_page_does_not_vouch_for_the_rest_of_its_site() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("chrome.exe", "Rust 강의 - NAVER"),
            session("learn Rust"),
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(61));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        *fixture.current.lock() = Some(foreground("chrome.exe", "웹툰 - NAVER"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(66));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn browser_tab_is_closed_and_stays_closed() {
        let mut config = session("write thesis");
        config.strikes_to_block = 1;
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "Funny cats - YouTube"),
            config,
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        assert!(fixture.monitor.state().blocked_processes.is_empty());
        // Reopened: closed on the next sample without asking the judge.
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn tab_is_not_closed_if_user_switched_tabs_meanwhile() {
        let mut config = session("write thesis");
        config.strikes_to_block = 1;
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "Funny cats - YouTube"),
            config,
        );
        // While the judge is "thinking", the user switches to another tab
        // in the same browser window.
        *fixture.switch_to.lock() = Some(foreground("chrome.exe", "Overleaf - thesis.tex"));
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 0);
        // The block is still recorded for the next time that tab is in front.
        assert_eq!(fixture.monitor.state().blocked_tabs.len(), 1);
    }

    #[test]
    fn idle_without_session_and_skips_exempt_apps() {
        let fixture = harness(
            Verdict::OffTask,
            foreground("steam.exe", "Steam"),
            AiFocusConfig::default(),
        );
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);

        let mut config = session("x");
        config.always_allow.insert("steam.exe".into());
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.monitor.state().history.len(), 1);
    }

    #[test]
    fn private_apps_are_not_screenshotted() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("KeePass.exe", "db"),
            session("x"),
        );
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        assert!(!fixture.saw_screenshot.load(Ordering::SeqCst));
    }

    #[test]
    fn changing_task_rechecks_at_once_and_keeps_blocks() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.monitor.state().blocked_processes.len(), 1);
        fixture
            .monitor
            .store()
            .update(|stored_config| stored_config.task = "play games".into())
            .unwrap();
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(1));
        // Reset, then judged again under the new task; the block stays.
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.monitor.state().task, "play games");
        assert_eq!(fixture.monitor.state().blocked_processes.len(), 1);
    }

    #[test]
    fn history_merges_consecutive_samples() {
        let mut state = MonitorState::default();
        let start_time = Utc::now();
        state.record(start_time, &foreground("code.exe", "a"));
        state.record(
            start_time + ChronoDuration::seconds(90),
            &foreground("code.exe", "a"),
        );
        state.record(
            start_time + ChronoDuration::seconds(95),
            &foreground("chrome.exe", "b"),
        );
        assert_eq!(state.history.len(), 2);
        assert!(state.history_text().contains("(1m 30s)"));
    }

    #[test]
    fn screenshot_is_downscaled_jpeg() {
        let screenshot = Screenshot {
            width: 2560,
            height: 1440,
            rgba: vec![200; 2560 * 1440 * 4],
        };
        let jpeg = encode_jpeg(screenshot).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(decoded.width(), 1280);
        assert_eq!(decoded.height(), 720);
    }

    // --- access grants ---------------------------------------------------------

    use crate::access_review::{AccessDecision, AccessReviewer};

    /// Responsibility: Test double for AccessReviewer: approve or reject everything, counting
    /// calls.
    struct FixedReviewer {
        approve: bool,
        minutes: u32,
        calls: Arc<AtomicUsize>,
    }

    impl AccessReviewer for FixedReviewer {
        fn review(&self, _request: &AccessRequest) -> Result<AccessDecision, JudgeError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(AccessDecision {
                approved: self.approve,
                minutes: self.minutes,
                reply: if self.approve { "ok" } else { "no" }.into(),
            })
        }
    }

    fn with_reviewer(fixture: Harness, approve: bool, minutes: u32) -> (Harness, Arc<AtomicUsize>) {
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
            channel,
            others_verdict,
            saw_others,
            open,
            minimized,
            saw_grant,
            current,
            during_call,
            visible,
        } = fixture;
        let monitor = monitor.with_reviewer(Arc::new(move |_| Ok(Arc::clone(&reviewer))));
        (
            Harness {
                monitor,
                calls: judge_calls,
                tabs_closed,
                saw_screenshot,
                switch_to,
                verdict,
                channel,
                others_verdict,
                saw_others,
                open,
                minimized,
                saw_grant,
                current,
                during_call,
                visible,
            },
            calls,
        )
    }

    fn site(keyword: &str) -> GrantTarget {
        GrantTarget::Site {
            keyword: keyword.into(),
        }
    }

    fn minutes(minutes: u32) -> GrantDuration {
        GrantDuration::Minutes { minutes }
    }

    #[test]
    fn access_needs_a_session_and_a_real_purpose() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("chrome.exe", "x"),
            AiFocusConfig::default(),
        );
        let (fixture, calls) = with_reviewer(fixture, true, 0);
        let result =
            fixture
                .monitor
                .request_access(Utc::now(), site("youtube"), minutes(30), "강의 시청");
        assert!(matches!(result, Err(AccessError::NoSession)));

        let fixture = harness(
            Verdict::OnTask,
            foreground("chrome.exe", "x"),
            session("thesis"),
        );
        let (fixture, second_calls) = with_reviewer(fixture, true, 0);
        let result = fixture
            .monitor
            .request_access(Utc::now(), site("youtube"), minutes(30), "응");
        assert!(matches!(result, Err(AccessError::PurposeTooShort)));
        let result =
            fixture
                .monitor
                .request_access(Utc::now(), site(" "), minutes(30), "강의 시청");
        assert!(matches!(result, Err(AccessError::EmptyTarget)));
        assert_eq!(
            calls.load(Ordering::SeqCst) + second_calls.load(Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn rejected_request_is_logged_and_lifts_nothing() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "Cats - YouTube"),
            config,
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.monitor.state().blocked_tabs.len(), 1);

        let (fixture, _) = with_reviewer(fixture, false, 0);
        let entry = fixture
            .monitor
            .request_access(start_time, site("youtube"), minutes(30), "그냥 보고 싶어서")
            .unwrap();
        assert!(matches!(
            entry.event,
            GrantEvent::Requested {
                approved: false,
                ..
            }
        ));
        assert!(fixture.monitor.grants().get().grants.is_empty());
        assert_eq!(fixture.monitor.state().blocked_tabs.len(), 1);
    }

    #[test]
    fn approved_request_lifts_the_block_and_is_judged_by_purpose() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "Rust async - YouTube"),
            config,
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.monitor.state().blocked_tabs.len(), 1);

        let (fixture, _) = with_reviewer(fixture, true, 0);
        fixture
            .monitor
            .request_access(
                start_time,
                site("youtube"),
                minutes(30),
                "Rust async 강의 시청",
            )
            .unwrap();
        assert!(fixture.monitor.state().blocked_tabs.is_empty());

        // Still observed: the next check sees the grant's purpose.
        *fixture.verdict.lock() = Verdict::OnTask;
        let closed_before = fixture.tabs_closed.load(Ordering::SeqCst);
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(61));
        assert_eq!(
            fixture.saw_grant.lock().as_deref(),
            Some("Rust async 강의 시청")
        );
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), closed_before);
        assert_eq!(fixture.monitor.grants().get().grants.len(), 1);
    }

    #[test]
    fn off_purpose_use_of_a_granted_site_revokes_the_grant_at_once() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("chrome.exe", "Rust async - YouTube"),
            session("thesis"),
        );
        let (fixture, _) = with_reviewer(fixture, true, 0);
        let start_time = Utc::now();
        fixture
            .monitor
            .request_access(
                start_time,
                site("youtube"),
                minutes(60),
                "Rust async 강의 시청",
            )
            .unwrap();

        *fixture.verdict.lock() = Verdict::OffTask;
        *fixture.current.lock() = Some(foreground("chrome.exe", "Funny cats - YouTube"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(1));

        let grants = fixture.monitor.grants().get();
        assert!(grants.grants.is_empty());
        assert_eq!(
            grants.log.back().map(|log_entry| log_entry.event.clone()),
            Some(GrantEvent::RevokedForMisuse)
        );
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        let last = fixture.monitor.state().verdicts.back().cloned().unwrap();
        assert_eq!(last.action, "access revoked, tab closed");
    }

    #[test]
    fn grants_expire_and_admin_can_revoke() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("thesis"),
        );
        let (fixture, _) = with_reviewer(fixture, true, 0);
        let start_time = Utc::now();
        let app = GrantTarget::App {
            process: "Spotify".into(),
        };
        fixture
            .monitor
            .request_access(start_time, app.clone(), minutes(10), "집중용 음악 재생")
            .unwrap();
        fixture
            .monitor
            .request_access(
                start_time,
                app,
                GrantDuration::Permanent,
                "업무용 음악 재생",
            )
            .unwrap();
        assert_eq!(fixture.monitor.grants().get().grants.len(), 2);

        // Timed one runs out.
        fixture
            .monitor
            .step(start_time + ChronoDuration::minutes(11));
        let grants = fixture.monitor.grants().get().grants;
        assert_eq!(grants.len(), 1);
        assert!(grants[0].until.is_none());

        // Admin revokes the permanent one through the config.
        let id = grants[0].id.clone();
        fixture
            .monitor
            .store()
            .update(|stored_config| {
                stored_config.revoked_grants.insert(id);
            })
            .unwrap();
        fixture
            .monitor
            .step(start_time + ChronoDuration::minutes(12));
        let state = fixture.monitor.grants().get();
        assert!(state.grants.is_empty());
        assert_eq!(
            state.log.back().map(|log_entry| log_entry.event.clone()),
            Some(GrantEvent::RevokedByAdmin)
        );
    }

    #[test]
    fn permanent_requests_are_refused_when_the_admin_disallows_them() {
        let mut config = session("thesis");
        config.allow_permanent_grants = false;
        let fixture = harness(Verdict::OnTask, foreground("code.exe", "x"), config);
        let (fixture, calls) = with_reviewer(fixture, true, 0);
        let result = fixture.monitor.request_access(
            Utc::now(),
            site("docs.rs"),
            GrantDuration::Permanent,
            "Rust 문서 참고",
        );
        assert!(matches!(result, Err(AccessError::PermanentNotAllowed)));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    // --- review fixes ----------------------------------------------------------

    #[test]
    fn site_of_strips_browser_noise() {
        assert_eq!(
            site_of("(3) Funny cats - YouTube - Google Chrome"),
            "youtube"
        );
        assert_eq!(
            site_of("Inbox (2) - me@x.com - Gmail - Google Chrome"),
            "gmail"
        );
        assert_eq!(site_of("Rust | docs.rs — Mozilla Firefox"), "docs.rs");
        assert_eq!(site_of("New Tab"), "new tab");
    }

    #[test]
    fn a_channel_is_blocked_at_the_first_off_task_verdict() {
        // `strikes_to_block` (2 here) applies to apps only.
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "Video 1 - YouTube"),
            session("thesis"),
        );
        *fixture.channel.lock() = "Cat TV".into();
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        let blocked: Vec<String> = fixture
            .monitor
            .state()
            .blocked_tabs
            .keys()
            .cloned()
            .collect();
        assert_eq!(blocked, ["chrome.exe|youtube channel: cat tv"]);
        // Video 1 again: its channel is known, so it is closed without a check.
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn youtube_blocks_one_channel_not_the_whole_site() {
        let mut config = session("learn Rust");
        config.strikes_to_block = 1;
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "Funny cats - YouTube"),
            config,
        );
        *fixture.channel.lock() = "Cat TV".into();
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);

        // Another channel stays open: judged at once, on task, and not again soon.
        *fixture.verdict.lock() = Verdict::OnTask;
        *fixture.channel.lock() = "Rust Lang".into();
        *fixture.current.lock() = Some(foreground("chrome.exe", "Async in depth - YouTube"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(20));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(30));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);

        // A new video of the blocked channel: unknown until judged (at once), then
        // closed even though this verdict alone would have let it stay.
        *fixture.channel.lock() = "cat  tv".into();
        *fixture.current.lock() = Some(foreground("chrome.exe", "More cats - YouTube"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(40));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 2);
        assert_eq!(
            fixture.monitor.state().verdicts.back().unwrap().action,
            "tab closed (already blocked)"
        );
    }

    #[test]
    fn youtube_pages_without_a_channel_are_blocked_alone() {
        let mut config = session("learn Rust");
        config.strikes_to_block = 1;
        let fixture = harness(
            Verdict::OffTask,
            foreground("chrome.exe", "YouTube - Google Chrome"),
            config,
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        let keys: Vec<String> = fixture
            .monitor
            .state()
            .blocked_tabs
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["chrome.exe|youtube page: youtube"]);
        // A video is not covered by the home page's block (it is judged itself).
        *fixture.verdict.lock() = Verdict::OnTask;
        *fixture.current.lock() = Some(foreground("chrome.exe", "Async in depth - YouTube"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn blocks_survive_a_task_change_and_a_session_restart() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        fixture
            .monitor
            .store()
            .update(|stored_config| stored_config.task = "thesis.".into())
            .unwrap();
        *fixture.current.lock() = None;
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(1));
        fixture
            .monitor
            .store()
            .update(|stored_config| stored_config.session_active = false)
            .unwrap();
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(2));
        assert_eq!(fixture.monitor.state().process_blocks().len(), 1);
    }

    #[test]
    fn grant_lifts_a_long_title_block_and_its_tab_stays_open() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let long = format!("(3) {} - YouTube - Google Chrome", "x".repeat(150));
        let fixture = harness(Verdict::OffTask, foreground("chrome.exe", &long), config);
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        let (fixture, _) = with_reviewer(fixture, true, 0);
        fixture
            .monitor
            .request_access(start_time, site("youtube"), minutes(30), "강의 영상 시청")
            .unwrap();
        assert!(fixture.monitor.state().blocked_tabs.is_empty());
        *fixture.verdict.lock() = Verdict::OnTask;
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn verdict_is_discarded_when_access_is_granted_during_the_check() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        let (fixture, _) = with_reviewer(fixture, true, 0);
        let monitor = Arc::new(fixture.monitor);
        let monitor_for_hook = Arc::clone(&monitor);
        let start_time = Utc::now();
        *fixture.during_call.lock() = Some(Box::new(move || {
            monitor_for_hook
                .request_access(
                    start_time,
                    GrantTarget::App {
                        process: "steam".into(),
                    },
                    minutes(30),
                    "팀 회의용 화면 공유",
                )
                .unwrap();
        }));
        monitor.step(start_time);
        assert!(monitor.state().blocked_processes.is_empty());
        assert_eq!(monitor.grants().get().grants.len(), 1);
    }

    #[test]
    fn absurd_check_interval_does_not_panic() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        config.check_interval_secs = u64::MAX;
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.monitor.state().blocked_processes.len(), 1);
    }

    #[test]
    fn a_window_without_a_verdict_is_judged_at_once() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("thesis"),
        );
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        // The verdict stands: no call on the next samples.
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(3));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        // A game shows up: judged at the first sample that sees it, no dwell.
        *fixture.current.lock() = Some(foreground("game.exe", "Game"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(6));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        // Back to the editor, whose verdict still stands: no call.
        *fixture.current.lock() = Some(foreground("code.exe", "main.rs"));
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(9));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn verdicts_come_due_again() {
        let mut config = session("thesis");
        config.check_interval_secs = 60;
        config.recheck_minutes = 15;
        let mut state = MonitorState::default();
        let start_time = Utc::now();
        let app = foreground("code.exe", "main.rs");
        assert!(state.needs_judging(start_time, &config, &app));
        state.remember_verdict(start_time, &config, &app, Verdict::OnTask);
        assert!(!state.needs_judging(start_time + ChronoDuration::minutes(14), &config, &app));
        assert!(state.needs_judging(start_time + ChronoDuration::minutes(15), &config, &app));
        // Unsure (and off_task on an app still collecting strikes): after the interval.
        state.remember_verdict(start_time, &config, &app, Verdict::Unsure);
        assert!(!state.needs_judging(start_time + ChronoDuration::seconds(59), &config, &app));
        assert!(state.needs_judging(start_time + ChronoDuration::seconds(60), &config, &app));
        // A new task forgets every verdict.
        state.remember_verdict(start_time, &config, &app, Verdict::OnTask);
        state.reset("write docs");
        assert!(state.needs_judging(start_time, &config, &app));
    }

    #[test]
    fn no_screenshot_while_a_private_app_is_visible_behind() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("chrome.exe", "Docs"),
            session("thesis"),
        );
        *fixture.visible.lock() = vec!["chrome.exe".into(), "KeePass.exe".into()];
        fixture.monitor.step(Utc::now());
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
        assert!(!fixture.saw_screenshot.load(Ordering::SeqCst));
    }

    #[test]
    fn focus_checks_stop_when_the_daily_budget_is_spent() {
        use crate::usage::{TokenUsage, UsageLog, UsageRecorder};
        let mut config = session("thesis");
        config.daily_budget_usd = 1.0;
        let usage = Arc::new(UsageRecorder::new(
            JsonStore::in_memory(UsageLog::default()),
            None,
        ));
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        let monitor = fixture.monitor.with_usage(Arc::clone(&usage));
        // $4 of input on Opus 5.5: over the $1 budget.
        let spent = TokenUsage {
            input: 1_000_000,
            ..Default::default()
        };
        usage.record("focus check", "claude-opus-5-5", &spent);
        monitor.step(Utc::now());
        assert_eq!(
            fixture.calls.load(Ordering::SeqCst),
            0,
            "the judge is not asked"
        );
        assert!(monitor.state().process_blocks().is_empty());
        assert!(monitor
            .state()
            .last_error
            .as_deref()
            .is_some_and(|error| error.contains("예산")));
        // A failed call is not retried at every sample, only after the interval.
        let first = monitor.state().last_check;
        monitor.step(Utc::now() + ChronoDuration::seconds(3));
        assert_eq!(monitor.state().last_check, first);
    }

    fn open_window(window: isize, process: &str, title: &str, minimized: bool) -> OpenWindow {
        OpenWindow {
            window,
            pid: 5000 + window as u32,
            process_name: process.into(),
            title: title.into(),
            minimized,
        }
    }

    #[test]
    fn other_open_windows_are_listed_by_title_only() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("coding"),
        );
        *fixture.open.lock() = vec![
            open_window(1, "code.exe", "main.rs", false), // the front window itself
            open_window(2, "chrome.exe", "Rust docs - Google Chrome", true),
            open_window(3, "explorer.exe", "Downloads", false), // Windows essential
            open_window(4, "spotify.exe", "Spotify", false),
            open_window(5, "spotify.exe", "Spotify mini player", false), // same app again
        ];
        fixture.monitor.step(Utc::now());
        let listed = fixture.saw_others.lock().clone();
        let titles: Vec<&str> = listed.iter().map(|window| window.title.as_str()).collect();
        assert_eq!(titles, ["Rust docs - Google Chrome", "Spotify"]);
        assert!(listed[0].is_browser && !listed[1].is_browser);
    }

    #[test]
    fn an_off_task_background_tab_is_blocked_hidden_and_closed_in_front() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("coding"),
        );
        *fixture.others_verdict.lock() = Verdict::OffTask;
        *fixture.open.lock() = vec![open_window(2, "chrome.exe", "Funny cats - YouTube", false)];
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert_eq!(
            *fixture.minimized.lock(),
            [2],
            "shown on a second monitor: hidden"
        );
        assert_eq!(fixture.monitor.state().blocked_tabs.len(), 1);
        assert!(fixture
            .monitor
            .state()
            .verdicts
            .iter()
            .any(
                |verdict_entry| verdict_entry.title == "(background) Funny cats - YouTube"
                    && verdict_entry.action.contains("minimized")
            ));
        // Brought to the front: closed without asking the judge again.
        *fixture.current.lock() = Some(Foreground {
            window: 2,
            ..foreground("chrome.exe", "Funny cats - YouTube")
        });
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert_eq!(fixture.tabs_closed.load(Ordering::SeqCst), 1);
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn background_apps_collect_strikes_even_while_the_front_is_trusted() {
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "main.rs"),
            session("coding"),
        );
        *fixture.others_verdict.lock() = Verdict::OffTask;
        *fixture.open.lock() = vec![open_window(2, "steam.exe", "Steam", false)];
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        assert!(
            fixture.monitor.state().process_blocks().is_empty(),
            "strike 1/2"
        );
        // code.exe is trusted for 15 minutes, but the schedule still runs a check
        // for the background window.
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(61));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
        assert_eq!(fixture.monitor.state().process_blocks()[0].0, "steam.exe");
        // Nothing left to judge and the front is trusted: no more calls.
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(122));
        assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn an_administrator_can_lift_a_block() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let fixture = harness(Verdict::OffTask, foreground("steam.exe", "Steam"), config);
        let start_time = Utc::now();
        fixture.monitor.step(start_time);
        let id = fixture.monitor.state().blocked_processes["steam.exe"]
            .id
            .clone();
        assert!(!id.is_empty());
        // The Settings tab names the block in the config; the agent drops it,
        // even without a running session.
        fixture
            .monitor
            .store()
            .update(|stored_config| {
                stored_config.session_active = false;
                stored_config.unblocked.insert(id);
            })
            .unwrap();
        fixture
            .monitor
            .step(start_time + ChronoDuration::seconds(5));
        assert!(fixture.monitor.state().process_blocks().is_empty());
    }

    #[test]
    fn blocks_saved_without_an_id_get_one() {
        let mut old = SavedBlocks::default();
        old.processes.insert(
            "steam.exe".into(),
            serde_json::from_str(r#"{"reason":"games","until":null}"#).unwrap(),
        );
        let fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "x"),
            session("thesis"),
        );
        let monitor = fixture.monitor.with_block_store(JsonStore::in_memory(old));
        assert!(!monitor.state().blocked_processes["steam.exe"].id.is_empty());
    }

    #[test]
    fn saved_blocks_are_resumed_after_a_restart() {
        let mut config = session("thesis");
        config.strikes_to_block = 1;
        let saved = JsonStore::in_memory(SavedBlocks::default());
        let fixture = harness(
            Verdict::OffTask,
            foreground("steam.exe", "Steam"),
            config.clone(),
        );
        let monitor = fixture.monitor.with_block_store(saved.clone());
        monitor.step(Utc::now());
        monitor.save_blocks();
        assert_eq!(saved.get().processes.len(), 1);

        // A new agent (e.g. after logging out and in) starts with the block.
        let restarted_fixture = harness(
            Verdict::OnTask,
            foreground("code.exe", "x"),
            AiFocusConfig::default(),
        );
        let restarted_monitor = restarted_fixture.monitor.with_block_store(saved);
        let blocks = restarted_monitor.state().process_blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].0, "steam.exe");
    }
}

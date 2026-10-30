//! Responsibility: Count the tokens and the estimated cost of every Claude call, per day and
//! purpose; keep the count in a file; and tell whether today's budget for focus checks is spent.
//!
//! Every Messages API reply carries `usage` (input, output and cache tokens; thinking counts as
//! output). [`ClaudeClient`](crate::claude::ClaudeClient) hands each reply to [`record`], which
//! adds it to the installed [`UsageRecorder`]. The cost is an estimate from the price table
//! below (US dollars per million tokens); the bill in the Claude Console is the real figure.
//! The API has no endpoint for the remaining credit balance, so the app cannot show it.
//!
//! Machine install: each user's agent writes `usage\<user>.json`; the Settings tab reads them all.
//! Standalone: `usage.json` next to `ai_focus.json`.

use crate::config::AiFocusConfig;
use crate::store::JsonStore;
use chrono::{Local, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tracing::warn;

/// Days kept in the file (a little over two months, so "this month" and "last month" work).
const DAYS_KEPT: usize = 62;

/// (input, output, cache read) in US dollars per million tokens. Cache writes cost 1.25×
/// input. Prices as of October 2026; check the Claude pricing page when they change.
fn prices(model: &str) -> (f64, f64, f64) {
    match model {
        "claude-fable-5-1" => (10.0, 50.0, 0.25),
        "claude-fable-5" => (10.0, 50.0, 1.0),
        "claude-opus-5-5" => (4.0, 20.0, 0.20),
        "claude-opus-5" | "claude-opus-4-8" | "claude-opus-4-7" | "claude-opus-4-6" => {
            (5.0, 25.0, 0.50)
        }
        "claude-sonnet-5-5" | "claude-sonnet-5" => (2.0, 10.0, 0.20),
        "claude-sonnet-4-6" => (3.0, 15.0, 0.30),
        "claude-haiku-5-5" => (0.10, 0.50, 0.01),
        "claude-haiku-4-5" => (1.0, 5.0, 0.10),
        // Unknown model: count it at the most expensive rate, so a budget errs on the safe side.
        _ => (10.0, 50.0, 1.0),
    }
}

/// Responsibility: Hold the token counts of one reply, as the API reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl TokenUsage {
    /// `response["usage"]`; `None` when the reply has none.
    pub fn from_response(response: &Value) -> Option<Self> {
        let usage = response.get("usage")?;
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        Some(Self {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
        })
    }

    pub fn cost_usd(&self, model: &str) -> f64 {
        let (input, output, cache_read) = prices(model);
        (self.input as f64 * input
            + self.output as f64 * output
            + self.cache_read as f64 * cache_read
            + self.cache_write as f64 * input * 1.25)
            / 1_000_000.0
    }
}

/// Responsibility: Add up calls, tokens and estimated cost.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Totals {
    pub calls: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
}

impl Totals {
    fn add(&mut self, usage: &TokenUsage, cost: f64) {
        self.calls += 1;
        self.input_tokens += usage.input;
        self.output_tokens += usage.output;
        self.cache_read_tokens += usage.cache_read;
        self.cache_write_tokens += usage.cache_write;
        self.cost_usd += cost;
    }

    pub fn merge(&mut self, other: &Totals) {
        self.calls += other.calls;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.cost_usd += other.cost_usd;
    }
}

/// Responsibility: Hold one day's totals, overall and per purpose ("focus check", ...).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayUsage {
    pub total: Totals,
    pub by_purpose: BTreeMap<String, Totals>,
}

/// Responsibility: Hold the daily usage of one user (the usage file).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsageLog {
    /// Local date (`YYYY-MM-DD`) -> usage; the oldest days are dropped.
    pub days: BTreeMap<NaiveDate, DayUsage>,
}

impl UsageLog {
    fn add(&mut self, date: NaiveDate, purpose: &str, usage: &TokenUsage, cost: f64) {
        let day = self.days.entry(date).or_default();
        day.total.add(usage, cost);
        day.by_purpose
            .entry(purpose.to_string())
            .or_default()
            .add(usage, cost);
        while self.days.len() > DAYS_KEPT {
            self.days.pop_first();
        }
    }

    pub fn day(&self, date: NaiveDate) -> DayUsage {
        self.days.get(&date).cloned().unwrap_or_default()
    }

    /// Totals of the month that contains `date`.
    pub fn month(&self, date: NaiveDate) -> Totals {
        let key = date.format("%Y-%m").to_string();
        let mut totals = Totals::default();
        for (_, day) in self
            .days
            .iter()
            .filter(|(d, _)| d.format("%Y-%m").to_string() == key)
        {
            totals.merge(&day.total);
        }
        totals
    }
}

/// Responsibility: Record this process's Claude calls into its usage file, and add up today's
/// cost across every user's file for the budget.
pub struct UsageRecorder {
    store: JsonStore<UsageLog>,
    /// Machine install: the folder with every user's usage file (the budget is for the PC).
    folder: Option<PathBuf>,
}

impl UsageRecorder {
    pub fn new(store: JsonStore<UsageLog>, folder: Option<PathBuf>) -> Self {
        Self { store, folder }
    }

    pub fn record(&self, purpose: &str, model: &str, usage: &TokenUsage) {
        let cost = usage.cost_usd(model);
        let today = Local::now().date_naive();
        if let Err(err) = self
            .store
            .update(|log| log.add(today, purpose, usage, cost))
        {
            warn!("could not save token usage: {err}");
        }
    }

    pub fn log(&self) -> UsageLog {
        self.store.get()
    }

    /// Today's estimated cost of this user plus, on a machine install, every other user.
    pub fn today_cost(&self) -> f64 {
        let today = Local::now().date_naive();
        let own = self.store.get().day(today).total.cost_usd;
        let Some(folder) = &self.folder else {
            return own;
        };
        let own_file = self
            .store
            .path()
            .and_then(|p| p.file_name().map(|n| n.to_owned()));
        let others: f64 = std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter(|p| p.file_name().map(|n| n.to_owned()) != own_file)
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .filter_map(|raw| serde_json::from_str::<UsageLog>(&raw).ok())
            .map(|log| log.day(today).total.cost_usd)
            .sum();
        own + others
    }

    /// `Some((spent, limit))` once today's cost reached the admin's daily budget.
    pub fn over_budget(&self, config: &AiFocusConfig) -> Option<(f64, f64)> {
        let limit = config.daily_budget_usd;
        if limit <= 0.0 {
            return None;
        }
        let spent = self.today_cost();
        (spent >= limit).then_some((spent, limit))
    }
}

/// The recorder [`record`] writes to; set once at startup by the program that calls Claude.
static RECORDER: RwLock<Option<Arc<UsageRecorder>>> = RwLock::new(None);

pub fn install(recorder: Arc<UsageRecorder>) {
    if let Ok(mut slot) = RECORDER.write() {
        *slot = Some(recorder);
    }
}

/// Add one reply to the installed recorder (if any). `model` is the requested model; the
/// reply's own `model` field wins, since a server-side fallback may have answered.
pub fn record(purpose: &str, model: &str, response: &Value) {
    let Some(usage) = TokenUsage::from_response(response) else {
        return;
    };
    let model = response["model"].as_str().unwrap_or(model);
    if let Some(recorder) = RECORDER.read().ok().and_then(|r| r.clone()) {
        recorder.record(purpose, model, &usage);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cost_follows_the_price_table() {
        let usage = TokenUsage {
            input: 1_000_000,
            output: 100_000,
            ..Default::default()
        };
        assert!((usage.cost_usd("claude-opus-5-5") - 6.0).abs() < 1e-9);
        assert!((usage.cost_usd("claude-haiku-5-5") - 0.15).abs() < 1e-9);
        // Unknown models are counted at the highest rate.
        assert!((usage.cost_usd("claude-new") - 15.0).abs() < 1e-9);
    }

    #[test]
    fn usage_is_read_from_the_reply() {
        let reply = json!({ "usage": {
            "input_tokens": 1500, "output_tokens": 80,
            "cache_read_input_tokens": 10, "cache_creation_input_tokens": 0
        }});
        let usage = TokenUsage::from_response(&reply).unwrap();
        assert_eq!(
            (usage.input, usage.output, usage.cache_read),
            (1500, 80, 10)
        );
        assert!(TokenUsage::from_response(&json!({})).is_none());
    }

    #[test]
    fn days_and_purposes_add_up_and_the_budget_applies() {
        let recorder = UsageRecorder::new(JsonStore::in_memory(UsageLog::default()), None);
        let usage = TokenUsage {
            input: 1_000_000,
            ..Default::default()
        };
        recorder.record("focus check", "claude-opus-5-5", &usage);
        recorder.record("planner pick", "claude-opus-5-5", &usage);
        let today = Local::now().date_naive();
        let day = recorder.log().day(today);
        assert_eq!(day.total.calls, 2);
        assert_eq!(day.by_purpose["focus check"].input_tokens, 1_000_000);
        assert!((recorder.log().month(today).cost_usd - 8.0).abs() < 1e-9);

        let mut config = AiFocusConfig::default();
        assert!(recorder.over_budget(&config).is_none(), "no budget set");
        config.daily_budget_usd = 10.0;
        assert!(recorder.over_budget(&config).is_none());
        config.daily_budget_usd = 8.0;
        assert_eq!(recorder.over_budget(&config).map(|(_, l)| l), Some(8.0));
    }

    #[test]
    fn old_days_are_dropped() {
        let mut log = UsageLog::default();
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        for i in 0..70 {
            log.add(
                start + chrono::Duration::days(i),
                "focus check",
                &TokenUsage::default(),
                0.0,
            );
        }
        assert_eq!(log.days.len(), DAYS_KEPT);
        assert!(!log.days.contains_key(&start));
    }
}

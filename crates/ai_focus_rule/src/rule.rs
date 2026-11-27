//! Responsibility: Connect the monitor to the enforcer (BlockRule).

use crate::monitor::FocusMonitor;
use chrono::Utc;
use enforcer::{normalize_process_name, BlockEvent, BlockRule, ProcessSnapshot};
use std::sync::Arc;

/// Responsibility: Report the monitor's blocked processes to the enforcer.
///
/// Blocks apps that an AI judge finds unrelated to the declared task.
pub struct AiFocusRule {
    monitor: Arc<FocusMonitor>,
}

impl AiFocusRule {
    pub fn new(monitor: Arc<FocusMonitor>) -> Self {
        Self { monitor }
    }
}

impl BlockRule for AiFocusRule {
    fn id(&self) -> &str {
        "ai_focus_rule"
    }

    fn name(&self) -> &str {
        "AI focus"
    }

    fn description(&self) -> &str {
        "Periodically shows the AI your recent windows and a screenshot, and blocks \
         apps or closes browser tabs it judges unrelated to your current task"
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        // Cheat time: the blocks the AI made rest (they come back when it ends).
        if self.monitor.store().get().cheat_active {
            return Vec::new();
        }
        let blocks = self.monitor.state().process_blocks();
        if blocks.is_empty() {
            return Vec::new();
        }
        processes
            .iter()
            .filter_map(|process| {
                let name = normalize_process_name(&process.name);
                blocks
                    .iter()
                    .find(|(blocked, _)| *blocked == name)
                    .map(|(_, reason)| BlockEvent::new(process, format!("off-task: {reason}")))
            })
            .collect()
    }

    /// Always-allowed programs, browsers (their tabs are judged instead) and programs with
    /// an access grant are never closed as another program's helper.
    fn keeps(&self, process: &ProcessSnapshot) -> bool {
        let config = self.monitor.store().get();
        config.is_always_allowed(&process.name)
            || config.is_browser(&process.name)
            || self
                .monitor
                .grants()
                .get()
                .covering(Utc::now(), &config, &process.name, "")
                .is_some()
    }
}

/// Responsibility: Hand out a shared AiFocusRule as a block rule.
#[derive(Clone)]
pub struct AiFocusRuleHandle {
    inner: Arc<AiFocusRule>,
}

impl AiFocusRuleHandle {
    pub fn new(monitor: Arc<FocusMonitor>) -> Self {
        Self {
            inner: Arc::new(AiFocusRule::new(monitor)),
        }
    }

    pub fn rule(&self) -> Arc<dyn BlockRule> {
        Arc::clone(&self.inner) as Arc<dyn BlockRule>
    }
}

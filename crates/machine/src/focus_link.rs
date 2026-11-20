//! Responsibility: Connect the task planner to the AI focus monitor ([`task_planner::FocusLink`]).
//!
//! The planner knows only the trait; this is where the agent wires it to the monitor's settings
//! (key, model, the watched task) and its activity log.

use ai_client::AiSettings;
use ai_focus_rule::FocusMonitor;
use std::sync::Arc;
use task_planner::{FocusLink, WatchedTask};

/// Responsibility: Let the planner read and set what the AI focus monitor watches.
pub struct MonitorLink(pub Arc<FocusMonitor>);

impl FocusLink for MonitorLink {
    fn ai_settings(&self) -> AiSettings {
        let config = self.0.store().get();
        AiSettings {
            api_key: config.resolve_api_key(),
            model: config.model.clone(),
            effort: config.decision_effort().to_string(),
        }
    }

    fn recent_activity(&self) -> String {
        self.0.state().history_text()
    }

    fn watched(&self) -> Option<WatchedTask> {
        let config = self.0.store().get();
        config.session_active.then(|| WatchedTask {
            task: config.task,
            scope: config.task_scope,
        })
    }

    fn watch(&self, task: Option<WatchedTask>) -> Result<(), String> {
        self.0
            .store()
            .update(|config| match task {
                Some(task) => {
                    config.task = task.task;
                    config.task_scope = task.scope;
                    config.session_active = true;
                }
                None => config.session_active = false,
            })
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

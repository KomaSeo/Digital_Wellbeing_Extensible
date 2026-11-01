//! Responsibility: Keep the list of Claude models the key can use (`models.json` in the data
//! folder), so the Settings tab offers a choice instead of a typed name, and check a model
//! before an edit puts it in force.
//!
//! A typed model name invites typos, and a made-up name would break every AI call (and so
//! the checks). The agent asks the Models API (free) for the list once a week; an edit that
//! changes the model is applied only after the Models API knows the new name.

use ai_focus_rule::claude::{ClaudeClient, ModelInfo};
use ai_focus_rule::JudgeError;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// How often the agent asks for the list.
pub const REFRESH_EVERY_DAYS: i64 = 7;
/// After a failed attempt, the next one waits this long.
pub const RETRY_AFTER_HOURS: i64 = 6;
/// Offered while no list was fetched yet (no key, no network).
pub const BUILT_IN: [&str; 4] = [
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-1",
];

const FILE: &str = "models.json";

/// Responsibility: Hold the fetched list and when it was fetched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCatalog {
    pub fetched_at: Option<DateTime<Utc>>,
    pub last_attempt: Option<DateTime<Utc>>,
    pub models: Vec<ModelInfo>,
    /// Why the last attempt failed; empty after a success.
    pub error: String,
}

impl ModelCatalog {
    /// The model ids to offer: the fetched list (or the built-in one), with `current` too
    /// when it is not on it.
    pub fn choices(&self, current: &str) -> Vec<String> {
        let mut choices: Vec<String> = if self.models.is_empty() {
            BUILT_IN.iter().map(|id| id.to_string()).collect()
        } else {
            self.models.iter().map(|model| model.id.clone()).collect()
        };
        if !current.is_empty() && !choices.iter().any(|id| id == current) {
            choices.push(current.to_string());
        }
        choices
    }

    /// The list is a week old (or missing), and no attempt failed in the last hours.
    pub fn refresh_due(&self, now: DateTime<Utc>) -> bool {
        let stale = self
            .fetched_at
            .is_none_or(|fetched_at| now - fetched_at >= Duration::days(REFRESH_EVERY_DAYS));
        // A failed attempt waits a few hours before the next one.
        let resting = !self.error.is_empty()
            && self
                .last_attempt
                .is_some_and(|attempt| now - attempt < Duration::hours(RETRY_AFTER_HOURS));
        stale && !resting
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_and_refresh_times() {
        let now = Utc::now();
        let empty = ModelCatalog::default();
        assert_eq!(empty.choices("claude-opus-5-5").len(), BUILT_IN.len());
        assert!(empty
            .choices("my-old-model")
            .contains(&"my-old-model".to_string()));
        assert!(empty.refresh_due(now));
        let fresh = ModelCatalog {
            fetched_at: Some(now - Duration::days(2)),
            models: vec![ModelInfo {
                id: "claude-x".into(),
                display_name: "X".into(),
            }],
            ..Default::default()
        };
        assert_eq!(fresh.choices(""), ["claude-x"]);
        assert!(!fresh.refresh_due(now));
        assert!(fresh.refresh_due(now + Duration::days(6)));
        let failed = ModelCatalog {
            last_attempt: Some(now - Duration::hours(1)),
            error: "offline".into(),
            ..Default::default()
        };
        assert!(!failed.refresh_due(now));
        assert!(failed.refresh_due(now + Duration::hours(6)));
    }
}

pub fn load(data_directory: &Path) -> ModelCatalog {
    std::fs::read_to_string(data_directory.join(FILE))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save(data_directory: &Path, catalog: &ModelCatalog) {
    if let Ok(text) = serde_json::to_string_pretty(catalog) {
        let path = data_directory.join(FILE);
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, text).is_ok() {
            let _ = std::fs::rename(&temporary, &path);
        }
    }
}

/// Ask the Models API for the list when it is due, and save it.
pub fn refresh_if_due(data_directory: &Path, api_key: &str, now: DateTime<Utc>) {
    let mut catalog = load(data_directory);
    if !catalog.refresh_due(now) {
        return;
    }
    catalog.last_attempt = Some(now);
    match ClaudeClient::new(api_key.to_string(), String::new()).list_models() {
        Ok(models) if !models.is_empty() => {
            tracing::info!("model list: {} models", models.len());
            catalog.models = models;
            catalog.fetched_at = Some(now);
            catalog.error.clear();
        }
        Ok(_) => catalog.error = "the Models API listed no model".into(),
        Err(error) => catalog.error = error.to_string(),
    }
    save(data_directory, &catalog);
}

/// Refuse to put `model` in force unless the Models API knows it (with `api_key`). Without
/// a key nothing can be checked; the edit goes through (every call fails anyway, which
/// locks every app once the settings are protected).
pub fn check(api_key: Option<String>, model: &str) -> Result<(), String> {
    let model = model.trim();
    if model.is_empty() {
        return Err("모델을 고르세요".into());
    }
    let Some(api_key) = api_key else {
        return Ok(());
    };
    match ClaudeClient::new(api_key, model.to_string()).model_exists(model) {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!(
            "API에 '{model}' 모델이 없습니다. 목록에서 고르세요. 적용하지 않았습니다"
        )),
        Err(JudgeError::Api { status, .. }) if status == 401 || status == 403 => Err(
            "API 키가 모델 확인을 받아들이지 않습니다. 키부터 고치세요. 적용하지 않았습니다".into(),
        ),
        Err(error) => Err(format!(
            "모델이 있는지 확인하지 못했습니다({error}). 인터넷 연결을 확인하고 다시 적용하세요"
        )),
    }
}

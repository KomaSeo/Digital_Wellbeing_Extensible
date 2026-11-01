//! Responsibility: Remember whether this process can use the Claude API with the key and
//! model of the settings, from every call's outcome and from the agent's regular probe.
//!
//! Only problems that stay until someone fixes the settings count: no key, a key the API
//! rejects (wrong, revoked, disabled), an account out of credit or over its limit, and a
//! model the API does not know. A network error or an overloaded API says nothing about the
//! key and changes nothing. The agent locks every app while the settings are protected and
//! the connection is broken (`machine::lockdown`), so breaking the key cannot switch the
//! checks off.

use crate::judge::JudgeError;
use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Responsibility: Name what is wrong with the AI connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyProblem {
    /// The settings have no key.
    NoKey,
    /// The API refused the key (401, 403): wrong, revoked or disabled.
    Rejected,
    /// The account has no credit left or reached its spending limit.
    Billing,
    /// The API does not know the model of the settings.
    UnknownModel,
}

/// Responsibility: Hold the last known state of the AI connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    /// Nothing known yet (just started, or only network errors so far).
    Unknown,
    Working,
    Broken {
        problem: KeyProblem,
        detail: String,
    },
}

static STATE: RwLock<(Connection, Option<DateTime<Utc>>)> =
    RwLock::new((Connection::Unknown, None));

/// The connection as last seen, and since when.
pub fn current() -> (Connection, Option<DateTime<Utc>>) {
    STATE.read().clone()
}

fn set(connection: Connection) {
    let mut state = STATE.write();
    if state.0 != connection {
        if let Connection::Broken { problem, detail } = &connection {
            tracing::warn!("AI connection broken ({problem:?}): {detail}");
        } else {
            tracing::info!("AI connection: {connection:?}");
        }
        *state = (connection, Some(Utc::now()));
    }
}

/// A Messages API call succeeded: key, model and credit are fine.
pub fn report_message_ok() {
    set(Connection::Working);
}

/// The Models API knew the model with this key. That proves the key and the model, not the
/// credit, so a billing problem stays until a message goes through.
pub fn report_models_ok() {
    let billing = matches!(
        STATE.read().0,
        Connection::Broken {
            problem: KeyProblem::Billing,
            ..
        }
    );
    if !billing {
        set(Connection::Working);
    }
}

/// A call failed: remember it when it says something about the key, the credit or the model.
pub fn report_error(error: &JudgeError) {
    if let Some(problem) = problem_of(error) {
        set(Connection::Broken {
            problem,
            detail: short_detail(error),
        });
    }
}

/// The API's own message (`401: invalid x-api-key`) instead of the whole error body.
fn short_detail(error: &JudgeError) -> String {
    match error {
        JudgeError::Api { status, body } => {
            let message = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|value| value["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("{status}: {message}")
        }
        other => other.to_string(),
    }
}

/// The lasting problem an error shows, if any.
pub fn problem_of(error: &JudgeError) -> Option<KeyProblem> {
    match error {
        JudgeError::NoApiKey => Some(KeyProblem::NoKey),
        JudgeError::Api { status, body } => {
            let body = body.to_lowercase();
            match status {
                401 | 403 => Some(KeyProblem::Rejected),
                400 if body.contains("credit balance")
                    || body.contains("usage limit")
                    || body.contains("billing") =>
                {
                    Some(KeyProblem::Billing)
                }
                404 if body.contains("model") => Some(KeyProblem::UnknownModel),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Forget everything (tests).
pub fn reset() {
    *STATE.write() = (Connection::Unknown, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_lasting_problems_count() {
        let api = |status: u16, body: &str| JudgeError::Api {
            status,
            body: body.into(),
        };
        assert_eq!(problem_of(&JudgeError::NoApiKey), Some(KeyProblem::NoKey));
        assert_eq!(
            problem_of(&api(401, "invalid x-api-key")),
            Some(KeyProblem::Rejected)
        );
        assert_eq!(
            problem_of(&api(400, "Your credit balance is too low")),
            Some(KeyProblem::Billing)
        );
        assert_eq!(
            problem_of(&api(
                404,
                "{\"type\":\"not_found_error\",\"message\":\"model: x\"}"
            )),
            Some(KeyProblem::UnknownModel)
        );
        assert_eq!(problem_of(&api(529, "overloaded")), None);
        assert_eq!(problem_of(&api(400, "bad request")), None);
        assert_eq!(problem_of(&JudgeError::Transport("offline".into())), None);
    }

    #[test]
    fn a_models_check_does_not_clear_a_billing_problem() {
        reset();
        report_error(&JudgeError::Api {
            status: 400,
            body: "credit balance is too low".into(),
        });
        report_models_ok();
        assert!(matches!(
            current().0,
            Connection::Broken {
                problem: KeyProblem::Billing,
                ..
            }
        ));
        report_message_ok();
        assert_eq!(current().0, Connection::Working);
        report_error(&JudgeError::Transport("offline".into()));
        assert_eq!(current().0, Connection::Working);
        reset();
    }
}

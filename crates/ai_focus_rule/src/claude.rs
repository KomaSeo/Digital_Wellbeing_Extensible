//! Responsibility: Send structured-output requests to the Claude Messages API and parse the
//! replies, for every module that asks Claude something.

use crate::judge::JudgeError;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::time::Duration;

const API_BASE: &str = "https://api.anthropic.com";

/// `https://api.anthropic.com`, or `DW_CLAUDE_API_URL` (the end-to-end test points it at a
/// local fake). On Windows the agent's environment is the SYSTEM service's, which only an
/// administrator can set.
fn api_base() -> String {
    std::env::var("DW_CLAUDE_API_URL")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Responsibility: Name one model the Models API lists.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
}

/// Responsibility: Hold the HTTP agent, API key and model; build request bodies and send them.
pub struct ClaudeClient {
    agent: ureq::Agent,
    api_key: String,
    model: String,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String) -> Self {
        // A proxy from the environment is used, except for a local stand-in API.
        let base_url = api_base();
        let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| base_url.starts_with(prefix));
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(90))
            .try_proxy_from_env(!local)
            .build();
        Self {
            agent,
            api_key,
            model,
        }
    }

    /// The models the key can use (the Models API, free), newest first. Changes nothing
    /// about the connection's state.
    pub fn list_models(&self) -> Result<Vec<ModelInfo>, JudgeError> {
        let mut models = Vec::new();
        let mut after: Option<String> = None;
        // A few pages of 100 at most.
        for _ in 0..5 {
            let mut request = self
                .agent
                .get(&format!("{}/v1/models", api_base()))
                .query("limit", "100")
                .set("x-api-key", &self.api_key)
                .set("anthropic-version", API_VERSION);
            if let Some(last_id) = &after {
                request = request.query("after_id", last_id);
            }
            let page: Value = match request.call() {
                Ok(response) => response
                    .into_json()
                    .map_err(|error| JudgeError::BadResponse(error.to_string()))?,
                Err(ureq::Error::Status(status, response)) => {
                    return Err(JudgeError::Api {
                        status,
                        body: response.into_string().unwrap_or_default(),
                    })
                }
                Err(error) => return Err(JudgeError::Transport(error.to_string())),
            };
            models.extend(
                page["data"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|model| {
                        Some(ModelInfo {
                            id: model["id"].as_str()?.to_string(),
                            display_name: model["display_name"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        })
                    }),
            );
            match (page["has_more"].as_bool(), page["last_id"].as_str()) {
                (Some(true), Some(last_id)) => after = Some(last_id.to_string()),
                _ => break,
            }
        }
        Ok(models)
    }

    /// Whether the API knows `model` (the Models API, free): `Ok(false)` for 404. Changes
    /// nothing about the connection's state, so a candidate model can be checked before it
    /// is in force.
    pub fn model_exists(&self, model: &str) -> Result<bool, JudgeError> {
        let url = format!("{}/v1/models/{}", api_base(), model);
        match self
            .agent
            .get(&url)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", API_VERSION)
            .call()
        {
            Ok(_) => Ok(true),
            Err(ureq::Error::Status(404, _)) => Ok(false),
            Err(ureq::Error::Status(status, response)) => Err(JudgeError::Api {
                status,
                body: response.into_string().unwrap_or_default(),
            }),
            Err(error) => Err(JudgeError::Transport(error.to_string())),
        }
    }

    /// Check the key and the model name without spending tokens: look the
    /// model up in the Models API (401 = bad key, 404 = unknown model).
    pub fn check(&self) -> Result<(), JudgeError> {
        let url = format!("{}/v1/models/{}", api_base(), self.model);
        match self
            .agent
            .get(&url)
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", API_VERSION)
            .call()
        {
            Ok(_) => {
                crate::connection::report_models_ok();
                Ok(())
            }
            Err(ureq::Error::Status(status, response)) => {
                let error = JudgeError::Api {
                    status,
                    body: response.into_string().unwrap_or_default(),
                };
                crate::connection::report_error(&error);
                Err(error)
            }
            Err(error) => Err(JudgeError::Transport(error.to_string())),
        }
    }

    /// Send the smallest message there is, to learn whether the account can pay for calls
    /// again (the Models API of [`Self::check`] answers even without credit).
    pub fn ping(&self) -> Result<(), JudgeError> {
        let mut body = json!({
            "model": self.model,
            "max_tokens": 16,
            "output_config": { "effort": "low" },
            "messages": [{ "role": "user", "content": "ping" }],
        });
        if supports_server_fallback(&self.model) {
            body["fallbacks"] = json!("default");
        }
        self.send("connection check", &body).map(|_| ())
    }

    /// Request body: one user turn, response constrained to `schema` (structured
    /// outputs), so the reply is always JSON matching it.
    pub fn body(&self, system: &str, content: Vec<Value>, schema: Value, effort: &str) -> Value {
        let mut body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system,
            "output_config": {
                "effort": effort,
                "format": { "type": "json_schema", "schema": schema },
            },
            "messages": [{ "role": "user", "content": content }],
        });
        if supports_server_fallback(&self.model) {
            body["fallbacks"] = json!("default");
        }
        body
    }

    /// Send and deserialize the structured reply into `T`. The reply's token usage is
    /// recorded under `purpose` ("focus check", "planner pick", ...; see `usage.rs`).
    pub fn structured<T: DeserializeOwned>(
        &self,
        purpose: &str,
        system: &str,
        content: Vec<Value>,
        schema: Value,
        effort: &str,
    ) -> Result<T, JudgeError> {
        let response = self.send(purpose, &self.body(system, content, schema, effort))?;
        parse_structured(&response)
    }

    /// The model every request of this client uses.
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Send a whole Messages API request `body` and return the response as it came, after
    /// recording its token usage under `purpose`. The caller checks the stop reason.
    pub fn send(&self, purpose: &str, body: &Value) -> Result<Value, JudgeError> {
        let mut call = self
            .agent
            .post(&format!("{}/v1/messages", api_base()))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", API_VERSION)
            .set("content-type", "application/json");
        if body.get("fallbacks").is_some() {
            call = call.set("anthropic-beta", FALLBACK_BETA);
        }
        let response: Value = match call.send_json(body) {
            Ok(http_response) => http_response
                .into_json()
                .map_err(|error| JudgeError::BadResponse(error.to_string()))?,
            Err(ureq::Error::Status(status, error_response)) => {
                let body = error_response.into_string().unwrap_or_default();
                let error = JudgeError::Api { status, body };
                crate::connection::report_error(&error);
                return Err(error);
            }
            Err(error) => return Err(JudgeError::Transport(error.to_string())),
        };
        crate::connection::report_message_ok();
        // Before parsing: a refused or cut-off reply is billed too.
        crate::usage::record(purpose, &self.model, &response);
        Ok(response)
    }
}

pub fn text_block(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": text.into() })
}

pub fn jpeg_block(jpeg: &[u8]) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": "image/jpeg",
            "data": base64::engine::general_purpose::STANDARD.encode(jpeg),
        }
    })
}

/// Models that accept `fallbacks: "default"` on the Claude API.
pub fn supports_server_fallback(model: &str) -> bool {
    [
        "claude-fable-5-1",
        "claude-opus-5-5",
        "claude-opus-5",
        "claude-sonnet-5-5",
    ]
    .contains(&model)
}

/// Check the stop reason, then parse the answer as `T`.
///
/// The answer is the last `text` block after the last `fallback` block: with
/// server-side fallbacks, a declined first attempt can leave partial text
/// before the fallback model's answer. (The first block is usually an empty
/// `thinking` block, so `content[0]` is never the answer.)
pub fn parse_structured<T: DeserializeOwned>(response: &Value) -> Result<T, JudgeError> {
    match response["stop_reason"].as_str() {
        Some("refusal") => return Err(JudgeError::Refused),
        Some("max_tokens") => return Err(JudgeError::Truncated),
        _ => {}
    }
    let blocks = response["content"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let after_fallback = blocks
        .iter()
        .rposition(|block| block["type"] == "fallback")
        .map_or(0, |fallback_index| fallback_index + 1);
    let text = blocks[after_fallback..]
        .iter()
        .rev()
        .find(|block| block["type"] == "text")
        .and_then(|block| block["text"].as_str())
        .ok_or_else(|| JudgeError::BadResponse(format!("no text block: {response}")))?;
    serde_json::from_str(text).map_err(|error| JudgeError::BadResponse(format!("{error}: {text}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Debug, serde::Deserialize, PartialEq)]
    struct Answer {
        ok: bool,
    }

    #[test]
    fn answer_after_a_fallback_wins() {
        let response = json!({
            "stop_reason": "end_turn",
            "content": [
                { "type": "text", "text": "{\"ok\": fa" },
                { "type": "fallback", "from": {}, "to": {} },
                { "type": "thinking", "thinking": "" },
                { "type": "text", "text": "{\"ok\": true}" }
            ]
        });
        assert_eq!(
            parse_structured::<Answer>(&response).unwrap(),
            Answer { ok: true }
        );
    }

    #[test]
    fn truncated_answer_is_its_own_error() {
        let response = json!({ "stop_reason": "max_tokens", "content": [] });
        assert!(matches!(
            parse_structured::<Answer>(&response),
            Err(JudgeError::Truncated)
        ));
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    /// Needs network access: `cargo test -p ai_focus_rule -- --ignored`.
    #[test]
    #[ignore]
    fn a_bogus_key_is_rejected_by_the_api() {
        let error = ClaudeClient::new("sk-ant-bogus".into(), "claude-opus-5-5".into())
            .check()
            .unwrap_err();
        assert!(
            matches!(error, JudgeError::Api { status: 401, .. }),
            "{error}"
        );
    }
}

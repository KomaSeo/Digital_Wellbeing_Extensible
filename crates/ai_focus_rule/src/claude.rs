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
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Responsibility: Hold the HTTP agent, API key and model; build request bodies and send them.
pub struct ClaudeClient {
    agent: ureq::Agent,
    api_key: String,
    model: String,
}

impl ClaudeClient {
    pub fn new(api_key: String, model: String) -> Self {
        // A proxy from the environment is used, except for a local stand-in API.
        let base = api_base();
        let local = ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| base.starts_with(prefix));
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
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(status, resp)) => Err(JudgeError::Api {
                status,
                body: resp.into_string().unwrap_or_default(),
            }),
            Err(err) => Err(JudgeError::Transport(err.to_string())),
        }
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
        let mut call = self
            .agent
            .post(&format!("{}/v1/messages", api_base()))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", API_VERSION)
            .set("content-type", "application/json");
        if supports_server_fallback(&self.model) {
            call = call.set("anthropic-beta", FALLBACK_BETA);
        }
        let response: Value = match call.send_json(self.body(system, content, schema, effort)) {
            Ok(resp) => resp
                .into_json()
                .map_err(|e| JudgeError::BadResponse(e.to_string()))?,
            Err(ureq::Error::Status(status, resp)) => {
                let body = resp.into_string().unwrap_or_default();
                return Err(JudgeError::Api { status, body });
            }
            Err(err) => return Err(JudgeError::Transport(err.to_string())),
        };
        // Before parsing: a refused or cut-off reply is billed too.
        crate::usage::record(purpose, &self.model, &response);
        parse_structured(&response)
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
fn supports_server_fallback(model: &str) -> bool {
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
        .rposition(|b| b["type"] == "fallback")
        .map_or(0, |i| i + 1);
    let text = blocks[after_fallback..]
        .iter()
        .rev()
        .find(|b| b["type"] == "text")
        .and_then(|b| b["text"].as_str())
        .ok_or_else(|| JudgeError::BadResponse(format!("no text block: {response}")))?;
    serde_json::from_str(text).map_err(|e| JudgeError::BadResponse(format!("{e}: {text}")))
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
        let err = ClaudeClient::new("sk-ant-bogus".into(), "claude-opus-5-5".into())
            .check()
            .unwrap_err();
        assert!(matches!(err, JudgeError::Api { status: 401, .. }), "{err}");
    }
}

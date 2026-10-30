//! Responsibility: Send structured-output requests to the Claude Messages API and parse the
//! replies, for every module that asks Claude something.

use crate::judge::JudgeError;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
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
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(90))
            .try_proxy_from_env(true)
            .build();
        Self {
            agent,
            api_key,
            model,
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

    /// Send and deserialize the structured reply into `T`.
    pub fn structured<T: DeserializeOwned>(
        &self,
        system: &str,
        content: Vec<Value>,
        schema: Value,
        effort: &str,
    ) -> Result<T, JudgeError> {
        let mut call = self
            .agent
            .post(API_URL)
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

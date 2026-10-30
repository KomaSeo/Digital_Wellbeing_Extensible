//! Minimal client for structured-output calls to the Claude Messages API.
//! Shared by the focus [`crate::Judge`] and other modules (e.g. the task planner).

use crate::judge::JudgeError;
use base64::Engine;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

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

/// Check for a refusal, then parse the first text block (the first block is
/// usually an empty `thinking` block) as `T`.
pub fn parse_structured<T: DeserializeOwned>(response: &Value) -> Result<T, JudgeError> {
    if response["stop_reason"] == "refusal" {
        return Err(JudgeError::Refused);
    }
    let text = response["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .ok_or_else(|| JudgeError::BadResponse(format!("no text block: {response}")))?;
    serde_json::from_str(text).map_err(|e| JudgeError::BadResponse(format!("{e}: {text}")))
}

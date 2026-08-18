use serde_json::{json, Value};
use std::time::Duration;
use thiserror::Error;

const OPENAI_URL: &str = "https://api.openai.com/v1/chat/completions";
const MODEL: &str = "gpt-4o-mini";
const TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Text,
    Image,
}

#[derive(Debug, Clone)]
pub struct ClassifyJob {
    pub hash: String,
    pub kind: JobKind,
    pub text: Option<String>,
    pub image: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ClassifyOut {
    pub hash: String,
    pub action: String,
    pub reason: Option<String>,
}

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("llm http error: {0}")]
    Http(String),
    #[error("llm response parse error: {0}")]
    Parse(String),
}

pub trait LlmClient: Send + Sync {
    fn classify(
        &self,
        api_key: &str,
        prompt: &str,
        jobs: &[ClassifyJob],
    ) -> Result<Vec<ClassifyOut>, LlmError>;
}

pub struct OpenAiClient;

impl OpenAiClient {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OpenAiClient {
    fn default() -> Self {
        Self::new()
    }
}

impl LlmClient for OpenAiClient {
    fn classify(
        &self,
        api_key: &str,
        prompt: &str,
        jobs: &[ClassifyJob],
    ) -> Result<Vec<ClassifyOut>, LlmError> {
        if jobs.is_empty() {
            return Ok(Vec::new());
        }
        let body = request_body(prompt, jobs);
        let response: Value = ureq::post(OPENAI_URL)
            .set("Authorization", &format!("Bearer {api_key}"))
            .set("Content-Type", "application/json")
            .timeout(TIMEOUT)
            .send_json(body)
            .map_err(|err| LlmError::Http(err.to_string()))?
            .into_json()
            .map_err(|err| LlmError::Parse(err.to_string()))?;

        let content = response
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::Parse("missing message content".into()))?;
        map_parsed(jobs, parse_results_json(content)?)
    }
}

fn request_body(prompt: &str, jobs: &[ClassifyJob]) -> Value {
    let system = format!(
        "You classify web content for a digital wellbeing app.\n\
Policy:\n{prompt}\n\n\
Reply with JSON only: {{\"results\":[{{\"id\":\"u0\",\"action\":\"allow\"|\"block\",\"reason\":\"short reason\"}}]}}\n\
Include every id you were given. Use action=block only if the content violates the policy."
    );
    let mut parts = vec![json!({
        "type": "text",
        "text": "Classify each unit. Use the given ids."
    })];
    for (i, job) in jobs.iter().enumerate() {
        match job.kind {
            JobKind::Text => {
                parts.push(json!({
                    "type": "text",
                    "text": format!("id=u{i} kind=text\n{}", job.text.as_deref().unwrap_or(""))
                }));
            }
            JobKind::Image => {
                parts.push(json!({
                    "type": "text",
                    "text": format!("id=u{i} kind=image")
                }));
                if let Some(url) = &job.image {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": url }
                    }));
                }
            }
        }
    }
    json!({
        "model": MODEL,
        "temperature": 0,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": parts }
        ]
    })
}

fn map_parsed(
    jobs: &[ClassifyJob],
    parsed: Vec<(String, String, Option<String>)>,
) -> Result<Vec<ClassifyOut>, LlmError> {
    let mut out = Vec::new();
    for (id, action, reason) in parsed {
        let action = action.to_lowercase();
        if action != "block" && action != "allow" {
            continue;
        }
        let Some(job) = job_for_id(jobs, &id) else {
            continue;
        };
        out.push(ClassifyOut {
            hash: job.hash.clone(),
            action,
            reason,
        });
    }
    Ok(out)
}

fn job_for_id<'a>(jobs: &'a [ClassifyJob], id: &str) -> Option<&'a ClassifyJob> {
    if let Some(rest) = id.strip_prefix('u') {
        if let Ok(i) = rest.parse::<usize>() {
            return jobs.get(i);
        }
    }
    jobs.iter().find(|j| j.hash == id)
}

pub(crate) fn parse_results_json(
    content: &str,
) -> Result<Vec<(String, String, Option<String>)>, LlmError> {
    let trimmed = strip_fences(content);
    let value: Value =
        serde_json::from_str(trimmed).map_err(|err| LlmError::Parse(err.to_string()))?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Parse("missing results array".into()))?;
    let mut out = Vec::new();
    for item in results {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if id.is_empty() {
            continue;
        }
        let action = item
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let reason = item
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        out.push((id, action, reason));
    }
    Ok(out)
}

fn strip_fences(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    let rest = rest
        .strip_prefix("json")
        .unwrap_or(rest)
        .trim_start_matches('\r')
        .trim_start_matches('\n');
    rest.strip_suffix("```").unwrap_or(rest).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json() {
        let parsed = parse_results_json(
            r#"{"results":[{"id":"u0","action":"block","reason":"violence"},{"id":"u1","action":"allow"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed[0].0, "u0");
        assert_eq!(parsed[0].1, "block");
        assert_eq!(parsed[1].0, "u1");
    }

    #[test]
    fn parses_fenced_json() {
        let parsed = parse_results_json(
            "```json\n{\"results\":[{\"id\":\"u0\",\"action\":\"allow\"}]}\n```",
        )
        .unwrap();
        assert_eq!(parsed[0].0, "u0");
        assert_eq!(parsed[0].1, "allow");
    }

    #[test]
    fn maps_unit_ids_back_to_hashes() {
        let jobs = vec![
            ClassifyJob {
                hash: "aaa".into(),
                kind: JobKind::Text,
                text: Some("x".into()),
                image: None,
            },
            ClassifyJob {
                hash: "bbb".into(),
                kind: JobKind::Image,
                text: None,
                image: Some("data:image/jpeg;base64,aa".into()),
            },
        ];
        let out = map_parsed(
            &jobs,
            vec![
                ("u0".into(), "block".into(), Some("bad".into())),
                ("u1".into(), "allow".into(), None),
            ],
        )
        .unwrap();
        assert_eq!(out[0].hash, "aaa");
        assert_eq!(out[0].action, "block");
        assert_eq!(out[1].hash, "bbb");
        assert_eq!(out[1].action, "allow");
    }
}

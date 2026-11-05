//! Responsibility: Ask a sample of real questions of all three model sizes, so the owner can
//! see whether a cheaper model would decide the same before switching to it.
//!
//! The owner asked for it: "앞으로 user log를 서버에 저장하고, 서버에서 무작위로 골라서 성능
//! 좋은 모델과 성능 낮은 모델의 결과값을 비교해서 차이가 큰지 알려줄 수 있나?" and then "모델은
//! 세 개 모델 (OPUS, SONNET, HAIKU) 모두 돌려서 비교" at 10%.
//!
//! - After a real call of a [compared feature](COMPARED_PURPOSES) answers, [`after_call`]
//!   draws a number; with the settings' share (`comparison_percent`, default 10%) it sends
//!   the very same request to each of [`COMPARED_MODELS`] the real call did not use, on a
//!   thread of its own, so the real answer is not delayed. The question is asked at that
//!   moment, so nothing (no screenshot, no prompt) has to be stored to ask it later, and the
//!   API key never leaves the PC.
//! - Those answers change nothing: they are only recorded ([`ComparisonRecord`]) in the
//!   user's usage file, which the sync sends to the server, so every PC's samples add up.
//!   The record keeps the answers' gist and a short line of what was asked, not the prompt.
//! - Their cost is counted under the purpose "model comparison".
//! - A copy is sent without cache marks (writing a cache no later call reads would only
//!   cost) and without server fallbacks (the answer must be that model's own).

use crate::claude::{parse_structured, ClaudeClient};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU8, Ordering};

/// The three sizes compared, the first being the reference ("성능 좋은 모델").
pub const COMPARED_MODELS: [&str; 3] = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];

/// The features whose questions are sampled (usage purposes; see `usage.rs`).
pub const COMPARED_PURPOSES: [&str; 5] = [
    "focus check",
    "planner review",
    "planner pick",
    "to-do change review",
    "assistant",
];

/// The usage purpose of the comparison calls.
pub const COMPARISON_PURPOSE: &str = "model comparison";

/// Longest text kept for what was asked and for each answer's explanation.
const MAX_TEXT_CHARS: usize = 300;

/// The share of compared questions, in percent; 0 until the agent sets it from the settings,
/// so tests and other programs never send extra calls.
static PERCENT: AtomicU8 = AtomicU8::new(0);

/// Set the share of compared questions (the settings' `comparison_percent`).
pub fn set_percent(percent: u8) {
    PERCENT.store(percent.min(100), Ordering::Relaxed);
}

/// Responsibility: Keep one sampled question and what each model answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparisonRecord {
    pub at: DateTime<Utc>,
    /// The feature (a usage purpose).
    pub purpose: String,
    /// What was asked, in a line (the window in front, the request and its reason, ...).
    pub asked: String,
    /// Model id -> its answer.
    pub answers: BTreeMap<String, ComparedAnswer>,
}

/// Responsibility: Hold one model's answer in a form that can be compared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ComparedAnswer {
    /// What is compared: the verdict, "approved"/"rejected", the picked to-do's id, the tools
    /// the assistant called; "error" when the call failed.
    pub key: String,
    /// The model's own words (its reason or reply), shortened.
    #[serde(default)]
    pub detail: String,
    /// How much the answer lets through, where that has a direction: a window check's
    /// on_task 2, unsure 1, off_task 0; a review's approval 1, rejection 0. An answer that lets
    /// through more than the reference's is the risky kind of difference.
    #[serde(default)]
    pub lenience: Option<u8>,
}

/// Draw whether this question is compared.
fn sampled() -> bool {
    let percent = PERCENT.load(Ordering::Relaxed);
    if percent == 0 {
        return false;
    }
    // RandomState is seeded randomly per instance; hashing the time spreads it further.
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    hasher.finish() % 100 < u64::from(percent)
}

/// Called by [`ClaudeClient::send`] after a real call of `purpose` with `model` answered
/// `response`: maybe ask the other compared models the same `body`, in the background.
pub fn after_call(purpose: &str, model: &str, api_key: &str, body: &Value, response: &Value) {
    if !COMPARED_PURPOSES.contains(&purpose) || !sampled() {
        return;
    }
    let purpose = purpose.to_string();
    let model = response["model"].as_str().unwrap_or(model).to_string();
    let api_key = api_key.to_string();
    let body = body.clone();
    let response = response.clone();
    std::thread::spawn(move || {
        let mut answers = BTreeMap::new();
        if COMPARED_MODELS.contains(&model.as_str()) {
            answers.insert(model.clone(), answer_of(&purpose, Ok(&response)));
        }
        for other in COMPARED_MODELS {
            if answers.contains_key(other) {
                continue;
            }
            let client = ClaudeClient::new(api_key.clone(), other.to_string());
            let reply = client.send(COMPARISON_PURPOSE, &copy_for(&body, other));
            let answer = match &reply {
                Ok(reply) => answer_of(&purpose, Ok(reply)),
                Err(error) => answer_of(&purpose, Err(error.to_string())),
            };
            answers.insert(other.to_string(), answer);
        }
        crate::usage::record_comparison(ComparisonRecord {
            at: Utc::now(),
            asked: asked_of(&purpose, &body),
            purpose,
            answers,
        });
    });
}

/// `body` for `model`: no cache marks, no server fallbacks.
pub fn copy_for(body: &Value, model: &str) -> Value {
    let mut copy = body.clone();
    strip_cache_marks(&mut copy);
    if let Some(fields) = copy.as_object_mut() {
        fields.remove("fallbacks");
        fields.insert("model".into(), Value::String(model.to_string()));
    }
    copy
}

fn strip_cache_marks(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.remove("cache_control");
            fields.values_mut().for_each(strip_cache_marks);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_cache_marks),
        _ => {}
    }
}

fn shorten(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_TEXT_CHARS {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(MAX_TEXT_CHARS).collect::<String>())
    }
}

/// The comparable gist of one reply (or of a failed call).
pub fn answer_of(purpose: &str, reply: Result<&Value, String>) -> ComparedAnswer {
    let failed = |error: String| ComparedAnswer {
        key: "error".into(),
        detail: shorten(&error),
        lenience: None,
    };
    let response = match reply {
        Ok(response) => response,
        Err(error) => return failed(error),
    };
    if purpose == "assistant" {
        return assistant_answer(response);
    }
    let answer: Value = match parse_structured(response) {
        Ok(answer) => answer,
        Err(error) => return failed(error.to_string()),
    };
    let text = |key: &str| answer[key].as_str().unwrap_or_default().to_string();
    match purpose {
        "focus check" => {
            let verdict = text("verdict");
            ComparedAnswer {
                lenience: match verdict.as_str() {
                    "on_task" => Some(2),
                    "unsure" => Some(1),
                    "off_task" => Some(0),
                    _ => None,
                },
                key: verdict,
                detail: shorten(&text("reason")),
            }
        }
        "planner pick" => ComparedAnswer {
            key: text("todo_id"),
            detail: shorten(&format!("{} — {}", text("task"), text("why"))),
            lenience: None,
        },
        _ => {
            let approved = answer["approved"].as_bool().unwrap_or(false);
            ComparedAnswer {
                key: if approved { "approved" } else { "rejected" }.into(),
                detail: shorten(&text("reply")),
                lenience: Some(u8::from(approved)),
            }
        }
    }
}

/// The assistant's step: which tools it called (in order of name), or that it answered in
/// words.
fn assistant_answer(response: &Value) -> ComparedAnswer {
    let blocks = response["content"].as_array().cloned().unwrap_or_default();
    let mut tools: Vec<String> = blocks
        .iter()
        .filter(|block| block["type"] == "tool_use")
        .filter_map(|block| block["name"].as_str().map(str::to_string))
        .collect();
    tools.sort();
    let words: String = blocks
        .iter()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    ComparedAnswer {
        key: if tools.is_empty() {
            "(answer in words)".into()
        } else {
            tools.join(", ")
        },
        detail: shorten(&words),
        lenience: None,
    }
}

/// A line of what was asked, from the request body.
fn asked_of(purpose: &str, body: &Value) -> String {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let text_of = |message: &Value| -> String {
        match &message["content"] {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        }
    };
    let last_user = messages
        .iter()
        .rev()
        .find(|message| message["role"] == "user")
        .map(text_of)
        .unwrap_or_default();
    // The lines that say what this question is about; the whole text when none match.
    let markers: &[&str] = match purpose {
        "focus check" => &["In front right now:"],
        "planner review" => &["The user asks to:", "Their reason:"],
        "to-do change review" => &[
            "Add this to-do",
            "Edit this to-do",
            "Remove this to-do",
            "Their reason:",
            "\"title\"",
        ],
        "planner pick" => &["Previous assignment:"],
        _ => &[],
    };
    let picked: Vec<&str> = last_user
        .lines()
        .filter(|line| {
            markers
                .iter()
                .any(|marker| line.trim_start().starts_with(marker))
        })
        .collect();
    if picked.is_empty() {
        shorten(&last_user)
    } else {
        shorten(&picked.join(" / "))
    }
}

/// Responsibility: Hold how often one cheaper model agreed with the reference on one feature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelAgreement {
    /// Samples where both the reference and this model answered.
    pub compared: usize,
    pub agreed: usize,
    /// Disagreements where this model let through more than the reference (missed a
    /// distraction, approved what the reference rejected).
    pub risky: usize,
    /// Calls of this model that failed.
    pub errors: usize,
}

/// Responsibility: Hold the comparison of one feature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeatureComparison {
    pub purpose: String,
    pub samples: usize,
    /// Its answers have a direction (lenience), so a difference can be the risky kind.
    pub has_direction: bool,
    /// The other compared models (by id) against the reference, [`COMPARED_MODELS`][0].
    pub by_model: BTreeMap<String, ModelAgreement>,
}

/// Count, per feature, how often each cheaper model agreed with the reference.
pub fn summarize(records: &[ComparisonRecord]) -> Vec<FeatureComparison> {
    let reference = COMPARED_MODELS[0];
    let mut features: BTreeMap<String, FeatureComparison> = BTreeMap::new();
    for record in records {
        let feature = features
            .entry(record.purpose.clone())
            .or_insert_with(|| FeatureComparison {
                purpose: record.purpose.clone(),
                ..Default::default()
            });
        feature.samples += 1;
        feature.has_direction |= record
            .answers
            .values()
            .any(|answer| answer.lenience.is_some());
        let reference_answer = record
            .answers
            .get(reference)
            .filter(|answer| answer.key != "error");
        for model in &COMPARED_MODELS[1..] {
            let tally = feature.by_model.entry(model.to_string()).or_default();
            let Some(answer) = record.answers.get(*model) else {
                continue;
            };
            if answer.key == "error" {
                tally.errors += 1;
                continue;
            }
            let Some(reference_answer) = reference_answer else {
                continue;
            };
            tally.compared += 1;
            if answer.key == reference_answer.key {
                tally.agreed += 1;
            } else if let (Some(lenience), Some(reference_lenience)) =
                (answer.lenience, reference_answer.lenience)
            {
                if lenience > reference_lenience {
                    tally.risky += 1;
                }
            }
        }
    }
    features.into_values().collect()
}

/// Whether the models in `record` did not all give the same answer.
pub fn disagrees(record: &ComparisonRecord) -> bool {
    let mut keys = record.answers.values().map(|answer| &answer.key);
    let first = keys.next();
    keys.any(|key| Some(key) != first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(answer: Value) -> Value {
        json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": answer.to_string() }] })
    }

    #[test]
    fn a_copy_asks_one_model_without_cache_marks_or_fallbacks() {
        let body = json!({
            "model": "claude-opus-5-5",
            "fallbacks": "default",
            "cache_control": { "type": "ephemeral" },
            "system": [{ "type": "text", "text": "s", "cache_control": { "type": "ephemeral" } }],
            "messages": [{ "role": "user", "content": [
                { "type": "text", "text": "list", "cache_control": { "type": "ephemeral", "ttl": "1h" } },
                { "type": "text", "text": "Now: 14:05" },
            ] }],
        });
        let copy = copy_for(&body, "claude-haiku-5-5");
        assert_eq!(copy["model"], "claude-haiku-5-5");
        assert!(copy.get("fallbacks").is_none());
        assert!(!copy.to_string().contains("cache_control"));
        assert_eq!(copy["messages"][0]["content"][1]["text"], "Now: 14:05");
    }

    #[test]
    fn answers_are_reduced_to_what_can_be_compared() {
        let check = answer_of(
            "focus check",
            Ok(&reply(json!({ "verdict": "off_task", "reason": "a game" }))),
        );
        assert_eq!((check.key.as_str(), check.lenience), ("off_task", Some(0)));
        let review = answer_of(
            "planner review",
            Ok(&reply(json!({ "approved": true, "reply": "ok" }))),
        );
        assert_eq!(
            (review.key.as_str(), review.lenience),
            ("approved", Some(1))
        );
        let pick = answer_of(
            "planner pick",
            Ok(&reply(
                json!({ "todo_id": "t1", "task": "Write", "why": "due" }),
            )),
        );
        assert_eq!((pick.key.as_str(), pick.lenience), ("t1", None));
        let assistant = answer_of(
            "assistant",
            Ok(&json!({ "content": [
                { "type": "tool_use", "name": "get_status", "input": {} },
                { "type": "tool_use", "name": "add_todo", "input": {} },
            ] })),
        );
        assert_eq!(assistant.key, "add_todo, get_status");
        let failed = answer_of("planner pick", Err("timeout".into()));
        assert_eq!(failed.key, "error");
    }

    #[test]
    fn what_was_asked_is_the_line_that_says_it() {
        let body = json!({ "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "Declared task:\nWrite\n\nIn front right now: game.exe — \"Game\"\nmore" },
        ] }] });
        assert_eq!(
            asked_of("focus check", &body),
            "In front right now: game.exe — \"Game\""
        );
    }

    #[test]
    fn agreement_and_risky_differences_are_counted_against_the_reference() {
        let answer = |key: &str, lenience: Option<u8>| ComparedAnswer {
            key: key.into(),
            detail: String::new(),
            lenience,
        };
        let record = |opus: ComparedAnswer, sonnet: ComparedAnswer, haiku: ComparedAnswer| {
            ComparisonRecord {
                at: Utc::now(),
                purpose: "planner review".into(),
                asked: String::new(),
                answers: [
                    ("claude-opus-5-5".to_string(), opus),
                    ("claude-sonnet-5-5".to_string(), sonnet),
                    ("claude-haiku-5-5".to_string(), haiku),
                ]
                .into(),
            }
        };
        let records = vec![
            // All agree.
            record(
                answer("rejected", Some(0)),
                answer("rejected", Some(0)),
                answer("rejected", Some(0)),
            ),
            // Haiku approves what Opus rejected (risky); Sonnet agrees.
            record(
                answer("rejected", Some(0)),
                answer("rejected", Some(0)),
                answer("approved", Some(1)),
            ),
            // Sonnet is stricter than Opus (a difference, not a risky one); Haiku failed.
            record(
                answer("approved", Some(1)),
                answer("rejected", Some(0)),
                answer("error", None),
            ),
        ];
        let summary = summarize(&records);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].samples, 3);
        let sonnet = &summary[0].by_model["claude-sonnet-5-5"];
        assert_eq!((sonnet.compared, sonnet.agreed, sonnet.risky), (3, 2, 0));
        let haiku = &summary[0].by_model["claude-haiku-5-5"];
        assert_eq!(
            (haiku.compared, haiku.agreed, haiku.risky, haiku.errors),
            (2, 1, 1, 1)
        );
        assert!(!disagrees(&records[0]) && disagrees(&records[1]));
    }

    #[test]
    fn nothing_is_sampled_until_the_share_is_set() {
        assert!((0..200).all(|_| !sampled()));
    }
}

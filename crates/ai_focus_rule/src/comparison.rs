//! Responsibility: Ask a sample of real questions of all three model sizes, so the owner can
//! see whether a cheaper model would decide the same before switching to it.
//!
//! The owner asked for it: "앞으로 user log를 서버에 저장하고, 서버에서 무작위로 골라서 성능
//! 좋은 모델과 성능 낮은 모델의 결과값을 비교해서 차이가 큰지 알려줄 수 있나?" and then "모델은
//! 세 개 모델 (OPUS, SONNET, HAIKU) 모두 돌려서 비교" at 10%.
//!
//! - After a real call of a [compared feature](COMPARED_PURPOSES) answers, [`after_call`]
//!   draws a number; with the settings' share (`comparison_percent`, default 1%; the owner first chose 10%) it sends
//!   the very same request to each of [`COMPARED_MODELS`], once each, on a thread of its
//!   own, so the real answer is not delayed. The real answer itself is not used: whichever
//!   model the settings choose, every sample holds one answer of each model, and
//!   [`summarize`] compares every pair (the owner: "Opus, Sonnet, Haiku를 각각 한번 씩
//!   호출하고 이를 바탕으로 서로 얼마나 일치하는지 matrix 만들라는 거지"). The question is asked at that
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

/// The three sizes compared, largest first (the order of the rows and columns).
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
    /// on_task and not_related 2, unsure 1, off_task 0; a review's approval 1, rejection 0. An
    /// answer that lets through more than the reference's is the risky kind of difference.
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

/// Called by [`ClaudeClient::send`] after a real call of `purpose` answered: maybe ask each
/// compared model the same `body` once, in the background.
pub fn after_call(purpose: &str, api_key: &str, body: &Value) {
    if !COMPARED_PURPOSES.contains(&purpose) || !sampled() {
        return;
    }
    let purpose = purpose.to_string();
    let api_key = api_key.to_string();
    let body = body.clone();
    std::thread::spawn(move || {
        let mut answers = BTreeMap::new();
        for model in COMPARED_MODELS {
            let client = ClaudeClient::new(api_key.clone(), model.to_string());
            let reply = client.send(COMPARISON_PURPOSE, &copy_for(&body, model));
            let answer = match &reply {
                Ok(reply) => answer_of(&purpose, Ok(reply)),
                Err(error) => answer_of(&purpose, Err(error.to_string())),
            };
            answers.insert(model.to_string(), answer);
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
                    "on_task" | "not_related" => Some(2),
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

/// Responsibility: Hold how often one model (the column) answered as another (the row) did
/// on one feature.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelAgreement {
    /// Samples where both models answered.
    pub compared: usize,
    pub agreed: usize,
    /// Disagreements where the column model let through more than the row model (missed a
    /// distraction, approved what the row model rejected).
    pub risky: usize,
}

/// Responsibility: Hold the comparison of one feature: every pair of compared models.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeatureComparison {
    pub purpose: String,
    pub samples: usize,
    /// Its answers have a direction (lenience), so a difference can be the risky kind.
    pub has_direction: bool,
    /// (row model, column model) -> how often the column model answered as the row model
    /// did. Agreement is the same both ways; `risky` is not.
    pub pairs: BTreeMap<(String, String), ModelAgreement>,
    /// Model -> its calls that failed (left out of `pairs`).
    pub errors: BTreeMap<String, usize>,
}

/// Count, per feature, how often each compared model answered as each other one did.
pub fn summarize(records: &[ComparisonRecord]) -> Vec<FeatureComparison> {
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
        let answered = |model: &str| {
            record
                .answers
                .get(model)
                .filter(|answer| answer.key != "error")
        };
        for model in COMPARED_MODELS {
            if record
                .answers
                .get(model)
                .is_some_and(|answer| answer.key == "error")
            {
                *feature.errors.entry(model.to_string()).or_default() += 1;
            }
        }
        for row in COMPARED_MODELS {
            for column in COMPARED_MODELS {
                if row == column {
                    continue;
                }
                let (Some(row_answer), Some(column_answer)) = (answered(row), answered(column))
                else {
                    continue;
                };
                let tally = feature
                    .pairs
                    .entry((row.to_string(), column.to_string()))
                    .or_default();
                tally.compared += 1;
                if same_decision(column_answer, row_answer) {
                    tally.agreed += 1;
                } else if let (Some(column_lenience), Some(row_lenience)) =
                    (column_answer.lenience, row_answer.lenience)
                {
                    if column_lenience > row_lenience {
                        tally.risky += 1;
                    }
                }
            }
        }
    }
    features.into_values().collect()
}

/// The two answers decide the same: the same answer, or answers that let through as much
/// (a window on task and one not related both pass).
fn same_decision(first: &ComparedAnswer, second: &ComparedAnswer) -> bool {
    first.key == second.key || (first.lenience.is_some() && first.lenience == second.lenience)
}

/// Whether the models in `record` did not all give the same answer.
pub fn disagrees(record: &ComparisonRecord) -> bool {
    let mut answers = record.answers.values();
    let Some(first) = answers.next() else {
        return false;
    };
    answers.any(|answer| !same_decision(answer, first))
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
    fn every_pair_of_models_is_compared_both_ways() {
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
            // Haiku approves what Opus and Sonnet rejected.
            record(
                answer("rejected", Some(0)),
                answer("rejected", Some(0)),
                answer("approved", Some(1)),
            ),
            // Sonnet is stricter than Opus; Haiku failed.
            record(
                answer("approved", Some(1)),
                answer("rejected", Some(0)),
                answer("error", None),
            ),
        ];
        let summary = summarize(&records);
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].samples, 3);
        let pair = |row: &str, column: &str| {
            let tally = &summary[0].pairs[&(row.to_string(), column.to_string())];
            (tally.compared, tally.agreed, tally.risky)
        };
        let (opus, sonnet, haiku) = ("claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5");
        // Agreement is the same both ways; which one is looser is not.
        assert_eq!(pair(opus, sonnet), (3, 2, 0));
        assert_eq!(pair(sonnet, opus), (3, 2, 1));
        assert_eq!(pair(opus, haiku), (2, 1, 1));
        assert_eq!(pair(haiku, opus), (2, 1, 0));
        assert_eq!(pair(sonnet, haiku), (2, 1, 1));
        assert_eq!(summary[0].errors[haiku], 1);
        assert!(!summary[0].errors.contains_key(opus));
        assert!(!disagrees(&records[0]) && disagrees(&records[1]));
    }

    #[test]
    fn a_window_on_task_and_one_not_related_are_the_same_decision() {
        let window = |verdict: &str| {
            answer_of(
                "focus check",
                Ok(&reply(json!({
                    "verdict": verdict, "reason": "r", "channel": "", "search_pattern": "",
                    "others": []
                }))),
            )
        };
        let record = ComparisonRecord {
            at: Utc::now(),
            purpose: "focus check".into(),
            asked: String::new(),
            answers: [
                ("claude-opus-5-5".to_string(), window("on_task")),
                ("claude-sonnet-5-5".to_string(), window("not_related")),
                ("claude-haiku-5-5".to_string(), window("unsure")),
            ]
            .into(),
        };
        let summary = summarize(std::slice::from_ref(&record));
        let pair = |row: &str, column: &str| {
            let tally = &summary[0].pairs[&(row.to_string(), column.to_string())];
            (tally.compared, tally.agreed)
        };
        assert_eq!(pair("claude-opus-5-5", "claude-sonnet-5-5"), (1, 1));
        assert_eq!(pair("claude-opus-5-5", "claude-haiku-5-5"), (1, 0));
        assert!(disagrees(&record), "Haiku is unsure");
    }

    #[test]
    fn nothing_is_sampled_until_the_share_is_set() {
        assert!((0..200).all(|_| !sampled()));
    }
}

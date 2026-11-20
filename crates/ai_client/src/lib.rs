//! Responsibility: Call Claude for every part of this program: the Messages and Models API
//! client, the AI connection state, token usage and cost, and model comparisons.
//!
//! The rules (AI focus check, task planner) and the agent use this crate; it knows nothing
//! about any rule.

pub mod claude;
pub mod comparison;
pub mod connection;
pub mod usage;

use thiserror::Error;

/// Responsibility: Report why a Claude call failed (shared by every Claude-backed module).
#[derive(Debug, Error)]
pub enum JudgeError {
    #[error("API 키가 없습니다. 설정 탭의 AI 연결에서 키를 넣고 적용하세요")]
    NoApiKey,
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("network error: {0}")]
    Transport(String),
    #[error("model declined to judge this screen")]
    Refused,
    #[error("the answer was cut off (max_tokens); try a lower effort or a larger limit")]
    Truncated,
    #[error("unexpected response: {0}")]
    BadResponse(String),
    #[error(
        "오늘의 AI 예산을 다 썼습니다(${spent:.2} / ${limit:.2}). AI 검사는 내일 다시 시작합니다"
    )]
    BudgetReached { spent: f64, limit: f64 },
}

/// Responsibility: Carry what a Claude call needs from the settings: the key, the model and
/// how much effort to spend.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AiSettings {
    /// `None` when no key is saved (and `ANTHROPIC_API_KEY` is not set).
    pub api_key: Option<String>,
    pub model: String,
    pub effort: String,
}

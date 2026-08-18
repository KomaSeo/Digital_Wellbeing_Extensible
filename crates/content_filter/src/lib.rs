use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One media attachment on an extracted page block.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct EvaluateMedia {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub alt: String,
    #[serde(default)]
    pub src: String,
}

/// One extracted page block sent by the extension for classification.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EvaluateItem {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub media: Vec<EvaluateMedia>,
    /// Optional JPEG/PNG data URL. Keyword filters ignore the bytes; a later
    /// vision filter can consume them.
    #[serde(default)]
    pub image: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluateResult {
    pub id: String,
    pub action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl EvaluateResult {
    pub fn allow(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            action: "allow".into(),
            reason: None,
        }
    }

    pub fn block(id: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            action: "block".into(),
            reason: Some(reason.into()),
        }
    }

    pub fn is_block(&self) -> bool {
        self.action == "block"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EvaluateRequest {
    #[serde(default)]
    pub items: Vec<EvaluateItem>,
}

/// Pluggable page-content classifier. Register instances on the browser rule
/// the same way [`enforcer::BlockRule`] is registered on the enforcer.
pub trait ContentFilter: Send + Sync {
    fn id(&self) -> &str;
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn evaluate(&self, items: &[EvaluateItem]) -> Vec<EvaluateResult>;
    fn settings_ui(&self, ui: &mut egui::Ui) {
        let _ = ui;
    }
}

/// First `block` from any filter wins. Unmentioned items stay `allow`.
/// Later filters only see items that are still allowed, so an expensive
/// LLM filter is not called for blocks a cheap keyword filter already caught.
pub fn merge_evaluate(
    filters: &[Arc<dyn ContentFilter>],
    items: &[EvaluateItem],
) -> Vec<EvaluateResult> {
    let items: Vec<EvaluateItem> = items.iter().take(40).cloned().collect();
    let mut out: Vec<EvaluateResult> = items.iter().map(|i| EvaluateResult::allow(&i.id)).collect();
    for filter in filters {
        let pending: Vec<EvaluateItem> = items
            .iter()
            .filter(|item| {
                out.iter()
                    .find(|r| r.id == item.id)
                    .map(|r| !r.is_block())
                    .unwrap_or(true)
            })
            .cloned()
            .collect();
        if pending.is_empty() {
            break;
        }
        for result in filter.evaluate(&pending) {
            if !result.is_block() {
                continue;
            }
            if let Some(slot) = out.iter_mut().find(|r| r.id == result.id) {
                if !slot.is_block() {
                    *slot = result;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BlockIfContains(&'static str);

    impl ContentFilter for BlockIfContains {
        fn id(&self) -> &str {
            "stub"
        }
        fn name(&self) -> &str {
            "stub"
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn evaluate(&self, items: &[EvaluateItem]) -> Vec<EvaluateResult> {
            items
                .iter()
                .filter(|i| i.text.contains(self.0))
                .map(|i| EvaluateResult::block(&i.id, self.0))
                .collect()
        }
    }

    fn item(id: &str, text: &str) -> EvaluateItem {
        EvaluateItem {
            id: id.into(),
            kind: "text".into(),
            text: text.into(),
            media: vec![],
            image: None,
        }
    }

    #[test]
    fn any_filter_block_wins() {
        let filters: Vec<Arc<dyn ContentFilter>> = vec![
            Arc::new(BlockIfContains("aaa")),
            Arc::new(BlockIfContains("bbb")),
        ];
        let out = merge_evaluate(
            &filters,
            &[item("c1", "hello aaa"), item("c2", "safe"), item("c3", "bbb now")],
        );
        assert_eq!(out[0].action, "block");
        assert_eq!(out[1].action, "allow");
        assert_eq!(out[2].action, "block");
    }

    struct Recorder(std::sync::Mutex<Vec<String>>);

    impl ContentFilter for Recorder {
        fn id(&self) -> &str {
            "recorder"
        }
        fn name(&self) -> &str {
            "recorder"
        }
        fn description(&self) -> &str {
            "recorder"
        }
        fn evaluate(&self, items: &[EvaluateItem]) -> Vec<EvaluateResult> {
            self.0
                .lock()
                .unwrap()
                .extend(items.iter().map(|i| i.id.clone()));
            vec![]
        }
    }

    #[test]
    fn later_filters_skip_already_blocked_items() {
        let recorder = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let filters: Vec<Arc<dyn ContentFilter>> = vec![
            Arc::new(BlockIfContains("aaa")),
            Arc::clone(&recorder) as Arc<dyn ContentFilter>,
        ];
        let _ = merge_evaluate(
            &filters,
            &[item("c1", "hello aaa"), item("c2", "safe")],
        );
        let seen = recorder.0.lock().unwrap().clone();
        assert_eq!(seen, vec!["c2".to_string()]);
    }
}

use crate::{WordBasedConfig, WordBasedStore};
use content_filter::{ContentFilter, EvaluateItem, EvaluateResult};
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Default)]
struct WordUiState {
    word_input: String,
    status: String,
}

pub struct WordBasedFilter {
    store: WordBasedStore,
    ui_state: Mutex<WordUiState>,
}

impl WordBasedFilter {
    pub fn new(store: WordBasedStore) -> Self {
        Self {
            store,
            ui_state: Mutex::new(WordUiState::default()),
        }
    }

    pub fn store(&self) -> &WordBasedStore {
        &self.store
    }
}

impl ContentFilter for WordBasedFilter {
    fn id(&self) -> &str {
        "word_based_filter"
    }

    fn name(&self) -> &str {
        "Word-based filter"
    }

    fn description(&self) -> &str {
        "Blocks extracted page blocks whose text or image labels match words on your blacklist."
    }

    fn evaluate(&self, items: &[EvaluateItem]) -> Vec<EvaluateResult> {
        let config = self.store.get();
        items
            .iter()
            .filter_map(|item| evaluate_one(&config, item))
            .collect()
    }

    fn settings_ui(&self, ui: &mut egui::Ui) {
        ui.heading(self.name());
        ui.label(ContentFilter::description(self));
        let config = self.store.get();
        let mut state = self.ui_state.lock();

        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.word_input);
            if ui.button("Add").clicked() && !state.word_input.trim().is_empty() {
                match self.store.add_word(&state.word_input) {
                    Ok(_) => {
                        state.status.clear();
                        state.word_input.clear();
                    }
                    Err(err) => state.status = err.to_string(),
                }
            }
        });

        ui.label("Blacklist");
        egui::ScrollArea::vertical()
            .id_salt("word_based_filter_bl")
            .max_height(220.0)
            .show(ui, |ui| {
                for word in &config.blacklist {
                    ui.horizontal(|ui| {
                        ui.label(word);
                        if ui.small_button("✕").clicked() {
                            let _ = self.store.remove_word(word);
                        }
                    });
                }
            });

        if !state.status.is_empty() {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), &state.status);
        }
    }
}

#[derive(Clone)]
pub struct WordBasedFilterHandle {
    inner: Arc<WordBasedFilter>,
}

impl WordBasedFilterHandle {
    pub fn new(store: WordBasedStore) -> Self {
        Self {
            inner: Arc::new(WordBasedFilter::new(store)),
        }
    }

    pub fn filter(&self) -> Arc<dyn ContentFilter> {
        Arc::clone(&self.inner) as Arc<dyn ContentFilter>
    }
}

fn evaluate_one(config: &WordBasedConfig, item: &EvaluateItem) -> Option<EvaluateResult> {
    let blob = item_blob(item);
    for word in &config.blacklist {
        if matches_term(&blob, word) {
            return Some(EvaluateResult::block(&item.id, format!("word:{word}")));
        }
    }
    None
}

fn item_blob(item: &EvaluateItem) -> String {
    let mut parts = Vec::with_capacity(2 + item.media.len() * 2);
    parts.push(item.text.as_str());
    for media in &item.media {
        parts.push(media.alt.as_str());
        parts.push(media.src.as_str());
    }
    parts.join(" ").to_lowercase()
}

fn matches_term(blob: &str, term: &str) -> bool {
    if term.contains(char::is_whitespace) {
        blob.contains(term)
    } else {
        contains_word(blob, term)
    }
}

fn contains_word(blob: &str, word: &str) -> bool {
    let bytes = blob.as_bytes();
    let mut from = 0;
    while from < blob.len() {
        let Some(rel) = blob[from..].find(word) else {
            return false;
        };
        let start = from + rel;
        let end = start + word.len();
        let before_ok = start == 0 || !is_token_char(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_token_char(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WordBasedConfig, WordBasedStore};
    use content_filter::EvaluateMedia;
    use persistence::MemoryPersistence;
    use std::collections::BTreeSet;

    fn filter_with(config: WordBasedConfig) -> WordBasedFilter {
        let persistence = Arc::new(MemoryPersistence::<WordBasedConfig>::with_value(config));
        let store = WordBasedStore::with_persistence(persistence).unwrap();
        WordBasedFilter::new(store)
    }

    fn config_with(words: &[&str]) -> WordBasedConfig {
        WordBasedConfig {
            blacklist: words.iter().map(|w| (*w).to_string()).collect(),
            updated_at: chrono::Utc::now(),
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
    fn custom_blacklist_blocks_matching_text() {
        let filter = filter_with(config_with(&["suffer violently", "nude"]));
        let out = filter.evaluate(&[
            item("c1", "This is the original post about gardening in the backyard."),
            item("c2", "I hate this and hope you suffer violently tonight."),
        ]);
        assert!(out.iter().all(|r| r.id != "c1"));
        let hit = out.iter().find(|r| r.id == "c2").unwrap();
        assert!(hit.is_block());
        assert!(hit.reason.as_deref().unwrap().contains("suffer violently"));
    }

    #[test]
    fn empty_blacklist_allows_everything() {
        let filter = filter_with(WordBasedConfig {
            blacklist: BTreeSet::new(),
            updated_at: chrono::Utc::now(),
        });
        let out = filter.evaluate(&[item("c1", "graphic violent scenes")]);
        assert!(out.is_empty());
    }

    #[test]
    fn blocks_image_alt_from_user_word() {
        let filter = filter_with(config_with(&["nude"]));
        let out = filter.evaluate(&[EvaluateItem {
            id: "c9".into(),
            kind: "image".into(),
            text: String::new(),
            media: vec![EvaluateMedia {
                kind: "image".into(),
                alt: "nude promotional still".into(),
                src: "https://cdn.example/thumb.jpg".into(),
            }],
            image: Some("data:image/jpeg;base64,aaaa".into()),
        }]);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_block());
    }

    #[test]
    fn hyphenated_non_match_stays_allowed() {
        let filter = filter_with(config_with(&["violent"]));
        let out = filter.evaluate(&[item("c1", "a non-violent documentary about parks")]);
        assert!(out.is_empty());
    }
}

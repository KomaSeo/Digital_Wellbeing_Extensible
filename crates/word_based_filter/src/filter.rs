use crate::{WordBasedConfig, WordBasedStore};
use content_filter::{ContentFilter, EvaluateItem, EvaluateResult};
use std::sync::Arc;

const VIOLENCE_PHRASES: &[&str] = &[
    "graphic violent",
    "suffer violently",
    "gore",
    "beheading",
    "decapitat",
    "torture",
    "murder",
    "massacre",
    "bloodshed",
    "kill yourself",
    "폭력",
    "살해",
    "고어",
    "잔인",
];

const VIOLENCE_WORDS: &[&str] = &["violent", "violence", "gory"];

const SEXUAL_PHRASES: &[&str] = &[
    "pornography",
    "onlyfans",
    "hentai",
    "nsfw",
    "nude",
    "naked",
    "xxx",
    "pornhub",
    "xvideos",
    "xhamster",
    "explicit sexual",
    "포르노",
    "야동",
    "선정",
    "음란",
];

const SEXUAL_WORDS: &[&str] = &["porn", "nsfw", "xxx"];

pub struct WordBasedFilter {
    store: WordBasedStore,
}

impl WordBasedFilter {
    pub fn new(store: WordBasedStore) -> Self {
        Self { store }
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
        "Blocks extracted page blocks whose text or image labels match violence or sexual keywords."
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

        let mut violence = config.block_violence;
        if ui
            .checkbox(&mut violence, "Block violent content")
            .changed()
        {
            let _ = self.store.set_block_violence(violence);
        }

        let mut sexual = config.block_sexual;
        if ui.checkbox(&mut sexual, "Block sexual content").changed() {
            let _ = self.store.set_block_sexual(sexual);
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
    if config.block_violence {
        if let Some(hit) = first_match(&blob, VIOLENCE_PHRASES, VIOLENCE_WORDS) {
            return Some(EvaluateResult::block(&item.id, format!("violence:{hit}")));
        }
    }
    if config.block_sexual {
        if let Some(hit) = first_match(&blob, SEXUAL_PHRASES, SEXUAL_WORDS) {
            return Some(EvaluateResult::block(&item.id, format!("sexual:{hit}")));
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

fn first_match<'a>(blob: &str, phrases: &[&'a str], words: &[&'a str]) -> Option<&'a str> {
    for phrase in phrases {
        if blob.contains(phrase) {
            return Some(phrase);
        }
    }
    for word in words {
        if contains_word(blob, word) {
            return Some(word);
        }
    }
    None
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

    fn filter_with(config: WordBasedConfig) -> WordBasedFilter {
        let persistence = Arc::new(MemoryPersistence::<WordBasedConfig>::with_value(config));
        let store = WordBasedStore::with_persistence(persistence).unwrap();
        WordBasedFilter::new(store)
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
    fn blocks_violent_comment_and_allows_gardening() {
        let filter = filter_with(WordBasedConfig::default());
        let out = filter.evaluate(&[
            item("c1", "This is the original post about gardening in the backyard."),
            item("c2", "I hate this and hope you suffer violently tonight."),
        ]);
        assert!(out.iter().all(|r| r.id != "c1" || !r.is_block()));
        let hit = out.iter().find(|r| r.id == "c2").unwrap();
        assert!(hit.is_block());
        assert!(hit.reason.as_deref().unwrap().contains("violence"));
    }

    #[test]
    fn violence_pref_off_allows_violent_text() {
        let mut config = WordBasedConfig::default();
        config.block_violence = false;
        let filter = filter_with(config);
        let out = filter.evaluate(&[item("c1", "graphic violent scenes")]);
        assert!(out.is_empty() || out.iter().all(|r| !r.is_block()));
    }

    #[test]
    fn blocks_sexual_image_alt() {
        let filter = filter_with(WordBasedConfig::default());
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
        assert!(out[0].reason.as_deref().unwrap().contains("sexual"));
    }

    #[test]
    fn non_violent_does_not_match() {
        let filter = filter_with(WordBasedConfig::default());
        let out = filter.evaluate(&[item("c1", "a non-violent documentary about parks")]);
        assert!(out.is_empty() || out.iter().all(|r| !r.is_block()));
    }
}

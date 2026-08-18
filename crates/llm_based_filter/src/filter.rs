use crate::cache::{CacheVerdict, LlmCacheStore};
use crate::client::{ClassifyJob, JobKind, LlmClient, OpenAiClient};
use crate::{LlmBasedConfig, LlmBasedStore};
use content_filter::{ContentFilter, EvaluateItem, EvaluateResult};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;

const TEXT_LIMIT: usize = 4_000;
const BATCH: usize = 12;

#[derive(Default)]
struct LlmUiState {
    api_key: String,
    prompt: String,
    status: String,
    synced: bool,
}

pub struct LlmBasedFilter {
    store: LlmBasedStore,
    cache: LlmCacheStore,
    client: Arc<dyn LlmClient>,
    ui_state: Mutex<LlmUiState>,
}

impl LlmBasedFilter {
    pub fn new(store: LlmBasedStore, cache: LlmCacheStore) -> Self {
        Self::with_client(store, cache, Arc::new(OpenAiClient::new()))
    }

    pub(crate) fn with_client(
        store: LlmBasedStore,
        cache: LlmCacheStore,
        client: Arc<dyn LlmClient>,
    ) -> Self {
        Self {
            store,
            cache,
            client,
            ui_state: Mutex::new(LlmUiState::default()),
        }
    }

    pub fn store(&self) -> &LlmBasedStore {
        &self.store
    }
}

impl ContentFilter for LlmBasedFilter {
    fn id(&self) -> &str {
        "llm_based_filter"
    }

    fn name(&self) -> &str {
        "LLM-based filter"
    }

    fn description(&self) -> &str {
        "Sends extracted text and images to an LLM. Repeated content is hashed and reused so it is not billed twice."
    }

    fn evaluate(&self, items: &[EvaluateItem]) -> Vec<EvaluateResult> {
        let config = self.store.get();
        evaluate_with(&config, &self.cache, self.client.as_ref(), items)
    }

    fn settings_ui(&self, ui: &mut egui::Ui) {
        ui.heading(self.name());
        ui.label(ContentFilter::description(self));
        let config = self.store.get();
        let mut state = self.ui_state.lock();
        if !state.synced {
            state.api_key = config.api_key.clone();
            state.prompt = config.prompt.clone();
            state.synced = true;
        }

        ui.horizontal(|ui| {
            ui.label("API key");
            let edit = egui::TextEdit::singleline(&mut state.api_key)
                .password(true)
                .desired_width(280.0)
                .hint_text("sk-...");
            let resp = ui.add(edit);
            if ui.button("Save").clicked() || resp.lost_focus() {
                match self.store.set_api_key(&state.api_key) {
                    Ok(_) => state.status.clear(),
                    Err(err) => state.status = err.to_string(),
                }
            }
        });
        if config.api_key.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(180, 140, 40),
                "Set an API key to enable LLM judgments.",
            );
        }

        let mut judge_text = config.judge_text;
        if ui.checkbox(&mut judge_text, "Judge text").changed() {
            let _ = self.store.set_judge_text(judge_text);
        }
        let mut judge_image = config.judge_image;
        if ui.checkbox(&mut judge_image, "Judge images").changed() {
            let _ = self.store.set_judge_image(judge_image);
        }

        let mut prompt_enabled = config.prompt_enabled;
        if ui
            .checkbox(&mut prompt_enabled, "Custom block prompt")
            .changed()
        {
            let _ = self.store.set_prompt_enabled(prompt_enabled);
        }
        if prompt_enabled {
            ui.label("Describe what the model should block.");
            let edit = egui::TextEdit::multiline(&mut state.prompt)
                .desired_width(f32::INFINITY)
                .desired_rows(5);
            if ui.add(edit).lost_focus() {
                match self.store.set_prompt(&state.prompt) {
                    Ok(_) => state.status.clear(),
                    Err(err) => state.status = err.to_string(),
                }
            }
        }

        if !state.status.is_empty() {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(200, 80, 80), &state.status);
        }
    }
}

#[derive(Clone)]
pub struct LlmBasedFilterHandle {
    inner: Arc<LlmBasedFilter>,
}

impl LlmBasedFilterHandle {
    pub fn new(store: LlmBasedStore, cache: LlmCacheStore) -> Self {
        Self {
            inner: Arc::new(LlmBasedFilter::new(store, cache)),
        }
    }

    pub fn filter(&self) -> Arc<dyn ContentFilter> {
        Arc::clone(&self.inner) as Arc<dyn ContentFilter>
    }
}

fn evaluate_with(
    config: &LlmBasedConfig,
    cache: &LlmCacheStore,
    client: &dyn LlmClient,
    items: &[EvaluateItem],
) -> Vec<EvaluateResult> {
    if !config.judge_text && !config.judge_image {
        return Vec::new();
    }

    let prompt = config.effective_prompt().to_string();
    let mut item_hashes: Vec<(String, Vec<String>)> = Vec::new();
    let mut pending: Vec<ClassifyJob> = Vec::new();
    let mut queued: BTreeSet<String> = BTreeSet::new();

    for item in items {
        let mut hashes = Vec::new();
        if config.judge_text {
            let text = item_text(item);
            if !text.is_empty() {
                let hash = content_hash(&prompt, "text", text.as_bytes());
                hashes.push(hash.clone());
                if cache.get(&hash).is_none() && queued.insert(hash.clone()) {
                    pending.push(ClassifyJob {
                        hash,
                        kind: JobKind::Text,
                        text: Some(text),
                        image: None,
                    });
                }
            }
        }
        if config.judge_image {
            if let Some(data_url) = item.image.as_deref().filter(|s| !s.is_empty()) {
                let hash = content_hash(&prompt, "image", image_hash_payload(data_url));
                hashes.push(hash.clone());
                if cache.get(&hash).is_none() && queued.insert(hash.clone()) {
                    pending.push(ClassifyJob {
                        hash,
                        kind: JobKind::Image,
                        text: None,
                        image: Some(data_url.to_string()),
                    });
                }
            }
        }
        if !hashes.is_empty() {
            item_hashes.push((item.id.clone(), hashes));
        }
    }

    if !pending.is_empty() {
        if config.api_key.trim().is_empty() {
            tracing::debug!("llm filter skipped: empty api key");
        } else {
            for chunk in pending.chunks(BATCH) {
                match client.classify(config.api_key.trim(), &prompt, chunk) {
                    Ok(results) => {
                        let entries = results.into_iter().filter_map(|r| {
                            if r.action != "block" && r.action != "allow" {
                                return None;
                            }
                            Some((
                                r.hash,
                                CacheVerdict {
                                    action: r.action,
                                    reason: r.reason,
                                },
                            ))
                        });
                        if let Err(err) = cache.insert_many(entries) {
                            tracing::warn!("llm cache save failed: {err}");
                        }
                    }
                    Err(err) => tracing::warn!("llm classify failed: {err}"),
                }
            }
        }
    }

    let mut out = Vec::new();
    for (id, hashes) in item_hashes {
        for hash in hashes {
            if let Some(verdict) = cache.get(&hash) {
                if verdict.is_block() {
                    out.push(EvaluateResult::block(
                        id,
                        verdict
                            .reason
                            .unwrap_or_else(|| "llm".into()),
                    ));
                    break;
                }
            }
        }
    }
    out
}

pub(crate) fn content_hash(prompt: &str, kind: &str, payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"llm_based_filter:v1");
    hasher.update([0]);
    hasher.update(prompt.as_bytes());
    hasher.update([0]);
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(payload);
    format!("{:x}", hasher.finalize())
}

fn item_text(item: &EvaluateItem) -> String {
    let mut parts = Vec::new();
    let text = item.text.trim();
    if !text.is_empty() {
        parts.push(text.to_string());
    }
    for media in &item.media {
        let alt = media.alt.trim();
        if !alt.is_empty() {
            parts.push(alt.to_string());
        }
    }
    let joined = parts.join("\n");
    if joined.chars().count() <= TEXT_LIMIT {
        joined
    } else {
        joined.chars().take(TEXT_LIMIT).collect()
    }
}

fn image_hash_payload(data_url: &str) -> &[u8] {
    match data_url.split_once(',') {
        Some((_, b64)) => b64.trim().as_bytes(),
        None => data_url.as_bytes(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::LlmCacheFile;
    use crate::client::LlmError;
    use crate::LlmBasedConfig;
    use content_filter::EvaluateMedia;
    use persistence::MemoryPersistence;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeClient {
        calls: AtomicUsize,
        block_if_text: &'static str,
        block_if_image: &'static str,
    }

    impl FakeClient {
        fn new(block_if_text: &'static str, block_if_image: &'static str) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                block_if_text,
                block_if_image,
            })
        }
    }

    impl LlmClient for FakeClient {
        fn classify(
            &self,
            _api_key: &str,
            _prompt: &str,
            jobs: &[ClassifyJob],
        ) -> Result<Vec<crate::client::ClassifyOut>, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(jobs
                .iter()
                .map(|job| {
                    let block = match job.kind {
                        JobKind::Text => job
                            .text
                            .as_deref()
                            .unwrap_or("")
                            .contains(self.block_if_text),
                        JobKind::Image => job
                            .image
                            .as_deref()
                            .unwrap_or("")
                            .contains(self.block_if_image),
                    };
                    crate::client::ClassifyOut {
                        hash: job.hash.clone(),
                        action: if block { "block".into() } else { "allow".into() },
                        reason: if block { Some("policy".into()) } else { None },
                    }
                })
                .collect())
        }
    }

    fn stores(config: LlmBasedConfig) -> (LlmBasedStore, LlmCacheStore) {
        let store = LlmBasedStore::with_persistence(Arc::new(
            MemoryPersistence::<LlmBasedConfig>::with_value(config),
        ))
        .unwrap();
        let cache = LlmCacheStore::with_persistence(Arc::new(
            MemoryPersistence::<LlmCacheFile>::new(),
        ))
        .unwrap();
        (store, cache)
    }

    fn config() -> LlmBasedConfig {
        LlmBasedConfig {
            api_key: "sk-test".into(),
            judge_text: true,
            judge_image: true,
            prompt_enabled: true,
            prompt: "block violence".into(),
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
    fn blocks_matching_text_and_caches() {
        let client = FakeClient::new("violence", "nude");
        let (store, cache) = stores(config());
        let filter = LlmBasedFilter::with_client(store, cache, Arc::clone(&client) as Arc<dyn LlmClient>);
        let first = filter.evaluate(&[
            item("c1", "flowers in the garden"),
            item("c2", "graphic violence tonight"),
        ]);
        assert!(first.iter().all(|r| r.id != "c1"));
        let hit = first.iter().find(|r| r.id == "c2").unwrap();
        assert!(hit.is_block());
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);

        let second = filter.evaluate(&[item("c9", "graphic violence tonight")]);
        assert!(second[0].is_block());
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn empty_api_key_does_not_call_llm() {
        let mut cfg = config();
        cfg.api_key.clear();
        let client = FakeClient::new("violence", "nude");
        let (store, cache) = stores(cfg);
        let filter = LlmBasedFilter::with_client(store, cache, Arc::clone(&client) as Arc<dyn LlmClient>);
        let out = filter.evaluate(&[item("c1", "graphic violence tonight")]);
        assert!(out.is_empty());
        assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn text_toggle_off_skips_text() {
        let mut cfg = config();
        cfg.judge_text = false;
        let client = FakeClient::new("violence", "nude");
        let (store, cache) = stores(cfg);
        let filter = LlmBasedFilter::with_client(store, cache, Arc::clone(&client) as Arc<dyn LlmClient>);
        let out = filter.evaluate(&[item("c1", "graphic violence tonight")]);
        assert!(out.is_empty());
        assert_eq!(client.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn image_hash_is_reused() {
        let client = FakeClient::new("violence", "nude");
        let (store, cache) = stores(config());
        let filter = LlmBasedFilter::with_client(store, cache, Arc::clone(&client) as Arc<dyn LlmClient>);
        let img = EvaluateItem {
            id: "c1".into(),
            kind: "image".into(),
            text: String::new(),
            media: vec![EvaluateMedia {
                kind: "image".into(),
                alt: String::new(),
                src: "https://cdn.example/a.jpg".into(),
            }],
            image: Some("data:image/jpeg;base64,nudeimg".into()),
        };
        let first = filter.evaluate(&[img.clone()]);
        assert!(first[0].is_block());
        let mut again = img;
        again.id = "c2".into();
        let second = filter.evaluate(&[again]);
        assert!(second[0].is_block());
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn prompt_change_busts_cache() {
        let client = FakeClient::new("zzz", "nude");
        let (store, cache) = stores(config());
        let filter =
            LlmBasedFilter::with_client(store, cache, Arc::clone(&client) as Arc<dyn LlmClient>);
        let _ = filter.evaluate(&[item("c1", "harmless text")]);
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
        filter.store().set_prompt("a different policy").unwrap();
        let _ = filter.evaluate(&[item("c2", "harmless text")]);
        assert_eq!(client.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn same_text_two_items_one_call() {
        let client = FakeClient::new("violence", "nude");
        let (store, cache) = stores(config());
        let filter = LlmBasedFilter::with_client(store, cache, Arc::clone(&client) as Arc<dyn LlmClient>);
        let out = filter.evaluate(&[
            item("c1", "graphic violence tonight"),
            item("c2", "graphic violence tonight"),
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(client.calls.load(Ordering::SeqCst), 1);
    }
}

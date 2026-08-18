use crate::heartbeat::HeartbeatMonitor;
use crate::{BrowserStore, DEFAULT_GRACE, DEFAULT_HEARTBEAT_TTL};
use content_filter::{merge_evaluate, ContentFilter, EvaluateItem, EvaluateResult};
use enforcer::{BlockEvent, BlockRule, ProcessSnapshot};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Default)]
struct BrowserUiState {
    host_input: String,
    status: String,
}

pub struct BrowserRule {
    store: BrowserStore,
    heartbeat: HeartbeatMonitor,
    ui_state: Mutex<BrowserUiState>,
    filters: Mutex<Vec<Arc<dyn ContentFilter>>>,
}

impl BrowserRule {
    pub fn new(store: BrowserStore) -> Self {
        Self::with_timeouts(store, DEFAULT_GRACE, DEFAULT_HEARTBEAT_TTL)
    }

    pub fn with_timeouts(store: BrowserStore, grace: Duration, heartbeat_ttl: Duration) -> Self {
        Self {
            store,
            heartbeat: HeartbeatMonitor::new(grace, heartbeat_ttl),
            ui_state: Mutex::new(BrowserUiState::default()),
            filters: Mutex::new(Vec::new()),
        }
    }

    pub fn store(&self) -> &BrowserStore {
        &self.store
    }

    pub fn record_heartbeat(&self, payload: crate::HeartbeatPayload) {
        self.heartbeat.record(payload);
    }

    pub fn heartbeats_snapshot(&self) -> std::collections::HashMap<String, crate::Heartbeat> {
        self.heartbeat.snapshot()
    }

    pub fn register_content_filter(&self, filter: Arc<dyn ContentFilter>) {
        let id = filter.id().to_string();
        let mut filters = self.filters.lock();
        if let Some(pos) = filters.iter().position(|f| f.id() == id) {
            filters[pos] = filter;
        } else {
            filters.push(filter);
        }
    }

    pub fn evaluate_content(&self, items: &[EvaluateItem]) -> Vec<EvaluateResult> {
        let filters = self.filters.lock().clone();
        merge_evaluate(&filters, items)
    }

    #[cfg(test)]
    pub(crate) fn touch_first_seen(&self, browser_id: &str, at: Instant) {
        self.heartbeat.touch_first_seen(browser_id, at);
    }

    #[cfg(test)]
    pub(crate) fn age_heartbeat(&self, browser_id: &str, ago: Duration) {
        self.heartbeat.age_heartbeat(browser_id, ago);
    }
}

impl BlockRule for BrowserRule {
    fn id(&self) -> &str {
        "browser_rule"
    }

    fn name(&self) -> &str {
        "Browser guard"
    }

    fn description(&self) -> &str {
        "Kills guarded browsers unless the extension is sending a healthy heartbeat (incognito + blocking permission). Site blacklist is enforced by the extension."
    }

    fn evaluate(&self, processes: &[ProcessSnapshot]) -> Vec<BlockEvent> {
        let config = self.store.get();
        let now = Instant::now();

        let present_ids: HashSet<String> = processes
            .iter()
            .filter(|p| config.is_guarded(&p.name))
            .map(|p| crate::BrowserConfig::browser_id_from_process(&p.name))
            .collect();
        self.heartbeat.observe_running(&present_ids, now);

        processes
            .iter()
            .filter(|p| config.is_guarded(&p.name))
            .filter_map(|p| {
                let id = crate::BrowserConfig::browser_id_from_process(&p.name);
                self.heartbeat
                    .block_reason(&id, now)
                    .map(|reason| BlockEvent::new(p, reason))
            })
            .collect()
    }

    fn settings_ui(&self, ui: &mut egui::Ui, _processes: &[ProcessSnapshot]) {
        let config = self.store.get();
        let mut state = self.ui_state.lock();
        let heartbeats = self.heartbeat.snapshot();
        let now = Instant::now();
        let ttl = self.heartbeat.ttl();

        ui.label(BlockRule::description(self));
        ui.label(format!(
            "Extension heartbeat: POST http://127.0.0.1:{}/heartbeat",
            crate::DEFAULT_PORT
        ));
        ui.separator();

        ui.heading("Guarded browsers");
        ui.label("Process names this rule will terminate if the extension heartbeat is missing or unhealthy.");
        for name in &config.browsers {
            let id = crate::BrowserConfig::browser_id_from_process(name);
            let status = match heartbeats.get(&id) {
                Some(hb) if hb.is_healthy(now, ttl) => {
                    format!(
                        "ok  incognito={}  blocking={}  last={}",
                        hb.incognito_allowed,
                        hb.has_blocking_permission,
                        hb.received_at_utc.format("%H:%M:%S")
                    )
                }
                Some(hb) => format!(
                    "unhealthy: {}",
                    hb.fail_reason(now, ttl)
                        .unwrap_or("unhealthy heartbeat")
                ),
                None => "no heartbeat".to_string(),
            };
            ui.horizontal(|ui| {
                ui.label(name);
                ui.label(status);
            });
        }

        ui.separator();
        let filters = self.filters.lock().clone();
        if !filters.is_empty() {
            ui.heading("Content filters");
            ui.label("The extension sends extracted text and images to POST /evaluate. Any filter that returns block hides the element.");
            for filter in filters.iter() {
                ui.separator();
                filter.settings_ui(ui);
            }
        }

        ui.separator();
        ui.heading("Site blacklist");
        ui.label("The extension redirects these hosts to a block page.");
        ui.horizontal(|ui| {
            ui.text_edit_singleline(&mut state.host_input);
            if ui.button("Add").clicked() && !state.host_input.trim().is_empty() {
                let host = state.host_input.trim().to_string();
                match self.store.add_blacklist(&host) {
                    Ok(_) => {
                        state.status.clear();
                        state.host_input.clear();
                    }
                    Err(err) => state.status = err.to_string(),
                }
            }
        });

        egui::ScrollArea::vertical()
            .id_salt("browser_rule_bl")
            .max_height(220.0)
            .show(ui, |ui| {
                for host in &config.blacklist {
                    ui.horizontal(|ui| {
                        ui.label(host);
                        if ui.small_button("✕").clicked() {
                            let _ = self.store.remove_blacklist(host);
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

/// Shared handle so the app can register one object for enforcement and settings.
#[derive(Clone)]
pub struct BrowserRuleHandle {
    inner: Arc<BrowserRule>,
}

impl BrowserRuleHandle {
    pub fn new(store: BrowserStore) -> Self {
        Self {
            inner: Arc::new(BrowserRule::new(store)),
        }
    }

    pub fn with_timeouts(store: BrowserStore, grace: Duration, heartbeat_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(BrowserRule::with_timeouts(store, grace, heartbeat_ttl)),
        }
    }

    pub fn rule(&self) -> Arc<dyn BlockRule> {
        Arc::clone(&self.inner) as Arc<dyn BlockRule>
    }

    pub fn store(&self) -> &BrowserStore {
        self.inner.store()
    }

    pub fn register_content_filter(&self, filter: Arc<dyn ContentFilter>) {
        self.inner.register_content_filter(filter);
    }

    pub fn spawn_http(&self, port: u16) -> std::sync::mpsc::Sender<()> {
        crate::http::spawn_http(Arc::clone(&self.inner), port)
    }
}

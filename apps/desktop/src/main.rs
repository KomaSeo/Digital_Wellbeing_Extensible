mod fonts;

use browser_rule::{BrowserRuleHandle, BrowserStore};
use eframe::egui;
use enforcer::{BlockRule, Enforcer, ProcessSnapshot};
use list_based_rule::{ListBasedRuleHandle, ListStore};
use llm_based_filter::{LlmBasedFilterHandle, LlmBasedStore, LlmCacheStore};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tool_api::{spawn_tcp, ToolRouter, DEFAULT_PORT};
use word_based_filter::{WordBasedFilterHandle, WordBasedStore};

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = ListStore::default().expect("failed to open list store");
    let list_rule = ListBasedRuleHandle::new(store.clone());
    let browser_store = BrowserStore::default().expect("failed to open browser store");
    let browser_guard = BrowserRuleHandle::new(browser_store);
    let word_store = WordBasedStore::default().expect("failed to open word-based filter store");
    let word_filter = WordBasedFilterHandle::new(word_store);
    let llm_store = LlmBasedStore::default().expect("failed to open llm-based filter store");
    let llm_cache = LlmCacheStore::default().expect("failed to open llm-based filter cache");
    let llm_filter = LlmBasedFilterHandle::new(llm_store, llm_cache);
    browser_guard.register_content_filter(word_filter.filter());
    browser_guard.register_content_filter(llm_filter.filter());
    let enforcer = Arc::new(Enforcer::new());
    enforcer.register_rule(list_rule.rule());
    enforcer.register_rule(browser_guard.rule());

    let rules: Vec<Arc<dyn BlockRule>> = vec![list_rule.rule(), browser_guard.rule()];

    let _enforce_handle = enforcer.start_background(Duration::from_millis(800));
    let _heartbeat_keep_alive = browser_guard.spawn_http(browser_rule::DEFAULT_PORT);

    let router = ToolRouter::new(Arc::clone(&enforcer), store);
    let _api_keep_alive = spawn_tcp(router.clone(), DEFAULT_PORT);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 680.0])
            .with_title("Digital Wellbeing"),
        ..Default::default()
    };

    eframe::run_native(
        "Digital Wellbeing",
        options,
        Box::new(move |cc| {
            fonts::install_korean_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(router, rules)))
        }),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MainTab {
    /// Running processes + recent blocks overview.
    Monitor,
    /// A registered rule's settings panel.
    Rule(String),
}

struct App {
    router: ToolRouter,
    rules: Vec<Arc<dyn BlockRule>>,
    tab: MainTab,
    status: String,
    last_refresh: Instant,
    process_filter: String,
}

impl App {
    fn new(router: ToolRouter, rules: Vec<Arc<dyn BlockRule>>) -> Self {
        Self {
            router,
            rules,
            tab: MainTab::Monitor,
            status: format!(
                "Function call API: 127.0.0.1:{DEFAULT_PORT}  ·  Heartbeat: 127.0.0.1:{}",
                browser_rule::DEFAULT_PORT
            ),
            last_refresh: Instant::now() - Duration::from_secs(10),
            process_filter: String::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() > Duration::from_millis(700) {
            self.last_refresh = Instant::now();
            ctx.request_repaint_after(Duration::from_millis(700));
        }

        let blocks = self.router.dispatch("list_blocks", serde_json::json!({ "limit": 40 }));
        let process_infos = self.router.enforcer().list_processes();
        let snapshots: Vec<ProcessSnapshot> = process_infos
            .iter()
            .map(|p| ProcessSnapshot {
                pid: p.pid,
                name: p.name.clone(),
                exe: p.exe.clone(),
            })
            .collect();

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Digital Wellbeing");
                ui.separator();
                ui.label(&self.status);
            });
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.tab == MainTab::Monitor, "Monitor")
                    .clicked()
                {
                    self.tab = MainTab::Monitor;
                }
                ui.separator();
                ui.label("Rules:");
                for rule in &self.rules {
                    let selected = matches!(&self.tab, MainTab::Rule(id) if id == rule.id());
                    if ui.selectable_label(selected, rule.name()).clicked() {
                        self.tab = MainTab::Rule(rule.id().to_string());
                    }
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let tab = self.tab.clone();
            match tab {
                MainTab::Monitor => {
                    ui.columns(2, |cols| {
                        cols[0].heading("Running processes");
                        cols[0].text_edit_singleline(&mut self.process_filter);
                        egui::ScrollArea::vertical()
                            .id_salt("monitor_procs")
                            .max_width(f32::INFINITY)
                            .show(&mut cols[0], |ui| {
                                let filter = self.process_filter.to_lowercase();
                                for process in &snapshots {
                                    if !filter.is_empty()
                                        && !process.name.to_lowercase().contains(&filter)
                                    {
                                        continue;
                                    }
                                    ui.label(format!("{} ({})", process.name, process.pid));
                                }
                            });

                        cols[1].heading("Recent blocks");
                        egui::ScrollArea::vertical()
                            .id_salt("monitor_blocks")
                            .max_width(f32::INFINITY)
                            .show(&mut cols[1], |ui| {
                                if let Ok(value) = &blocks {
                                    if let Some(arr) = value.as_array() {
                                        for item in arr {
                                            let name = item
                                                .get("process_name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("?");
                                            let pid = item
                                                .get("pid")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);
                                            let at = item
                                                .get("at")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let reason = item
                                                .get("reason")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            ui.label(format!(
                                                "{at}  {name} ({pid})  [{reason}]"
                                            ));
                                        }
                                    }
                                }
                            });
                    });
                }
                MainTab::Rule(rule_id) => {
                    let selected = self.rules.iter().find(|r| r.id() == rule_id);
                    if let Some(rule) = selected {
                        egui::ScrollArea::vertical()
                            .id_salt("rule_settings_scroll")
                            .max_width(f32::INFINITY)
                            .show(ui, |ui| {
                                rule.settings_ui(ui, &snapshots);
                            });
                    } else {
                        ui.label("Rule not found.");
                    }
                }
            }
        });
    }
}

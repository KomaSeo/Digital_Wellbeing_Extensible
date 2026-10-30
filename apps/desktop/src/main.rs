use ai_focus_rule::{native_monitor, AiFocusRuleHandle, AiFocusStore};
use eframe::egui;
use enforcer::{Enforcer, ProcessSnapshot};
use list_based_rule::{ListBasedRuleHandle, ListStore};
use rule_ui::RuleUi;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tool_api::{spawn_tcp, ToolRouter, DEFAULT_PORT};

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let store = ListStore::default().expect("failed to open list store");
    let list_rule = ListBasedRuleHandle::new(store.clone());
    let enforcer = Arc::new(Enforcer::new());
    enforcer.register_rule(list_rule.rule());

    let ai_store = AiFocusStore::default().expect("failed to open AI focus settings");
    let ai_rule = AiFocusRuleHandle::new(native_monitor(ai_store));
    enforcer.register_rule(ai_rule.rule());
    let _ai_monitor_handle = ai_rule.monitor().start_background();
    // Future: enforcer.register_rule(browser_guard.rule());

    let rule_uis: Vec<Arc<dyn RuleUi>> = vec![list_rule.ui(), ai_rule.ui()];
    // Future: rule_uis.push(browser_guard.ui());

    let _enforce_handle = enforcer.start_background(Duration::from_millis(800));

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
        Box::new(move |_cc| Ok(Box::new(App::new(router, rule_uis)))),
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
    rule_uis: Vec<Arc<dyn RuleUi>>,
    tab: MainTab,
    status: String,
    last_refresh: Instant,
    process_filter: String,
}

impl App {
    fn new(router: ToolRouter, rule_uis: Vec<Arc<dyn RuleUi>>) -> Self {
        Self {
            router,
            rule_uis,
            tab: MainTab::Monitor,
            status: format!("Function call API: 127.0.0.1:{DEFAULT_PORT}"),
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
                for rule in &self.rule_uis {
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
                    let selected = self.rule_uis.iter().find(|r| r.id() == rule_id);
                    if let Some(rule) = selected {
                        egui::ScrollArea::vertical()
                            .id_salt("rule_settings_scroll")
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

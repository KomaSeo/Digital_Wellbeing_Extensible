use eframe::egui;
use enforcer::Enforcer;
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
    // Future: enforcer.register_rule(browser_guard.rule());

    let rule_uis: Vec<Arc<dyn RuleUi>> = vec![list_rule.ui()];
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

struct App {
    router: ToolRouter,
    rule_uis: Vec<Arc<dyn RuleUi>>,
    selected_rule_id: String,
    status: String,
    last_refresh: Instant,
    process_filter: String,
}

impl App {
    fn new(router: ToolRouter, rule_uis: Vec<Arc<dyn RuleUi>>) -> Self {
        let selected_rule_id = rule_uis
            .first()
            .map(|r| r.id().to_string())
            .unwrap_or_default();
        Self {
            router,
            rule_uis,
            selected_rule_id,
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
        let processes = self
            .router
            .dispatch("list_processes", serde_json::json!({}));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("Digital Wellbeing");
                ui.separator();
                ui.label(&self.status);
            });
            ui.horizontal(|ui| {
                ui.label("Rules:");
                for rule in &self.rule_uis {
                    let selected = self.selected_rule_id == rule.id();
                    if ui.selectable_label(selected, rule.name()).clicked() {
                        self.selected_rule_id = rule.id().to_string();
                    }
                }
            });
        });

        egui::SidePanel::left("rule_settings")
            .default_width(340.0)
            .show(ctx, |ui| {
                let selected = self
                    .rule_uis
                    .iter()
                    .find(|r| r.id() == self.selected_rule_id);
                if let Some(rule) = selected {
                    ui.heading(rule.name());
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .id_salt("rule_settings_scroll")
                        .show(ui, |ui| {
                            rule.settings_ui(ui);
                        });
                } else {
                    ui.label("No rule selected.");
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                cols[0].heading("Running processes");
                cols[0].text_edit_singleline(&mut self.process_filter);
                egui::ScrollArea::vertical()
                    .id_salt("procs")
                    .show(&mut cols[0], |ui| {
                        if let Ok(value) = &processes {
                            if let Some(arr) = value.as_array() {
                                let filter = self.process_filter.to_lowercase();
                                for item in arr {
                                    let name = item
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    if !filter.is_empty()
                                        && !name.to_lowercase().contains(&filter)
                                    {
                                        continue;
                                    }
                                    let pid = item.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                                    ui.horizontal(|ui| {
                                        ui.label(format!("{name} ({pid})"));
                                        for rule in &self.rule_uis {
                                            rule.process_row_ui(ui, name);
                                        }
                                    });
                                }
                            }
                        }
                    });

                cols[1].heading("Recent blocks");
                egui::ScrollArea::vertical()
                    .id_salt("blocks")
                    .show(&mut cols[1], |ui| {
                        if let Ok(value) = &blocks {
                            if let Some(arr) = value.as_array() {
                                for item in arr {
                                    let name = item
                                        .get("process_name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    let pid =
                                        item.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let at =
                                        item.get("at").and_then(|v| v.as_str()).unwrap_or("");
                                    let reason = item
                                        .get("reason")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");
                                    ui.label(format!("{at}  {name} ({pid})  [{reason}]"));
                                }
                            }
                        }
                    });
            });
        });
    }
}

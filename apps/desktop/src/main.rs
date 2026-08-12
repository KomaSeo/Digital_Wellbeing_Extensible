use eframe::egui;
use enforcer::Enforcer;
use list_based_rule::{ListBasedRuleHandle, ListMode, ListStore};
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
    // Future: enforcer.register_rule(browser_guard.rule()); // e.g. 60s grace on extension loss
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
        Box::new(move |_cc| Ok(Box::new(App::new(router)))),
    )
}

struct App {
    router: ToolRouter,
    name_input: String,
    strategy_input: String,
    status: String,
    last_refresh: Instant,
    process_filter: String,
}

impl App {
    fn new(router: ToolRouter) -> Self {
        Self {
            router,
            name_input: String::new(),
            strategy_input: String::new(),
            status: format!("Function call API: 127.0.0.1:{DEFAULT_PORT}"),
            last_refresh: Instant::now() - Duration::from_secs(10),
            process_filter: String::new(),
        }
    }

    fn call(&mut self, method: &str, params: serde_json::Value) {
        match self.router.dispatch(method, params) {
            Ok(_) => self.status = format!("OK: {method}"),
            Err(err) => self.status = format!("ERR: {err}"),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() > Duration::from_millis(700) {
            self.last_refresh = Instant::now();
            ctx.request_repaint_after(Duration::from_millis(700));
        }

        let policy = self.router.store().get();
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
                ui.label("Enforcement mode:");
                let mut mode = policy.mode;
                if ui
                    .selectable_value(&mut mode, ListMode::Blacklist, "Blacklist")
                    .clicked()
                {
                    self.call("set_mode", serde_json::json!({ "mode": "blacklist" }));
                }
                if ui
                    .selectable_value(&mut mode, ListMode::Whitelist, "Whitelist")
                    .clicked()
                {
                    self.call("set_mode", serde_json::json!({ "mode": "whitelist" }));
                }
            });
        });

        egui::SidePanel::left("lists")
            .default_width(320.0)
            .show(ctx, |ui| {
                ui.heading("Lists");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.name_input);
                });
                ui.horizontal(|ui| {
                    if ui.button("Add blacklist").clicked() && !self.name_input.trim().is_empty() {
                        self.call(
                            "add_blacklist",
                            serde_json::json!({ "name": self.name_input.trim() }),
                        );
                        self.name_input.clear();
                    }
                    if ui.button("Add whitelist").clicked() && !self.name_input.trim().is_empty() {
                        self.call(
                            "add_whitelist",
                            serde_json::json!({ "name": self.name_input.trim() }),
                        );
                        self.name_input.clear();
                    }
                });

                ui.separator();
                ui.label("Blacklist");
                egui::ScrollArea::vertical()
                    .id_salt("bl")
                    .max_height(200.0)
                    .show(ui, |ui| {
                        for name in &policy.blacklist {
                            ui.horizontal(|ui| {
                                ui.label(name);
                                if ui.small_button("✕").clicked() {
                                    self.call(
                                        "remove_blacklist",
                                        serde_json::json!({ "name": name }),
                                    );
                                }
                            });
                        }
                    });

                ui.separator();
                ui.label("Whitelist");
                egui::ScrollArea::vertical()
                    .id_salt("wl")
                    .max_height(220.0)
                    .show(ui, |ui| {
                        for name in &policy.whitelist {
                            ui.horizontal(|ui| {
                                ui.label(name);
                                if ui.small_button("✕").clicked() {
                                    self.call(
                                        "remove_whitelist",
                                        serde_json::json!({ "name": name }),
                                    );
                                }
                            });
                        }
                    });

                ui.separator();
                ui.heading("Strategies");
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.strategy_input);
                });
                ui.horizontal(|ui| {
                    if ui.button("Save current").clicked()
                        && !self.strategy_input.trim().is_empty()
                    {
                        self.call(
                            "save_strategy",
                            serde_json::json!({ "name": self.strategy_input.trim() }),
                        );
                    }
                    if ui.button("Apply").clicked() && !self.strategy_input.trim().is_empty() {
                        self.call(
                            "apply_strategy",
                            serde_json::json!({ "name": self.strategy_input.trim() }),
                        );
                    }
                });
                for s in &policy.strategies {
                    ui.horizontal(|ui| {
                        ui.label(&s.name);
                        if ui.small_button("load").clicked() {
                            self.strategy_input = s.name.clone();
                            self.call(
                                "apply_strategy",
                                serde_json::json!({ "name": s.name }),
                            );
                        }
                    });
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
                                        if ui.small_button("+BL").clicked() {
                                            self.call(
                                                "add_blacklist",
                                                serde_json::json!({ "name": name }),
                                            );
                                        }
                                        if ui.small_button("+WL").clicked() {
                                            self.call(
                                                "add_whitelist",
                                                serde_json::json!({ "name": name }),
                                            );
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

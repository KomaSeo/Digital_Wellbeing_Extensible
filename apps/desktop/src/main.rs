//! Responsibility: Start the desktop window. Standalone: enforce the rules in this process and
//! serve the local API. On a PC with the machine install it only points to the Focus app,
//! whose Settings tab edits the shared settings (while they are not locked).

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::{native_monitor, AiFocusRuleHandle, AiFocusStore};
use eframe::egui;
use enforcer::{Enforcer, ProcessSnapshot};
use list_based_rule::{ListBasedRuleHandle, ListStore};
use machine::session::{current_session_id, process_session_id};
use rule_ui::{Panel, RuleUi};
use std::sync::Arc;
use std::time::{Duration, Instant};
use task_planner::JsonStore;
use tool_api::{spawn_tcp, ToolRouter, DEFAULT_PORT};

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // A machine install (service + agents) exists: the agents enforce, and the
    // Focus app's Settings tab edits the settings while they are unlocked. This
    // window then only says so. `--standalone` forces single-account mode.
    // Decided by the registered service, not by a file (a file is left behind
    // by uninstall, and anyone may create one).
    if !std::env::args().any(|a| a == "--standalone") && service_installed() {
        return eframe::run_native(
            "Digital Wellbeing",
            eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default()
                    .with_inner_size([520.0, 200.0])
                    .with_title("Digital Wellbeing"),
                ..Default::default()
            },
            Box::new(|cc| {
                rule_ui::install_system_fonts(&cc.egui_ctx);
                Ok(Box::new(InstalledNotice))
            }),
        );
    }
    let (router, rule_uis, panels, status) = start_standalone();

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
            rule_ui::install_system_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(router, rule_uis, panels, status)))
        }),
    )
}

type Started = (
    ToolRouter,
    Vec<Arc<dyn RuleUi>>,
    Vec<Arc<dyn Panel>>,
    String,
);

/// Single account: enforce in this process and serve the function-call API.
/// (The task planner needs the machine install: here the user is the admin and
/// could edit the list anyway.)
fn start_standalone() -> Started {
    let store = ListStore::default().expect("failed to open list store");
    let list_rule = ListBasedRuleHandle::new(store.clone());
    let enforcer = Arc::new(Enforcer::new());
    enforcer.register_rule(list_rule.rule());

    let ai_store = AiFocusStore::default().expect("failed to open AI focus settings");
    let grants_path = ai_store.grants_path().expect("settings have a file");
    let grants =
        JsonStore::<GrantState>::open(grants_path, true).expect("failed to open grants.json");
    let usage_path = ai_store.usage_path().expect("settings have a file");
    let usage = JsonStore::open(usage_path, true).expect("failed to open usage.json");
    let usage = Arc::new(ai_focus_rule::usage::UsageRecorder::new(usage, None));
    let ai_rule = AiFocusRuleHandle::new(native_monitor(ai_store, grants, Some(usage)));
    enforcer.register_rule(ai_rule.rule());
    // Future: enforcer.register_rule(browser_guard.rule());

    // Only this user's session: other users' programs and Windows services
    // (session 0) are none of this window's business.
    let session = current_session_id();
    enforcer.set_process_filter(Arc::new(move |pid| {
        process_session_id(pid) == Some(session)
    }));

    let rule_uis: Vec<Arc<dyn RuleUi>> = vec![list_rule.ui(), ai_rule.ui()];
    // Future: rule_uis.push(browser_guard.ui());

    let router = ToolRouter::new(Arc::clone(&enforcer), store);
    // The API port doubles as a single-instance lock: a second window would
    // enforce the same policy twice.
    let status = match spawn_tcp(router.clone(), DEFAULT_PORT) {
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            return (
                router,
                rule_uis,
                Vec::new(),
                "Another Digital Wellbeing window is already running; this one does not enforce."
                    .into(),
            );
        }
        Err(err) => format!("Function call API unavailable: {err}"),
        Ok(()) => format!("Function call API: 127.0.0.1:{DEFAULT_PORT}"),
    };
    let _ai_monitor_handle = ai_rule.monitor().start_background();
    let _enforce_handle = enforcer.start_background(Duration::from_millis(800));
    (router, rule_uis, Vec::new(), status)
}

/// Responsibility: Tell the user where the settings are on a PC with the machine install.
struct InstalledNotice;

impl eframe::App for InstalledNotice {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Digital Wellbeing is installed for this PC");
            ui.label(
                "The service enforces the rules in the managed accounts. Open the Digital \
                 Wellbeing Focus app in a managed account (Start menu or the tray icon); its \
                 Settings tab edits the settings while they are not locked.",
            );
            ui.label("This window does nothing else; close it.");
        });
    }
}

/// Is the machine-wide service registered?
#[cfg(windows)]
fn service_installed() -> bool {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .and_then(|m| m.open_service("DigitalWellbeing", ServiceAccess::QUERY_STATUS))
        .is_ok()
}

#[cfg(not(windows))]
fn service_installed() -> bool {
    false
}

/// How often the monitor view re-reads processes and blocks.
const REFRESH: Duration = Duration::from_millis(700);

/// Responsibility: Remember which tab the window shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum MainTab {
    /// Running processes + recent blocks overview.
    Monitor,
    /// A registered rule's settings panel.
    Rule(String),
    /// A non-rule panel (task planner).
    Panel(String),
}

/// Responsibility: Draw the desktop window every frame: tab bar, monitor view, and the selected
/// rule's or panel's settings.
struct App {
    router: ToolRouter,
    rule_uis: Vec<Arc<dyn RuleUi>>,
    panels: Vec<Arc<dyn Panel>>,
    tab: MainTab,
    status: String,
    last_refresh: Instant,
    /// Cached between refreshes: listing processes on every repaint (every
    /// mouse move or key press) made the window slow.
    snapshots: Vec<ProcessSnapshot>,
    blocks: serde_json::Value,
    process_filter: String,
}

impl App {
    fn new(
        router: ToolRouter,
        rule_uis: Vec<Arc<dyn RuleUi>>,
        panels: Vec<Arc<dyn Panel>>,
        status: String,
    ) -> Self {
        Self {
            router,
            rule_uis,
            panels,
            tab: MainTab::Monitor,
            status,
            last_refresh: Instant::now() - Duration::from_secs(10),
            snapshots: Vec::new(),
            blocks: serde_json::Value::Null,
            process_filter: String::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_refresh.elapsed() >= REFRESH {
            self.last_refresh = Instant::now();
            self.blocks = self
                .router
                .dispatch("list_blocks", serde_json::json!({ "limit": 40 }))
                .unwrap_or_default();
            self.snapshots = self
                .router
                .enforcer()
                .list_processes()
                .into_iter()
                .map(|p| ProcessSnapshot {
                    pid: p.pid,
                    name: p.name,
                    exe: p.exe,
                })
                .collect();
        }
        ctx.request_repaint_after(REFRESH);
        let snapshots = &self.snapshots;
        let blocks = &self.blocks;

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
                if !self.panels.is_empty() {
                    ui.separator();
                }
                for panel in &self.panels {
                    let selected = matches!(&self.tab, MainTab::Panel(id) if id == panel.id());
                    if ui.selectable_label(selected, panel.name()).clicked() {
                        self.tab = MainTab::Panel(panel.id().to_string());
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
                        egui::ScrollArea::vertical().id_salt("monitor_procs").show(
                            &mut cols[0],
                            |ui| {
                                let filter = self.process_filter.to_lowercase();
                                for process in snapshots {
                                    if !filter.is_empty()
                                        && !process.name.to_lowercase().contains(&filter)
                                    {
                                        continue;
                                    }
                                    ui.label(format!("{} ({})", process.name, process.pid));
                                }
                            },
                        );

                        cols[1].heading("Recent blocks");
                        egui::ScrollArea::vertical().id_salt("monitor_blocks").show(
                            &mut cols[1],
                            |ui| {
                                if let Some(arr) = blocks.as_array() {
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
                            },
                        );
                    });
                }
                MainTab::Rule(rule_id) => {
                    let selected = self.rule_uis.iter().find(|r| r.id() == rule_id);
                    if let Some(rule) = selected {
                        egui::ScrollArea::vertical()
                            .id_salt("rule_settings_scroll")
                            .show(ui, |ui| {
                                rule.settings_ui(ui, snapshots);
                            });
                    } else {
                        ui.label("Rule not found.");
                    }
                }
                MainTab::Panel(panel_id) => {
                    if let Some(panel) = self.panels.iter().find(|p| p.id() == panel_id) {
                        egui::ScrollArea::vertical()
                            .id_salt("panel_scroll")
                            .show(ui, |ui| panel.ui(ui));
                    }
                }
            }
        });
    }
}

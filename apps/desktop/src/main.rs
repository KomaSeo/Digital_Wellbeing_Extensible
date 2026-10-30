//! Responsibility: Start the desktop window. Standalone: enforce the rules in this process and
//! serve the local API. Machine install: only edit the shared settings files.

use ai_focus_rule::grants::GrantState;
use ai_focus_rule::{native_monitor, AiFocusRuleHandle, AiFocusStore};
use eframe::egui;
use enforcer::{Enforcer, ProcessSnapshot};
use list_based_rule::{FileListPersistence, ListBasedRuleHandle, ListStore};
use machine::session::{current_session_id, process_session_id};
use rule_ui::{Panel, RuleUi};
use std::sync::Arc;
use std::time::{Duration, Instant};
use task_planner::{JsonStore, PlannerAdminUi};
use tool_api::{spawn_tcp, ToolRouter, DEFAULT_PORT};

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // A machine install (service + agents) exists: this window only edits the
    // shared settings; the agents enforce them. `--standalone` forces the old
    // single-account mode. Decided by the registered service, not by a file
    // (a file is left behind by uninstall, and anyone may create one).
    let machine_mode = !std::env::args().any(|a| a == "--standalone") && service_installed();
    let (router, rule_uis, panels, status) = if machine_mode {
        start_machine_admin()
    } else {
        start_standalone()
    };

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
            Ok(Box::new(App::new(
                router,
                rule_uis,
                panels,
                status,
                machine_mode,
            )))
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
    let ai_rule = AiFocusRuleHandle::new(native_monitor(ai_store, grants));
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

/// Machine install: edit `%ProgramData%\DigitalWellbeing\*.json` only. Nothing
/// is enforced here and no API is opened (it has no authentication, and a
/// standard user on the same PC could otherwise call it).
fn start_machine_admin() -> Started {
    let store =
        ListStore::with_persistence(Arc::new(FileListPersistence::new(machine::policy_path())))
            .expect("failed to open machine policy (run as administrator)");
    let list_rule = ListBasedRuleHandle::new(store.clone());

    let ai_store = AiFocusStore::open(machine::ai_focus_path())
        .expect("failed to open machine AI settings (run as administrator)");
    // The admin view reads every user's grants and blocks files; it revokes
    // and unblocks through the config.
    let monitor = native_monitor(ai_store, JsonStore::in_memory(GrantState::default()));
    let ai_rule =
        AiFocusRuleHandle::settings_only(monitor, machine::grants_dir(), machine::blocks_dir());

    let rule_uis: Vec<Arc<dyn RuleUi>> = vec![list_rule.ui(), ai_rule.ui()];

    let planner = PlannerAdminUi::new(
        JsonStore::open(machine::planner_path(), true).expect("failed to open planner.json"),
        JsonStore::open(machine::planner_state_path(), false)
            .expect("failed to open planner_state.json"),
    );
    let panels: Vec<Arc<dyn Panel>> = vec![Arc::new(planner)];

    let router = ToolRouter::new(Arc::new(Enforcer::new()), store);
    let status = format!(
        "Machine install: editing {} — enforced by the service in managed accounts",
        machine::data_dir().display()
    );
    (router, rule_uis, panels, status)
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
    machine_mode: bool,
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
        machine_mode: bool,
    ) -> Self {
        Self {
            router,
            rule_uis,
            panels,
            tab: MainTab::Monitor,
            status,
            machine_mode,
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
        let machine_mode = self.machine_mode;

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
                                if machine_mode {
                                    ui.label(
                                        "The service enforces and logs blocks in each managed \
                                         account (see logs in the data folder).",
                                    );
                                }
                                {
                                    if let Some(arr) = blocks.as_array() {
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
                                            ui.label(format!("{at}  {name} ({pid})  [{reason}]"));
                                        }
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

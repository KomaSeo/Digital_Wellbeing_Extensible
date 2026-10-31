//! Responsibility: Show the standard user their daily wellbeing (what to do now, the to-do
//! list, what is blocked), and send their requests (get a task, change it, change the list,
//! ask for access) to the agent of their session.
//!
//! Digital Wellbeing Focus — the app a *standard* (non-admin) user sees.
//!
//! It starts at logon (HKLM Run key written by the installer), runs with the
//! user's own rights, and talks to the agent of the user's session through
//! [`machine::user_api`]. Closing it changes nothing: the agent keeps enforcing.
//!
//! Three tabs: **To-do** (`todo.rs`: what to do now, the list, today), **Blocking** (what the
//! AI checks and blocks, asking for access) and **Settings** (`settings.rs`). The app runs all
//! day; the AI checks are called "daily wellbeing", never a "session".
//! - **planner active** (enabled and to-dos are open): the AI assigns the task; changing it
//!   needs a reason the AI approves.
//! - otherwise: the user types what they do and starts or pauses the AI checks freely.
//!
//! The app sits in the tray (`tray.rs`): closing the window hides it, and the tray icon
//! opens it again (or its Settings tab). Starting the app again shows the running one.

#![cfg_attr(windows, windows_subsystem = "windows")]

mod settings;
mod todo;
mod tray;

use chrono::{Local, Utc};
use eframe::egui;
use machine::session::current_session_id;
use machine::user_api::grants::{GrantEvent, GrantLogEntry};
use machine::user_api::{call, Status, Verdict};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use task_planner::{ChangeRecord, ProposalRecord, TodoChangeRecord};

const POLL: Duration = Duration::from_secs(2);
const TITLE: &str = "Digital Wellbeing Focus";
const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);

fn main() -> eframe::Result<()> {
    // One Focus app per session: a second start (Start menu, logon) shows the first.
    if tray::activate_existing(TITLE) {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 680.0])
            .with_title(TITLE),
        ..Default::default()
    };
    eframe::run_native(
        TITLE,
        options,
        Box::new(|creation_context| {
            rule_ui::install_system_fonts(&creation_context.egui_ctx);
            Ok(Box::new(FocusApp::new(creation_context.egui_ctx.clone())))
        }),
    )
}

/// Responsibility: Hold the latest agent status and request results, shared by the UI thread
/// and the threads that talk to the agent.
#[derive(Default)]
struct Shared {
    status: Option<Result<Status, String>>,
    /// Error of the last request the user sent.
    message: String,
    /// The AI's answer to the last change request.
    decision: Option<ChangeRecord>,
    /// The AI's answer to the last access request.
    access: Option<GrantLogEntry>,
    /// The AI's answer to the last proposed to-do.
    proposal: Option<ProposalRecord>,
    /// The AI's answer to the last to-do change.
    todo_change: Option<TodoChangeRecord>,
}

/// Responsibility: Keep what the user types into the access request form.
struct AccessForm {
    site: bool,
    target: String,
    /// `None` = permanent.
    minutes: Option<u32>,
    purpose: String,
}

impl Default for AccessForm {
    fn default() -> Self {
        Self {
            site: true,
            target: String::new(),
            minutes: Some(30),
            purpose: String::new(),
        }
    }
}

/// Responsibility: Remember which kind of change the user picked in the planner's change form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChangeChoice {
    Done,
    OtherTodo,
    Free,
    End,
    /// Keep the task past its planned end; only show a notice (the AI decides).
    NotifyWhenTimeIsUp,
    /// Back to the default: a new task is picked when the time is up.
    ReassignWhenTimeIsUp,
}

/// Responsibility: Name the Focus window's tabs: what to do (the planner's assignment, the
/// to-do list, today), what is blocked (blocks, access requests, the AI's checks), and the
/// settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Todo,
    Blocking,
    Settings,
}

/// Responsibility: Draw the Focus window, keep the form inputs, and send requests to the agent
/// without blocking the UI.
struct FocusApp {
    tab: Tab,
    /// Set by the tray thread: switch to the Settings tab on the next frame.
    open_settings: Arc<AtomicBool>,
    settings: settings::SettingsTab,
    session: u32,
    context: egui::Context,
    shared: Arc<Mutex<Shared>>,
    /// A request is in flight (the AI may take tens of seconds).
    pending: Arc<AtomicBool>,
    // free mode
    task_input: String,
    task_loaded: bool,
    // planner mode
    choice: ChangeChoice,
    other_todo: Option<String>,
    free_task: String,
    reason: String,
    /// Proof that a finished to-do is really done, sent with "I finished it".
    evidence: String,
    /// Take a screenshot as proof a few seconds after sending "I finished it".
    proof_screenshot: bool,
    /// When the last change request was sent, until its answer (or error) has come.
    change_sent_at: Option<chrono::DateTime<chrono::Utc>>,
    access: AccessForm,
    todo_ui: todo::TodoUiState,
    /// Settings questions due at the last status; more than before brings the window up.
    questions_due: usize,
}

impl FocusApp {
    fn new(context: egui::Context) -> Self {
        let session = current_session_id();
        let shared = Arc::new(Mutex::new(Shared::default()));
        // Poll off the UI thread so a missing agent never freezes the window.
        let poll = Arc::clone(&shared);
        let poll_context = context.clone();
        thread::spawn(move || loop {
            let result = call(session, "status", json!({}));
            poll.lock().status = Some(result);
            poll_context.request_repaint();
            thread::sleep(POLL);
        });
        let open_settings = Arc::new(AtomicBool::new(false));
        let (tray_context, tray_flag) = (context.clone(), Arc::clone(&open_settings));
        tray::start(move |command| {
            if command == tray::TrayCommand::Settings {
                tray_flag.store(true, Ordering::SeqCst);
            }
            // The window may be hidden, and a hidden window draws no frames that
            // could process a viewport command: show it directly, then sync egui.
            tray::show_window(TITLE);
            tray_context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            tray_context.request_repaint();
        });
        Self {
            tab: Tab::Todo,
            open_settings,
            settings: settings::SettingsTab::new(session),
            session,
            context,
            shared,
            pending: Arc::new(AtomicBool::new(false)),
            task_input: String::new(),
            task_loaded: false,
            choice: ChangeChoice::Done,
            other_todo: None,
            free_task: String::new(),
            reason: String::new(),
            evidence: String::new(),
            proof_screenshot: false,
            change_sent_at: None,
            access: AccessForm::default(),
            todo_ui: todo::TodoUiState::default(),
            questions_due: 0,
        }
    }

    /// Send a request on a background thread; the reply replaces the status.
    fn send(&self, method: &'static str, params: Value) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let (session, shared, pending, context) = (
            self.session,
            Arc::clone(&self.shared),
            Arc::clone(&self.pending),
            self.context.clone(),
        );
        thread::spawn(move || {
            let result = call(session, method, params);
            {
                let mut shared_state = shared.lock();
                match result {
                    Ok(status) => {
                        shared_state.message.clear();
                        if status.last_decision.is_some() {
                            shared_state.decision = status.last_decision.clone();
                        }
                        if status.last_access.is_some() {
                            shared_state.access = status.last_access.clone();
                        }
                        if status.last_proposal.is_some() {
                            shared_state.proposal = status.last_proposal.clone();
                        }
                        if status.last_todo_change.is_some() {
                            shared_state.todo_change = status.last_todo_change.clone();
                        }
                        shared_state.status = Some(Ok(status));
                    }
                    Err(error) => shared_state.message = error,
                }
            }
            pending.store(false, Ordering::SeqCst);
            context.request_repaint();
        });
    }

    /// Fill the access form from a block-list key: `steam.exe`, `chrome.exe|naver`,
    /// an older `chrome.exe|Funny cats - YouTube` (site name = text after the last
    /// " - "), or a YouTube channel/page key `chrome.exe|youtube channel: cat tv`.
    /// Tab titles never name the channel, so a YouTube block is asked for as
    /// "youtube"; the purpose the user gives then limits what the grant covers.
    fn prefill_access(&mut self, key: &str) {
        match key.split_once('|') {
            Some((_, title)) => {
                self.access.site = true;
                // Page names may contain " - " themselves, so look for the
                // YouTube marker before splitting off the site.
                let site = match title
                    .split_once(" channel: ")
                    .or(title.split_once(" page: "))
                {
                    Some((site, _)) => site,
                    None => title.rsplit(" - ").next().unwrap_or(title),
                };
                self.access.target = site.trim().to_string();
            }
            None => {
                self.access.site = false;
                self.access.target = key.to_string();
            }
        }
    }

    fn access_ui(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("Need an app or website?");
        if !status.session_active {
            ui.label("The AI checks are paused; nothing needs asking for.");
            return;
        }
        ui.label(
            "Ask for access and say what it is for. The AI decides. While access is granted, \
             the AI keeps checking that you use it for that purpose; other use ends the access.",
        );
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.access.site, true, "Website");
            ui.radio_value(&mut self.access.site, false, "App");
            let hint = if self.access.site {
                "word in the tab title, e.g. YouTube"
            } else {
                "program, e.g. spotify.exe"
            };
            ui.add(egui::TextEdit::singleline(&mut self.access.target).hint_text(hint));
        });
        ui.horizontal(|ui| {
            ui.label("for");
            for minutes in [15, 30, 60, 120] {
                ui.radio_value(
                    &mut self.access.minutes,
                    Some(minutes),
                    format!("{minutes} min"),
                );
            }
            ui.radio_value(&mut self.access.minutes, None, "permanently");
        });
        ui.add(
            egui::TextEdit::multiline(&mut self.access.purpose)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("what you will use it for, e.g. Rust async 강의 영상 시청"),
        );
        let idle = !self.pending.load(Ordering::SeqCst);
        let ready = idle
            && !self.access.target.trim().is_empty()
            && self.access.purpose.trim().chars().count() >= 4;
        ui.horizontal(|ui| {
            if ui.add_enabled(ready, egui::Button::new("Ask")).clicked() {
                let target = if self.access.site {
                    json!({ "kind": "site", "keyword": self.access.target.trim() })
                } else {
                    json!({ "kind": "app", "process": self.access.target.trim() })
                };
                let duration = match self.access.minutes {
                    Some(minutes) => json!({ "kind": "minutes", "minutes": minutes }),
                    None => json!({ "kind": "permanent" }),
                };
                self.send(
                    "request_access",
                    json!({ "target": target, "duration": duration, "purpose": self.access.purpose }),
                );
                self.access.purpose.clear();
            }
            if !idle {
                ui.spinner();
            }
        });
        if let Some(entry) = &self.shared.lock().access {
            let approved = matches!(entry.event, GrantEvent::Requested { approved: true, .. });
            let (color, label) = if approved {
                (GREEN, "granted")
            } else {
                (RED, "refused")
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, label);
                ui.label(&entry.message);
            });
        }

        if !status.grants.is_empty() {
            ui.label("Access you have now (still observed):");
        }
        for grant in &status.grants {
            let until = grant
                .until
                .map(|expiry| {
                    expiry
                        .with_timezone(&Local)
                        .format("until %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "permanent".into());
            ui.label(format!(
                "• {} ({until}) — {}",
                grant.target.label(),
                grant.purpose
            ));
        }
        if let Some(revoked) = status
            .grant_log
            .iter()
            .find(|entry| entry.event == GrantEvent::RevokedForMisuse)
            .filter(|entry| Utc::now() - entry.at < chrono::Duration::minutes(30))
        {
            ui.colored_label(
                RED,
                format!(
                    "Access to {} was revoked at {}: {}",
                    revoked.target.label(),
                    revoked.at.with_timezone(&Local).format("%H:%M"),
                    revoked.message
                ),
            );
        }
    }
}

impl FocusApp {
    /// The Blocking tab: what the AI checks, what is blocked, and asking for access.
    fn blocking_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("AI checks");
        if status.session_active {
            match status.last_check {
                Some(at) => ui.label(format!(
                    "Last AI check {} (new apps and sites are checked at once)",
                    at.with_timezone(&Local).format("%H:%M:%S"),
                )),
                None => ui.label("First AI check in a moment…"),
            };
        } else {
            ui.label("The AI checks are paused (the blacklist still applies).");
        }
        if let Some(error) = &status.last_error {
            ui.colored_label(RED, format!("AI check failed: {error}"));
        }
        if let Some(cost) = status.ai_cost_today_usd {
            ui.small(if status.daily_budget_usd > 0.0 {
                format!(
                    "AI cost today (estimate): ${cost:.2} of ${:.2}",
                    status.daily_budget_usd
                )
            } else {
                format!("AI cost today (estimate): ${cost:.2}")
            });
        }

        ui.separator();
        ui.heading("Blocked now");
        if status.blocked.is_empty() {
            ui.label("Nothing.");
        }
        for blocked_entry in &status.blocked {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{} — {}", blocked_entry.key, blocked_entry.reason));
                if ui.small_button("Ask for access").clicked() {
                    self.prefill_access(&blocked_entry.key);
                }
            });
        }

        ui.separator();
        self.access_ui(ui, status);

        ui.separator();
        ui.heading("Recent AI checks");
        for verdict_record in &status.verdicts {
            let color = match verdict_record.verdict {
                Verdict::OnTask => GREEN,
                Verdict::OffTask => RED,
                Verdict::Unsure => egui::Color32::GRAY,
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    verdict_record
                        .at
                        .with_timezone(&Local)
                        .format("%H:%M")
                        .to_string(),
                );
                ui.colored_label(color, format!("{:?}", verdict_record.verdict));
                ui.label(format!(
                    "{} — {}",
                    verdict_record.process_name, verdict_record.reason
                ));
                if !verdict_record.action.is_empty() {
                    ui.strong(format!("[{}]", verdict_record.action));
                }
            });
        }
    }
}

impl eframe::App for FocusApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        let (status, message) = {
            let shared_state = self.shared.lock();
            (shared_state.status.clone(), shared_state.message.clone())
        };
        if tray::AVAILABLE && context.input(|input| input.viewport().close_requested()) {
            // Keep running in the tray; "Quit" in the tray menu ends the app.
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if self.open_settings.swap(false, Ordering::SeqCst) {
            self.tab = Tab::Settings;
        }
        // A new "is this update really right?" question: show the window (also from the
        // tray) on the Settings tab, once.
        if let Some(Ok(current_status)) = &status {
            if current_status.settings_questions_due > self.questions_due {
                context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                context.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.tab = Tab::Settings;
            }
            self.questions_due = current_status.settings_questions_due;
        }
        egui::TopBottomPanel::top("tabs").show(context, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Todo, "To-do");
                ui.selectable_value(&mut self.tab, Tab::Blocking, "Blocking");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Settings");
            });
        });
        egui::CentralPanel::default().show(context, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let status = match status {
                    Some(Ok(status)) => status,
                    Some(Err(error)) => {
                        ui.colored_label(RED, error);
                        ui.label("Blocking rules are enforced by the service either way.");
                        return;
                    }
                    None => {
                        ui.label("connecting…");
                        return;
                    }
                };
                // A refused or failed request from either tab.
                if !message.is_empty() && self.tab != Tab::Settings {
                    ui.colored_label(RED, &message);
                }
                match self.tab {
                    Tab::Todo => self.todo_tab(ui, &status),
                    Tab::Blocking => self.blocking_tab(ui, &status),
                    Tab::Settings => self.settings.ui(ui, status.settings_protected),
                }
            });
        });
    }
}

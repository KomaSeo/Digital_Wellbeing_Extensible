//! Responsibility: Show the standard user their focus session, and send their requests (start,
//! get a task, change it, ask for access) to the agent of their session.
//!
//! Digital Wellbeing Focus — the app a *standard* (non-admin) user sees.
//!
//! It starts at logon (HKLM Run key written by the installer), runs with the
//! user's own rights, and talks to the agent of the user's session through
//! [`machine::user_api`]. Closing it changes nothing: the agent keeps enforcing.
//!
//! Two modes, decided by the agent:
//! - **planner active** (admin enabled it and to-dos are open): the AI assigns
//!   the task; changing or ending it needs a reason the AI approves.
//! - otherwise: the user types their task and starts / ends the session freely.

#![cfg_attr(windows, windows_subsystem = "windows")]

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
use task_planner::{ChangeKind, ChangeRecord, PlannerStatus};

const POLL: Duration = Duration::from_secs(2);
const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([560.0, 680.0])
            .with_title("Digital Wellbeing Focus"),
        ..Default::default()
    };
    eframe::run_native(
        "Digital Wellbeing Focus",
        options,
        Box::new(|cc| {
            rule_ui::install_system_fonts(&cc.egui_ctx);
            Ok(Box::new(FocusApp::new(cc.egui_ctx.clone())))
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
}

/// Responsibility: Draw the Focus window, keep the form inputs, and send requests to the agent
/// without blocking the UI.
struct FocusApp {
    session: u32,
    ctx: egui::Context,
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
    access: AccessForm,
}

impl FocusApp {
    fn new(ctx: egui::Context) -> Self {
        let session = current_session_id();
        let shared = Arc::new(Mutex::new(Shared::default()));
        // Poll off the UI thread so a missing agent never freezes the window.
        let poll = Arc::clone(&shared);
        let poll_ctx = ctx.clone();
        thread::spawn(move || loop {
            let result = call(session, "status", json!({}));
            poll.lock().status = Some(result);
            poll_ctx.request_repaint();
            thread::sleep(POLL);
        });
        Self {
            session,
            ctx,
            shared,
            pending: Arc::new(AtomicBool::new(false)),
            task_input: String::new(),
            task_loaded: false,
            choice: ChangeChoice::Done,
            other_todo: None,
            free_task: String::new(),
            reason: String::new(),
            access: AccessForm::default(),
        }
    }

    /// Send a request on a background thread; the reply replaces the status.
    fn send(&self, method: &'static str, params: Value) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let (session, shared, pending, ctx) = (
            self.session,
            Arc::clone(&self.shared),
            Arc::clone(&self.pending),
            self.ctx.clone(),
        );
        thread::spawn(move || {
            let result = call(session, method, params);
            {
                let mut s = shared.lock();
                match result {
                    Ok(status) => {
                        s.message.clear();
                        if status.last_decision.is_some() {
                            s.decision = status.last_decision.clone();
                        }
                        if status.last_access.is_some() {
                            s.access = status.last_access.clone();
                        }
                        s.status = Some(Ok(status));
                    }
                    Err(err) => s.message = err,
                }
            }
            pending.store(false, Ordering::SeqCst);
            ctx.request_repaint();
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
            ui.label("Start a focus session first.");
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
            for m in [15, 30, 60, 120] {
                ui.radio_value(&mut self.access.minutes, Some(m), format!("{m} min"));
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
        for g in &status.grants {
            let until = g
                .until
                .map(|u| u.with_timezone(&Local).format("until %H:%M").to_string())
                .unwrap_or_else(|| "permanent".into());
            ui.label(format!("• {} ({until}) — {}", g.target.label(), g.purpose));
        }
        if let Some(revoked) = status
            .grant_log
            .iter()
            .find(|e| e.event == GrantEvent::RevokedForMisuse)
            .filter(|e| Utc::now() - e.at < chrono::Duration::minutes(30))
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

    fn free_mode_ui(&mut self, ui: &mut egui::Ui, status: &Status) {
        if !self.task_loaded {
            self.task_input = status.task.clone();
            self.task_loaded = true;
        }
        ui.label("What are you working on? Be specific: the AI judges every app against this.");
        ui.add(
            egui::TextEdit::multiline(&mut self.task_input)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. 졸업논문 3장 작성 — Overleaf, 논문 PDF, Google Scholar 필요"),
        );
        let has_task = !self.task_input.trim().is_empty();
        let idle = !self.pending.load(Ordering::SeqCst);
        ui.horizontal(|ui| {
            if status.session_active {
                ui.colored_label(GREEN, "● running");
                if ui
                    .add_enabled(has_task && idle, egui::Button::new("Change task"))
                    .clicked()
                {
                    self.send("start_session", json!({ "task": self.task_input }));
                }
                if ui
                    .add_enabled(idle, egui::Button::new("End session"))
                    .clicked()
                {
                    self.send("stop_session", json!({}));
                }
            } else {
                ui.label("○ not running");
                if ui
                    .add_enabled(has_task && idle, egui::Button::new("Start focus"))
                    .clicked()
                {
                    self.send("start_session", json!({ "task": self.task_input }));
                }
            }
        });
    }

    fn planner_ui(&mut self, ui: &mut egui::Ui, status: &Status, planner: &PlannerStatus) {
        let idle = !self.pending.load(Ordering::SeqCst);
        let assignment = planner
            .assignment
            .as_ref()
            .filter(|_| status.session_active);

        let Some(assignment) = assignment else {
            ui.label("Your task is picked by the AI from your to-do list.");
            if ui
                .add_enabled(idle, egui::Button::new("Get my task"))
                .clicked()
            {
                self.send("assign", json!({}));
            }
            open_todos_ui(ui, planner);
            return;
        };

        ui.colored_label(GREEN, "● running");
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong(&assignment.task);
            ui.small(format!(
                "{} — since {}",
                assignment.why,
                assignment.at.with_timezone(&Local).format("%H:%M")
            ));
        });

        // --- change request --------------------------------------------------
        ui.separator();
        ui.heading("Ask to change");
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut self.choice, ChangeChoice::Done, "I finished it");
            ui.radio_value(&mut self.choice, ChangeChoice::OtherTodo, "Another to-do");
            ui.radio_value(&mut self.choice, ChangeChoice::Free, "Something else");
            ui.radio_value(&mut self.choice, ChangeChoice::End, "End session");
        });
        let others: Vec<_> = planner
            .open_todos
            .iter()
            .filter(|t| assignment.todo_id.as_deref() != Some(t.id.as_str()))
            .collect();
        match self.choice {
            ChangeChoice::OtherTodo => {
                let selected = others
                    .iter()
                    .find(|t| Some(&t.id) == self.other_todo.as_ref())
                    .map(|t| t.title.clone())
                    .unwrap_or_else(|| "choose…".into());
                egui::ComboBox::from_id_salt("other_todo")
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for t in &others {
                            ui.selectable_value(&mut self.other_todo, Some(t.id.clone()), &t.title);
                        }
                    });
            }
            ChangeChoice::Free => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.free_task)
                        .desired_width(f32::INFINITY)
                        .hint_text("what you want to do instead"),
                );
            }
            ChangeChoice::Done | ChangeChoice::End => {}
        }
        ui.add(
            egui::TextEdit::multiline(&mut self.reason)
                .desired_rows(2)
                .desired_width(f32::INFINITY)
                .hint_text("reason — the AI decides whether it is good enough"),
        );
        let request = match self.choice {
            ChangeChoice::Done => Some(ChangeKind::Done),
            ChangeChoice::End => Some(ChangeKind::End),
            ChangeChoice::OtherTodo => self
                .other_todo
                .clone()
                .filter(|id| others.iter().any(|t| &t.id == id))
                .map(|todo_id| ChangeKind::SwitchTodo { todo_id }),
            ChangeChoice::Free => Some(self.free_task.trim().to_string())
                .filter(|t| !t.is_empty())
                .map(|task| ChangeKind::SwitchFree { task }),
        };
        let ready = idle && request.is_some() && self.reason.trim().chars().count() >= 4;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Send request"))
                .clicked()
            {
                if let Some(request) = request {
                    self.send(
                        "request_change",
                        json!({ "request": request, "reason": self.reason }),
                    );
                    self.reason.clear();
                }
            }
            if !idle {
                ui.spinner();
                ui.label("the AI is deciding…");
            }
        });
        if let Some(d) = &self.shared.lock().decision {
            let (color, label) = if d.approved {
                (GREEN, "approved")
            } else {
                (RED, "rejected")
            };
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(color, label);
                ui.label(&d.reply);
            });
        }

        open_todos_ui(ui, planner);
    }
}

fn open_todos_ui(ui: &mut egui::Ui, planner: &PlannerStatus) {
    ui.separator();
    ui.collapsing(
        format!("Open to-dos ({})", planner.open_todos.len()),
        |ui| {
            for t in &planner.open_todos {
                let deadline = t
                    .deadline
                    .map(|d| format!(" · due {d}"))
                    .unwrap_or_default();
                ui.label(format!("P{} {}{deadline}", t.priority, t.title));
            }
        },
    );
}

impl eframe::App for FocusApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (status, message) = {
            let s = self.shared.lock();
            (s.status.clone(), s.message.clone())
        };
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading("Focus session");
                let status = match status {
                    Some(Ok(status)) => status,
                    Some(Err(err)) => {
                        ui.colored_label(RED, err);
                        ui.label("Blocking rules are enforced by the service either way.");
                        return;
                    }
                    None => {
                        ui.label("connecting…");
                        return;
                    }
                };

                match status.planner.clone().filter(|p| p.active) {
                    Some(planner) => self.planner_ui(ui, &status, &planner),
                    None => self.free_mode_ui(ui, &status),
                }
                if !message.is_empty() {
                    ui.colored_label(RED, &message);
                }

                if status.session_active {
                    match status.last_check {
                        Some(at) => ui.label(format!(
                            "Last AI check {} (every {} s)",
                            at.with_timezone(&Local).format("%H:%M:%S"),
                            status.check_interval_secs
                        )),
                        None => ui.label("First AI check in a moment…"),
                    };
                }
                if let Some(err) = &status.last_error {
                    ui.colored_label(RED, format!("AI check failed: {err}"));
                }

                ui.separator();
                ui.heading("Blocked now");
                if status.blocked.is_empty() {
                    ui.label("Nothing.");
                }
                for b in &status.blocked {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{} — {}", b.key, b.reason));
                        if ui.small_button("Ask for access").clicked() {
                            self.prefill_access(&b.key);
                        }
                    });
                }

                ui.separator();
                self.access_ui(ui, &status);

                ui.separator();
                ui.heading("Recent AI checks");
                for v in &status.verdicts {
                    let color = match v.verdict {
                        Verdict::OnTask => GREEN,
                        Verdict::OffTask => RED,
                        Verdict::Unsure => egui::Color32::GRAY,
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(v.at.with_timezone(&Local).format("%H:%M").to_string());
                        ui.colored_label(color, format!("{:?}", v.verdict));
                        ui.label(format!("{} — {}", v.process_name, v.reason));
                        if !v.action.is_empty() {
                            ui.strong(format!("[{}]", v.action));
                        }
                    });
                }
            });
        });
    }
}

//! Digital Wellbeing Focus — the app a *standard* (non-admin) user sees.
//!
//! It starts at logon (HKLM Run key written by the installer), runs with the
//! user's own rights, and talks to the agent of the user's session through
//! [`machine::user_api`]. All it can do is declare / change / end the user's
//! focus session and show what the AI decided. Closing it changes nothing:
//! the agent keeps enforcing.

#![cfg_attr(windows, windows_subsystem = "windows")]

use chrono::Local;
use eframe::egui;
use machine::session::current_session_id;
use machine::user_api::{call, Status, Verdict};
use parking_lot::Mutex;
use serde_json::json;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_secs(2);

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 560.0])
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

type Shared = Arc<Mutex<Result<Status, String>>>;

struct FocusApp {
    session: u32,
    status: Shared,
    task_input: String,
    /// Copy the agent's task into the input box once, then leave it to the user.
    task_loaded: bool,
    message: String,
}

impl FocusApp {
    fn new(ctx: egui::Context) -> Self {
        let session = current_session_id();
        let status: Shared = Arc::new(Mutex::new(Err("connecting…".into())));
        // Poll off the UI thread so a missing agent never freezes the window.
        let poll = Arc::clone(&status);
        thread::spawn(move || loop {
            *poll.lock() = call(session, "status", json!({}));
            ctx.request_repaint();
            thread::sleep(POLL);
        });
        Self {
            session,
            status,
            task_input: String::new(),
            task_loaded: false,
            message: String::new(),
        }
    }

    fn send(&mut self, method: &str, params: serde_json::Value) {
        let result = call(self.session, method, params);
        self.message = match &result {
            Ok(_) => String::new(),
            Err(err) => err.clone(),
        };
        *self.status.lock() = result;
    }
}

impl eframe::App for FocusApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let status = self.status.lock().clone();
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Focus session");
            let status = match status {
                Ok(status) => status,
                Err(err) => {
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), err);
                    ui.label("Blocking rules are enforced by the service either way.");
                    return;
                }
            };
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
            ui.horizontal(|ui| {
                if status.session_active {
                    ui.colored_label(egui::Color32::from_rgb(80, 170, 90), "● running");
                    if ui
                        .add_enabled(has_task, egui::Button::new("Change task"))
                        .clicked()
                    {
                        let task = self.task_input.clone();
                        self.send("start_session", json!({ "task": task }));
                    }
                    if ui.button("End session").clicked() {
                        self.send("stop_session", json!({}));
                    }
                } else {
                    ui.label("○ not running");
                    if ui
                        .add_enabled(has_task, egui::Button::new("Start focus"))
                        .clicked()
                    {
                        let task = self.task_input.clone();
                        self.send("start_session", json!({ "task": task }));
                    }
                }
            });
            if !self.message.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(200, 80, 80), &self.message);
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
                ui.colored_label(egui::Color32::from_rgb(200, 80, 80), format!("AI check failed: {err}"));
            }

            ui.separator();
            ui.heading("Blocked now");
            if status.blocked.is_empty() {
                ui.label("Nothing.");
            }
            for b in &status.blocked {
                let until = b
                    .until
                    .map(|u| u.with_timezone(&Local).format("until %H:%M").to_string())
                    .unwrap_or_else(|| "until the session ends".into());
                ui.label(format!("{} ({until}) — {}", b.key, b.reason));
            }

            ui.separator();
            ui.heading("Recent AI checks");
            egui::ScrollArea::vertical().show(ui, |ui| {
                for v in &status.verdicts {
                    let color = match v.verdict {
                        Verdict::OnTask => egui::Color32::from_rgb(80, 170, 90),
                        Verdict::OffTask => egui::Color32::from_rgb(200, 80, 80),
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

//! Responsibility: Draw the To-do tab: what to do now (progress, notes and steps, asking for a
//! change), the to-do list (details, adding, editing and removing through the AI), today's
//! plan and summary, finished to-dos, and the log of requests.
//!
//! The app runs all day; the user-facing name of the AI checks is "daily wellbeing".
//! Every change of the list goes to the planner's AI (`todo_change`); when it says no or
//! cannot answer, the user may schedule the change anyway (`todo_change_schedule`), which
//! takes effect after three confirmations two hours apart (Settings tab). Notes and steps
//! change nothing the rules depend on and are saved at once (`task_notes_put`).

use crate::{ChangeChoice, FocusApp, Tab, GREEN, RED};
use chrono::{DateTime, Local, NaiveDate, Utc};
use eframe::egui;
use machine::user_api::{call, Status};
use serde_json::json;
use std::sync::atomic::Ordering;
use std::thread;
use task_planner::{
    change_summary, ChangeKind, PlannerStatus, TaskNotes, TaskStep, TodoChangeRecord, TodoItem,
    WhenTimeIsUp,
};

const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
/// Seconds between sending "I finished it" and the proof screenshot, to switch windows.
const PROOF_SCREENSHOT_DELAY_SECONDS: u64 = 5;

/// Responsibility: Keep what the user types into the To-do tab's forms.
#[derive(Default)]
pub struct TodoUiState {
    /// The to-do whose details are open in the list.
    expanded: Option<String>,
    /// Add (when `editing` is `None`) or edit form.
    form: TodoForm,
    form_open: bool,
    /// The to-do the form edits; `None` = adding.
    editing: Option<String>,
    /// The to-do the user is asking to remove, and why.
    removing: Option<String>,
    remove_reason: String,
    /// Notes and steps being edited, for this to-do.
    notes_for: Option<String>,
    notes: TaskNotes,
    new_step: String,
}

/// Responsibility: Hold the fields of a to-do as the user types them; empty ones are filled
/// in by the AI (adding) or stay as they are (editing).
#[derive(Default, Clone)]
struct TodoForm {
    title: String,
    notes: String,
    deadline: String,
    /// 0 = let the AI decide (adding) / keep (editing).
    priority: u8,
    /// 0 = let the AI decide (adding) / keep (editing).
    estimate_minutes: u32,
    reason: String,
}

impl TodoForm {
    /// The deadline as written ("이틀 뒤", "금요일", "11/5", "2026-11-05"), read as a date.
    fn deadline_date(&self) -> Option<NaiveDate> {
        task_planner::parse_deadline(&self.deadline, Local::now().date_naive())
    }

    fn draft(&self) -> serde_json::Value {
        json!({
            "title": self.title.trim(),
            "notes": self.notes.trim(),
            "deadline": self.deadline_date(),
            "priority": (self.priority > 0).then_some(self.priority),
            "estimate_minutes": (self.estimate_minutes > 0).then_some(self.estimate_minutes),
        })
    }
}

impl FocusApp {
    /// The To-do tab.
    pub(crate) fn todo_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        if status.settings_questions_due > 0 {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    RED,
                    "A scheduled update is waiting for your answer: is it really right?",
                );
                if ui.link("Answer in the Settings tab").clicked() {
                    self.tab = Tab::Settings;
                }
            });
            ui.separator();
        }
        if !status.settings_protected {
            // Setup: anyone can change anything at once until the settings are protected.
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    YELLOW,
                    "Setup: the settings are not protected (locked) yet, so any change takes \
                     effect at once.",
                );
                if ui.link("Protect them in the Settings tab").clicked() {
                    self.tab = Tab::Settings;
                }
            });
            ui.separator();
        }
        ui.heading("Daily wellbeing");
        let planner = status.planner.clone();
        match planner.clone().filter(|planner| planner.active) {
            Some(planner) => self.now_ui(ui, status, &planner),
            None => self.free_mode_ui(ui, status),
        }
        if !status.blocked.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("{} blocked now.", status.blocked.len()));
                if ui.link("See the Blocking tab").clicked() {
                    self.tab = Tab::Blocking;
                }
            });
        }
        let Some(planner) = planner else {
            return;
        };
        ui.separator();
        self.list_ui(ui, &planner);
        ui.separator();
        self.today_ui(ui, status, &planner);
        done_ui(ui, &planner);
        log_ui(ui, &planner);
    }

    /// Without the planner the user types what they do; the AI judges every app against it.
    fn free_mode_ui(&mut self, ui: &mut egui::Ui, status: &Status) {
        if !self.task_loaded {
            self.task_input = status.task.clone();
            self.task_loaded = true;
        }
        ui.label("What are you doing now? Be specific: the AI judges every app against this.");
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
                ui.colored_label(GREEN, "on");
                if ui
                    .add_enabled(has_task && idle, egui::Button::new("Change what I do"))
                    .clicked()
                {
                    self.send("start_session", json!({ "task": self.task_input }));
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Pause the AI checks"))
                    .clicked()
                {
                    self.send("stop_session", json!({}));
                }
            } else {
                ui.label("paused");
                if ui
                    .add_enabled(has_task && idle, egui::Button::new("Start"))
                    .clicked()
                {
                    self.send("start_session", json!({ "task": self.task_input }));
                }
            }
        });
    }

    /// What the planner assigned, its progress, notes and steps, and asking for a change.
    fn now_ui(&mut self, ui: &mut egui::Ui, status: &Status, planner: &PlannerStatus) {
        let idle = !self.pending.load(Ordering::SeqCst);
        let assignment = planner
            .assignment
            .as_ref()
            .filter(|_| status.session_active);
        let Some(assignment) = assignment else {
            ui.label("The AI picks what you do from your to-do list.");
            if ui
                .add_enabled(idle, egui::Button::new("Get my task"))
                .clicked()
            {
                self.send("assign", json!({}));
            }
            return;
        };

        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong(&assignment.task);
            ui.small(format!(
                "{} — since {}",
                assignment.why,
                local_time(assignment.at)
            ));
            if let Some(until) = assignment.until {
                let total = (until - assignment.at).num_seconds().max(1) as f32;
                let elapsed = (Utc::now() - assignment.at).num_seconds().max(0) as f32;
                let minutes_left = (until - Utc::now()).num_minutes();
                ui.add(egui::ProgressBar::new((elapsed / total).min(1.0)).text(
                    if minutes_left >= 0 {
                        format!(
                            "until {} · {} left",
                            local_time(until),
                            minutes_text(minutes_left)
                        )
                    } else {
                        format!("time was up at {}", local_time(until))
                    },
                ));
                match assignment.when_time_is_up {
                    WhenTimeIsUp::Reassign if minutes_left >= 0 => {
                        ui.small("When the time is up, the AI picks what comes next.");
                    }
                    WhenTimeIsUp::Reassign => {
                        ui.small("Picking what comes next…");
                    }
                    WhenTimeIsUp::NotifyOnly if minutes_left >= 0 => {
                        ui.small("When the time is up you only get a notice and keep this.");
                    }
                    WhenTimeIsUp::NotifyOnly => {
                        ui.colored_label(
                            RED,
                            "Time is up. Keep going, or ask for a change when you are done.",
                        );
                    }
                }
            }
            if let Some(todo_id) = &assignment.todo_id {
                if let Some(minutes) = planner.minutes_today.get(todo_id) {
                    ui.small(format!("Today on this to-do: {}", minutes_text(*minutes)));
                }
            }
        });

        if let Some(todo_id) = assignment.todo_id.clone() {
            self.notes_ui(ui, &todo_id, planner);
        }

        // --- quick requests -------------------------------------------------------------
        ui.horizontal_wrapped(|ui| {
            if ui.button("✔ I finished it").clicked() {
                self.choice = ChangeChoice::Done;
                self.reason = "Finished: ".into();
            }
            if ui.button("⛔ I'm stuck").clicked() {
                self.choice = ChangeChoice::OtherTodo;
                self.reason = "Stuck because: ".into();
            }
            if ui.button("☕ I need a break").clicked() {
                self.choice = ChangeChoice::Free;
                self.free_task = "A short break away from the screen".into();
                self.reason = "I need a break because: ".into();
            }
        });
        self.change_ui(ui, assignment, planner);
    }

    /// The user's own notes and steps for the current to-do; the AI sees them when it
    /// reviews "I finished it".
    fn notes_ui(&mut self, ui: &mut egui::Ui, todo_id: &str, planner: &PlannerStatus) {
        if self.todo_ui.notes_for.as_deref() != Some(todo_id) {
            self.todo_ui.notes_for = Some(todo_id.to_string());
            self.todo_ui.notes = planner.task_notes.get(todo_id).cloned().unwrap_or_default();
        }
        let mut save = false;
        egui::CollapsingHeader::new("Notes and steps")
            .id_salt("task_notes")
            .default_open(true)
            .show(ui, |ui| {
                let mut remove = None;
                for (index, step) in self.todo_ui.notes.steps.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        save |= ui.checkbox(&mut step.done, &step.text).changed();
                        if ui.small_button("x").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                if let Some(index) = remove {
                    self.todo_ui.notes.steps.remove(index);
                    save = true;
                }
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.todo_ui.new_step)
                            .hint_text("add a step")
                            .desired_width(280.0),
                    );
                    let entered = response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if (ui.small_button("Add").clicked() || entered)
                        && !self.todo_ui.new_step.trim().is_empty()
                    {
                        self.todo_ui.notes.steps.push(TaskStep {
                            text: self.todo_ui.new_step.trim().to_string(),
                            done: false,
                        });
                        self.todo_ui.new_step.clear();
                        save = true;
                    }
                });
                let response = ui.add(
                    egui::TextEdit::multiline(&mut self.todo_ui.notes.text)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("notes: where you are, what is left"),
                );
                save |= response.lost_focus();
            });
        if save {
            // Off the UI thread and outside the one-request-at-a-time rule: notes are small
            // and harmless.
            let (session, todo_id, notes) = (
                self.session,
                todo_id.to_string(),
                self.todo_ui.notes.clone(),
            );
            thread::spawn(move || {
                let _ = call(
                    session,
                    "task_notes_put",
                    json!({ "todo_id": todo_id, "notes": notes }),
                );
            });
        }
    }

    /// Asking the AI to change what the planner assigned.
    fn change_ui(
        &mut self,
        ui: &mut egui::Ui,
        assignment: &task_planner::Assignment,
        planner: &PlannerStatus,
    ) {
        let idle = !self.pending.load(Ordering::SeqCst);
        // The answer to the request sent last has come (or the request failed): an answer
        // clears the reason, an approval also the proof. After an error both stay.
        if let Some(sent_at) = self.change_sent_at.filter(|_| idle) {
            self.change_sent_at = None;
            let answer = self
                .shared
                .lock()
                .decision
                .as_ref()
                .filter(|decision| decision.at >= sent_at)
                .map(|decision| decision.approved);
            if let Some(approved) = answer {
                self.reason.clear();
                if approved {
                    self.evidence.clear();
                    self.proof_screenshot = false;
                }
            }
        }
        egui::CollapsingHeader::new("Ask to change")
            .id_salt("ask_to_change")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.choice, ChangeChoice::Done, "I finished it");
                    ui.radio_value(&mut self.choice, ChangeChoice::OtherTodo, "Another to-do");
                    ui.radio_value(&mut self.choice, ChangeChoice::Free, "Something else");
                    ui.radio_value(&mut self.choice, ChangeChoice::End, "Pause the AI checks");
                    match assignment.when_time_is_up {
                        WhenTimeIsUp::Reassign => ui.radio_value(
                            &mut self.choice,
                            ChangeChoice::NotifyWhenTimeIsUp,
                            "Only notify me when time is up",
                        ),
                        WhenTimeIsUp::NotifyOnly => ui.radio_value(
                            &mut self.choice,
                            ChangeChoice::ReassignWhenTimeIsUp,
                            "Pick a new task when time is up",
                        ),
                    };
                });
                let others: Vec<&TodoItem> = planner
                    .open_todos
                    .iter()
                    .filter(|todo| assignment.todo_id.as_deref() != Some(todo.id.as_str()))
                    .collect();
                match self.choice {
                    ChangeChoice::OtherTodo => {
                        let selected = others
                            .iter()
                            .find(|todo| Some(&todo.id) == self.other_todo.as_ref())
                            .map(|todo| todo.title.clone())
                            .unwrap_or_else(|| "choose…".into());
                        egui::ComboBox::from_id_salt("other_todo")
                            .selected_text(selected)
                            .show_ui(ui, |ui| {
                                for todo in &others {
                                    ui.selectable_value(
                                        &mut self.other_todo,
                                        Some(todo.id.clone()),
                                        &todo.title,
                                    );
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
                    ChangeChoice::Done => {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.evidence)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .hint_text(
                                    "proof of the result — needed when the result is a fact \
                                     (a mail or message sent, an order placed)",
                                ),
                        );
                        ui.checkbox(
                            &mut self.proof_screenshot,
                            "attach a screenshot as proof (taken 5 seconds after sending: switch \
                             to the window that shows the result)",
                        );
                    }
                    _ => {}
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
                        .filter(|id| others.iter().any(|todo| &todo.id == id))
                        .map(|todo_id| ChangeKind::SwitchTodo { todo_id }),
                    ChangeChoice::Free => Some(self.free_task.trim().to_string())
                        .filter(|task| !task.is_empty())
                        .map(|task| ChangeKind::SwitchFree { task }),
                    // Offered only for the mode that is not in force.
                    ChangeChoice::NotifyWhenTimeIsUp => (assignment.when_time_is_up
                        == WhenTimeIsUp::Reassign)
                        .then_some(ChangeKind::SetWhenTimeIsUp {
                            action: WhenTimeIsUp::NotifyOnly,
                        }),
                    ChangeChoice::ReassignWhenTimeIsUp => (assignment.when_time_is_up
                        == WhenTimeIsUp::NotifyOnly)
                        .then_some(ChangeKind::SetWhenTimeIsUp {
                            action: WhenTimeIsUp::Reassign,
                        }),
                };
                let ready = idle && request.is_some() && self.reason.trim().chars().count() >= 4;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(ready, egui::Button::new("Send request"))
                        .clicked()
                    {
                        if let Some(request) = request {
                            let done = matches!(request, ChangeKind::Done);
                            let mut params = json!({ "request": request, "reason": self.reason });
                            if done {
                                params["evidence"] = json!(self.evidence);
                                if self.proof_screenshot {
                                    params["proof_screenshot_after_secs"] =
                                        json!(PROOF_SCREENSHOT_DELAY_SECONDS);
                                }
                            }
                            // The texts stay until the AI answers (see below), so an error
                            // does not lose them.
                            self.change_sent_at = Some(Utc::now());
                            self.send("request_change", params);
                        }
                    }
                    if !idle {
                        ui.spinner();
                        ui.label("the AI is deciding…");
                    }
                });
                if let Some(decision) = &self.shared.lock().decision {
                    let (color, label) = if decision.approved {
                        (GREEN, "approved")
                    } else {
                        (RED, "rejected")
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(color, label);
                        ui.label(&decision.reply);
                    });
                }
            });
    }

    /// The to-do list: soonest deadline first, each with details and change buttons, and the
    /// form to add one.
    fn list_ui(&mut self, ui: &mut egui::Ui, planner: &PlannerStatus) {
        ui.heading(format!("To-do list ({})", planner.open_todos.len()));
        let today = Local::now().date_naive();
        let mut todos = planner.open_todos.clone();
        todos.sort_by_key(|todo| {
            (
                todo.deadline.unwrap_or(NaiveDate::MAX),
                std::cmp::Reverse(todo.priority),
            )
        });
        let current = planner
            .assignment
            .as_ref()
            .and_then(|assignment| assignment.todo_id.clone());
        for todo in &todos {
            let expanded = self.todo_ui.expanded.as_deref() == Some(todo.id.as_str());
            let due = todo.deadline.map(|deadline| (deadline - today).num_days());
            let due_text = match due {
                Some(days) if days < 0 => format!("overdue {}d", -days),
                Some(0) => "due today".into(),
                Some(1) => "due tomorrow".into(),
                Some(days) => format!("D-{days}"),
                None => String::new(),
            };
            ui.horizontal_wrapped(|ui| {
                let marker = if expanded { "▼" } else { "▶" };
                if ui.small_button(marker).clicked() {
                    self.todo_ui.expanded = (!expanded).then(|| todo.id.clone());
                }
                if current.as_deref() == Some(todo.id.as_str()) {
                    ui.colored_label(GREEN, "NOW");
                }
                match due {
                    Some(days) if days <= 0 => {
                        ui.colored_label(RED, &due_text);
                    }
                    Some(_) => {
                        ui.colored_label(YELLOW, &due_text);
                    }
                    None => {}
                }
                ui.label(format!("P{}", todo.priority));
                if let Some(minutes) = todo.estimate_minutes {
                    ui.small(minutes_text(minutes as i64));
                }
                ui.strong(&todo.title);
                if todo.proposed {
                    ui.small("(added by you)");
                }
            });
            if expanded {
                self.todo_details_ui(ui, todo, planner, current.as_deref());
            }
        }
        if todos.is_empty() {
            ui.label("Nothing on the list.");
        }
        self.form_ui(ui, planner);
        self.last_change_ui(ui);
    }

    fn todo_details_ui(
        &mut self,
        ui: &mut egui::Ui,
        todo: &TodoItem,
        planner: &PlannerStatus,
        current: Option<&str>,
    ) {
        let idle = !self.pending.load(Ordering::SeqCst);
        ui.indent(("details", &todo.id), |ui| {
            if !todo.notes.is_empty() {
                ui.label(&todo.notes);
            }
            ui.small(format!(
                "deadline {} · priority {} · estimate {} · spent {}",
                todo.deadline
                    .map_or("none".into(), |deadline| deadline.to_string()),
                todo.priority,
                todo.estimate_minutes
                    .map_or("none".into(), |minutes| minutes_text(minutes as i64)),
                minutes_text(planner.minutes_total.get(&todo.id).copied().unwrap_or(0)),
            ));
            ui.horizontal_wrapped(|ui| {
                if current != Some(todo.id.as_str())
                    && planner.assignment.is_some()
                    && ui.button("Work on this instead").clicked()
                {
                    self.choice = ChangeChoice::OtherTodo;
                    self.other_todo = Some(todo.id.clone());
                    self.reason = "I want to switch because: ".into();
                }
                // An "Improve: …" to-do is made by the app from a to-do finished early; it
                // goes away when it is done or its time is used up.
                let improvement = todo.id.starts_with("polish:");
                if improvement {
                    ui.weak("improvement time kept from an early finish");
                }
                if !improvement && ui.button("Edit").clicked() {
                    self.todo_ui.form = TodoForm {
                        title: todo.title.clone(),
                        notes: todo.notes.clone(),
                        deadline: todo
                            .deadline
                            .map(|deadline| deadline.to_string())
                            .unwrap_or_default(),
                        priority: todo.priority,
                        estimate_minutes: todo.estimate_minutes.unwrap_or(0),
                        reason: String::new(),
                    };
                    self.todo_ui.editing = Some(todo.id.clone());
                    self.todo_ui.form_open = true;
                }
                if !improvement && ui.button("Remove").clicked() {
                    self.todo_ui.removing = Some(todo.id.clone());
                    self.todo_ui.remove_reason.clear();
                }
            });
            if self.todo_ui.removing.as_deref() == Some(todo.id.as_str()) {
                ui.add(
                    egui::TextEdit::singleline(&mut self.todo_ui.remove_reason)
                        .desired_width(f32::INFINITY)
                        .hint_text("why remove it? the AI decides"),
                );
                ui.horizontal(|ui| {
                    let ready = idle && self.todo_ui.remove_reason.trim().chars().count() >= 4;
                    if ui
                        .add_enabled(ready, egui::Button::new("Ask to remove"))
                        .clicked()
                    {
                        self.send(
                            "todo_change",
                            json!({
                                "change": { "kind": "remove", "todo_id": todo.id },
                                "reason": self.todo_ui.remove_reason.trim(),
                            }),
                        );
                        self.todo_ui.removing = None;
                    }
                    if ui.button("Cancel").clicked() {
                        self.todo_ui.removing = None;
                    }
                });
            }
        });
    }

    /// Adding a to-do (empty fields are filled in by the AI) or editing one (empty fields
    /// stay as they are). The AI reviews both.
    fn form_ui(&mut self, ui: &mut egui::Ui, planner: &PlannerStatus) {
        let idle = !self.pending.load(Ordering::SeqCst);
        if !self.todo_ui.form_open {
            if ui.button("+ Add a to-do").clicked() {
                self.todo_ui.form = TodoForm::default();
                self.todo_ui.editing = None;
                self.todo_ui.form_open = true;
            }
            return;
        }
        let editing = self.todo_ui.editing.clone();
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            match &editing {
                Some(todo_id) => {
                    let title = planner
                        .open_todos
                        .iter()
                        .find(|todo| &todo.id == todo_id)
                        .map_or(todo_id.as_str(), |todo| todo.title.as_str());
                    ui.strong(format!("Edit \"{title}\" — the AI reviews the change"));
                }
                None => {
                    ui.strong("Add a to-do — leave a field empty and the AI fills it in");
                }
            }
            let form = &mut self.todo_ui.form;
            egui::Grid::new("todo_form").num_columns(2).show(ui, |ui| {
                ui.label("Title");
                ui.add(egui::TextEdit::singleline(&mut form.title).desired_width(320.0));
                ui.end_row();
                ui.label("Notes");
                ui.add(
                    egui::TextEdit::multiline(&mut form.notes)
                        .desired_rows(2)
                        .desired_width(320.0)
                        .hint_text("what done means, what it needs"),
                );
                ui.end_row();
                ui.label("Deadline");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut form.deadline)
                            .desired_width(160.0)
                            .hint_text("이틀 뒤, 금요일, 11/5 …"),
                    );
                    // The date the words were read as, so a misreading shows at once.
                    if let Some(deadline) = form.deadline_date() {
                        ui.label(format!("→ {}", deadline.format("%Y-%m-%d (%a)")));
                    }
                });
                ui.end_row();
                ui.label("Priority");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut form.priority).range(0..=5));
                    ui.small("1 low … 5 urgent, 0 = the AI decides");
                });
                ui.end_row();
                ui.label("Estimate (min)");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut form.estimate_minutes).range(0..=6000));
                    ui.small("0 = the AI decides");
                });
                ui.end_row();
                ui.label("Reason");
                ui.add(
                    egui::TextEdit::singleline(&mut form.reason)
                        .desired_width(320.0)
                        .hint_text(if editing.is_some() {
                            "why change it? the AI decides"
                        } else {
                            "optional"
                        }),
                );
                ui.end_row();
            });
            let bad_deadline = !form.deadline.trim().is_empty() && form.deadline_date().is_none();
            if bad_deadline {
                ui.colored_label(
                    RED,
                    "The deadline could not be read. Write it like 오늘, 내일, 이틀 뒤, 금요일, \
                     다음 주 수요일, 월말, 11/5 or 2026-11-05.",
                );
            }
            let ready = idle
                && !bad_deadline
                && match &editing {
                    None => form.title.trim().chars().count() >= 2,
                    Some(_) => form.reason.trim().chars().count() >= 4,
                };
            let change = match &editing {
                None => json!({ "kind": "add", "todo": form.draft() }),
                Some(todo_id) => {
                    json!({ "kind": "edit", "todo_id": todo_id, "todo": form.draft() })
                }
            };
            let reason = form.reason.trim().to_string();
            let mut close = false;
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(ready, egui::Button::new("Send to the AI"))
                    .clicked()
                {
                    self.send("todo_change", json!({ "change": change, "reason": reason }));
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
                if !idle {
                    ui.spinner();
                }
            });
            if close {
                self.todo_ui.form_open = false;
            }
        });
    }

    /// The AI's answer to the last to-do change, and the way around a "no".
    fn last_change_ui(&mut self, ui: &mut egui::Ui) {
        let Some(record) = self.shared.lock().todo_change.clone() else {
            return;
        };
        let idle = !self.pending.load(Ordering::SeqCst);
        let summary = change_summary(&record.change, record.before.as_ref());
        ui.horizontal_wrapped(|ui| {
            let (color, label) = if record.approved {
                (GREEN, "approved")
            } else if record.ai_failed {
                (YELLOW, "not reviewed")
            } else {
                (RED, "rejected")
            };
            ui.colored_label(color, label);
            ui.label(format!("{summary}: {}", record.reply));
        });
        if record.approved {
            return;
        }
        match &record.scheduled_update {
            Some(_) => {
                ui.small(
                    "Scheduled: the Settings tab asks you about it three times, two hours apart.",
                );
            }
            None => {
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new("Do it without the AI (confirm 3 times, 2 hours apart)"),
                    )
                    .clicked()
                {
                    self.send("todo_change_schedule", json!({ "id": record.id }));
                }
            }
        }
    }

    /// Today: what happened so far.
    fn today_ui(&mut self, ui: &mut egui::Ui, status: &Status, planner: &PlannerStatus) {
        egui::CollapsingHeader::new("Today")
            .id_salt("today")
            .show(ui, |ui| {
                let title_of = |todo_id: &str| {
                    planner
                        .open_todos
                        .iter()
                        .find(|todo| todo.id == todo_id)
                        .map(|todo| todo.title.clone())
                        .or_else(|| {
                            planner
                                .recent_done
                                .iter()
                                .find(|done| done.todo_id == todo_id)
                                .map(|done| done.title.clone())
                        })
                        .unwrap_or_else(|| todo_id.to_string())
                };
                let today = Local::now().date_naive();
                let is_today = |at: DateTime<Utc>| at.with_timezone(&Local).date_naive() == today;
                let done_today: Vec<_> = planner
                    .recent_done
                    .iter()
                    .filter(|done| is_today(done.at))
                    .collect();
                ui.label(format!("Finished today: {}", done_today.len()));
                for done in &done_today {
                    ui.small(format!("✔ {} at {}", done.title, local_time(done.at)));
                }
                let total_today: i64 = planner.minutes_today.values().sum();
                ui.label(format!(
                    "Time on to-dos today: {}",
                    minutes_text(total_today)
                ));
                for (todo_id, minutes) in &planner.minutes_today {
                    ui.small(format!(
                        "• {} — {}",
                        title_of(todo_id),
                        minutes_text(*minutes)
                    ));
                }
                let requests_today = planner
                    .recent_requests
                    .iter()
                    .filter(|request| is_today(request.at));
                let (approved, rejected) = requests_today.fold((0, 0), |(yes, no), request| {
                    if request.approved {
                        (yes + 1, no)
                    } else {
                        (yes, no + 1)
                    }
                });
                ui.label(format!(
                    "Change requests today: {approved} approved, {rejected} rejected"
                ));
                ui.label(format!("Blocked now: {}", status.blocked.len()));
            });
    }
}

/// Finished to-dos of the last seven days, with the time they took.
fn done_ui(ui: &mut egui::Ui, planner: &PlannerStatus) {
    egui::CollapsingHeader::new(format!("Finished ({})", planner.recent_done.len()))
        .id_salt("finished")
        .show(ui, |ui| {
            if planner.recent_done.is_empty() {
                ui.label("Nothing finished in the last seven days.");
            }
            for done in &planner.recent_done {
                let spent = planner
                    .minutes_total
                    .get(&done.todo_id)
                    .map(|minutes| format!(" · took {}", minutes_text(*minutes)))
                    .unwrap_or_default();
                ui.label(format!(
                    "✔ {} — {}{spent}",
                    done.title,
                    done.at.with_timezone(&Local).format("%m-%d %H:%M")
                ));
            }
        });
}

/// Every change request and to-do change, newest first, with the AI's answers.
fn log_ui(ui: &mut egui::Ui, planner: &PlannerStatus) {
    egui::CollapsingHeader::new("Requests and answers")
        .id_salt("request_log")
        .show(ui, |ui| {
            let mut lines: Vec<(DateTime<Utc>, bool, String)> = planner
                .recent_requests
                .iter()
                .map(|request| {
                    (
                        request.at,
                        request.approved,
                        format!(
                            "{} — \"{}\" → {}",
                            task_planner_label(&request.request),
                            request.reason,
                            request.reply
                        ),
                    )
                })
                .chain(
                    planner
                        .recent_todo_changes
                        .iter()
                        .map(|record| (record.at, record.approved, todo_change_line(record))),
                )
                .collect();
            lines.sort_by(|left, right| right.0.cmp(&left.0));
            if lines.is_empty() {
                ui.label("No requests yet.");
            }
            for (at, approved, text) in lines {
                ui.horizontal_wrapped(|ui| {
                    ui.small(at.with_timezone(&Local).format("%m-%d %H:%M").to_string());
                    ui.colored_label(
                        if approved { GREEN } else { RED },
                        if approved { "✔" } else { "x" },
                    );
                    ui.label(text);
                });
            }
        });
}

fn todo_change_line(record: &TodoChangeRecord) -> String {
    let scheduled = if record.scheduled_update.is_some() {
        " (scheduled without the AI)"
    } else {
        ""
    };
    format!(
        "{}{} — \"{}\" → {}",
        change_summary(&record.change, record.before.as_ref()),
        scheduled,
        record.reason,
        record.reply
    )
}

fn task_planner_label(request: &ChangeKind) -> String {
    match request {
        ChangeKind::Done => "finished".into(),
        ChangeKind::SwitchTodo { todo_id } => format!("switch to {todo_id}"),
        ChangeKind::SwitchFree { task } => format!("switch to \"{task}\""),
        ChangeKind::End => "pause".into(),
        ChangeKind::SetWhenTimeIsUp {
            action: WhenTimeIsUp::NotifyOnly,
        } => "only notify when time is up".into(),
        ChangeKind::SetWhenTimeIsUp {
            action: WhenTimeIsUp::Reassign,
        } => "new task when time is up".into(),
    }
}

fn local_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M").to_string()
}

/// `95` → `1 h 35 min`.
fn minutes_text(minutes: i64) -> String {
    let minutes = minutes.max(0);
    match (minutes / 60, minutes % 60) {
        (0, rest) => format!("{rest} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, rest) => format!("{hours} h {rest} min"),
    }
}

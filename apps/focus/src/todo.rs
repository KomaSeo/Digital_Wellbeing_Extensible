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
    ChangeKind, PlannerStatus, TaskNotes, TaskStep, TodoChange, TodoChangeRecord, TodoItem,
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
                    "예약된 업데이트가 답을 기다립니다: 이 업데이트가 정말 맞나요?",
                );
                if ui.link("설정 탭에서 답하기").clicked() {
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
                    "설정 단계: 설정이 아직 보호(잠금)되지 않아서, 어떤 변경이든 바로 적용됩니다.",
                );
                if ui.link("설정 탭에서 보호하기").clicked() {
                    self.tab = Tab::Settings;
                }
            });
            ui.separator();
        }
        ui.heading("데일리 웰빙");
        let planner = status.planner.clone();
        match planner.clone().filter(|planner| planner.active) {
            Some(planner) => self.now_ui(ui, status, &planner),
            None => self.free_mode_ui(ui, status),
        }
        if !status.blocked.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "지금 {}개가 차단되어 있습니다.",
                    status.blocked.len()
                ));
                if ui.link("차단 탭 보기").clicked() {
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
        ui.label(
            "지금 무엇을 하나요? 구체적으로 적으세요. AI는 모든 앱을 이 글에 비추어 판정합니다.",
        );
        ui.add(
            egui::TextEdit::multiline(&mut self.task_input)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("예: 졸업논문 3장 작성 — Overleaf, 논문 PDF, Google Scholar 필요"),
        );
        let has_task = !self.task_input.trim().is_empty();
        let idle = !self.pending.load(Ordering::SeqCst);
        ui.horizontal(|ui| {
            if status.session_active {
                ui.colored_label(GREEN, "켜짐");
                if ui
                    .add_enabled(has_task && idle, egui::Button::new("하는 일 바꾸기"))
                    .clicked()
                {
                    self.send("start_session", json!({ "task": self.task_input }));
                }
                if ui
                    .add_enabled(idle, egui::Button::new("AI 검사 멈추기"))
                    .clicked()
                {
                    self.send("stop_session", json!({}));
                }
            } else {
                ui.label("멈춤");
                if ui
                    .add_enabled(has_task && idle, egui::Button::new("시작"))
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
            ui.label("AI가 할 일 목록에서 지금 할 일을 고릅니다.");
            if ui
                .add_enabled(idle, egui::Button::new("할 일 받기"))
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
                "{} — {}부터",
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
                            "{}까지 · {} 남음",
                            local_time(until),
                            minutes_text(minutes_left)
                        )
                    } else {
                        format!("{}에 시간이 다 되었습니다", local_time(until))
                    },
                ));
                match assignment.when_time_is_up {
                    WhenTimeIsUp::Reassign if minutes_left >= 0 => {
                        ui.small("시간이 다 되면 AI가 다음 할 일을 고릅니다.");
                    }
                    WhenTimeIsUp::Reassign => {
                        ui.small("다음 할 일을 고르는 중…");
                    }
                    WhenTimeIsUp::NotifyOnly if minutes_left >= 0 => {
                        ui.small("시간이 다 되어도 알림만 오고, 이 일을 계속합니다.");
                    }
                    WhenTimeIsUp::NotifyOnly => {
                        ui.colored_label(
                            RED,
                            "시간이 다 되었습니다. 계속하거나, 끝났으면 변경을 요청하세요.",
                        );
                    }
                }
            }
            if let Some(todo_id) = &assignment.todo_id {
                if let Some(minutes) = planner.minutes_today.get(todo_id) {
                    ui.small(format!(
                        "오늘 이 할 일에 들인 시간: {}",
                        minutes_text(*minutes)
                    ));
                }
            }
        });

        if let Some(todo_id) = assignment.todo_id.clone() {
            self.notes_ui(ui, &todo_id, planner);
        }

        // --- quick requests -------------------------------------------------------------
        ui.horizontal_wrapped(|ui| {
            if ui.button("✔ 끝냈어요").clicked() {
                self.choice = ChangeChoice::Done;
                self.reason = "끝낸 것: ".into();
            }
            if ui.button("⛔ 막혔어요").clicked() {
                self.choice = ChangeChoice::Repick;
                self.reason = "막힌 까닭: ".into();
            }
            if ui.button("☕ 쉬고 싶어요").clicked() {
                self.choice = ChangeChoice::Repick;
                self.reason = "쉬어야 하는 까닭: ".into();
            }
        });
        self.change_ui(ui, assignment);
    }

    /// The user's own notes and steps for the current to-do; the AI sees them when it
    /// reviews "I finished it".
    fn notes_ui(&mut self, ui: &mut egui::Ui, todo_id: &str, planner: &PlannerStatus) {
        if self.todo_ui.notes_for.as_deref() != Some(todo_id) {
            self.todo_ui.notes_for = Some(todo_id.to_string());
            self.todo_ui.notes = planner.task_notes.get(todo_id).cloned().unwrap_or_default();
        }
        let mut save = false;
        egui::CollapsingHeader::new("메모와 단계")
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
                            .hint_text("단계 추가")
                            .desired_width(280.0),
                    );
                    let entered = response.lost_focus()
                        && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if (ui.small_button("추가").clicked() || entered)
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
                        .hint_text("메모: 어디까지 했는지, 무엇이 남았는지"),
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
    fn change_ui(&mut self, ui: &mut egui::Ui, assignment: &task_planner::Assignment) {
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
        egui::CollapsingHeader::new("변경 요청")
            .id_salt("ask_to_change")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut self.choice, ChangeChoice::Done, "끝냈다");
                    ui.radio_value(&mut self.choice, ChangeChoice::Repick, "다른 일 받기");
                    ui.radio_value(&mut self.choice, ChangeChoice::End, "AI 검사 멈추기");
                    match assignment.when_time_is_up {
                        WhenTimeIsUp::Reassign => ui.radio_value(
                            &mut self.choice,
                            ChangeChoice::NotifyWhenTimeIsUp,
                            "시간이 다 되어도 알림만 받기",
                        ),
                        WhenTimeIsUp::NotifyOnly => ui.radio_value(
                            &mut self.choice,
                            ChangeChoice::ReassignWhenTimeIsUp,
                            "시간이 다 되면 새 할 일 받기",
                        ),
                    };
                });
                match self.choice {
                    ChangeChoice::Repick => {
                        ui.small(
                            "지금 할 일을 그만두고 다른 일을 받습니다. 무엇을 할지는 AI가 생활 규칙과 \
                             할 일 목록을 보고 정합니다. 이유만 적으세요.",
                        );
                    }
                    ChangeChoice::Done => {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.evidence)
                                .desired_rows(2)
                                .desired_width(f32::INFINITY)
                                .hint_text(
                                    "결과의 증거 — 결과가 분명한 일(메일이나 메시지 보내기, 주문하기)이면 \
                                     필요합니다",
                                ),
                        );
                        ui.checkbox(
                            &mut self.proof_screenshot,
                            "증거로 화면 캡처 붙이기(보내고 5초 뒤에 찍습니다. 그 사이에 결과가 보이는 \
                             창으로 바꾸세요)",
                        );
                    }
                    _ => {}
                }
                ui.add(
                    egui::TextEdit::multiline(&mut self.reason)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("이유 — 충분한 이유인지 AI가 정합니다"),
                );
                let request = match self.choice {
                    ChangeChoice::Done => Some(ChangeKind::Done),
                    ChangeChoice::End => Some(ChangeKind::End),
                    ChangeChoice::Repick => Some(ChangeKind::Repick),
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
                        .add_enabled(ready, egui::Button::new("요청 보내기"))
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
                        ui.label("AI가 정하는 중…");
                    }
                });
                if let Some(decision) = &self.shared.lock().decision {
                    let (color, label) = if decision.approved {
                        (GREEN, "승인")
                    } else {
                        (RED, "거절")
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
        ui.heading(format!("할 일 목록 ({})", planner.open_todos.len()));
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
                Some(days) if days < 0 => format!("{}일 지남", -days),
                Some(0) => "오늘 마감".into(),
                Some(1) => "내일 마감".into(),
                Some(days) => format!("D-{days}"),
                None => String::new(),
            };
            ui.horizontal_wrapped(|ui| {
                let marker = if expanded { "▼" } else { "▶" };
                if ui.small_button(marker).clicked() {
                    self.todo_ui.expanded = (!expanded).then(|| todo.id.clone());
                }
                if current.as_deref() == Some(todo.id.as_str()) {
                    ui.colored_label(GREEN, "지금");
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
                    ui.small("(내가 추가)");
                }
            });
            if expanded {
                self.todo_details_ui(ui, todo, planner);
            }
        }
        if todos.is_empty() {
            ui.label("목록이 비어 있습니다.");
        }
        self.form_ui(ui, planner);
        self.last_change_ui(ui);
    }

    fn todo_details_ui(&mut self, ui: &mut egui::Ui, todo: &TodoItem, planner: &PlannerStatus) {
        let idle = !self.pending.load(Ordering::SeqCst);
        ui.indent(("details", &todo.id), |ui| {
            if !todo.notes.is_empty() {
                ui.label(&todo.notes);
            }
            ui.small(format!(
                "마감 {} · 중요도 {} · 예상 {} · 들인 시간 {}",
                todo.deadline
                    .map_or("없음".into(), |deadline| deadline.to_string()),
                todo.priority,
                todo.estimate_minutes
                    .map_or("없음".into(), |minutes| minutes_text(minutes as i64)),
                minutes_text(planner.minutes_total.get(&todo.id).copied().unwrap_or(0)),
            ));
            ui.horizontal_wrapped(|ui| {
                // An "Improve: …" to-do is made by the app from a to-do finished early; it
                // goes away when it is done or its time is used up.
                let improvement = todo.id.starts_with("polish:");
                if improvement {
                    ui.weak("일찍 끝낸 일의 다듬기 시간");
                }
                if !improvement && ui.button("고치기").clicked() {
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
                if !improvement && ui.button("지우기").clicked() {
                    self.todo_ui.removing = Some(todo.id.clone());
                    self.todo_ui.remove_reason.clear();
                }
            });
            if self.todo_ui.removing.as_deref() == Some(todo.id.as_str()) {
                ui.add(
                    egui::TextEdit::singleline(&mut self.todo_ui.remove_reason)
                        .desired_width(f32::INFINITY)
                        .hint_text("왜 지우나요? AI가 정합니다"),
                );
                ui.horizontal(|ui| {
                    let ready = idle && self.todo_ui.remove_reason.trim().chars().count() >= 4;
                    if ui
                        .add_enabled(ready, egui::Button::new("지우기 요청"))
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
                    if ui.button("취소").clicked() {
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
            if ui.button("+ 할 일 추가").clicked() {
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
                    ui.strong(format!("\"{title}\" 고치기 — AI가 심사합니다"));
                }
                None => {
                    ui.strong("할 일 추가 — 비워 둔 칸은 AI가 채웁니다");
                }
            }
            let form = &mut self.todo_ui.form;
            egui::Grid::new("todo_form").num_columns(2).show(ui, |ui| {
                ui.label("제목");
                ui.add(egui::TextEdit::singleline(&mut form.title).desired_width(320.0));
                ui.end_row();
                ui.label("메모");
                ui.add(
                    egui::TextEdit::multiline(&mut form.notes)
                        .desired_rows(2)
                        .desired_width(320.0)
                        .hint_text("무엇을 하면 끝인지, 무엇이 필요한지"),
                );
                ui.end_row();
                ui.label("마감");
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
                ui.label("중요도");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut form.priority).range(0..=5));
                    ui.small("1 낮음 … 5 급함, 0 = AI가 정함");
                });
                ui.end_row();
                ui.label("예상 시간(분)");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut form.estimate_minutes).range(0..=6000));
                    ui.small("0 = AI가 정함");
                });
                ui.end_row();
                ui.label("이유");
                ui.add(
                    egui::TextEdit::singleline(&mut form.reason)
                        .desired_width(320.0)
                        .hint_text(if editing.is_some() {
                            "왜 고치나요? AI가 정합니다"
                        } else {
                            "선택"
                        }),
                );
                ui.end_row();
            });
            let bad_deadline = !form.deadline.trim().is_empty() && form.deadline_date().is_none();
            if bad_deadline {
                ui.colored_label(
                    RED,
                    "마감을 읽지 못했습니다. 오늘, 내일, 이틀 뒤, 금요일, 다음 주 수요일, 월말, 11/5, \
                     2026-11-05처럼 적으세요.",
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
                    .add_enabled(ready, egui::Button::new("AI에게 보내기"))
                    .clicked()
                {
                    self.send("todo_change", json!({ "change": change, "reason": reason }));
                    close = true;
                }
                if ui.button("취소").clicked() {
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
        let summary = change_summary_korean(&record.change, record.before.as_ref());
        ui.horizontal_wrapped(|ui| {
            let (color, label) = if record.approved {
                (GREEN, "승인")
            } else if record.ai_failed {
                (YELLOW, "심사 못 함")
            } else {
                (RED, "거절")
            };
            ui.colored_label(color, label);
            ui.label(format!("{summary}: {}", record.reply));
        });
        if record.approved {
            return;
        }
        match &record.scheduled_update {
            Some(_) => {
                ui.small("예약됨: 설정 탭이 두 시간 간격으로 세 번 묻습니다.");
            }
            None => {
                if ui
                    .add_enabled(
                        idle,
                        egui::Button::new("AI 없이 하기(두 시간 간격으로 세 번 확인)"),
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
        egui::CollapsingHeader::new("오늘")
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
                ui.label(format!("오늘 끝낸 일: {}개", done_today.len()));
                for done in &done_today {
                    ui.small(format!("✔ {} ({})", done.title, local_time(done.at)));
                }
                let total_today: i64 = planner.minutes_today.values().sum();
                ui.label(format!(
                    "오늘 할 일에 들인 시간: {}",
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
                    "오늘의 변경 요청: 승인 {approved}개, 거절 {rejected}개"
                ));
                ui.label(format!("지금 걸린 차단: {}개", status.blocked.len()));
            });
    }
}

/// Finished to-dos of the last seven days, with the time they took.
fn done_ui(ui: &mut egui::Ui, planner: &PlannerStatus) {
    egui::CollapsingHeader::new(format!("끝낸 일 ({})", planner.recent_done.len()))
        .id_salt("finished")
        .show(ui, |ui| {
            if planner.recent_done.is_empty() {
                ui.label("지난 7일 동안 끝낸 일이 없습니다.");
            }
            for done in &planner.recent_done {
                let spent = planner
                    .minutes_total
                    .get(&done.todo_id)
                    .map(|minutes| format!(" · {} 걸림", minutes_text(*minutes)))
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
    egui::CollapsingHeader::new("요청과 AI의 답")
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
                ui.label("아직 요청이 없습니다.");
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
        " (AI 없이 예약됨)"
    } else {
        ""
    };
    format!(
        "{}{} — \"{}\" → {}",
        change_summary_korean(&record.change, record.before.as_ref()),
        scheduled,
        record.reason,
        record.reply
    )
}

fn task_planner_label(request: &ChangeKind) -> String {
    match request {
        ChangeKind::Done => "끝냈다".into(),
        ChangeKind::SwitchTodo { todo_id } => format!("{todo_id}(으)로 바꾸기"),
        ChangeKind::SwitchFree { task } => format!("\"{task}\"(으)로 바꾸기"),
        ChangeKind::Repick => "다른 일 받기(AI가 고름)".into(),
        ChangeKind::End => "AI 검사 멈추기".into(),
        ChangeKind::SetWhenTimeIsUp {
            action: WhenTimeIsUp::NotifyOnly,
        } => "시간이 다 되어도 알림만".into(),
        ChangeKind::SetWhenTimeIsUp {
            action: WhenTimeIsUp::Reassign,
        } => "시간이 다 되면 새 할 일".into(),
    }
}

/// "\"제목\" 추가" and so on, for the To-do tab.
fn change_summary_korean(change: &TodoChange, before: Option<&TodoItem>) -> String {
    let title = |todo_id: &str| {
        before
            .map(|todo| todo.title.clone())
            .unwrap_or_else(|| todo_id.to_string())
    };
    match change {
        TodoChange::Add { todo } => format!("\"{}\" 추가", todo.title),
        TodoChange::Edit { todo_id, .. } => format!("\"{}\" 고치기", title(todo_id)),
        TodoChange::Remove { todo_id } => format!("\"{}\" 지우기", title(todo_id)),
    }
}

fn local_time(time: DateTime<Utc>) -> String {
    time.with_timezone(&Local).format("%H:%M").to_string()
}

/// `95` → `1 h 35 min`.
fn minutes_text(minutes: i64) -> String {
    let minutes = minutes.max(0);
    match (minutes / 60, minutes % 60) {
        (0, rest) => format!("{rest}분"),
        (hours, 0) => format!("{hours}시간"),
        (hours, rest) => format!("{hours}시간 {rest}분"),
    }
}

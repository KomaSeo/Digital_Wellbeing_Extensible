//! Admin tab: edit the to-do list, see what the agent assigned and decided.

use crate::model::{ChangeKind, PlannerConfig, PlannerState, TodoItem};
use ai_focus_rule::store::JsonStore;
use chrono::{Local, NaiveDate};
use parking_lot::Mutex;
use rule_ui::Panel;
use std::time::{Duration, Instant};

const STATE_REFRESH: Duration = Duration::from_secs(3);

/// Editable copy of the list. Edits stay here until "Save", so a half-typed
/// list never reaches the agent.
struct Draft {
    config: PlannerConfig,
    /// Deadline input per row, kept as text so partial dates can be typed.
    deadlines: Vec<String>,
    dirty: bool,
}

impl Draft {
    fn from(config: PlannerConfig) -> Self {
        let deadlines = config
            .todos
            .iter()
            .map(|t| t.deadline.map(|d| d.to_string()).unwrap_or_default())
            .collect();
        Self {
            config,
            deadlines,
            dirty: false,
        }
    }
}

struct UiState {
    draft: Option<Draft>,
    new_title: String,
    message: String,
    state: PlannerState,
    state_read: Option<Instant>,
}

pub struct PlannerAdminUi {
    config: JsonStore<PlannerConfig>,
    /// Read-only view of the agent's `planner_state.json`.
    state: JsonStore<PlannerState>,
    ui: Mutex<UiState>,
}

impl PlannerAdminUi {
    pub fn new(config: JsonStore<PlannerConfig>, state: JsonStore<PlannerState>) -> Self {
        Self {
            config,
            state,
            ui: Mutex::new(UiState {
                draft: None,
                new_title: String::new(),
                message: String::new(),
                state: PlannerState::default(),
                state_read: None,
            }),
        }
    }
}

impl Panel for PlannerAdminUi {
    fn id(&self) -> &str {
        "task_planner"
    }

    fn name(&self) -> &str {
        "Task planner"
    }

    fn ui(&self, ui: &mut egui::Ui) {
        let mut s = self.ui.lock();
        if s.state_read.is_none_or(|t| t.elapsed() > STATE_REFRESH) {
            let _ = self.state.reload();
            s.state = self.state.get();
            s.state_read = Some(Instant::now());
        }
        let UiState {
            draft,
            new_title,
            message,
            state,
            ..
        } = &mut *s;
        let draft = draft.get_or_insert_with(|| Draft::from(self.config.get()));

        ui.label(
            "The AI picks the user's task from this list. While the planner is on and \
             something is open, the user cannot type a task or end the session freely: \
             each change needs a reason the AI approves.",
        );
        ui.separator();
        if ui
            .checkbox(&mut draft.config.enabled, "Planner on")
            .changed()
        {
            draft.dirty = true;
        }

        // --- list -------------------------------------------------------------
        ui.heading("To-do list");
        let mut remove = None;
        for (i, (todo, deadline)) in draft
            .config
            .todos
            .iter_mut()
            .zip(draft.deadlines.iter_mut())
            .enumerate()
        {
            let finished = state.is_done(&todo.id);
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    if finished {
                        ui.colored_label(egui::Color32::from_rgb(80, 170, 90), "✔ done");
                    }
                    draft.dirty |= ui
                        .add(egui::TextEdit::singleline(&mut todo.title).desired_width(320.0))
                        .changed();
                    if ui.small_button("✕").clicked() {
                        remove = Some(i);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("deadline");
                    let response = ui.add(
                        egui::TextEdit::singleline(deadline)
                            .desired_width(90.0)
                            .hint_text("YYYY-MM-DD"),
                    );
                    if response.changed() {
                        draft.dirty = true;
                        todo.deadline = NaiveDate::parse_from_str(deadline.trim(), "%Y-%m-%d").ok();
                    }
                    if !deadline.trim().is_empty() && todo.deadline.is_none() {
                        ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "invalid");
                    }
                    ui.label("priority");
                    draft.dirty |= ui
                        .add(egui::DragValue::new(&mut todo.priority).range(1..=5))
                        .changed();
                    ui.label("estimate (min)");
                    let mut minutes = todo.estimate_minutes.unwrap_or(0);
                    if ui
                        .add(egui::DragValue::new(&mut minutes).range(0..=6000))
                        .changed()
                    {
                        todo.estimate_minutes = (minutes > 0).then_some(minutes);
                        draft.dirty = true;
                    }
                });
                draft.dirty |= ui
                    .add(
                        egui::TextEdit::multiline(&mut todo.notes)
                            .desired_rows(1)
                            .desired_width(f32::INFINITY)
                            .hint_text("notes: what done means, apps/sites needed"),
                    )
                    .changed();
            });
        }
        if let Some(i) = remove {
            draft.config.todos.remove(i);
            draft.deadlines.remove(i);
            draft.dirty = true;
        }
        ui.horizontal(|ui| {
            ui.text_edit_singleline(new_title);
            if ui.button("Add").clicked() && !new_title.trim().is_empty() {
                draft.config.todos.push(TodoItem::new(new_title.trim()));
                draft.deadlines.push(String::new());
                new_title.clear();
                draft.dirty = true;
            }
        });

        ui.horizontal(|ui| {
            let save = ui.add_enabled(draft.dirty, egui::Button::new("Save"));
            if save.clicked() {
                let config = draft.config.clone();
                *message = match self.config.update(|c| *c = config) {
                    Ok(()) => {
                        draft.dirty = false;
                        "Saved. The agent picks it up within a few seconds.".into()
                    }
                    Err(err) => format!("save failed: {err}"),
                };
            }
            if ui
                .add_enabled(draft.dirty, egui::Button::new("Discard changes"))
                .clicked()
            {
                *draft = Draft::from(self.config.get());
            }
        });
        if !message.is_empty() {
            ui.label(message.as_str());
        }

        // --- what the agent did --------------------------------------------------
        ui.separator();
        ui.heading("Current assignment");
        match &state.assignment {
            Some(a) => {
                ui.label(format!(
                    "{}  (since {})",
                    a.task,
                    a.at.with_timezone(&Local).format("%m-%d %H:%M")
                ));
                ui.small(&a.why);
            }
            None => {
                ui.label("None.");
            }
        }
        ui.heading("Change requests");
        egui::ScrollArea::vertical()
            .id_salt("planner_requests")
            .max_height(220.0)
            .show(ui, |ui| {
                if state.requests.is_empty() {
                    ui.label("None yet.");
                }
                for r in state.requests.iter().rev() {
                    let (color, verdict) = if r.approved {
                        (egui::Color32::from_rgb(80, 170, 90), "approved")
                    } else {
                        (egui::Color32::from_rgb(200, 80, 80), "rejected")
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(r.at.with_timezone(&Local).format("%m-%d %H:%M").to_string());
                        ui.colored_label(color, verdict);
                        ui.label(format!(
                            "{} — \"{}\" → {}",
                            request_label(&r.request),
                            r.reason,
                            r.reply
                        ));
                    });
                }
            });
    }
}

pub(crate) fn request_label(request: &ChangeKind) -> String {
    match request {
        ChangeKind::Done => "done".into(),
        ChangeKind::SwitchTodo { todo_id } => format!("switch to {todo_id}"),
        ChangeKind::SwitchFree { task } => format!("switch to \"{task}\""),
        ChangeKind::End => "end session".into(),
    }
}

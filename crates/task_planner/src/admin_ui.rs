//! Responsibility: Draw the admin's planner tab: edit the to-do list and show the agent's
//! assignments and change requests.

use crate::model::{ChangeKind, PlannerConfig, PlannerState, TodoItem, WhenTimeIsUp};
use ai_focus_rule::store::JsonStore;
use chrono::{Local, NaiveDate};
use parking_lot::Mutex;
use rule_ui::Panel;
use std::time::{Duration, Instant};

const STATE_REFRESH: Duration = Duration::from_secs(3);

/// Responsibility: Hold unsaved edits to the to-do list until the admin presses Save.
///
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
            .map(|todo| {
                todo.deadline
                    .map(|date| date.to_string())
                    .unwrap_or_default()
            })
            .collect();
        Self {
            config,
            deadlines,
            dirty: false,
        }
    }
}

/// Responsibility: Keep the tab's draft, inputs and last read of the agent's state between
/// frames.
struct UiState {
    draft: Option<Draft>,
    new_title: String,
    message: String,
    state: PlannerState,
    state_read: Option<Instant>,
}

/// Responsibility: Implement the Task planner tab over the planner's config and state files.
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
        let mut ui_state = self.ui.lock();
        if ui_state
            .state_read
            .is_none_or(|last_read| last_read.elapsed() > STATE_REFRESH)
        {
            let _ = self.state.reload();
            ui_state.state = self.state.get();
            ui_state.state_read = Some(Instant::now());
        }
        let UiState {
            draft,
            new_title,
            message,
            state,
            ..
        } = &mut *ui_state;
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

        // --- life rules -------------------------------------------------------
        ui.heading("Life rules");
        ui.label(
            "Standing rules the AI follows when it picks a task, reviews a change and \
             reviews a to-do the user proposes. They can be changed only here (or scheduled from \
             Claude Desktop); once the settings are protected, a change takes effect only after \
             you confirm it three times, two days apart.",
        );
        draft.dirty |= ui
            .add(
                egui::TextEdit::multiline(&mut draft.config.life_rules)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("e.g. No games or videos before 18:00 on weekdays. A 30-minute break after two hours of work is fine."),
            )
            .changed();
        ui.separator();

        // --- the AI's instructions ----------------------------------------------
        egui::CollapsingHeader::new("AI instructions")
            .id_salt("planner_instructions")
            .show(ui, |ui| {
                ui.label(
                    "What the planner's AI is told to do. An empty box means the built-in \
                     instruction. Like every setting, a change is scheduled and needs three \
                     confirmations once the settings are protected.",
                );
                let instructions = &mut draft.config.instructions;
                for (label, own, built_in) in [
                    (
                        "Picking the task and its time",
                        &mut instructions.pick,
                        crate::advisor::PICK_PROMPT,
                    ),
                    (
                        "Reviewing a change request",
                        &mut instructions.change_review,
                        crate::advisor::REVIEW_PROMPT,
                    ),
                    (
                        "Reviewing a proposed to-do",
                        &mut instructions.proposal_review,
                        crate::advisor::PROPOSAL_PROMPT,
                    ),
                ] {
                    ui.strong(label);
                    ui.horizontal(|ui| {
                        if own.trim().is_empty() {
                            ui.small("built-in");
                            if ui
                                .small_button("Edit a copy of the built-in text")
                                .clicked()
                            {
                                *own = built_in.to_string();
                                draft.dirty = true;
                            }
                        } else if ui.small_button("Back to the built-in text").clicked() {
                            own.clear();
                            draft.dirty = true;
                        }
                    });
                    if !own.trim().is_empty() {
                        draft.dirty |= ui
                            .add(
                                egui::TextEdit::multiline(own)
                                    .desired_rows(6)
                                    .desired_width(f32::INFINITY),
                            )
                            .changed();
                    }
                }
            });
        ui.separator();

        // --- list -------------------------------------------------------------
        ui.heading("To-do list");
        let mut row_to_remove = None;
        for (row, (todo, deadline)) in draft
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
                        row_to_remove = Some(row);
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
        if let Some(row) = row_to_remove {
            draft.config.todos.remove(row);
            draft.deadlines.remove(row);
            draft.dirty = true;
        }
        // --- proposed by the user --------------------------------------------
        let proposed: Vec<&TodoItem> = state
            .proposed
            .iter()
            .filter(|todo| !state.is_done(&todo.id) && !draft.config.dismissed.contains(&todo.id))
            .collect();
        if !proposed.is_empty() {
            ui.label("Proposed by the user (approved by the AI against the life rules):");
        }
        for todo in proposed {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("• {} (priority {})", todo.title, todo.priority));
                if let Some(deadline) = todo.deadline {
                    ui.label(format!("due {deadline}"));
                }
                if ui.small_button("remove").clicked() {
                    draft.config.dismissed.insert(todo.id.clone());
                    draft.dirty = true;
                }
            });
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
                *message = match self.config.update(|stored| *stored = config) {
                    Ok(()) => {
                        draft.dirty = false;
                        "Saved. The agent picks it up within a few seconds.".into()
                    }
                    Err(error) => format!("save failed: {error}"),
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
            Some(assignment) => {
                let until = assignment.until.map_or(String::new(), |until| {
                    format!(
                        ", until {}{}",
                        until.with_timezone(&Local).format("%H:%M"),
                        match assignment.when_time_is_up {
                            WhenTimeIsUp::Reassign => "",
                            WhenTimeIsUp::NotifyOnly => " (then only a notice)",
                        }
                    )
                });
                ui.label(format!(
                    "{}  (since {}{until})",
                    assignment.task,
                    assignment.at.with_timezone(&Local).format("%m-%d %H:%M")
                ));
                ui.small(&assignment.why);
            }
            None => {
                ui.label("None.");
            }
        }
        ui.collapsing("Proposals", |ui| {
            if state.proposals.is_empty() {
                ui.label("None yet.");
            }
            for proposal in state.proposals.iter().rev() {
                let (color, verdict) = if proposal.approved {
                    (egui::Color32::from_rgb(80, 170, 90), "approved")
                } else {
                    (egui::Color32::from_rgb(200, 80, 80), "rejected")
                };
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        proposal
                            .at
                            .with_timezone(&Local)
                            .format("%m-%d %H:%M")
                            .to_string(),
                    );
                    ui.colored_label(color, verdict);
                    ui.label(format!(
                        "\"{}\" via {} → {}",
                        proposal.item.title, proposal.via, proposal.reply
                    ));
                });
            }
        });
        ui.heading("Change requests");
        egui::ScrollArea::vertical()
            .id_salt("planner_requests")
            .max_height(220.0)
            .show(ui, |ui| {
                if state.requests.is_empty() {
                    ui.label("None yet.");
                }
                for change in state.requests.iter().rev() {
                    let (color, verdict) = if change.approved {
                        (egui::Color32::from_rgb(80, 170, 90), "approved")
                    } else {
                        (egui::Color32::from_rgb(200, 80, 80), "rejected")
                    };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            change
                                .at
                                .with_timezone(&Local)
                                .format("%m-%d %H:%M")
                                .to_string(),
                        );
                        ui.colored_label(color, verdict);
                        ui.label(format!(
                            "{} — \"{}\" → {}",
                            request_label(&change.request),
                            change.reason,
                            change.reply
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
        ChangeKind::SetWhenTimeIsUp {
            action: WhenTimeIsUp::NotifyOnly,
        } => "only notify when time is up".into(),
        ChangeKind::SetWhenTimeIsUp {
            action: WhenTimeIsUp::Reassign,
        } => "new task when time is up".into(),
    }
}

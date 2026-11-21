//! Responsibility: Draw the to-do list as an outline: groups ("묶음") with the to-dos inside
//! them, what each to-do waits for and what it lets start, and moving a to-do or group by
//! dragging it (sent to the planner as a `move` change).

use super::{minutes_text, YELLOW};
use crate::{FocusApp, GREEN, RED};
use chrono::Local;
use eframe::egui;
use serde_json::json;
use std::sync::atomic::Ordering;
use task_planner::outline::{Outline, OutlineRow};
use task_planner::{PlannerStatus, TodoItem};

pub(super) const BLUE: egui::Color32 = egui::Color32::from_rgb(110, 160, 230);
pub(super) const GREY: egui::Color32 = egui::Color32::from_gray(140);
/// How far each level of groups is indented.
const INDENT: f32 = 22.0;

/// Responsibility: Carry the id of the to-do or group being dragged (in the outline or the
/// graph).
pub(super) struct DraggedTodo(pub(super) String);

/// Responsibility: Say where a dragged to-do or group would land.
#[derive(Clone, PartialEq)]
enum DropTarget {
    /// Just before this to-do or group, in its group.
    Before(String),
    /// Just after this to-do, in its group.
    After(String),
    /// At the end of this group.
    Into(String),
    /// At the end of the top level.
    TopEnd,
}

/// "쇼핑 › 붓 사기": a to-do or group with the groups it is in.
pub(super) fn place_name(outline: &Outline, todo: &TodoItem) -> String {
    let path = outline.path_text(&todo.id);
    if path.is_empty() {
        todo.title.clone()
    } else {
        format!("{path} › {}", todo.title)
    }
}

/// "D-3", "오늘 마감", "2일 지남", and the colour to show it in; nothing without a deadline.
pub(super) fn due_label(
    outline: &Outline,
    todo: &TodoItem,
) -> Option<(String, egui::Color32, String)> {
    let due = outline.due(&todo.id)?;
    let days = (due.date - Local::now().date_naive()).num_days();
    let text = match days {
        days if days < 0 => format!("{}일 지남", -days),
        0 => "오늘 마감".into(),
        1 => "내일 마감".into(),
        days => format!("D-{days}"),
    };
    let color = if days <= 0 { RED } else { YELLOW };
    // Where the date comes from, when it is not the to-do's own.
    let source = match due.from {
        None => format!("마감 {}", due.date),
        Some(source)
            if outline
                .path(&todo.id)
                .iter()
                .any(|group| group.id == source.id) =>
        {
            format!("마감 {} (묶음 \"{}\"의 마감)", due.date, source.title)
        }
        Some(source) => format!(
            "마감 {} (\"{}\"이(가) 이 일을 기다리고, 그 일의 마감이 먼저 옵니다)",
            due.date, source.title
        ),
    };
    Some((text, color, source))
}

impl FocusApp {
    /// The outline: every group and to-do in order, indented by depth, with a handle to drag
    /// it elsewhere. Moving within a group applies at once; moving into another group goes to
    /// the AI in strict mode (no reason needed; one is asked for only if it says no).
    pub(super) fn outline_ui(&mut self, ui: &mut egui::Ui, planner: &PlannerStatus) {
        let idle = !self.pending.load(Ordering::SeqCst);
        let outline = Outline::new(&planner.open_todos);
        let current = planner
            .assignment
            .as_ref()
            .and_then(|assignment| assignment.todo_id.clone());
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(GREEN, "▶ 지금 할 수 있음");
            ui.colored_label(YELLOW, "🔒 먼저 끝낼 일을 기다림");
            ui.small("· 묶음은 AI가 고르지 않고, 묶음 안의 할 일만 고릅니다.");
        });
        ui.horizontal(|ui| {
            if ui.small_button("모두 펼치기").clicked() {
                self.todo_ui.collapsed.clear();
            }
            if ui.small_button("모두 접기").clicked() {
                self.todo_ui.collapsed = planner
                    .open_todos
                    .iter()
                    .filter(|todo| todo.group)
                    .map(|todo| todo.id.clone())
                    .collect();
            }
            ui.checkbox(&mut self.todo_ui.ready_only, "지금 할 수 있는 일만");
        });
        let dragged: Option<String> =
            egui::DragAndDrop::payload::<DraggedTodo>(ui.ctx()).map(|payload| payload.0.clone());
        let pointer = ui.input(|input| input.pointer.latest_pos());
        let mut target: Option<DropTarget> = None;
        let mut hidden_below: Option<usize> = None;
        for row in outline.rows() {
            let todo = row.todo;
            if let Some(depth) = hidden_below {
                if row.depth > depth {
                    continue;
                }
                hidden_below = None;
            }
            if self.todo_ui.ready_only && !shown_when_ready_only(&outline, todo) {
                continue;
            }
            let row_rect =
                self.outline_row_ui(ui, &outline, row, planner, current.as_deref(), idle);
            if todo.group && self.todo_ui.collapsed.contains(&todo.id) {
                hidden_below = Some(row.depth);
            }
            // Where the dragged one would land if dropped on this row.
            if let (Some(dragged_id), Some(pointer)) = (&dragged, pointer) {
                if row_rect.y_range().contains(pointer.y) && dragged_id != &todo.id {
                    let fraction = (pointer.y - row_rect.top()) / row_rect.height().max(1.0);
                    let candidate = if todo.group {
                        if fraction < 0.3 {
                            DropTarget::Before(todo.id.clone())
                        } else {
                            DropTarget::Into(todo.id.clone())
                        }
                    } else if fraction < 0.5 {
                        DropTarget::Before(todo.id.clone())
                    } else {
                        DropTarget::After(todo.id.clone())
                    };
                    if move_for(&outline, dragged_id, &candidate).is_some() {
                        paint_drop_marker(ui, row_rect, row.depth, &candidate);
                        target = Some(candidate);
                    }
                }
            }
            if !todo.group && self.todo_ui.expanded.as_deref() == Some(todo.id.as_str()) {
                ui.horizontal(|ui| {
                    ui.add_space(row.depth as f32 * INDENT + 14.0);
                    ui.vertical(|ui| self.todo_details_ui(ui, todo, planner));
                });
            }
        }
        // While dragging: a place at the very end, outside every group.
        if let Some(dragged_id) = &dragged {
            let response = ui.add(
                egui::Label::new(
                    egui::RichText::new("— 여기에 놓으면 맨 위 단계의 끝으로 옮깁니다 —")
                        .color(GREY),
                )
                .sense(egui::Sense::hover()),
            );
            if pointer.is_some_and(|pointer| {
                pointer.y >= response.rect.top() - 4.0 && pointer.y <= response.rect.bottom() + 8.0
            }) && move_for(&outline, dragged_id, &DropTarget::TopEnd).is_some()
            {
                ui.painter().rect_stroke(
                    response.rect.expand(2.0),
                    3.0,
                    egui::Stroke::new(2.0_f32, BLUE),
                    egui::StrokeKind::Outside,
                );
                target = Some(DropTarget::TopEnd);
            }
            // The dragged title follows the pointer.
            if let (Some(pointer), Some(todo)) = (pointer, outline.get(dragged_id)) {
                paint_at_pointer(ui, pointer, &todo.title, BLUE);
            }
        }
        if let (Some(dragged_id), Some(drop)) = (&dragged, &target) {
            if ui.input(|input| input.pointer.any_released()) {
                egui::DragAndDrop::clear_payload(ui.ctx());
                if let Some((parent, before)) = move_for(&outline, dragged_id, drop) {
                    self.send(
                        "todo_change",
                        json!({
                            "change": {
                                "kind": "move",
                                "todo_id": dragged_id,
                                "parent": parent,
                                "before": before,
                            },
                            "reason": "",
                        }),
                    );
                }
            }
        }
    }

    /// One line of the outline (and, for a to-do, the lines about what it waits for and what
    /// it lets start); returns the rect of the main line, where a drag can be dropped.
    fn outline_row_ui(
        &mut self,
        ui: &mut egui::Ui,
        outline: &Outline,
        row: OutlineRow,
        planner: &PlannerStatus,
        current: Option<&str>,
        idle: bool,
    ) -> egui::Rect {
        let todo = row.todo;
        let indent = row.depth as f32 * INDENT;
        let line = ui.horizontal(|ui| {
            ui.add_space(indent);
            drag_handle_ui(ui, todo, idle);
            if todo.group {
                self.group_line_ui(ui, outline, todo, idle);
            } else {
                self.todo_line_ui(ui, outline, todo, planner, current);
            }
        });
        if !todo.group {
            let blockers = outline.blockers(&todo.id);
            if !blockers.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent + 40.0);
                    let names: Vec<String> = blockers
                        .iter()
                        .map(|blocker| place_name(outline, blocker))
                        .collect();
                    ui.colored_label(YELLOW, format!("↳ 먼저 끝낼 일: {}", names.join(", ")));
                });
            }
            let waiting: Vec<String> = outline
                .waiting_for(&todo.id)
                .iter()
                .map(|waiter| place_name(outline, waiter))
                .collect();
            if !waiting.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.add_space(indent + 40.0);
                    ui.colored_label(BLUE, format!("↳ 이 일이 끝나야: {}", waiting.join(", ")));
                });
            }
        }
        line.response.rect
    }

    /// A group's line: fold it, its name, how many to-dos it holds, its deadline, what it
    /// waits for, and its buttons.
    fn group_line_ui(&mut self, ui: &mut egui::Ui, outline: &Outline, todo: &TodoItem, idle: bool) {
        let collapsed = self.todo_ui.collapsed.contains(&todo.id);
        if ui.small_button(if collapsed { "▶" } else { "▼" }).clicked() {
            if collapsed {
                self.todo_ui.collapsed.remove(&todo.id);
            } else {
                self.todo_ui.collapsed.insert(todo.id.clone());
            }
        }
        ui.label(egui::RichText::new(&todo.title).strong().size(15.0));
        let leaves = outline.leaves_under(&todo.id);
        ui.small(format!("할 일 {}개", leaves.len()));
        if let Some((text, color, source)) = due_label(outline, todo) {
            ui.colored_label(color, text).on_hover_text(source);
        }
        let blockers = outline.blockers(&todo.id);
        if !blockers.is_empty() {
            let names: Vec<String> = blockers
                .iter()
                .map(|blocker| place_name(outline, blocker))
                .collect();
            ui.colored_label(YELLOW, format!("🔒 먼저: {}", names.join(", ")));
        }
        if ui
            .small_button("+ 할 일")
            .on_hover_text("이 묶음 안에 할 일을 추가합니다")
            .clicked()
        {
            self.open_add_form_in(Some(todo.id.clone()), false);
        }
        let empty = outline.children(Some(&todo.id)).is_empty();
        ui.menu_button("…", |ui| {
            if ui.button("+ 묶음 넣기").clicked() {
                self.open_add_form_in(Some(todo.id.clone()), true);
                ui.close_menu();
            }
            if ui.button("고치기").clicked() {
                self.open_edit_form(todo);
                ui.close_menu();
            }
            let remove = ui
                .add_enabled(idle && empty, egui::Button::new("지우기"))
                .on_disabled_hover_text("안의 할 일을 먼저 옮기거나 지우세요");
            if remove.clicked() {
                self.send(
                    "todo_change",
                    json!({
                        "change": { "kind": "remove", "todo_id": todo.id },
                        "reason": "",
                    }),
                );
                ui.close_menu();
            }
        })
        .response
        .on_hover_text("묶음 넣기, 고치기, 지우기");
    }

    /// A to-do's line: open its details, whether it is the current task or waits, its
    /// deadline (its own, its group's, or one that comes from what waits for it), priority,
    /// time spent against the estimate, and its title.
    fn todo_line_ui(
        &mut self,
        ui: &mut egui::Ui,
        outline: &Outline,
        todo: &TodoItem,
        planner: &PlannerStatus,
        current: Option<&str>,
    ) {
        let expanded = self.todo_ui.expanded.as_deref() == Some(todo.id.as_str());
        if ui.small_button(if expanded { "▼" } else { "▶" }).clicked() {
            self.todo_ui.expanded = (!expanded).then(|| todo.id.clone());
        }
        let waiting = !outline.blockers(&todo.id).is_empty();
        if current == Some(todo.id.as_str()) {
            ui.colored_label(GREEN, "지금");
        } else if waiting {
            ui.colored_label(YELLOW, "🔒");
        }
        if let Some((text, color, source)) = due_label(outline, todo) {
            ui.colored_label(color, text).on_hover_text(source);
        }
        ui.label(format!("P{}", todo.priority));
        // Time spent (since the last finish, for a routine) against the estimate.
        let spent = planner
            .minutes_this_round
            .get(&todo.id)
            .copied()
            .unwrap_or(0);
        ui.small(match todo.estimate_minutes {
            Some(minutes) => format!("{} / {}", minutes_text(spent), minutes_text(minutes as i64)),
            None => format!("{} / 예상 없음", minutes_text(spent)),
        });
        if waiting {
            ui.label(egui::RichText::new(&todo.title).color(GREY));
        } else {
            ui.strong(&todo.title);
        }
        if todo.repeat {
            ui.small("(반복)");
        }
        if todo.proposed {
            ui.small("(내가 추가)");
        }
    }
}

/// `text` in a small framed label just below and right of `pointer`, above everything else:
/// what is being dragged, or what dropping it would do.
pub(super) fn paint_at_pointer(
    ui: &egui::Ui,
    pointer: egui::Pos2,
    text: &str,
    frame: egui::Color32,
) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("dragged_todo"),
    ));
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(14.0),
        ui.visuals().strong_text_color(),
    );
    let size = galley.size() + egui::vec2(12.0, 6.0);
    // Right of the pointer, or left of it when that would run off the window.
    let screen = ui.ctx().screen_rect();
    let mut left = pointer.x + 14.0;
    if left + size.x > screen.right() - 4.0 {
        left = (pointer.x - 14.0 - size.x).max(screen.left() + 4.0);
    }
    let rect = egui::Rect::from_min_size(egui::pos2(left, pointer.y + 4.0), size);
    painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0_f32, frame),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        rect.min + egui::vec2(6.0, 3.0),
        galley,
        egui::Color32::WHITE,
    );
}

/// The handle a to-do or group is dragged by: three short lines.
fn drag_handle_ui(ui: &mut egui::Ui, todo: &TodoItem, idle: bool) {
    let sense = if idle {
        egui::Sense::drag()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), sense);
    let color = if response.hovered() || response.dragged() {
        ui.visuals().strong_text_color()
    } else {
        GREY
    };
    for line in 0..3 {
        let height = rect.center().y - 4.0 + line as f32 * 4.0;
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + 2.0, height),
                egui::pos2(rect.right() - 2.0, height),
            ],
            egui::Stroke::new(1.5_f32, color),
        );
    }
    response.dnd_set_drag_payload(DraggedTodo(todo.id.clone()));
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    response.on_hover_text("끌어서 다른 자리나 다른 묶음으로 옮깁니다");
}

/// With "지금 할 수 있는 일만": a to-do that can start now, or a group holding one.
fn shown_when_ready_only(outline: &Outline, todo: &TodoItem) -> bool {
    if todo.group {
        outline
            .leaves_under(&todo.id)
            .iter()
            .any(|leaf| outline.is_ready(&leaf.id))
    } else {
        outline.is_ready(&todo.id)
    }
}

/// The group and the to-do to go before that dropping `dragged_id` at `target` means, or
/// `None` when it would change nothing or put a group inside itself.
fn move_for(
    outline: &Outline,
    dragged_id: &str,
    target: &DropTarget,
) -> Option<(Option<String>, Option<String>)> {
    let parent_of = |todo_id: &str| outline.parent(todo_id).map(|group| group.id.clone());
    let (parent, before) = match target {
        DropTarget::Before(todo_id) => (parent_of(todo_id), Some(todo_id.clone())),
        DropTarget::After(todo_id) => {
            let parent = parent_of(todo_id);
            let siblings = outline.children(parent.as_deref());
            let next = siblings
                .iter()
                .skip_while(|sibling| sibling.id != *todo_id)
                .skip(1)
                .find(|sibling| sibling.id != dragged_id)
                .map(|sibling| sibling.id.clone());
            (parent, next)
        }
        DropTarget::Into(group_id) => (Some(group_id.clone()), None),
        DropTarget::TopEnd => (None, None),
    };
    if let Some(parent_id) = &parent {
        if outline.is_inside(parent_id, dragged_id) {
            return None;
        }
    }
    if before.as_deref() == Some(dragged_id) {
        return None;
    }
    // Nothing changes when it lands where it already is.
    if parent_of(dragged_id) == parent {
        let siblings = outline.children(parent.as_deref());
        let next_now = siblings
            .iter()
            .skip_while(|sibling| sibling.id != dragged_id)
            .nth(1)
            .map(|sibling| sibling.id.clone());
        if next_now == before {
            return None;
        }
    }
    Some((parent, before))
}

/// A line where it would go (before or after a row), or a frame around the group it would go
/// into.
fn paint_drop_marker(ui: &egui::Ui, row_rect: egui::Rect, depth: usize, target: &DropTarget) {
    let left = row_rect.left() + depth as f32 * INDENT;
    let stroke = egui::Stroke::new(2.5_f32, BLUE);
    match target {
        DropTarget::Before(_) => {
            ui.painter()
                .hline(left..=row_rect.right(), row_rect.top() - 1.0, stroke);
        }
        DropTarget::After(_) => {
            ui.painter()
                .hline(left..=row_rect.right(), row_rect.bottom() + 1.0, stroke);
        }
        DropTarget::Into(_) | DropTarget::TopEnd => {
            ui.painter().rect_stroke(
                egui::Rect::from_min_max(egui::pos2(left, row_rect.top()), row_rect.max)
                    .expand(1.0),
                3.0,
                stroke,
                egui::StrokeKind::Outside,
            );
        }
    }
}

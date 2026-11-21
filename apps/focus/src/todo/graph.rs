//! Responsibility: Draw the to-do list as a graph: groups as areas, the to-dos inside them as
//! boxes, and an arrow from each to-do or group to what waits for it. A box dropped on
//! another box waits for it ("먼저 끝낼 일"); dropped on an area, it moves into that group. A
//! click on an arrow takes that order away; a click on a box opens it where it is.

use super::outline_list::{due_label, paint_at_pointer, place_name, DraggedTodo, BLUE, GREY};
use super::{minutes_text, YELLOW};
use crate::{FocusApp, GREEN, RED};
use eframe::egui;
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use task_planner::outline::Outline;
use task_planner::{PlannerStatus, TodoItem};

const BOX_WIDTH: f32 = 170.0;
const BOX_HEIGHT: f32 = 44.0;
/// Room between boxes side by side, for the arrows.
const GAP_ACROSS: f32 = 34.0;
const GAP_DOWN: f32 = 22.0;
/// Room inside an area around what it holds, and for its name at the top.
const PADDING: f32 = 12.0;
const HEADER: f32 = 26.0;
/// How tall an opened box is before it has been drawn once.
const OPENED_MIN_HEIGHT: f32 = 160.0;
/// How near an arrow the pointer must be to pick it.
const ARROW_REACH: f32 = 6.0;

/// Responsibility: Say what dropping a box in the graph would do.
#[derive(Clone, PartialEq)]
enum GraphDrop {
    /// The dragged to-do waits for the to-do of the box it is dropped on.
    WaitFor(String),
    /// The dragged to-do moves into this group (at its end).
    Into(String),
}

/// Responsibility: Hold where each box and area goes, relative to the top left of the graph.
#[derive(Default)]
struct Layout {
    boxes: BTreeMap<String, egui::Rect>,
    /// Each group's area, with how deep it is.
    areas: Vec<(String, egui::Rect, usize)>,
}

impl FocusApp {
    pub(super) fn graph_ui(&mut self, ui: &mut egui::Ui, planner: &PlannerStatus) {
        let idle = !self.pending.load(Ordering::SeqCst);
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(GREEN, "■ 지금 할 수 있음");
            ui.colored_label(YELLOW, "■ 먼저 끝낼 일을 기다림");
            ui.small("· 구역 = 묶음, 화살표 = 먼저 끝낼 일 → 기다리는 일");
        });
        ui.small(
            "상자를 다른 상자에 놓으면 놓은 곳의 일이 먼저 끝낼 일이 되고, 구역에 놓으면 그 묶음으로 \
             옮깁니다. 화살표를 누르면 그 순서를 지웁니다. 상자를 누르면 그 자리에서 펼쳐집니다.",
        );
        let outline = Outline::new(&planner.open_todos);
        let opened = self
            .todo_ui
            .expanded
            .clone()
            .filter(|todo_id| outline.get(todo_id).is_some_and(|todo| !todo.group));
        let opened_height = self.todo_ui.opened_box_height.max(OPENED_MIN_HEIGHT);
        let width = ui.available_width().max(BOX_WIDTH + 2.0 * PADDING);
        let mut layout = Layout::default();
        let height = lay_out(
            &outline,
            None,
            egui::pos2(0.0, 0.0),
            width,
            0,
            &mut layout,
            (opened.as_deref(), opened_height),
        );
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let offset = rect.min.to_vec2();
        let painter = ui.painter_at(rect.expand(4.0));
        let current = planner
            .assignment
            .as_ref()
            .and_then(|assignment| assignment.todo_id.as_deref());
        let dragged: Option<String> =
            egui::DragAndDrop::payload::<DraggedTodo>(ui.ctx()).map(|payload| payload.0.clone());
        let pointer = ui
            .input(|input| input.pointer.latest_pos())
            .filter(|pointer| rect.contains(*pointer));

        for (group_id, area, depth) in &layout.areas {
            let area = area.translate(offset);
            let Some(group) = outline.get(group_id) else {
                continue;
            };
            let shade = 28 + 18 * (*depth as u8).min(4);
            painter.rect_filled(
                area,
                6.0,
                egui::Color32::from_rgba_unmultiplied(110, 140, 190, shade),
            );
            let mut header = group.title.clone();
            if let Some((text, _, _)) = due_label(&outline, group) {
                header.push_str(&format!("  ·  {text}"));
            }
            let blockers = outline.blockers(&group.id);
            if !blockers.is_empty() {
                header.push_str("  ·  🔒");
            }
            painter.text(
                area.min + egui::vec2(PADDING, 6.0),
                egui::Align2::LEFT_TOP,
                header,
                egui::FontId::proportional(14.0),
                if blockers.is_empty() {
                    ui.visuals().strong_text_color()
                } else {
                    YELLOW
                },
            );
        }

        // Arrows from what comes first to what waits for it.
        let place = |todo_id: &str| {
            layout.boxes.get(todo_id).copied().or_else(|| {
                layout
                    .areas
                    .iter()
                    .find(|(group_id, _, _)| group_id == todo_id)
                    .map(|(_, area, _)| *area)
            })
        };
        let on_a_box = pointer.is_some_and(|pointer| {
            layout
                .boxes
                .values()
                .any(|place_rect| place_rect.translate(offset).contains(pointer))
        });
        let mut picked_arrow: Option<(String, String)> = None;
        for waiter in &planner.open_todos {
            let Some(to) = place(&waiter.id) else {
                continue;
            };
            for waited_id in &waiter.after {
                let Some(from) = place(waited_id) else {
                    continue;
                };
                let (from, to) = (from.translate(offset), to.translate(offset));
                let start = edge_point(from, to.center());
                let end = edge_point(to, from.center());
                let pointed = dragged.is_none()
                    && !on_a_box
                    && picked_arrow.is_none()
                    && pointer.is_some_and(|pointer| {
                        distance_to_segment(pointer, start, end) <= ARROW_REACH
                    });
                let color = if pointed {
                    ui.visuals().strong_text_color()
                } else if outline.is_finished(waited_id) {
                    GREY
                } else {
                    YELLOW
                };
                draw_arrow(&painter, start, end, color);
                if pointed {
                    picked_arrow = Some((waiter.id.clone(), waited_id.clone()));
                }
            }
        }
        if let (Some(_), Some(pointer)) = (&picked_arrow, pointer) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            paint_at_pointer(ui, pointer, "누르면 이 순서를 지웁니다", GREY);
            if idle && ui.input(|input| input.pointer.primary_clicked()) {
                self.todo_ui.removing_order = picked_arrow.clone();
            }
        }

        for (todo_id, place_rect) in &layout.boxes {
            if opened.as_deref() == Some(todo_id.as_str()) {
                continue;
            }
            let Some(todo) = outline.get(todo_id) else {
                continue;
            };
            let place_rect = place_rect.translate(offset);
            let sense = if idle {
                egui::Sense::click_and_drag()
            } else {
                egui::Sense::click()
            };
            let mut response =
                ui.interact(place_rect, egui::Id::new(("graph_box", todo_id)), sense);
            if idle {
                response.dnd_set_drag_payload(DraggedTodo(todo_id.clone()));
            }
            if dragged.is_none() {
                response = response.on_hover_text(hover_text(&outline, todo));
            }
            self.paint_box(
                &painter,
                &outline,
                todo,
                place_rect,
                current == Some(todo_id.as_str()),
                response.hovered() && dragged.is_none(),
                planner,
            );
            if response.clicked() {
                self.todo_ui.expanded = Some(todo.id.clone());
            }
        }
        if let Some(todo) = opened.as_deref().and_then(|todo_id| outline.get(todo_id)) {
            if let Some(place_rect) = layout.boxes.get(&todo.id) {
                self.opened_box_ui(
                    ui,
                    &outline,
                    todo,
                    place_rect.translate(offset),
                    current == Some(todo.id.as_str()),
                    planner,
                );
            }
        }

        // Where a dragged box would land, and what that would do.
        if let Some(dragged_id) = &dragged {
            let mut action: Option<(GraphDrop, egui::Rect)> = None;
            let mut refusal: Option<(String, egui::Rect)> = None;
            if let Some(pointer) = pointer {
                let on_box = layout.boxes.iter().find(|(todo_id, place_rect)| {
                    *todo_id != dragged_id && place_rect.translate(offset).contains(pointer)
                });
                if let Some((target_id, place_rect)) = on_box {
                    let place_rect = place_rect.translate(offset);
                    match why_not_wait(&outline, dragged_id, target_id) {
                        None => action = Some((GraphDrop::WaitFor(target_id.clone()), place_rect)),
                        Some(reason) => refusal = Some((reason, place_rect)),
                    }
                } else if let Some((group_id, area, _)) = layout
                    .areas
                    .iter()
                    .filter(|(_, area, _)| area.translate(offset).contains(pointer))
                    .max_by_key(|(_, _, depth)| *depth)
                {
                    let already_there = outline
                        .parent(dragged_id)
                        .is_some_and(|group| &group.id == group_id);
                    if !already_there {
                        action = Some((GraphDrop::Into(group_id.clone()), area.translate(offset)));
                    }
                }
                let title = outline
                    .get(dragged_id)
                    .map(|todo| todo.title.clone())
                    .unwrap_or_default();
                match (&action, &refusal) {
                    (Some((GraphDrop::WaitFor(target_id), target)), _) => {
                        painter.rect_stroke(
                            *target,
                            5.0,
                            egui::Stroke::new(3.0_f32, BLUE),
                            egui::StrokeKind::Outside,
                        );
                        let target_title = outline
                            .get(target_id)
                            .map(|todo| todo.title.clone())
                            .unwrap_or_default();
                        paint_at_pointer(
                            ui,
                            pointer,
                            &format!("\"{title}\" 전에 \"{target_title}\" 먼저"),
                            BLUE,
                        );
                    }
                    (Some((GraphDrop::Into(group_id), target)), _) => {
                        painter.rect_stroke(
                            *target,
                            6.0,
                            egui::Stroke::new(2.5_f32, BLUE),
                            egui::StrokeKind::Inside,
                        );
                        let group_title = outline
                            .get(group_id)
                            .map(|group| group.title.clone())
                            .unwrap_or_default();
                        paint_at_pointer(
                            ui,
                            pointer,
                            &format!("묶음 \"{group_title}\"(으)로 옮기기"),
                            BLUE,
                        );
                    }
                    (None, Some((reason, target))) => {
                        painter.rect_stroke(
                            *target,
                            5.0,
                            egui::Stroke::new(3.0_f32, RED),
                            egui::StrokeKind::Outside,
                        );
                        paint_at_pointer(ui, pointer, reason, RED);
                    }
                    (None, None) => paint_at_pointer(ui, pointer, &title, GREY),
                }
            }
            if ui.input(|input| input.pointer.any_released()) {
                egui::DragAndDrop::clear_payload(ui.ctx());
                match action {
                    Some((GraphDrop::WaitFor(target_id), _)) => {
                        let mut after = outline
                            .get(dragged_id)
                            .map(|todo| todo.after.clone())
                            .unwrap_or_default();
                        after.push(target_id);
                        self.send(
                            "todo_change",
                            json!({
                                "change": { "kind": "set_after", "todo_id": dragged_id, "after": after },
                                "reason": "",
                            }),
                        );
                    }
                    Some((GraphDrop::Into(group_id), _)) => {
                        self.send(
                            "todo_change",
                            json!({
                                "change": {
                                    "kind": "move",
                                    "todo_id": dragged_id,
                                    "parent": group_id,
                                    "before": null,
                                },
                                "reason": "",
                            }),
                        );
                    }
                    None => {}
                }
            }
        }
        self.remove_order_ui(ui, &outline);
        if planner.open_todos.iter().any(|todo| !todo.after.is_empty()) {
            ui.small("노란 화살표는 아직 끝나지 않은 먼저 끝낼 일입니다.");
        }
    }

    /// A box opened in place: its title and place, what it waits for and what waits for it,
    /// and the same details and buttons as in the list. Its height is kept for the next
    /// layout.
    fn opened_box_ui(
        &mut self,
        ui: &mut egui::Ui,
        outline: &Outline,
        todo: &TodoItem,
        place_rect: egui::Rect,
        is_current: bool,
        planner: &PlannerStatus,
    ) {
        let painter = ui.painter_at(place_rect.expand(4.0));
        // Lighter than the closed boxes, so the text fields inside stand out.
        painter.rect_filled(place_rect, 6.0, ui.visuals().window_fill);
        painter.rect_stroke(
            place_rect,
            6.0,
            egui::Stroke::new(2.0_f32, BLUE),
            egui::StrokeKind::Inside,
        );
        let mut inner = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(place_rect.shrink(10.0))
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        let mut close = false;
        inner.horizontal_wrapped(|ui| {
            if is_current {
                ui.colored_label(GREEN, "지금");
            } else if !outline.blockers(&todo.id).is_empty() {
                ui.colored_label(YELLOW, "🔒");
            }
            ui.label(egui::RichText::new(&todo.title).strong().size(15.0));
            if let Some((text, color, source)) = due_label(outline, todo) {
                ui.colored_label(color, text).on_hover_text(source);
            }
            if ui.small_button("접기").clicked() {
                close = true;
            }
        });
        let path = outline.path_text(&todo.id);
        if !path.is_empty() {
            inner.colored_label(BLUE, path);
        }
        let blockers: Vec<String> = outline
            .blockers(&todo.id)
            .iter()
            .map(|blocker| place_name(outline, blocker))
            .collect();
        if !blockers.is_empty() {
            inner.colored_label(YELLOW, format!("먼저 끝낼 일: {}", blockers.join(", ")));
        }
        let waiting: Vec<String> = outline
            .waiting_for(&todo.id)
            .iter()
            .map(|waiter| place_name(outline, waiter))
            .collect();
        if !waiting.is_empty() {
            inner.colored_label(BLUE, format!("이 일이 끝나야: {}", waiting.join(", ")));
        }
        self.todo_details_ui(&mut inner, todo, planner);
        let used = inner.min_rect().height() + 20.0;
        if (used - self.todo_ui.opened_box_height).abs() > 1.0 {
            self.todo_ui.opened_box_height = used;
            ui.ctx().request_repaint();
        }
        if close {
            self.todo_ui.expanded = None;
        }
    }

    /// The window that asks before an arrow the user clicked is taken away.
    fn remove_order_ui(&mut self, ui: &mut egui::Ui, outline: &Outline) {
        let Some((waiter_id, waited_id)) = self.todo_ui.removing_order.clone() else {
            return;
        };
        let (Some(waiter), Some(waited)) = (outline.get(&waiter_id), outline.get(&waited_id))
        else {
            self.todo_ui.removing_order = None;
            return;
        };
        let idle = !self.pending.load(Ordering::SeqCst);
        let mut open = true;
        let mut close = false;
        egui::Window::new("순서 지우기")
            .id(egui::Id::new("remove_order_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "\"{}\"은(는) 더 이상 \"{}\"을(를) 기다리지 않습니다.",
                    place_name(outline, waiter),
                    place_name(outline, waited)
                ));
                if !self.todo_ui.without_review {
                    ui.small("엄격 모드라서 AI가 심사합니다. 거절하면 이유를 적어 다시 보낼 수 있습니다.");
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(idle, egui::Button::new("지우기")).clicked() {
                        let after: Vec<String> = waiter
                            .after
                            .iter()
                            .filter(|entry| **entry != waited_id)
                            .cloned()
                            .collect();
                        self.send(
                            "todo_change",
                            json!({
                                "change": { "kind": "set_after", "todo_id": waiter_id, "after": after },
                                "reason": "",
                            }),
                        );
                        close = true;
                    }
                    if ui.button("취소").clicked() {
                        close = true;
                    }
                });
            });
        if !open || close {
            self.todo_ui.removing_order = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_box(
        &self,
        painter: &egui::Painter,
        outline: &Outline,
        todo: &TodoItem,
        place: egui::Rect,
        is_current: bool,
        hovered: bool,
        planner: &PlannerStatus,
    ) {
        let waiting = !outline.blockers(&todo.id).is_empty();
        let color = if waiting { YELLOW } else { GREEN };
        let fill = if hovered {
            painter.ctx().style().visuals.widgets.hovered.bg_fill
        } else {
            painter.ctx().style().visuals.extreme_bg_color
        };
        painter.rect_filled(place, 5.0, fill);
        painter.rect_stroke(
            place,
            5.0,
            egui::Stroke::new(
                if is_current { 3.0_f32 } else { 1.5 },
                if is_current { BLUE } else { color },
            ),
            egui::StrokeKind::Inside,
        );
        let mut title = String::new();
        if is_current {
            title.push_str("지금 · ");
        } else if waiting {
            title.push_str("🔒 ");
        }
        title.push_str(&todo.title);
        let text_color = if waiting {
            GREY
        } else {
            painter.ctx().style().visuals.strong_text_color()
        };
        let galley = one_line_galley(painter, title, 14.0, text_color, place.width() - 12.0);
        painter.galley(place.min + egui::vec2(6.0, 5.0), galley, text_color);
        let mut details: Vec<String> = Vec::new();
        if let Some((text, _, _)) = due_label(outline, todo) {
            details.push(text);
        }
        details.push(format!("P{}", todo.priority));
        let spent = planner
            .minutes_this_round
            .get(&todo.id)
            .copied()
            .unwrap_or(0);
        if let Some(estimate) = todo.estimate_minutes {
            details.push(format!(
                "{} / {}",
                minutes_text(spent),
                minutes_text(estimate as i64)
            ));
        }
        if todo.repeat {
            details.push("반복".into());
        }
        let galley = one_line_galley(
            painter,
            details.join(" · "),
            11.0,
            GREY,
            place.width() - 12.0,
        );
        painter.galley(place.min + egui::vec2(6.0, 25.0), galley, GREY);
    }
}

/// Lay out what `parent` holds (`None`: the top level) from `origin`, `width` wide: its
/// to-dos as boxes in rows, those that come first to the left, then each group inside as an
/// area. The opened box (`opened`: its id and height) takes a line of its own, as wide as its
/// group. Returns the height used.
fn lay_out(
    outline: &Outline,
    parent: Option<&str>,
    origin: egui::Pos2,
    width: f32,
    depth: usize,
    layout: &mut Layout,
    opened: (Option<&str>, f32),
) -> f32 {
    let (opened_id, opened_height) = opened;
    let children = outline.children(parent);
    let mut leaves: Vec<&TodoItem> = children
        .iter()
        .copied()
        .filter(|todo| !todo.group)
        .collect();
    // A to-do that waits for another one here goes after it.
    let mut levels: BTreeMap<String, usize> = BTreeMap::new();
    for _ in 0..leaves.len() {
        for leaf in &leaves {
            let level = leaf
                .after
                .iter()
                .filter_map(|waited_id| levels.get(waited_id).map(|level| level + 1))
                .max()
                .unwrap_or(0);
            levels.insert(leaf.id.clone(), level);
        }
    }
    leaves.sort_by_key(|leaf| levels.get(&leaf.id).copied().unwrap_or(0));
    let per_row = (((width + GAP_ACROSS) / (BOX_WIDTH + GAP_ACROSS)).floor() as usize).max(1);
    let (mut column, mut line_top, mut used) = (0usize, 0.0_f32, 0.0_f32);
    for leaf in &leaves {
        if opened_id == Some(leaf.id.as_str()) {
            if column > 0 {
                line_top += BOX_HEIGHT + GAP_DOWN;
                column = 0;
            }
            layout.boxes.insert(
                leaf.id.clone(),
                egui::Rect::from_min_size(
                    origin + egui::vec2(0.0, line_top),
                    egui::vec2(width, opened_height),
                ),
            );
            used = line_top + opened_height;
            line_top += opened_height + GAP_DOWN;
            continue;
        }
        layout.boxes.insert(
            leaf.id.clone(),
            egui::Rect::from_min_size(
                origin + egui::vec2(column as f32 * (BOX_WIDTH + GAP_ACROSS), line_top),
                egui::vec2(BOX_WIDTH, BOX_HEIGHT),
            ),
        );
        used = line_top + BOX_HEIGHT;
        column += 1;
        if column == per_row {
            column = 0;
            line_top += BOX_HEIGHT + GAP_DOWN;
        }
    }
    for group in children.iter().filter(|todo| todo.group) {
        if used > 0.0 {
            used += GAP_DOWN * 0.6;
        }
        let top = origin.y + used;
        let inner_origin = egui::pos2(origin.x + PADDING, top + HEADER);
        let inner_width = (width - 2.0 * PADDING).max(BOX_WIDTH);
        let inner = lay_out(
            outline,
            Some(&group.id),
            inner_origin,
            inner_width,
            depth + 1,
            layout,
            opened,
        );
        let height = HEADER + inner + PADDING;
        layout.areas.push((
            group.id.clone(),
            egui::Rect::from_min_size(egui::pos2(origin.x, top), egui::vec2(width, height)),
            depth,
        ));
        used += height;
    }
    used
}

/// Where the line from the middle of `place` towards `toward` leaves `place`.
fn edge_point(place: egui::Rect, toward: egui::Pos2) -> egui::Pos2 {
    let center = place.center();
    let direction = toward - center;
    if direction.length() < 1.0 {
        return center;
    }
    let across = if direction.x.abs() > 0.0 {
        place.width() / 2.0 / direction.x.abs()
    } else {
        f32::INFINITY
    };
    let down = if direction.y.abs() > 0.0 {
        place.height() / 2.0 / direction.y.abs()
    } else {
        f32::INFINITY
    };
    center + direction * across.min(down).min(1.0)
}

/// How far `point` is from the line from `start` to `end`.
fn distance_to_segment(point: egui::Pos2, start: egui::Pos2, end: egui::Pos2) -> f32 {
    let along = end - start;
    let length_squared = along.length_sq();
    if length_squared < 1.0 {
        return (point - start).length();
    }
    let fraction = ((point - start).dot(along) / length_squared).clamp(0.0, 1.0);
    (point - (start + along * fraction)).length()
}

/// Why `dragged_id` cannot wait for `target_id`, in words for the user; `None` when it can.
fn why_not_wait(outline: &Outline, dragged_id: &str, target_id: &str) -> Option<String> {
    let dragged = outline.get(dragged_id)?;
    if dragged.after.iter().any(|waited_id| waited_id == target_id) {
        return Some("이미 먼저 끝낼 일입니다".into());
    }
    let mut changed: Vec<TodoItem> = outline
        .rows()
        .into_iter()
        .map(|row| row.todo.clone())
        .collect();
    if let Some(entry) = changed.iter_mut().find(|entry| entry.id == dragged_id) {
        entry.after.push(target_id.to_string());
    }
    if Outline::new(&changed).waiting_loop().is_some() && outline.waiting_loop().is_none() {
        return Some("서로를 기다리게 되어 놓을 수 없습니다".into());
    }
    None
}

fn draw_arrow(painter: &egui::Painter, start: egui::Pos2, end: egui::Pos2, color: egui::Color32) {
    let stroke = egui::Stroke::new(2.0_f32, color);
    painter.line_segment([start, end], stroke);
    let direction = (end - start).normalized();
    if direction.length() < 0.5 {
        return;
    }
    let back = -direction * 10.0;
    let side = egui::vec2(-direction.y, direction.x) * 5.0;
    painter.line_segment([end, end + back + side], stroke);
    painter.line_segment([end, end + back - side], stroke);
}

/// `text` on one line, cut with "…" to `width`.
fn one_line_galley(
    painter: &egui::Painter,
    text: String,
    size: f32,
    color: egui::Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job =
        egui::text::LayoutJob::simple_singleline(text, egui::FontId::proportional(size), color);
    job.wrap = egui::text::TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// The whole name, the deadline and its source, and what it waits for, on hover.
fn hover_text(outline: &Outline, todo: &TodoItem) -> String {
    let mut lines = vec![place_name(outline, todo)];
    if let Some((_, _, source)) = due_label(outline, todo) {
        lines.push(source);
    }
    let blockers: Vec<String> = outline
        .blockers(&todo.id)
        .iter()
        .map(|blocker| place_name(outline, blocker))
        .collect();
    if !blockers.is_empty() {
        lines.push(format!("먼저 끝낼 일: {}", blockers.join(", ")));
    }
    lines.join("\n")
}

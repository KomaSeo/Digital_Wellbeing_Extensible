//! Responsibility: Draw the to-do list as a graph, only to look at: groups as areas, the
//! to-dos inside them as boxes, and an arrow from each to-do or group to what waits for it.
//! Clicking a box opens that to-do in the outline.

use super::outline_list::{due_label, place_name, BLUE, GREY};
use super::{minutes_text, YELLOW};
use crate::{FocusApp, GREEN};
use eframe::egui;
use std::collections::BTreeMap;
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

/// Responsibility: Hold where each box and area goes, relative to the top left of the graph.
#[derive(Default)]
struct Layout {
    boxes: BTreeMap<String, egui::Rect>,
    /// Each group's area, with how deep it is.
    areas: Vec<(String, egui::Rect, usize)>,
}

impl FocusApp {
    pub(super) fn graph_ui(&mut self, ui: &mut egui::Ui, planner: &PlannerStatus) {
        ui.horizontal_wrapped(|ui| {
            ui.colored_label(GREEN, "■ 지금 할 수 있음");
            ui.colored_label(YELLOW, "■ 먼저 끝낼 일을 기다림");
            ui.small("· 구역 = 묶음, 화살표 = 먼저 끝낼 일 → 기다리는 일. 상자를 누르면 목록에서 엽니다.");
        });
        let outline = Outline::new(&planner.open_todos);
        let width = ui.available_width().max(BOX_WIDTH + 2.0 * PADDING);
        let mut layout = Layout::default();
        let height = lay_out(&outline, None, egui::pos2(0.0, 0.0), width, 0, &mut layout);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        let offset = rect.min.to_vec2();
        let painter = ui.painter_at(rect.expand(4.0));
        let current = planner
            .assignment
            .as_ref()
            .and_then(|assignment| assignment.todo_id.as_deref());

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
        for waiter in &planner.open_todos {
            let Some(to) = place(&waiter.id) else {
                continue;
            };
            for waited_id in &waiter.after {
                let Some(from) = place(waited_id) else {
                    continue;
                };
                let color = if outline.is_finished(waited_id) {
                    GREY
                } else {
                    YELLOW
                };
                let (from, to) = (from.translate(offset), to.translate(offset));
                let start = edge_point(from, to.center());
                let end = edge_point(to, from.center());
                draw_arrow(&painter, start, end, color);
            }
        }

        for (todo_id, place_rect) in &layout.boxes {
            let Some(todo) = outline.get(todo_id) else {
                continue;
            };
            let place_rect = place_rect.translate(offset);
            let response = ui
                .interact(
                    place_rect,
                    egui::Id::new(("graph_box", todo_id)),
                    egui::Sense::click(),
                )
                .on_hover_text(hover_text(&outline, todo));
            self.paint_box(
                &painter,
                &outline,
                todo,
                place_rect,
                current == Some(todo_id.as_str()),
                response.hovered(),
                planner,
            );
            if response.clicked() {
                // Open it in the outline, with the groups it is in unfolded.
                self.todo_ui.graph_view = false;
                self.todo_ui.expanded = Some(todo.id.clone());
                for group in outline.path(&todo.id) {
                    self.todo_ui.collapsed.remove(&group.id);
                }
            }
        }
        if planner.open_todos.iter().any(|todo| !todo.after.is_empty()) {
            ui.small("노란 화살표는 아직 끝나지 않은 먼저 끝낼 일입니다.");
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
/// area. Returns the height used.
fn lay_out(
    outline: &Outline,
    parent: Option<&str>,
    origin: egui::Pos2,
    width: f32,
    depth: usize,
    layout: &mut Layout,
) -> f32 {
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
    for (index, leaf) in leaves.iter().enumerate() {
        let column = index % per_row;
        let line = index / per_row;
        let top_left = origin
            + egui::vec2(
                column as f32 * (BOX_WIDTH + GAP_ACROSS),
                line as f32 * (BOX_HEIGHT + GAP_DOWN),
            );
        layout.boxes.insert(
            leaf.id.clone(),
            egui::Rect::from_min_size(top_left, egui::vec2(BOX_WIDTH, BOX_HEIGHT)),
        );
    }
    let lines = leaves.len().div_ceil(per_row);
    let mut used = if lines == 0 {
        0.0
    } else {
        lines as f32 * BOX_HEIGHT + (lines - 1) as f32 * GAP_DOWN
    };
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

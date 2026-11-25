//! Responsibility: Split a to-do into to-dos at boundaries set between its steps ("할 일로
//! 나누기"; the owner: "할 일 나누기는 하나하나 클릭하지 말고 경계를 단위로 설정할 수 있게 해줘"):
//! the window, opened beside the steps of a to-do in the Work tab, the To-do tab and the
//! graph, and the `split` change it sends. The planner puts a split in force without the AI.

use super::outline_list::{BLUE, GREY};
use super::{minutes_text, YELLOW};
use crate::style::{self, Tone};
use crate::{FocusApp, GREEN, RED};
use eframe::egui::{self, RichText};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::atomic::Ordering;
use task_planner::{SplitPart, TaskStep, TodoItem};

/// Responsibility: Hold the split window's choices while it is open.
#[derive(Debug, Clone, Default)]
pub(super) struct SplitWindow {
    todo_id: String,
    todo_title: String,
    steps: Vec<TaskStep>,
    /// Cuts: `index` cuts between step `index` and the step after it (counted from 0).
    boundaries: BTreeSet<usize>,
    /// The titles typed for the parts of several steps, by the index of their first step.
    titles: BTreeMap<usize, String>,
    /// The first part stays the to-do itself; otherwise the to-do becomes a group.
    keep_first: bool,
    /// Each new to-do waits for the one before it.
    in_order: bool,
    /// Where it was opened, sent as `via`.
    via: &'static str,
}

/// Responsibility: Describe one part of the steps as the window shows it.
struct PartView {
    range: Range<usize>,
    /// It stays the to-do itself (the first part, when that is chosen).
    kept: bool,
    /// Every step of it is done: it becomes no to-do.
    finished: bool,
    /// The title its to-do gets; empty when a name is still missing.
    title: String,
    /// The minutes its steps not done yet add up to, when every one of them has minutes.
    minutes_left: Option<u32>,
}

impl SplitWindow {
    pub(super) fn new(todo: &TodoItem, steps: Vec<TaskStep>, via: &'static str) -> Self {
        Self {
            todo_id: todo.id.clone(),
            todo_title: todo.title.clone(),
            steps,
            via,
            ..Default::default()
        }
    }

    /// The steps of each part, in order.
    fn ranges(&self) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut start = 0;
        for boundary in &self.boundaries {
            if boundary + 1 < self.steps.len() {
                ranges.push(start..boundary + 1);
                start = boundary + 1;
            }
        }
        ranges.push(start..self.steps.len());
        ranges
    }

    fn parts(&self) -> Vec<PartView> {
        self.ranges()
            .into_iter()
            .enumerate()
            .map(|(number, range)| {
                let part = self.split_part(&range);
                PartView {
                    kept: number == 0 && self.keep_first,
                    finished: part.finished(),
                    title: if number == 0 && self.keep_first {
                        self.todo_title.clone()
                    } else {
                        part.todo_title()
                    },
                    minutes_left: part.minutes_left(),
                    range,
                }
            })
            .collect()
    }

    fn split_part(&self, range: &Range<usize>) -> SplitPart {
        SplitPart {
            title: self.titles.get(&range.start).cloned().unwrap_or_default(),
            steps: self.steps[range.clone()].to_vec(),
        }
    }

    /// What still keeps the split from being made, in the words the window shows.
    fn missing(&self, parts: &[PartView]) -> Option<String> {
        if parts.len() < 2 {
            return Some("단계 사이를 눌러 경계를 하나 이상 넣으세요.".into());
        }
        let made: Vec<(usize, &PartView)> = parts
            .iter()
            .enumerate()
            .filter(|(_, part)| !part.kept && !part.finished)
            .collect();
        if made.is_empty() {
            return Some("새로 만들 할 일이 없습니다. 나눈 부분의 단계를 모두 끝냈습니다.".into());
        }
        made.iter()
            .find(|(_, part)| part.title.chars().count() < 2)
            .map(|(number, _)| {
                format!(
                    "{}번 할 일의 이름을 적으면 나눌 수 있습니다(2글자 이상).",
                    number + 1
                )
            })
    }

    /// The `split` change for the agent.
    fn change(&self) -> serde_json::Value {
        let parts: Vec<SplitPart> = self
            .ranges()
            .iter()
            .map(|range| self.split_part(range))
            .collect();
        json!({
            "kind": "split",
            "todo_id": self.todo_id,
            "parts": parts,
            "keep_first": self.keep_first,
            "in_order": self.in_order,
        })
    }
}

/// "예상 90분" for a new to-do, or why there is no estimate.
fn minutes_left_text(minutes_left: Option<u32>) -> String {
    match minutes_left {
        Some(0) => "남은 단계 없음".into(),
        Some(minutes) => format!("예상 {}", minutes_text(i64::from(minutes))),
        None => "예상 없음(시간이 없는 단계가 있음)".into(),
    }
}

/// "남은 단계 1시간" for the to-do that stays: its estimate becomes the time already spent
/// plus this.
fn kept_left_text(minutes_left: Option<u32>) -> String {
    match minutes_left {
        Some(0) => "남은 단계 없음".into(),
        Some(minutes) => format!(
            "남은 단계 {}(들인 시간에 더해 새 예상이 됩니다)",
            minutes_text(i64::from(minutes))
        ),
        None => "예상 없음(시간이 없는 단계가 있음)".into(),
    }
}

/// The space between two steps: a boundary (✂), or, under the pointer, a place to put one.
/// True when it was clicked.
fn gap_ui(ui: &mut egui::Ui, is_boundary: bool) -> bool {
    let height = if is_boundary { 22.0 } else { 9.0 };
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    let line = rect.left() + 8.0..=rect.right() - 8.0;
    if is_boundary {
        painter.hline(line, rect.center().y, egui::Stroke::new(2.0_f32, YELLOW));
        let galley = painter.layout_no_wrap(
            if response.hovered() {
                "✂ 경계 (누르면 없앰)".to_string()
            } else {
                "✂ 경계".to_string()
            },
            egui::FontId::proportional(13.0),
            YELLOW,
        );
        let size = galley.size() + egui::vec2(12.0, 2.0);
        let corner = egui::pos2(
            rect.center().x - size.x / 2.0,
            rect.center().y - size.y / 2.0,
        );
        painter.rect_filled(
            egui::Rect::from_min_size(corner, size),
            4.0,
            ui.visuals().window_fill,
        );
        painter.galley(corner + egui::vec2(6.0, 1.0), galley, YELLOW);
    } else if response.hovered() {
        painter.hline(line, rect.center().y, egui::Stroke::new(1.0_f32, GREY));
        let galley = painter.layout_no_wrap(
            "+ 여기에 경계 넣기".to_string(),
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(210),
        );
        let size = galley.size() + egui::vec2(12.0, 0.0);
        let corner = egui::pos2(
            rect.center().x - size.x / 2.0,
            rect.center().y - size.y / 2.0,
        );
        painter.rect_filled(
            egui::Rect::from_min_size(corner, size),
            4.0,
            ui.visuals().window_fill,
        );
        painter.galley(
            corner + egui::vec2(6.0, 0.0),
            galley,
            egui::Color32::from_gray(210),
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// One step inside a part: its number, text and minutes; a finished one is ticked and grey.
fn step_line_ui(ui: &mut egui::Ui, number: usize, step: &TaskStep) {
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        let text = format!(
            "{}{number}. {}",
            if step.done { "✔ " } else { "" },
            step.text
        );
        if step.done {
            ui.label(RichText::new(text).color(GREY));
        } else {
            ui.label(text);
        }
        if let Some(minutes) = step.minutes {
            ui.label(
                RichText::new(minutes_text(i64::from(minutes)))
                    .small()
                    .color(GREY),
            );
        }
    });
}

impl FocusApp {
    /// Open the split window for `todo` with its steps as they are on the screen.
    pub(super) fn open_split(&mut self, todo: &TodoItem, steps: Vec<TaskStep>) {
        let via = if self.tab == crate::Tab::Work {
            "work tab"
        } else {
            "to-do tab"
        };
        self.todo_ui.split = Some(SplitWindow::new(todo, steps, via));
    }

    /// The split window, while it is open.
    pub(super) fn split_window_ui(&mut self, context: &egui::Context) {
        let Some(mut window) = self.todo_ui.split.take() else {
            return;
        };
        let idle = !self.pending.load(Ordering::SeqCst);
        let mut keep_open = true;
        let mut send = false;
        egui::Window::new(format!("할 일로 나누기 — {}", window.todo_title))
            .id(egui::Id::new("split_window"))
            .collapsible(false)
            .resizable(false)
            .default_width(660.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(context, |ui| {
                ui.label(
                    "단계 사이를 누르면 경계(✂)가 생기고, 다시 누르면 없어집니다. 경계로 나뉜 \
                     부분마다 할 일이 하나씩 생깁니다. 단계가 하나인 부분은 그 단계가 곧 할 일이 \
                     되고, 여러 개인 부분은 할 일의 이름을 적습니다(그 단계들은 새 할 일의 단계가 \
                     됩니다).",
                );
                ui.add_space(6.0);
                let parts = window.parts();
                // The window fits the screen: the steps scroll in what the rest leaves.
                let made = parts
                    .iter()
                    .filter(|part| !part.kept && !part.finished)
                    .count();
                let room = (context.screen_rect().height() - 470.0 - 33.0 * made as f32)
                    .clamp(120.0, 420.0);
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical()
                        .max_height(room)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.spacing_mut().interact_size.y = 20.0;
                            for (number, part) in parts.iter().enumerate() {
                                if parts.len() == 1 {
                                    ui.label(
                                        RichText::new(
                                            "아직 경계가 없습니다. 나눌 단계 사이를 누르세요.",
                                        )
                                        .color(YELLOW),
                                    );
                                } else {
                                    part_heading_ui(ui, &mut window, number, part);
                                }
                                for index in part.range.clone() {
                                    step_line_ui(ui, index + 1, &window.steps[index]);
                                    if index + 1 < window.steps.len() {
                                        let is_boundary = window.boundaries.contains(&index);
                                        if gap_ui(ui, is_boundary) {
                                            if is_boundary {
                                                window.boundaries.remove(&index);
                                            } else {
                                                window.boundaries.insert(index);
                                            }
                                        }
                                    }
                                }
                            }
                        });
                });
                ui.add_space(4.0);
                ui.radio_value(
                    &mut window.keep_first,
                    false,
                    format!(
                        "\"{}\"을(를) 묶음으로 바꾸고, 나눈 할 일들을 그 안에 넣기",
                        window.todo_title
                    ),
                );
                ui.radio_value(
                    &mut window.keep_first,
                    true,
                    format!(
                        "첫 부분은 \"{}\"(으)로 남기고(단계만 줄어듦), 나머지 부분을 바로 뒤에 새 \
                         할 일로 만들기",
                        window.todo_title
                    ),
                );
                ui.checkbox(
                    &mut window.in_order,
                    "순서대로 하기 (앞 할 일을 끝내야 뒤 할 일을 받을 수 있습니다)",
                );
                let parts = window.parts();
                ui.add_space(4.0);
                ui.label(RichText::new("결과").small().color(GREY));
                result_ui(ui, &window, &parts);
                let missing = window.missing(&parts);
                if let Some(missing) = &missing {
                    style::notice(ui, Tone::Warning, missing.clone());
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            idle && missing.is_none(),
                            style::primary("나누기", idle && missing.is_none()),
                        )
                        .clicked()
                    {
                        send = true;
                    }
                    if ui.button("취소").clicked() {
                        keep_open = false;
                    }
                });
                ui.label(
                    RichText::new(
                        "AI 심사 없이 바로 적용됩니다. 새 할 일의 중요도와 마감은 원래 할 일의 것을 \
                         그대로 받습니다. 지금까지 들인 시간은 묶음(또는 남은 원래 할 일)에 남습니다.",
                    )
                    .small()
                    .color(GREEN),
                );
            });
        if send {
            self.send(
                "todo_change",
                json!({ "change": window.change(), "reason": "", "via": window.via }),
            );
            keep_open = false;
        }
        if keep_open {
            self.todo_ui.split = Some(window);
        }
    }
}

/// The heading of one part: the to-do it becomes (a name field for several steps).
fn part_heading_ui(ui: &mut egui::Ui, window: &mut SplitWindow, number: usize, part: &PartView) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("할 일 {}", number + 1))
                .strong()
                .color(BLUE),
        );
        if part.kept {
            ui.label(RichText::new(&part.title).strong());
            ui.label(
                RichText::new(format!(
                    "그대로 남고 단계만 줄어듭니다 · {}",
                    kept_left_text(part.minutes_left)
                ))
                .small()
                .color(GREY),
            );
        } else if part.finished {
            ui.label(
                RichText::new("단계를 모두 끝내서 할 일로 만들지 않습니다")
                    .small()
                    .color(GREY),
            );
        } else if part.range.len() == 1 {
            ui.label(RichText::new(&part.title).strong());
            ui.label(
                RichText::new(format!(
                    "단계가 하나라서 그 단계가 곧 할 일입니다 · {}",
                    minutes_left_text(part.minutes_left)
                ))
                .small()
                .color(GREY),
            );
        } else {
            let title = window.titles.entry(part.range.start).or_default();
            ui.add_sized(
                [220.0, 24.0],
                egui::TextEdit::singleline(title).hint_text("할 일 이름(꼭 적기)"),
            );
            if title.trim().is_empty() {
                ui.label(RichText::new("이름을 적어야 합니다").small().color(RED));
            } else {
                ui.label(
                    RichText::new(format!(
                        "단계 {}개 · {}",
                        part.range.len(),
                        minutes_left_text(part.minutes_left)
                    ))
                    .small()
                    .color(GREY),
                );
            }
        }
    });
}

/// What the list will hold after the split.
fn result_ui(ui: &mut egui::Ui, window: &SplitWindow, parts: &[PartView]) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        let made: Vec<&PartView> = parts
            .iter()
            .filter(|part| !part.kept && !part.finished)
            .collect();
        let indent = if window.keep_first {
            if let Some(kept) = parts.first() {
                ui.horizontal(|ui| {
                    ui.label(format!("{} (그대로)", kept.title));
                    ui.label(
                        RichText::new(format!(
                            "단계 {}개 · {}",
                            kept.range.len(),
                            kept_left_text(kept.minutes_left)
                        ))
                        .small()
                        .color(GREY),
                    );
                });
            }
            0.0
        } else {
            ui.label(RichText::new(format!("▼ {} (묶음)", window.todo_title)).strong());
            22.0
        };
        let mut previous: Option<String> = window.keep_first.then(|| window.todo_title.clone());
        for part in made {
            let title = if part.title.is_empty() {
                "(이름을 적으세요)".to_string()
            } else {
                part.title.clone()
            };
            ui.horizontal(|ui| {
                ui.add_space(indent);
                let waits = previous.as_ref().filter(|_| window.in_order);
                if waits.is_some() {
                    ui.label(format!("🔒 {title}"));
                } else {
                    ui.label(&title);
                }
                ui.label(
                    RichText::new(format!(
                        "단계 {}개 · {}",
                        part.range.len(),
                        minutes_left_text(part.minutes_left)
                    ))
                    .small()
                    .color(GREY),
                );
                if let Some(waits) = waits {
                    ui.label(RichText::new(format!("{waits} 다음")).small().color(YELLOW));
                }
            });
            previous = Some(title);
        }
    });
}

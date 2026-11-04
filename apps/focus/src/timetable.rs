//! Responsibility: Show how the user spent a chosen period (the Statistics tab's timetable):
//! each day as a bar from 0 to 24 o'clock and as a list of stretches. A stretch is a task (at
//! the computer or away from it), an appointment, or "알 수 없음": during a task done at the
//! computer the user did not touch it.

use chrono::{DateTime, Duration, Local, NaiveDate, Timelike, Utc};
use eframe::egui;
use machine::user_api::call_value;
use serde_json::json;
use std::collections::BTreeMap;
use task_planner::{TimetableEntry, TimetableKind};

/// Responsibility: Hold the chosen period and the timetable read for it.
pub(crate) struct TimetableView {
    from: String,
    to: String,
    entries: Option<Result<Vec<TimetableEntry>, String>>,
}

impl Default for TimetableView {
    fn default() -> Self {
        let today = Local::now().date_naive();
        Self {
            from: (today - Duration::days(6)).to_string(),
            to: today.to_string(),
            entries: None,
        }
    }
}

fn kind_color(kind: TimetableKind) -> egui::Color32 {
    match kind {
        TimetableKind::Task => egui::Color32::from_rgb(70, 130, 200),
        TimetableKind::AwayFromComputer => egui::Color32::from_rgb(80, 160, 90),
        TimetableKind::Appointment => egui::Color32::from_rgb(200, 140, 60),
        TimetableKind::Unknown => egui::Color32::from_gray(150),
    }
}

fn kind_label(kind: TimetableKind) -> &'static str {
    match kind {
        TimetableKind::Task => "컴퓨터로 하는 일",
        TimetableKind::AwayFromComputer => "컴퓨터 없이 하는 일",
        TimetableKind::Appointment => "약속",
        TimetableKind::Unknown => "알 수 없음",
    }
}

fn duration_text(minutes: i64) -> String {
    match (minutes / 60, minutes % 60) {
        (0, minutes) => format!("{minutes}분"),
        (hours, 0) => format!("{hours}시간"),
        (hours, minutes) => format!("{hours}시간 {minutes}분"),
    }
}

/// The local date of `time`.
fn local_day(time: DateTime<Utc>) -> NaiveDate {
    time.with_timezone(&Local).date_naive()
}

/// The stretches cut at local midnights, by day.
fn by_day(entries: &[TimetableEntry]) -> BTreeMap<NaiveDate, Vec<TimetableEntry>> {
    let mut days: BTreeMap<NaiveDate, Vec<TimetableEntry>> = BTreeMap::new();
    for entry in entries {
        let mut start = entry.start;
        while start < entry.end {
            let day = local_day(start);
            let next_midnight = (day + Duration::days(1))
                .and_hms_opt(0, 0, 0)
                .and_then(|midnight| midnight.and_local_timezone(Local).earliest())
                .map(|midnight| midnight.with_timezone(&Utc))
                .unwrap_or(entry.end);
            let end = entry.end.min(next_midnight);
            days.entry(day).or_default().push(TimetableEntry {
                start,
                end,
                ..entry.clone()
            });
            start = end;
        }
    }
    days
}

fn weekday_korean(day: NaiveDate) -> &'static str {
    use chrono::Datelike;
    ["월", "화", "수", "목", "금", "토", "일"][day.weekday().num_days_from_monday() as usize]
}

/// Minutes from local midnight.
fn minute_of_day(time: DateTime<Utc>) -> f32 {
    let local = time.with_timezone(&Local);
    (local.hour() * 60 + local.minute()) as f32 + local.second() as f32 / 60.0
}

impl TimetableView {
    pub(crate) fn ui(&mut self, ui: &mut egui::Ui, session: u32) {
        crate::style::section(ui, "시간표");
        ui.label(
            "고른 기간에 어떤 할 일을 언제 했는지 보여 줍니다. 컴퓨터로 하는 일을 하는 동안 \
             5분 넘게 키보드와 마우스를 쓰지 않은 시간은 \"알 수 없음\"으로 나오고, 그동안은 \
             할 일의 시간이 멈춥니다. 이 기록은 이 기능이 생긴 뒤부터 쌓입니다.",
        );
        let today = Local::now().date_naive();
        let mut fetch = self.entries.is_none();
        ui.horizontal_wrapped(|ui| {
            ui.label("기간");
            ui.add(egui::TextEdit::singleline(&mut self.from).desired_width(90.0));
            ui.label("~");
            ui.add(egui::TextEdit::singleline(&mut self.to).desired_width(90.0));
            fetch |= ui.button("보기").clicked();
            for (label, days_back, days_long) in [
                ("오늘", 0, 0),
                ("어제", 1, 0),
                ("최근 7일", 6, 6),
                ("최근 30일", 29, 29),
            ] {
                if ui.small_button(label).clicked() {
                    let first = today - Duration::days(days_back);
                    self.from = first.to_string();
                    self.to = (first + Duration::days(days_long)).to_string();
                    fetch = true;
                }
            }
        });
        if fetch {
            self.entries = Some(
                call_value(
                    session,
                    "timetable",
                    json!({ "from": self.from.trim(), "to": self.to.trim() }),
                )
                .and_then(|value| serde_json::from_value(value).map_err(|error| error.to_string())),
            );
        }
        let entries = match &self.entries {
            Some(Ok(entries)) => entries,
            Some(Err(error)) => {
                ui.colored_label(crate::RED, error);
                return;
            }
            None => return,
        };
        if entries.is_empty() {
            ui.label("이 기간에는 기록이 없습니다.");
            return;
        }
        // The period in total, by kind and by task.
        let mut by_kind: BTreeMap<&'static str, i64> = BTreeMap::new();
        let mut by_task: BTreeMap<String, i64> = BTreeMap::new();
        for entry in entries {
            let minutes = (entry.end - entry.start).num_minutes();
            *by_kind.entry(kind_label(entry.kind)).or_default() += minutes;
            if entry.kind != TimetableKind::Unknown {
                *by_task.entry(entry.task.clone()).or_default() += minutes;
            }
        }
        ui.horizontal_wrapped(|ui| {
            for kind in [
                TimetableKind::Task,
                TimetableKind::AwayFromComputer,
                TimetableKind::Appointment,
                TimetableKind::Unknown,
            ] {
                let minutes = by_kind.get(kind_label(kind)).copied().unwrap_or(0);
                ui.colored_label(kind_color(kind), "■");
                ui.label(format!("{} {}", kind_label(kind), duration_text(minutes)));
            }
        });
        egui::CollapsingHeader::new("할 일마다 들인 시간")
            .id_salt("timetable_by_task")
            .show(ui, |ui| {
                let mut tasks: Vec<(String, i64)> = by_task.into_iter().collect();
                tasks.sort_by(|first, second| second.1.cmp(&first.1));
                for (task, minutes) in tasks {
                    ui.label(format!("{} — {task}", duration_text(minutes)));
                }
            });
        for (day, stretches) in by_day(entries).into_iter().rev() {
            ui.separator();
            ui.strong(format!(
                "{} ({})",
                day.format("%Y-%m-%d"),
                weekday_korean(day)
            ));
            day_bar(ui, &stretches);
            for entry in &stretches {
                let start = entry.start.with_timezone(&Local).format("%H:%M");
                let end = entry.end.with_timezone(&Local).format("%H:%M");
                let minutes = (entry.end - entry.start).num_minutes();
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(kind_color(entry.kind), "■");
                    ui.label(format!("{start}–{end} ({})", duration_text(minutes)));
                    if entry.kind == TimetableKind::Unknown {
                        ui.label(format!("알 수 없음 (그때의 할 일: {})", entry.task));
                    } else {
                        ui.label(&entry.task);
                    }
                });
            }
        }
    }
}

/// One day as a bar from 0 to 24 o'clock, with hour marks every 6 hours.
fn day_bar(ui: &mut egui::Ui, stretches: &[TimetableEntry]) {
    let width = ui.available_width().min(720.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().faint_bg_color);
    let x_of = |minute: f32| rect.left() + rect.width() * (minute / (24.0 * 60.0)).clamp(0.0, 1.0);
    for entry in stretches {
        let start = minute_of_day(entry.start);
        let mut end = minute_of_day(entry.end);
        if end <= start {
            end = 24.0 * 60.0;
        }
        let stretch = egui::Rect::from_min_max(
            egui::pos2(x_of(start), rect.top()),
            egui::pos2(x_of(end).max(x_of(start) + 1.0), rect.bottom()),
        );
        painter.rect_filled(stretch, 0.0, kind_color(entry.kind));
    }
    for hour in [6.0, 12.0, 18.0] {
        let x = x_of(hour * 60.0);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0_f32, ui.visuals().weak_text_color()),
        );
    }
}

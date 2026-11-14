//! Responsibility: Show how the user spent a chosen period (the Statistics tab's timetable):
//! the days as columns and the hours as rows like a calendar, each stretch as a colored block
//! that tells what was done when the mouse is over it. A stretch is a task (at the computer or
//! away from it), an appointment, or "멈춤": the task was paused.

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
        TimetableKind::Task => "할 일",
        TimetableKind::AwayFromComputer => "컴퓨터 없이 하는 일(예전 기록)",
        TimetableKind::Appointment => "약속",
        TimetableKind::Unknown => "멈춤",
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
            "고른 기간에 어떤 할 일을 언제 했는지 보여 줍니다. \"멈춤\"은 할 일을 일시 정지한 \
             시간입니다(예전 버전에서 자리를 비워 할 일의 시간이 멈췄던 때도 들어갑니다). \
             \"컴퓨터 없이 하는 일\"은 예전 버전의 기록에만 있습니다. 이 기록은 이 기능이 생긴 \
             뒤부터 쌓입니다.",
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
        let days = by_day(entries);
        let period_days =
            period_days(&self.from, &self.to).unwrap_or_else(|| days.keys().copied().collect());
        calendar(ui, &period_days, &days);
        egui::CollapsingHeader::new("하루마다 목록으로 보기")
            .id_salt("timetable_list")
            .show(ui, |ui| {
                for (day, stretches) in days.iter().rev() {
                    ui.separator();
                    ui.strong(format!(
                        "{} ({})",
                        day.format("%Y-%m-%d"),
                        weekday_korean(*day)
                    ));
                    for entry in stretches {
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(kind_color(entry.kind), "■");
                            ui.label(time_range_text(entry));
                            ui.label(task_text(entry));
                        });
                    }
                }
            });
    }
}

/// "09:00–10:30 (1시간 30분)".
fn time_range_text(entry: &TimetableEntry) -> String {
    let start = entry.start.with_timezone(&Local).format("%H:%M");
    let end = entry.end.with_timezone(&Local).format("%H:%M");
    let minutes = (entry.end - entry.start).num_minutes();
    format!("{start}–{end} ({})", duration_text(minutes))
}

fn task_text(entry: &TimetableEntry) -> String {
    if entry.kind == TimetableKind::Unknown {
        format!("멈춤 (그때의 할 일: {})", entry.task)
    } else {
        entry.task.clone()
    }
}

/// Every day from `from` to `to`, at most 31 of them; `None` when the dates do not read.
fn period_days(from: &str, to: &str) -> Option<Vec<NaiveDate>> {
    let first: NaiveDate = from.trim().parse().ok()?;
    let last: NaiveDate = to.trim().parse().ok()?;
    if last < first {
        return None;
    }
    Some(
        first
            .iter_days()
            .take_while(|day| *day <= last)
            .take(31)
            .collect(),
    )
}

const HOUR_HEIGHT: f32 = 26.0;
const AXIS_WIDTH: f32 = 42.0;
const HEADER_HEIGHT: f32 = 34.0;

/// The whole hours that hold every stretch: from the hour of the earliest start to the hour
/// after the latest end, so empty night hours above and below take no room.
fn hour_range(days: &BTreeMap<NaiveDate, Vec<TimetableEntry>>) -> (u32, u32) {
    let stretches = || days.values().flatten();
    let first = stretches()
        .map(|entry| minute_of_day(entry.start))
        .fold(f32::MAX, f32::min);
    let last = stretches()
        .map(|entry| match minute_of_day(entry.end) {
            0.0 => 24.0 * 60.0,
            minute => minute,
        })
        .fold(0.0, f32::max);
    if first > last {
        return (0, 24);
    }
    (
        (first / 60.0).floor() as u32,
        ((last / 60.0).ceil() as u32).clamp(1, 24),
    )
}

/// The days as columns and the hours as rows, like a calendar's week view. Each stretch is a
/// colored block; the mouse over a block shows what was done then.
fn calendar(
    ui: &mut egui::Ui,
    period_days: &[NaiveDate],
    days: &BTreeMap<NaiveDate, Vec<TimetableEntry>>,
) {
    if period_days.is_empty() {
        return;
    }
    let column_width =
        ((ui.available_width() - AXIS_WIDTH) / period_days.len() as f32).clamp(64.0, 220.0);
    let (first_hour, last_hour) = hour_range(days);
    egui::ScrollArea::horizontal()
        .id_salt("timetable_calendar")
        .show(ui, |ui| {
            let size = egui::vec2(
                AXIS_WIDTH + column_width * period_days.len() as f32,
                HEADER_HEIGHT + HOUR_HEIGHT * (last_hour - first_hour) as f32,
            );
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let painter = ui.painter_at(rect);
            let visuals = ui.visuals().clone();
            let line = egui::Stroke::new(1.0_f32, visuals.widgets.noninteractive.bg_stroke.color);
            let grid_top = rect.top() + HEADER_HEIGHT;
            let y_of = |minute: f32| {
                grid_top
                    + HOUR_HEIGHT
                        * (minute / 60.0 - first_hour as f32)
                            .clamp(0.0, (last_hour - first_hour) as f32)
            };
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(rect.left() + AXIS_WIDTH, grid_top), rect.max),
                0.0,
                visuals.faint_bg_color,
            );
            let small = egui::FontId::proportional(11.0);
            for hour in first_hour..=last_hour {
                let y = y_of(hour as f32 * 60.0);
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + AXIS_WIDTH, y),
                        egui::pos2(rect.right(), y),
                    ],
                    line,
                );
                if hour < last_hour {
                    painter.text(
                        egui::pos2(rect.left() + AXIS_WIDTH - 4.0, y),
                        egui::Align2::RIGHT_TOP,
                        format!("{hour}시"),
                        small.clone(),
                        visuals.weak_text_color(),
                    );
                }
            }
            let today = Local::now().date_naive();
            for (column, day) in period_days.iter().enumerate() {
                let left = rect.left() + AXIS_WIDTH + column_width * column as f32;
                painter.line_segment(
                    [
                        egui::pos2(left, rect.top()),
                        egui::pos2(left, rect.bottom()),
                    ],
                    line,
                );
                let header_color = if *day == today {
                    visuals.selection.bg_fill
                } else {
                    visuals.text_color()
                };
                painter.text(
                    egui::pos2(left + column_width / 2.0, rect.top() + 2.0),
                    egui::Align2::CENTER_TOP,
                    weekday_korean(*day),
                    small.clone(),
                    header_color,
                );
                painter.text(
                    egui::pos2(left + column_width / 2.0, rect.top() + 15.0),
                    egui::Align2::CENTER_TOP,
                    day.format("%m-%d").to_string(),
                    egui::FontId::proportional(13.0),
                    header_color,
                );
                for (index, entry) in days.get(day).into_iter().flatten().enumerate() {
                    let start = minute_of_day(entry.start);
                    let mut end = minute_of_day(entry.end);
                    if end <= start {
                        end = 24.0 * 60.0;
                    }
                    let block = egui::Rect::from_min_max(
                        egui::pos2(left + 2.0, y_of(start) + 1.0),
                        egui::pos2(
                            left + column_width - 2.0,
                            (y_of(end) - 1.0).max(y_of(start) + 3.0),
                        ),
                    );
                    painter.rect_filled(block, 3.0, kind_color(entry.kind));
                    if block.height() >= 14.0 {
                        painter.with_clip_rect(block.shrink(2.0)).text(
                            block.left_top() + egui::vec2(3.0, 1.0),
                            egui::Align2::LEFT_TOP,
                            &entry.task,
                            small.clone(),
                            egui::Color32::WHITE,
                        );
                    }
                    // A block too thin for the mouse still answers in a strip at least 6 points high.
                    let hover_area =
                        block.expand2(egui::vec2(0.0, ((6.0 - block.height()) / 2.0).max(0.0)));
                    ui.interact(
                        hover_area,
                        ui.id().with(("timetable_block", *day, index)),
                        egui::Sense::hover(),
                    )
                    .on_hover_ui(|ui| {
                        ui.strong(task_text(entry));
                        ui.horizontal(|ui| {
                            ui.colored_label(kind_color(entry.kind), "■");
                            ui.label(kind_label(entry.kind));
                        });
                        ui.label(format!(
                            "{} ({})",
                            day.format("%Y-%m-%d"),
                            weekday_korean(*day)
                        ));
                        ui.label(time_range_text(entry));
                    });
                }
            }
            let now_minute = minute_of_day(Utc::now());
            let now_shown =
                (first_hour as f32 * 60.0..=last_hour as f32 * 60.0).contains(&now_minute);
            if let Some(now_column) = period_days
                .iter()
                .position(|day| *day == today)
                .filter(|_| now_shown)
            {
                let left = rect.left() + AXIS_WIDTH + column_width * now_column as f32;
                let y = y_of(now_minute);
                painter.line_segment(
                    [egui::pos2(left, y), egui::pos2(left + column_width, y)],
                    egui::Stroke::new(2.0_f32, crate::RED),
                );
            }
        });
}

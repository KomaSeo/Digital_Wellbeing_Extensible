//! Responsibility: Show what already happened (the Statistics tab): AI usage and cost, today's
//! summary, finished to-dos, change requests with the AI's answers, and recent AI verdicts.

use crate::{FocusApp, GREEN, RED};
use chrono::{Datelike, Duration, Local, NaiveDate, Weekday};
use eframe::egui;
use machine::user_api::{Status, Verdict};
use serde_json::json;
use std::sync::atomic::Ordering;
use task_planner::{PlannerStatus, WeekPlan};

impl FocusApp {
    /// The Statistics tab.
    pub(crate) fn statistics_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("통계");
        ui.label(
            "이 탭은 앞으로 7일 동안 일이 얼마나 남았는지와 지난 기록을 보여 줍니다. 설정이나 할 \
             일은 여기서 바꾸지 않습니다.",
        );
        if let Some(planner) = status.planner.clone() {
            ui.separator();
            self.week_ui(ui, &planner);
        }

        ui.separator();
        ui.heading("AI 사용량");
        if let Some(cost) = status.ai_cost_today_usd {
            ui.label(if status.daily_budget_usd > 0.0 {
                format!(
                    "이 계정의 오늘 AI 비용(추정): ${cost:.2} / 하루 예산 ${:.2}",
                    status.daily_budget_usd
                )
            } else {
                format!("이 계정의 오늘 AI 비용(추정): ${cost:.2}")
            });
        }
        match self.settings.latest_snapshot() {
            Some(snapshot) => crate::settings::usage_ui(ui, &snapshot),
            None => {
                ui.label("agent에 연결하는 중…");
            }
        }

        if let Some(planner) = status.planner.clone() {
            ui.separator();
            ui.heading("할 일");
            self.today_ui(ui, status, &planner);
            crate::todo::done_ui(ui, &planner);
            crate::todo::log_ui(ui, &planner);
        }

        ui.separator();
        ui.heading("최근 AI 판정");
        if status.verdicts.is_empty() {
            ui.label("아직 판정이 없습니다.");
        }
        for verdict_record in &status.verdicts {
            let (color, verdict_label) = match verdict_record.verdict {
                Verdict::OnTask => (GREEN, "할 일에 맞음"),
                Verdict::OffTask => (RED, "딴짓"),
                Verdict::Unsure => (egui::Color32::GRAY, "애매함"),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    verdict_record
                        .at
                        .with_timezone(&Local)
                        .format("%m-%d %H:%M")
                        .to_string(),
                );
                ui.colored_label(color, verdict_label);
                ui.label(format!(
                    "{} — {}",
                    verdict_record.process_name, verdict_record.reason
                ));
                if !verdict_record.action.is_empty() {
                    ui.strong(format!("[{}]", action_label(&verdict_record.action)));
                }
            });
        }
    }
}

impl FocusApp {
    /// This week: the work left by deadline (counted here, no AI), and the AI's overview of
    /// how much work each day needs and how much free time is left. No clock times.
    fn week_ui(&mut self, ui: &mut egui::Ui, planner: &PlannerStatus) {
        ui.heading("앞으로 7일");
        let today = Local::now().date_naive();
        let last_day = today + Duration::days(6);
        // Minutes still needed per to-do: its estimate minus the time already spent.
        let mut buckets: [(&str, i64, usize); 5] = [
            ("마감 지남", 0, 0),
            ("오늘 마감", 0, 0),
            ("앞으로 7일 안 마감(내일부터)", 0, 0),
            ("그 뒤 마감", 0, 0),
            ("마감 없음", 0, 0),
        ];
        for todo in &planner.open_todos {
            let index = match todo.deadline {
                Some(deadline) if deadline < today => 0,
                Some(deadline) if deadline == today => 1,
                Some(deadline) if deadline <= last_day => 2,
                Some(_) => 3,
                None => 4,
            };
            match todo.estimate_minutes {
                Some(estimate) => {
                    let spent = planner.minutes_total.get(&todo.id).copied().unwrap_or(0);
                    buckets[index].1 += (i64::from(estimate) - spent).max(0);
                }
                None => buckets[index].2 += 1,
            }
        }
        ui.label("남은 일(할 일의 예상 시간에서 이미 들인 시간을 뺀 것):");
        egui::Grid::new("work_left").num_columns(2).show(ui, |ui| {
            for (label, minutes, without_estimate) in buckets {
                ui.label(label);
                let extra = if without_estimate > 0 {
                    format!(" (예상 시간이 없는 할 일 {without_estimate}개)")
                } else {
                    String::new()
                };
                ui.label(format!("{}{extra}", hours_text(minutes)));
                ui.end_row();
            }
        });

        ui.add_space(6.0);
        let idle = !self.pending.load(Ordering::SeqCst);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    idle && !planner.open_todos.is_empty(),
                    egui::Button::new("AI에게 앞으로 7일 계획 세우기"),
                )
                .clicked()
            {
                self.send("plan_week", json!({}));
            }
            if !idle {
                ui.spinner();
            }
            ui.small(
                "AI를 한 번 부릅니다. AI는 할 일과 마감, 생활 규칙을 보고 오늘부터 7일 동안 날마다 일에 \
                 써야 할 시간과 남는 여유를 계산합니다. 몇 시에 무엇을 할지는 정하지 않습니다.",
            );
        });
        match &planner.week_plan {
            Some(plan) => week_plan_ui(ui, plan, planner, today),
            None => {
                ui.label("아직 계획이 없습니다.");
            }
        }
    }
}

fn week_plan_ui(ui: &mut egui::Ui, plan: &WeekPlan, planner: &PlannerStatus, today: NaiveDate) {
    let made = plan
        .at
        .map(|at| at.with_timezone(&Local).format("%m-%d %H:%M").to_string())
        .unwrap_or_default();
    let made_today = plan
        .at
        .is_some_and(|at| at.with_timezone(&Local).date_naive() == today);
    if !plan
        .days
        .iter()
        .any(|day| day.date.is_some_and(|date| date >= today))
    {
        ui.colored_label(
            RED,
            format!("{made}에 세운 계획은 기간이 지났습니다. 새로 세우세요."),
        );
        return;
    }
    if made_today {
        ui.small(format!(
            "{made}에 세운 계획입니다. 그 뒤에 바뀐 할 일은 다시 세워야 반영됩니다."
        ));
    } else {
        ui.colored_label(
            RED,
            format!("{made}에 세운 계획입니다. 오늘 다시 세우면 더 정확합니다."),
        );
    }
    ui.label(&plan.summary);
    ui.strong(format!(
        "7일 동안 일에 써야 할 시간: {} · 남는 여유: {}",
        hours_text(i64::from(plan.work_minutes_total)),
        hours_text(i64::from(plan.free_minutes_total))
    ));
    for warning in &plan.warnings {
        ui.colored_label(RED, format!("주의: {warning}"));
    }
    let title_of = |todo_id: &str| {
        planner
            .open_todos
            .iter()
            .find(|todo| todo.id == todo_id)
            .map_or_else(|| todo_id.to_string(), |todo| todo.title.clone())
    };
    let worked_today: i64 = planner.minutes_today.values().sum();
    for day in &plan.days {
        let Some(date) = day.date else {
            continue;
        };
        let is_today = date == today;
        if date < today {
            continue;
        }
        ui.add_space(4.0);
        let heading = format!(
            "{} ({}){} · 일 {} · 여유 {}",
            date.format("%m-%d"),
            weekday_korean(date.weekday()),
            if is_today { " 오늘" } else { "" },
            hours_text(i64::from(day.work_minutes)),
            hours_text(i64::from(day.free_minutes))
        );
        if is_today {
            ui.colored_label(GREEN, egui::RichText::new(heading).strong());
            ui.small(format!(
                "오늘 이미 할 일에 들인 시간: {} (계획을 세운 뒤의 시간도 들어갑니다)",
                hours_text(worked_today)
            ));
        } else {
            ui.strong(heading);
        }
        if !day.note.is_empty() {
            ui.small(&day.note);
        }
        let todos: Vec<String> = day
            .todos
            .iter()
            .map(|planned| {
                format!(
                    "{} {}",
                    title_of(&planned.todo_id),
                    hours_text(i64::from(planned.minutes))
                )
            })
            .collect();
        if !todos.is_empty() {
            ui.small(todos.join(" · "));
        }
    }
}

fn weekday_korean(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "월",
        Weekday::Tue => "화",
        Weekday::Wed => "수",
        Weekday::Thu => "목",
        Weekday::Fri => "금",
        Weekday::Sat => "토",
        Weekday::Sun => "일",
    }
}

/// `150` → `2시간 30분`.
fn hours_text(minutes: i64) -> String {
    let minutes = minutes.max(0);
    match (minutes / 60, minutes % 60) {
        (0, rest) => format!("{rest}분"),
        (hours, 0) => format!("{hours}시간"),
        (hours, rest) => format!("{hours}시간 {rest}분"),
    }
}

/// What the monitor did, in Korean ("tab closed", "blocked", "strike 1/2").
fn action_label(action: &str) -> String {
    if let Some(rest) = action.strip_prefix("access revoked, ") {
        return format!("접근 허용 취소, {}", action_label(rest));
    }
    match action {
        "tab closed" => "탭 닫음".to_string(),
        "tab closed (already blocked)" => "탭 닫음(이미 차단된 탭)".to_string(),
        "blocked" => "차단".to_string(),
        "blocked, window minimized" => "차단, 창 최소화".to_string(),
        other => match other.strip_prefix("strike ") {
            Some(count) => format!("경고 {count}"),
            None => other.to_string(),
        },
    }
}

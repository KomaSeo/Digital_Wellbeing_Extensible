//! Responsibility: Show what already happened (the Statistics tab): AI usage and cost, today's
//! summary, finished to-dos, change requests with the AI's answers, and recent AI verdicts.

use crate::{FocusApp, GREEN, RED};
use chrono::Local;
use eframe::egui;
use machine::user_api::{Status, Verdict};

impl FocusApp {
    /// The Statistics tab.
    pub(crate) fn statistics_tab(&mut self, ui: &mut egui::Ui, status: &Status) {
        ui.heading("통계");
        ui.label("이 탭은 지난 기록을 보여 줍니다. 설정이나 할 일은 여기서 바꾸지 않습니다.");

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

/// What the monitor did, in Korean ("tab closed", "blocked", "strike 1/2").
fn action_label(action: &str) -> String {
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

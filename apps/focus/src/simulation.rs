//! Responsibility: Draw "시뮬레이션 돌려보기" under the AI rules in the Settings tab: start a
//! simulation at a chosen time and situation, play what happens, and show each rule set's
//! virtual day side by side (the owner: "AI규칙창 아래에 시뮬레이션 돌려보기를 하나
//! 만들어줄래?").
//!
//! The agent runs the simulation (`simulation_*` requests, `task_planner::Simulation`); this
//! panel sends the actions and asks about the progress once a second while one runs.

use agent_api::user_api::call_value;
use chrono::{DateTime, Duration, Local, NaiveDate, TimeZone, Utc};
use eframe::egui;
use serde_json::{json, Value};
use std::time::Instant;
use task_planner::{
    ChangeKind, LogKind, PlannerConfig, ProposalInput, RuleSet, SimulationEvent, SimulationView,
    TodoChange, Variability,
};

const GREEN: egui::Color32 = egui::Color32::from_rgb(80, 170, 90);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 80, 80);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(200, 150, 40);
const BLUE: egui::Color32 = egui::Color32::from_rgb(110, 160, 230);

/// The requests the panel can send, as the Work tab names them.
const REQUESTS: [&str; 7] = [
    "끝냈다",
    "이미 처리했어요",
    "일이 변경되었어요",
    "연장하기",
    "다 못 했다",
    "다른 일 받기",
    "AI 검사 멈추기",
];

/// Responsibility: Hold the panel's inputs and what the agent last said about the simulation.
pub(crate) struct SimulationPanel {
    day: String,
    time: String,
    situation: String,
    /// The agent's last answer: whether one runs, its view and what it cost.
    active: bool,
    view: Option<SimulationView>,
    cost: f64,
    message: String,
    polled_at: Option<Instant>,
    advance_minutes: i64,
    request: usize,
    extend_minutes: u32,
    reason: String,
    todo_title: String,
    todo_reason: String,
    appointment_title: String,
    appointment_start: String,
    appointment_minutes: i64,
    appointment_reason: String,
    auto_until: String,
    variability: Variability,
}

impl Default for SimulationPanel {
    fn default() -> Self {
        let tomorrow = Local::now().date_naive() + Duration::days(1);
        Self {
            day: tomorrow.format("%Y-%m-%d").to_string(),
            time: "09:00".into(),
            situation: String::new(),
            active: false,
            view: None,
            cost: 0.0,
            message: String::new(),
            polled_at: None,
            advance_minutes: 30,
            request: 0,
            extend_minutes: 30,
            reason: String::new(),
            todo_title: String::new(),
            todo_reason: String::new(),
            appointment_title: String::new(),
            appointment_start: "18:00".into(),
            appointment_minutes: 60,
            appointment_reason: String::new(),
            auto_until: "23:00".into(),
            variability: Variability::Some,
        }
    }
}

/// `day` at `time` in local time, as UTC.
fn local_moment(day: NaiveDate, time: chrono::NaiveTime) -> Option<DateTime<Utc>> {
    Local
        .from_local_datetime(&day.and_time(time))
        .earliest()
        .map(|moment| moment.with_timezone(&Utc))
}

/// `text` cut to `length` characters.
fn short(text: &str, length: usize) -> String {
    let mut shown: String = text.chars().take(length).collect();
    if text.chars().count() > length {
        shown.push('…');
    }
    shown
}

/// "11-24 (화) 09:00".
fn moment_text(moment: DateTime<Utc>) -> String {
    let local = moment.with_timezone(&Local);
    let weekday = ["월", "화", "수", "목", "금", "토", "일"]
        [local.format("%u").to_string().parse::<usize>().unwrap_or(1) - 1];
    format!(
        "{} ({weekday}) {}",
        local.format("%m-%d"),
        local.format("%H:%M")
    )
}

impl SimulationPanel {
    /// Send `method` to the agent and take its answer.
    fn send(&mut self, session: u32, method: &str, params: Value) {
        match call_value(session, method, params) {
            Ok(answer) => {
                self.take(&answer);
                self.message.clear();
            }
            Err(error) => self.message = error,
        }
    }

    fn take(&mut self, answer: &Value) {
        self.active = answer["active"].as_bool().unwrap_or(false);
        self.view = serde_json::from_value(answer["view"].clone()).ok();
        self.cost = answer["cost"].as_f64().unwrap_or(0.0);
        self.polled_at = Some(Instant::now());
    }

    fn act(&mut self, session: u32, action: Value) {
        self.send(session, "simulation_act", json!({ "action": action }));
    }

    fn running(&self) -> bool {
        self.view.as_ref().is_some_and(|view| view.running)
    }

    /// The virtual day the first sandbox is on (for the times the user types).
    fn virtual_day(&self) -> NaiveDate {
        self.view
            .as_ref()
            .and_then(|view| view.sandboxes.first())
            .map(|sandbox| sandbox.clock.with_timezone(&Local).date_naive())
            .unwrap_or_else(|| Local::now().date_naive())
    }

    /// `text` ("HH:MM") on the virtual day, or the day after when it is earlier than the
    /// virtual clock.
    fn virtual_moment(&self, text: &str) -> Option<DateTime<Utc>> {
        let time = crate::todo::parse_clock(text)?;
        let clock = self
            .view
            .as_ref()
            .and_then(|view| view.sandboxes.first())
            .map(|sandbox| sandbox.clock)?;
        let moment = local_moment(self.virtual_day(), time)?;
        Some(if moment < clock {
            moment + Duration::days(1)
        } else {
            moment
        })
    }

    /// The panel. `draft` is the planner settings being edited above; `in_force` the ones in
    /// force.
    pub(crate) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        session: u32,
        draft: Option<&PlannerConfig>,
        in_force: Option<&PlannerConfig>,
    ) {
        crate::style::section(ui, "시뮬레이션 돌려보기");
        ui.label(
            "가상의 하루를 돌려 보며 생활 규칙이 잘 동작하는지 봅니다. 할 일 목록과 기록을 복사해서 \
             쓰므로, 진짜 할 일과 시간표와 차단은 바뀌지 않습니다. AI를 진짜로 부르므로 AI 비용이 \
             듭니다(통계 탭의 AI 비용에 \"시뮬레이션\"으로 나옵니다). 위에서 고치는 중인 생활 규칙, \
             약속 규칙, 지시문이 지금 규칙과 다르면 둘을 나란히 돌립니다.",
        );
        // Once a second while an action runs.
        if self.running() || self.polled_at.is_none() {
            let due = self
                .polled_at
                .is_none_or(|polled_at| polled_at.elapsed().as_millis() >= 1000);
            if due {
                if let Ok(answer) = call_value(session, "simulation_status", json!({})) {
                    self.take(&answer);
                }
            }
            if self.running() {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(1000));
            }
        }
        let same_rules = match (draft, in_force) {
            (Some(draft), Some(in_force)) => RuleSet::of(draft) == RuleSet::of(in_force),
            _ => true,
        };
        if !self.active {
            self.start_ui(ui, session, draft, same_rules);
        } else {
            self.running_ui(ui, session);
        }
        if !self.message.is_empty() {
            ui.colored_label(RED, &self.message);
        }
    }

    fn start_ui(
        &mut self,
        ui: &mut egui::Ui,
        session: u32,
        draft: Option<&PlannerConfig>,
        same_rules: bool,
    ) {
        egui::Grid::new("simulation_start")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label("가상 날짜");
                ui.add(egui::TextEdit::singleline(&mut self.day).desired_width(120.0));
                ui.end_row();
                ui.label("시작 시각");
                ui.add(egui::TextEdit::singleline(&mut self.time).desired_width(80.0));
                ui.end_row();
                ui.label("상황(선택)");
                ui.add(
                    egui::TextEdit::singleline(&mut self.situation)
                        .hint_text("예: 조금 피곤함, 카페에서 노트북만 있음")
                        .desired_width(360.0),
                );
                ui.end_row();
            });
        ui.small(if same_rules {
            "고치는 중인 규칙이 지금 규칙과 같아서 지금 규칙으로 한 번만 돌립니다."
        } else {
            "고치는 중인 규칙과 지금 규칙을 나란히 돌립니다(AI 비용이 두 배)."
        });
        let start = NaiveDate::parse_from_str(self.day.trim(), "%Y-%m-%d")
            .ok()
            .zip(crate::todo::parse_clock(&self.time))
            .and_then(|(day, time)| local_moment(day, time));
        if start.is_none() {
            ui.colored_label(RED, "날짜는 2026-12-01처럼, 시각은 09:00처럼 적으세요.");
        }
        if ui
            .add_enabled(
                start.is_some(),
                crate::style::primary("시뮬레이션 시작", true),
            )
            .clicked()
        {
            if let Some(at) = start {
                let draft = draft.map(RuleSet::of);
                self.send(
                    session,
                    "simulation_start",
                    json!({ "at": at, "situation": self.situation, "draft": draft }),
                );
            }
        }
    }

    fn running_ui(&mut self, ui: &mut egui::Ui, session: u32) {
        let Some(view) = self.view.clone() else {
            return;
        };
        let calls: usize = view.sandboxes.iter().map(|sandbox| sandbox.ai_calls).sum();
        ui.horizontal_wrapped(|ui| {
            if view.running {
                ui.spinner();
                ui.label("AI에게 묻는 중…");
            }
            ui.small(format!(
                "AI 호출 {calls}번 · 이 시뮬레이션의 AI 비용 약 ${:.3}{}",
                self.cost,
                if view.review {
                    " · 엄격 모드처럼 할 일 변경도 AI가 심사"
                } else {
                    ""
                }
            ));
        });
        let count = view.sandboxes.len().max(1);
        ui.columns(count, |columns| {
            for (index, sandbox) in view.sandboxes.iter().enumerate() {
                // Columns justify their text; left-aligned reads better.
                let ui = &mut columns[index];
                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.group(|ui| {
                        ui.strong(&sandbox.label);
                        ui.label(format!("가상 시각: {}", moment_text(sandbox.clock)));
                        match (&sandbox.task, sandbox.task_until) {
                            (Some(task), Some(until)) => ui.label(format!(
                                "지금 할 일: {} ({}까지)",
                                short(task, 60),
                                until.with_timezone(&Local).format("%H:%M")
                            )),
                            (Some(task), None) => {
                                ui.label(format!("지금 할 일: {}", short(task, 60)))
                            }
                            (None, _) => ui.weak("지금 할 일: 없음"),
                        };
                        egui::ScrollArea::vertical()
                            .id_salt(("simulation_log", index))
                            .max_height(360.0)
                            .min_scrolled_height(260.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                for entry in &sandbox.log {
                                    let color = match entry.kind {
                                        LogKind::Time => ui.visuals().weak_text_color(),
                                        LogKind::Pick => BLUE,
                                        LogKind::Approved => GREEN,
                                        LogKind::Refused | LogKind::Problem => RED,
                                        LogKind::Event => YELLOW,
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "{} {}",
                                                entry.at.with_timezone(&Local).format("%H:%M"),
                                                entry.text
                                            ))
                                            .color(color),
                                        )
                                        .wrap(),
                                    );
                                }
                            });
                        egui::CollapsingHeader::new("요약")
                            .id_salt(("simulation_summary", index))
                            .default_open(true)
                            .show(ui, |ui| {
                                for line in &sandbox.summary {
                                    ui.small(line);
                                }
                            });
                    })
                });
            }
        });
        let ready = !view.running;
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(ready, crate::style::primary("할 일 받기", ready))
                .clicked()
            {
                self.act(session, json!({ "kind": "assign" }));
            }
            ui.add(
                egui::DragValue::new(&mut self.advance_minutes)
                    .range(5..=720)
                    .speed(5)
                    .suffix("분"),
            );
            if ui
                .add_enabled(ready, egui::Button::new("지나가기"))
                .clicked()
            {
                let minutes = self.advance_minutes;
                self.act(session, json!({ "kind": "advance", "minutes": minutes }));
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("일어난 일:");
            for (label, event, hint) in [
                (
                    "예상보다 일찍 끝냄",
                    SimulationEvent::Early,
                    "주어진 시간의 60%쯤에 끝냈다고 보고합니다.",
                ),
                (
                    "예상대로 끝냄",
                    SimulationEvent::OnTime,
                    "시간이 다 되었을 때 끝냈다고 보고합니다.",
                ),
                (
                    "오래 걸림",
                    SimulationEvent::Late,
                    "시간이 다 되었을 때 30분 연장을 요청합니다.",
                ),
                (
                    "일을 안 함",
                    SimulationEvent::Skipped,
                    "시간이 다 되도록 하지 않고 \"다 못 했다\"를 보냅니다.",
                ),
                (
                    "감을 못 잡음",
                    SimulationEvent::Stuck,
                    "15분 뒤 감이 안 잡힌다며 다른 일을 요청합니다.",
                ),
            ] {
                if ui
                    .add_enabled(ready, egui::Button::new(label))
                    .on_hover_text(hint)
                    .clicked()
                {
                    self.act(session, json!({ "kind": "event", "event": event }));
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("요청:");
            egui::ComboBox::from_id_salt("simulation_request")
                .selected_text(REQUESTS[self.request])
                .show_ui(ui, |ui| {
                    for (index, label) in REQUESTS.iter().enumerate() {
                        ui.selectable_value(&mut self.request, index, *label);
                    }
                });
            if self.request == 3 {
                ui.add(
                    egui::DragValue::new(&mut self.extend_minutes)
                        .range(5..=240)
                        .suffix("분"),
                );
            }
            ui.add(
                egui::TextEdit::singleline(&mut self.reason)
                    .hint_text("이유")
                    .desired_width(260.0),
            );
            if ui.add_enabled(ready, egui::Button::new("보내기")).clicked() {
                let request = match self.request {
                    0 => ChangeKind::Done,
                    1 => ChangeKind::AlreadyDone,
                    2 => ChangeKind::Changed,
                    3 => ChangeKind::Extend {
                        minutes: self.extend_minutes,
                    },
                    4 => ChangeKind::NotFinished,
                    5 => ChangeKind::Repick,
                    _ => ChangeKind::End,
                };
                let reason = self.reason.clone();
                self.act(
                    session,
                    json!({ "kind": "request", "request": request, "reason": reason }),
                );
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("할 일 추가:");
            ui.add(
                egui::TextEdit::singleline(&mut self.todo_title)
                    .hint_text("제목")
                    .desired_width(220.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.todo_reason)
                    .hint_text("이유")
                    .desired_width(200.0),
            );
            let filled = !self.todo_title.trim().is_empty();
            if ui
                .add_enabled(ready && filled, egui::Button::new("보내기"))
                .clicked()
            {
                let change = TodoChange::Add {
                    todo: ProposalInput {
                        title: self.todo_title.trim().to_string(),
                        ..Default::default()
                    },
                };
                let reason = self.todo_reason.clone();
                self.act(
                    session,
                    json!({ "kind": "todo_change", "change": change, "reason": reason }),
                );
                self.todo_title.clear();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("약속 추가:");
            ui.add(
                egui::TextEdit::singleline(&mut self.appointment_title)
                    .hint_text("제목")
                    .desired_width(180.0),
            );
            ui.add(egui::TextEdit::singleline(&mut self.appointment_start).desired_width(60.0));
            ui.add(
                egui::DragValue::new(&mut self.appointment_minutes)
                    .range(10..=600)
                    .speed(5)
                    .suffix("분"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.appointment_reason)
                    .hint_text("이유")
                    .desired_width(160.0),
            );
            let start = self.virtual_moment(&self.appointment_start);
            let filled = !self.appointment_title.trim().is_empty() && start.is_some();
            if ui
                .add_enabled(ready && filled, egui::Button::new("보내기"))
                .clicked()
            {
                if let Some(start) = start {
                    let change = json!({
                        "kind": "add_appointment",
                        "appointment": {
                            "title": self.appointment_title.trim(),
                            "start": start,
                            "end": start + Duration::minutes(self.appointment_minutes),
                        },
                    });
                    let reason = self.appointment_reason.clone();
                    self.act(
                        session,
                        json!({ "kind": "todo_change", "change": change, "reason": reason }),
                    );
                    self.appointment_title.clear();
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("자동으로 진행:");
            ui.add(egui::TextEdit::singleline(&mut self.auto_until).desired_width(60.0));
            ui.label("까지, 변수");
            ui.radio_value(&mut self.variability, Variability::None, "없음");
            ui.radio_value(&mut self.variability, Variability::Some, "가끔");
            ui.radio_value(&mut self.variability, Variability::Often, "자주");
            let until = self.virtual_moment(&self.auto_until);
            if view.running {
                if ui.button("멈추기").clicked() {
                    self.send(session, "simulation_stop", json!({}));
                }
            } else if ui
                .add_enabled(until.is_some(), egui::Button::new("자동으로 진행"))
                .on_hover_text(format!(
                    "할 일을 받고, 정한 변수에 따라 일찍·제때·늦게 끝내거나 안 하거나 감을 못 잡으며 \
                     그 시각까지 진행합니다. 한 번에 {}번까지 진행합니다.",
                    task_planner::MAX_AUTO_STEPS
                ))
                .clicked()
            {
                if let Some(until) = until {
                    let variability = self.variability;
                    self.act(
                        session,
                        json!({ "kind": "auto", "until": until, "variability": variability }),
                    );
                }
            }
        });
        if ui
            .add_enabled(ready, egui::Button::new("시뮬레이션 끝내기"))
            .clicked()
        {
            self.send(session, "simulation_end", json!({}));
            self.view = None;
        }
    }
}

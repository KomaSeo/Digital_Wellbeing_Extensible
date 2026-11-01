//! Responsibility: Draw the AI assistant tab: the user writes what they want in their own
//! words ("장보기랑 빨래 할 일에 올려 줘"), the agent's AI assistant does it with the app's
//! tools, and the tab shows the conversation with every tool it used and the app's answer.
//!
//! The turn runs in the agent (see `machine::assistant`); this tab sends the text, then asks
//! for the conversation every second until the turn is over.

use crate::RED;
use eframe::egui;
use machine::assistant::{AssistantView, ChatRole};
use machine::user_api::call_value;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const VIEW_POLL: Duration = Duration::from_secs(1);

/// Responsibility: Keep the conversation the agent last sent, what the user is typing, and
/// the background requests of the AI assistant tab.
pub struct AssistantTab {
    session: u32,
    context: egui::Context,
    /// The conversation, or why the agent could not be asked.
    view: Arc<Mutex<Option<Result<AssistantView, String>>>>,
    input: String,
    /// A thread is asking the agent (sending, or waiting for the turn to end).
    asking: Arc<AtomicBool>,
    /// Lines shown at the last frame; new lines scroll the input box into view.
    lines_shown: usize,
}

impl AssistantTab {
    pub fn new(session: u32, context: egui::Context) -> Self {
        Self {
            session,
            context,
            view: Arc::new(Mutex::new(None)),
            input: String::new(),
            asking: Arc::new(AtomicBool::new(false)),
            lines_shown: 0,
        }
    }

    /// Call `method` on a background thread, then keep reading the conversation while the
    /// agent is still answering.
    fn ask(&self, method: &'static str, params: Value) {
        if self.asking.swap(true, Ordering::SeqCst) {
            return;
        }
        let (session, view, asking, context) = (
            self.session,
            Arc::clone(&self.view),
            Arc::clone(&self.asking),
            self.context.clone(),
        );
        thread::spawn(move || {
            let mut next = (method, params);
            loop {
                let answer = call_value(session, next.0, next.1).and_then(|value| {
                    serde_json::from_value::<AssistantView>(value)
                        .map_err(|error| error.to_string())
                });
                let busy = answer.as_ref().is_ok_and(|current| current.busy);
                match answer {
                    Ok(current) => *view.lock() = Some(Ok(current)),
                    // A refused send keeps the conversation and shows why.
                    Err(error) => {
                        let mut shown = view.lock();
                        match shown.as_mut() {
                            Some(Ok(current)) if next.0 != "assistant_view" => {
                                current.lines.push(machine::assistant::ChatLine {
                                    at: chrono::Utc::now(),
                                    role: ChatRole::Error,
                                    text: error,
                                });
                            }
                            _ => *shown = Some(Err(error)),
                        }
                    }
                }
                context.request_repaint();
                if !busy {
                    break;
                }
                thread::sleep(VIEW_POLL);
                next = ("assistant_view", json!({}));
            }
            asking.store(false, Ordering::SeqCst);
            context.request_repaint();
        });
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if self.view.lock().is_none() {
            self.ask("assistant_view", json!({}));
        }
        ui.heading("AI 비서");
        ui.label(
            "이 탭에서는 하고 싶은 일을 말로 적으면 AI 비서가 이 프로그램의 기능으로 대신 \
             해 줍니다. 예를 들면 \"장보기, 빨래, 은행 가기를 할 일에 올려 줘\", \"남은 할 일 \
             보여 줘\", \"3장 초안 다 썼어\", \"앞으로 일주일 얼마나 바빠?\"처럼 적습니다.",
        );
        ui.small(
            "AI 비서는 할 일 탭, 차단 탭, 통계 탭에서 사용자가 할 수 있는 일만 합니다. 할 일 \
             추가와 변경 요청은 지금처럼 이 프로그램의 AI가 생활 규칙에 비추어 심사합니다. 다음에 \
             할 일은 AI 비서도 고르지 않고 이 프로그램의 AI가 정합니다. 대화는 이 PC의 agent \
             메모리에만 있어서, 로그온을 다시 하거나 \"새 대화\"를 누르면 사라집니다.",
        );
        ui.separator();

        let view = self.view.lock().clone();
        let current = match view {
            None => {
                ui.label("agent에 연결하는 중…");
                return;
            }
            Some(Err(error)) => {
                ui.colored_label(RED, error);
                if ui.button("다시 연결하기").clicked() {
                    *self.view.lock() = None;
                }
                return;
            }
            Some(Ok(current)) => current,
        };
        if current.lines.is_empty() {
            ui.label("아직 대화가 없습니다.");
        }
        for line in &current.lines {
            let time = line
                .at
                .with_timezone(&chrono::Local)
                .format("%H:%M")
                .to_string();
            match line.role {
                ChatRole::User => {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.small(time);
                        ui.strong("나:");
                        ui.add(egui::Label::new(&line.text).wrap());
                    });
                }
                ChatRole::Assistant => {
                    ui.horizontal_wrapped(|ui| {
                        ui.small(time);
                        ui.strong("AI 비서:");
                        // The tab shows plain text; bold marks that slip through are dropped.
                        ui.add(egui::Label::new(line.text.replace("**", "")).wrap());
                    });
                }
                ChatRole::Action => {
                    ui.indent(("action", line.at.timestamp_micros()), |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("· {}", line.text))
                                    .small()
                                    .color(egui::Color32::GRAY),
                            )
                            .wrap(),
                        );
                    });
                }
                ChatRole::Error => {
                    ui.colored_label(RED, format!("{time} {}", line.text));
                }
            }
        }

        ui.add_space(8.0);
        let waiting = current.busy || self.asking.load(Ordering::SeqCst);
        if current.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("AI 비서가 답하는 중입니다. 할 일을 여러 개 심사하면 1분 넘게 걸릴 수 있습니다.");
            });
        }
        let input = ui.add(
            egui::TextEdit::multiline(&mut self.input)
                .hint_text("여기에 적으세요. Ctrl+Enter로도 보낼 수 있습니다.")
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        let shortcut = input.has_focus()
            && ui.input(|input_state| {
                input_state.key_pressed(egui::Key::Enter) && input_state.modifiers.command
            });
        ui.horizontal(|ui| {
            let can_send = !waiting && !self.input.trim().is_empty();
            let send_clicked = ui
                .add_enabled(can_send, egui::Button::new("보내기"))
                .clicked();
            if send_clicked || (shortcut && can_send) {
                let text = self.input.trim().to_string();
                self.input.clear();
                self.ask("assistant_send", json!({ "text": text }));
            }
            if ui
                .add_enabled(
                    !waiting && !current.lines.is_empty(),
                    egui::Button::new("새 대화"),
                )
                .on_hover_text(
                    "지금까지의 대화를 지우고 새로 시작합니다. 이미 한 일은 그대로 남습니다.",
                )
                .clicked()
            {
                self.ask("assistant_reset", json!({}));
            }
        });
        if current.lines.len() != self.lines_shown {
            self.lines_shown = current.lines.len();
            input.scroll_to_me(Some(egui::Align::Max));
        }
    }
}

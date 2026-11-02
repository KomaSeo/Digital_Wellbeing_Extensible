//! Responsibility: Ask the user what to do about a block the AI just made: it was a mistake,
//! the app or site is needed (an access request), it belongs on the to-do list, or it is
//! urgent and should become the current task. The window comes up by itself when the agent
//! reports a new block (`Status::last_block`).
//!
//! Every way out goes through the AI as before: access requests and to-do changes are reviewed
//! as usual, and "urgent" is a change request to a task off the list (`switch_free`) that the
//! AI approves only for something urgent. An approved urgent task runs under its own block
//! list, so the blocks of the to-do it replaced do not apply to it.

use crate::{FocusApp, Tab, GREEN, RED};
use ai_focus_rule::NewBlock;
use eframe::egui;
use serde_json::json;
use std::sync::atomic::Ordering;

/// Responsibility: Hold the block being asked about and what the user types for an urgent
/// switch.
pub(crate) struct BlockPrompt {
    block: NewBlock,
    /// The user chose "urgent": show the task and reason fields.
    urgent: bool,
    urgent_task: String,
    urgent_reason: String,
    /// When the urgent request was sent, to show its answer.
    sent_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl BlockPrompt {
    pub(crate) fn new(block: NewBlock) -> Self {
        let urgent_task = what_it_is(&block);
        Self {
            block,
            urgent: false,
            urgent_task,
            urgent_reason: String::new(),
            sent_at: None,
        }
    }
}

/// A short name for the blocked app or site: the tab title without the browser's name, or
/// the program name.
fn what_it_is(block: &NewBlock) -> String {
    if block.is_tab {
        ai_focus_rule::strip_browser_noise(&block.title)
            .trim()
            .to_string()
    } else {
        block.process_name.trim_end_matches(".exe").to_string()
    }
}

impl FocusApp {
    /// The prompt window, over whatever tab is open.
    pub(crate) fn block_prompt_ui(&mut self, context: &egui::Context) {
        let Some(mut prompt) = self.block_prompt.take() else {
            return;
        };
        let idle = !self.pending.load(Ordering::SeqCst);
        let mut keep = true;
        egui::Window::new("방금 차단했습니다")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(context, |ui| {
                ui.strong(if prompt.block.is_tab {
                    format!("웹사이트: {}", prompt.block.title)
                } else {
                    format!("앱: {}", prompt.block.process_name)
                });
                ui.label(format!("AI가 본 까닭: {}", prompt.block.reason));
                ui.small(if prompt.block.is_tab {
                    "탭을 닫았습니다. 허용되면 브라우저에서 Ctrl+Shift+T로 닫힌 탭을 다시 열 수 있습니다."
                } else {
                    "앱을 껐습니다. 허용되면 다시 켤 수 있습니다."
                });
                ui.separator();
                if !prompt.urgent {
                    ui.label("어떻게 할까요?");
                    if ui.button("실수였어요(창 닫기)").clicked() {
                        keep = false;
                    }
                    if ui.button("이게 필요해요: 접근 요청하기(AI가 정함)").clicked() {
                        self.prefill_access(&prompt.block.key);
                        self.tab = Tab::Blocking;
                        keep = false;
                    }
                    if ui.button("나중에 할 일로 넣기(AI가 정함)").clicked() {
                        let title = what_it_is(&prompt.block);
                        self.tab = Tab::Todo;
                        self.open_add_form(&title);
                        keep = false;
                    }
                    if ui.button("급한 일이라 지금 할 일을 이것으로 바꾸기(AI가 정함)").clicked() {
                        prompt.urgent = true;
                    }
                    return;
                }
                ui.label("지금 할 급한 일");
                ui.add(
                    egui::TextEdit::singleline(&mut prompt.urgent_task)
                        .desired_width(f32::INFINITY),
                );
                ui.label("왜 지금 해야 하나요? AI가 정말 급한지 봅니다");
                ui.add(
                    egui::TextEdit::multiline(&mut prompt.urgent_reason)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                let ready = idle
                    && prompt.urgent_task.trim().chars().count() >= 2
                    && prompt.urgent_reason.trim().chars().count() >= 4;
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready, egui::Button::new("AI에게 보내기")).clicked() {
                        prompt.sent_at = Some(chrono::Utc::now());
                        self.send(
                            "request_change",
                            json!({
                                "request": { "kind": "switch_free", "task": prompt.urgent_task.trim() },
                                "reason": format!("급한 일: {}", prompt.urgent_reason.trim()),
                            }),
                        );
                    }
                    if ui.button("돌아가기").clicked() {
                        prompt.urgent = false;
                    }
                    if !idle {
                        ui.spinner();
                    }
                });
                let answer = prompt.sent_at.and_then(|sent_at| {
                    self.shared
                        .lock()
                        .decision
                        .clone()
                        .filter(|decision| decision.at >= sent_at)
                });
                if let Some(decision) = answer {
                    ui.horizontal_wrapped(|ui| {
                        if decision.approved {
                            ui.colored_label(GREEN, "승인");
                        } else {
                            ui.colored_label(RED, "거절");
                        }
                        ui.label(&decision.reply);
                    });
                    if decision.approved {
                        ui.small("지금 할 일이 바뀌었습니다. 이 일에는 앞의 할 일의 차단이 적용되지 않습니다.");
                    }
                    if ui.button("닫기").clicked() {
                        keep = false;
                    }
                }
                let message = self.shared.lock().message.clone();
                if !message.is_empty() {
                    ui.colored_label(RED, message);
                }
            });
        if keep {
            self.block_prompt = Some(prompt);
        }
    }
}

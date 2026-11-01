//! Responsibility: Show the usage guides in Korean (the Help tab).

use eframe::egui;

/// The Help tab: one collapsible guide per topic.
pub(crate) fn help_tab(ui: &mut egui::Ui) {
    ui.heading("도움말");
    ui.label("이 탭은 이 프로그램을 쓰는 방법을 주제마다 설명합니다.");
    ui.separator();
    egui::CollapsingHeader::new("Claude Desktop으로 이 프로그램 쓰기")
        .default_open(true)
        .show(ui, claude_desktop_guide);
}

fn claude_desktop_guide(ui: &mut egui::Ui) {
    ui.label("");
}

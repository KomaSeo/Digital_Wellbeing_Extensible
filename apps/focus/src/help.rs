//! Responsibility: Show the usage guides in Korean (the Help tab). Each topic is a collapsible
//! guide; the owner dictates the topics, and new ones are added here.

use eframe::egui;

/// The configuration Claude Desktop needs, as it is pasted into `claude_desktop_config.json`.
const CLAUDE_DESKTOP_CONFIG: &str = r#"{
  "mcpServers": {
    "digital-wellbeing": {
      "command": "C:\\Program Files\\DigitalWellbeing\\digital-wellbeing-mcp.exe"
    }
  }
}"#;

/// The Help tab: one collapsible guide per topic.
pub(crate) fn help_tab(ui: &mut egui::Ui) {
    ui.heading("도움말");
    ui.label("이 탭은 이 프로그램을 쓰는 방법을 주제마다 설명합니다. 제목을 누르면 펼쳐집니다.");
    ui.separator();
    egui::CollapsingHeader::new("Claude Desktop으로 이 프로그램 쓰기")
        .default_open(true)
        .show(ui, claude_desktop_guide);
}

fn claude_desktop_guide(ui: &mut egui::Ui) {
    ui.strong("Claude Desktop이란");
    paragraph(
        ui,
        "Claude Desktop은 Anthropic이 만든 Claude 대화 앱입니다. 이 프로그램을 Claude Desktop에 \
         연결하면, 사용자는 Focus 앱을 열지 않고 Claude와 대화하면서 할 일을 추가하고, 지금 할 일을 \
         묻고, 할 일을 끝냈다고 알릴 수 있습니다. 연결은 MCP(Model Context Protocol, Claude가 다른 \
         프로그램의 기능을 쓰게 하는 방식)로 합니다.",
    );

    ui.add_space(6.0);
    ui.strong("1. 연결하기 (한 번만)");
    paragraph(
        ui,
        "1) 작업 계정에서 Claude Desktop을 엽니다. 설정 → 개발자(Developer) → 구성 편집(Edit \
         Config)을 누르면 claude_desktop_config.json 파일이 열립니다.",
    );
    paragraph(
        ui,
        "2) 파일에 아래 내용을 넣고 저장합니다. 파일에 이미 \"mcpServers\"가 있으면, 그 안에 \
         \"digital-wellbeing\" 항목만 더합니다.",
    );
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::multiline(&mut CLAUDE_DESKTOP_CONFIG.to_string())
                .code_editor()
                .desired_rows(7)
                .desired_width(f32::INFINITY),
        );
    });
    if ui.button("이 내용 복사하기").clicked() {
        ui.ctx().copy_text(CLAUDE_DESKTOP_CONFIG.to_string());
    }
    paragraph(
        ui,
        "3) Claude Desktop을 완전히 끄고(트레이 아이콘에서 종료) 다시 켭니다. 대화창의 도구 목록에 \
         digital-wellbeing이 보이면 연결된 것입니다.",
    );

    ui.add_space(6.0);
    ui.strong("2. 이렇게 말하면 됩니다");
    for (said, what_happens) in [
        (
            "\"지금 할 일이 뭐야?\", \"남은 할 일 보여 줘\"",
            "Claude가 지금 할 일과 할 일 목록을 보여 줍니다.",
        ),
        (
            "\"'지도교수 메일 답장'을 할 일에 넣어 줘. 금요일까지야.\"",
            "Claude가 할 일 추가를 요청합니다. 이 프로그램의 AI가 생활 규칙에 비추어 심사하고, 비워 둔 \
             칸(메모, 중요도, 예상 시간)을 채웁니다.",
        ),
        (
            "\"'3장 초안' 마감이 다음 주 수요일로 미뤄졌어.\"",
            "Claude가 이유와 함께 할 일 고치기를 요청합니다. 이 프로그램의 AI가 심사합니다.",
        ),
        (
            "\"할 일 하나 줘\"",
            "이 프로그램의 AI가 생활 규칙과 할 일 목록을 보고 지금 할 일을 정합니다.",
        ),
        (
            "\"3장 초안 다 썼어. 끝났다고 해 줘.\"",
            "Claude가 '끝냈다' 요청을 보냅니다. 메일 보내기처럼 결과가 분명한 일이면 증거(보낸 시각, 받는 \
             사람, 제목 등)를 함께 말하세요. 화면 캡처는 Focus 앱의 할 일 탭에서만 붙일 수 있습니다.",
        ),
        (
            "\"자료가 아직 안 와서 막혔어. 다른 일 줘.\"",
            "Claude가 이유와 함께 '다른 일 받기'를 요청합니다. 무엇을 할지는 이 프로그램의 AI가 정합니다. \
             사용자도 Claude도 다음 할 일을 고르지 않습니다.",
        ),
        (
            "\"생활 규칙에 '일요일 오후에는 운동이나 쉬기만 한다'를 넣어 줘.\"",
            "설정이 보호된 뒤에만 됩니다. Claude가 설정 업데이트를 예약하고, 사용자가 설정 탭에서 이틀 \
             간격으로 세 번 \"예\"라고 답해야 적용됩니다.",
        ),
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            ui.strong(said);
        });
        ui.indent(said, |ui| paragraph(ui, what_happens));
    }

    ui.add_space(6.0);
    ui.strong("3. Claude Desktop으로 할 수 없는 것");
    for line in [
        "다음에 할 일을 직접 고르기: 결정은 언제나 이 프로그램의 AI가 합니다. 사용자가 고르면 게을러지기 \
         때문입니다.",
        "차단을 바로 풀기, 설정 보호 끄기: 그런 도구가 없습니다. 차단된 앱이나 사이트가 바로 필요하면 Focus \
         앱의 차단 탭에서 목적을 적어 접근을 요청하세요.",
        "확인 질문에 답하기: 예약된 업데이트의 \"이 업데이트가 정말 맞나요?\" 질문에는 사용자만 Focus 앱의 \
         설정 탭에서 답할 수 있습니다.",
        "AI가 거절한 할 일 변경을 AI 없이 하기: 사용자만 Focus 앱의 할 일 탭에서 할 수 있고, 두 시간 \
         간격으로 세 번 확인해야 적용됩니다.",
        "증거 화면 캡처 붙이기: Focus 앱의 할 일 탭에서만 할 수 있습니다.",
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            paragraph(ui, line);
        });
    }

    ui.add_space(6.0);
    ui.strong("4. 알아 두면 좋은 것");
    paragraph(
        ui,
        "Claude Desktop은 Focus 앱과 같은 길(이 PC의 agent)로만 이 프로그램에 닿습니다. 그래서 Claude를 \
         연결해도 차단을 피할 새 길은 생기지 않습니다. 이 프로그램의 AI가 요청을 거절하면, Claude는 그 \
         이유를 전하고 말을 바꿔 다시 요청하지 않습니다. 자세한 설명은 설치 폴더의 \
         docs\\claude_desktop.md에 있습니다.",
    );
}

/// Wrapped text that fills the width.
fn paragraph(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(text).wrap());
}

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

/// The Help tab: one collapsible guide per topic. `true` when the user asked to open the
/// Settings tab.
pub(crate) fn help_tab(ui: &mut egui::Ui) -> bool {
    ui.heading("도움말");
    ui.label("이 탭은 이 프로그램을 쓰는 방법을 주제마다 설명합니다. 제목을 누르면 펼쳐집니다.");
    ui.separator();
    let mut open_settings = false;
    egui::CollapsingHeader::new("설치하기와 업데이트하기")
        .default_open(false)
        .show(ui, install_guide);
    egui::CollapsingHeader::new("우회 막기 (이 프로그램이 막는 것과 관리자가 할 일)")
        .default_open(false)
        .show(ui, bypass_guide);
    egui::CollapsingHeader::new("설정 잠그기 (설정 보호)")
        .default_open(true)
        .show(ui, |ui| open_settings = protection_guide(ui));
    egui::CollapsingHeader::new("할 일 받기와 완료 보고 (할 일이 없을 때 잠기는 이유)")
        .default_open(false)
        .show(ui, task_flow_guide);
    egui::CollapsingHeader::new("AI 비서 탭으로 말로 쓰기")
        .default_open(false)
        .show(ui, assistant_guide);
    egui::CollapsingHeader::new("Claude Desktop으로 이 프로그램 쓰기")
        .default_open(false)
        .show(ui, claude_desktop_guide);
    egui::CollapsingHeader::new("모든 앱이 잠겼을 때 (AI 연결이 끊겼을 때)")
        .default_open(false)
        .show(ui, lockdown_guide);
    egui::CollapsingHeader::new("카카오톡처럼 관리자 권한이 필요한 프로그램 업데이트하기")
        .default_open(false)
        .show(ui, updates_guide);
    open_settings
}

/// How to install and update the app.
fn install_guide(ui: &mut egui::Ui) {
    paragraph(
        ui,
        "이 프로그램은 Windows 서비스로 설치됩니다. 서비스는 관리자 권한(SYSTEM)으로 돌기 때문에, 일반 \
         계정은 서비스를 끄거나 설정 파일을 직접 고치지 못합니다. 설치에는 관리자 비밀번호가 필요합니다.",
    );
    ui.horizontal_wrapped(|ui| {
        ui.label("1. 설치 파일은 GitHub의 릴리스 페이지에 있습니다:");
        ui.hyperlink_to(
            "릴리스 페이지 열기",
            "https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases",
        );
    });
    paragraph(
        ui,
        "2. 작업에 쓰는 Windows 계정을 표준 사용자로 바꿉니다(Windows 설정 → 계정 → 가족 및 기타 사용자 → \
         계정 유형 변경). 작업 계정이 관리자이면 이 프로그램의 보호를 모두 풀 수 있습니다.",
    );
    paragraph(
        ui,
        "3. DigitalWellbeing-Setup-….exe를 내려받아 실행하고, 관리자 비밀번호를 넣습니다. 설치 프로그램은 \
         계정을 묻지 않고, 관리자 계정을 포함한 이 PC의 모든 계정을 관리합니다.",
    );
    paragraph(
        ui,
        "4. 작업 계정으로 로그인하면 이 앱(Focus 앱)이 뜹니다. 설정 탭의 AI 연결에서 Anthropic API 키를 \
         넣고 적용하고, 모델, 생활 규칙, 약속 규칙, 항상 막을 프로그램과 웹사이트를 정합니다. 할 일은 할 일 \
         탭에서 넣습니다. 설치 직후는 설정 단계라서 적용한 설정이 바로 효력을 가집니다.",
    );
    paragraph(
        ui,
        "5. 며칠 써 보고 설정이 원하는 대로 동작하면, 설정 탭 맨 위에서 설정을 보호합니다(아래 \"설정 \
         잠그기\"). 보호는 되돌릴 수 없습니다.",
    );
    paragraph(
        ui,
        "업데이트: 새 버전의 설치 파일을 그냥 실행하면 됩니다. 설정, API 키, 설정 보호, 걸린 차단은 그대로 \
         남습니다. 프로그램을 지웠다가 다시 설치해도, 예전 설치가 잠가 둔 설정 폴더는 그대로 씁니다.",
    );
    paragraph(
        ui,
        "설치 프로그램 없이 쓰려면 …-windows-x64.zip을 풀고, 그 안의 install.cmd를 관리자 권한으로 \
         실행합니다. 자세한 설명은 설치 폴더(C:\\Program Files\\DigitalWellbeing)의 \
         docs\\multi_account.md 4장에 있습니다.",
    );
}

/// What the app does against getting around it, and what the administrator must do.
fn bypass_guide(ui: &mut egui::Ui) {
    paragraph(ui, "이 프로그램이 스스로 막는 것:");
    for line in [
        "• 서비스와 agent는 SYSTEM 권한으로 돌므로, 작업 관리자나 서비스 창에서 끌 수 없습니다. agent가 \
         꺼지면 서비스가 2초 안에 다시 띄웁니다. 다시 로그인하거나 컴퓨터를 다시 켜도 차단은 이어집니다.",
        "• 설정 폴더(C:\\ProgramData\\DigitalWellbeing)는 SYSTEM과 관리자만 열 수 있고, 프로그램 \
         폴더(C:\\Program Files\\DigitalWellbeing)는 관리자만 고치거나 지울 수 있습니다.",
        "• 설정을 보호한 뒤에는 규칙을 느슨하게 만드는 변경이 바로 적용되지 않습니다. 이틀 간격으로 세 번 \
         \"예\"라고 답해야 하므로 빨라도 6일 뒤에 적용됩니다. 순간적인 충동으로 바꿔도 얻는 것이 없습니다.",
        "• 지금 할 일은 AI가 정하고, 할 일 목록의 변경, 다른 일 받기, 접근 요청, 약속도 AI가 생활 규칙과 \
         약속 규칙에 따라 심사합니다.",
        "• 설정이 보호된 동안 API 키가 동작하지 않으면(키를 지우거나 취소하거나 크레딧을 비우면), Claude와 \
         이 앱 말고는 모든 앱과 웹사이트가 잠깁니다. 모델도 목록에서만 고를 수 있고, 적용하기 전에 모델이 \
         있는지 확인합니다.",
        "• 할 일이 없을 때, 컴퓨터 없이 하는 일(잠자기, 운동)을 하는 동안, \"사용 중지\"를 누른 뒤에는 모든 \
         앱과 웹사이트가 잠깁니다.",
        "• 시간대를 바꿔 시간을 속이는 것을 막습니다. 시간대가 바뀌면 서비스가 설정의 시간대로 되돌립니다. \
         시계 자체를 바꾸는 데는 관리자 권한이 필요합니다.",
        "• 관리자 권한이 필요한 프로그램 업데이트(예: 카카오톡)는 설정의 목록에 있는 프로그램만 서비스가 \
         대신 합니다. 사용자는 관리자 권한을 받지 않습니다.",
    ] {
        paragraph(ui, line);
    }
    ui.add_space(6.0);
    paragraph(
        ui,
        "관리자가 직접 해야 하는 것(이 프로그램은 Windows가 정상으로 켜진 뒤에만 동작합니다):",
    );
    for line in [
        "• 작업 계정은 표준 사용자로 두고, 관리자 비밀번호는 길게 만들어 쉽게 꺼낼 수 없는 곳(다른 사람)에 \
         맡깁니다.",
        "• BIOS(UEFI)에 관리자 비밀번호를 걸고, USB와 네트워크 부팅을 끄고, Secure Boot를 켭니다. 디스크를 \
         빼서 읽는 것까지 막으려면 BitLocker로 암호화합니다.",
        "• 안전 모드로 들어가는 길(Shift+다시 시작의 고급 시작 옵션)을 끕니다. 관리자 PowerShell에서 \
         reagentc /disable과 bcdedit 명령을 실행합니다. 명령은 docs\\multi_account.md 4.4절에 있습니다.",
        "• Microsoft 계정의 가족 기능이나 공유기에서 다른 기기(휴대폰)의 사용을 따로 막습니다. 이 프로그램은 \
         이 PC만 지킵니다.",
    ] {
        paragraph(ui, line);
    }
    ui.add_space(6.0);
    paragraph(
        ui,
        "남는 한계: 웹사이트는 탭 제목으로 판단하므로, 제목에 \"Claude\"가 든 다른 페이지는 잠금 중에도 \
         열릴 수 있습니다. 네트워크를 끄는 것은 막지 않지만, 네트워크가 끊기면 대부분의 웹사이트도 쓸 수 \
         없습니다. 작업 계정은 설정 탭에서 API 키를 볼 수 있습니다.",
    );
}

/// How a to-do is given, reported and repeated, and why everything locks while none is given.
fn task_flow_guide(ui: &mut egui::Ui) {
    paragraph(
        ui,
        "1. 할 일 받기: 할 일 탭의 \"할 일 받기\" 버튼을 누르면, 이 프로그램의 AI가 생활 규칙과 할 일 \
         목록을 보고 지금 할 일과 그 할 일을 할 시간(10분~12시간)을 정합니다. 시간은 예상 시간에 묶이지 \
         않고 AI가 판단해서 짧게도 길게도 정합니다.",
    );
    paragraph(
        ui,
        "2. 할 일이 없을 때의 잠금: 설정이 보호되어 있고 맡은 할 일이 없으면, 이 프로그램은 모든 앱과 \
         웹사이트를 잠그고 사용자가 \"할 일 받기\"를 누르기를 기다립니다. 할 일을 받는 순간부터 시간이 \
         흐르므로, 자리를 비운 동안 시간이 지나가 버리지 않습니다. 이 잠금에서도 Claude Desktop과 이 앱은 \
         열 수 있습니다.",
    );
    paragraph(
        ui,
        "3. 완료 보고: 정해진 시간이 끝나면 할 일 탭에 \"완료 보고\"가 나옵니다. 사용자는 \"끝냈다\"(AI가 \
         심사합니다) 또는 \"다 못 했다\"(심사 없이 받아들여지고 할 일은 목록에 남습니다) 중 하나를 \
         보고해야 합니다. 보고하기 전에는 다른 일을 받거나 할 일을 그만둘 수 없습니다. 보고한 뒤에는 다시 \
         \"할 일 받기\"를 누를 때까지 할 일이 없습니다.",
    );
    paragraph(
        ui,
        "4. 반복하는 할 일: 잠자기, 출근 준비, 운동처럼 되풀이하는 일은 할 일을 추가할 때 \"반복하는 할 \
         일\"에 표시하세요. 반복하는 할 일은 끝내도 목록에서 사라지지 않습니다. 할 일 목록은 할 일마다 \
         지금까지 쓴 시간과 예상 시간을 함께 보여 줍니다.",
    );
    paragraph(
        ui,
        "5. 할 일별 차단: AI가 건 차단은 그 차단을 건 할 일에 속합니다. 할 일이 바뀌면 차단 목록도 그 할 \
         일의 것으로 바뀌고, 예전 할 일로 돌아오면 그 할 일의 차단이 다시 적용됩니다.",
    );
}

/// How to protect (lock) the settings; `true` when "설정 탭 열기" was pressed.
fn protection_guide(ui: &mut egui::Ui) -> bool {
    paragraph(
        ui,
        "설치한 직후는 설정 단계입니다. 설정 단계에서는 이 PC를 쓰는 누구나 설정을 바꿀 수 있고, \
         바꾼 것이 바로 적용됩니다. 설정을 잠그는 것을 이 프로그램은 \"설정 보호\"라고 부릅니다. 설정을 \
         보호하면, 규칙을 더 엄격하게 만드는 변경만 바로 적용되고, 그 밖의 변경은 이틀 간격으로 세 번 \
         \"예\"라고 답해야 적용됩니다.",
    );
    ui.add_space(4.0);
    ui.strong("잠그는 방법");
    for line in [
        "1) 설정 탭을 엽니다(아래 \"설정 탭 열기\" 버튼, 또는 창 맨 위의 \"설정\").",
        "2) 설정 탭 맨 위의 \"설정 보호(잠금)\" 제목 아래에 노란 글 \"설정 단계: …\"가 있습니다.",
        "3) 그 아래의 \"프로그램 안의 어떤 것도 보호를 끄지 못한다는 것을 이해했습니다.\" 칸을 \
         체크합니다. 체크하기 전에는 \"설정 보호하기\" 버튼이 흐리게 보이고 눌리지 않습니다.",
        "4) \"설정 보호하기\" 버튼을 누릅니다.",
        "5) 맨 위의 글이 \"설정이 보호되어 있습니다.…\"로 바뀌면 잠긴 것입니다. 할 일 탭 맨 위의 노란 줄도 \
         사라집니다. 이미 보호되어 있으면 버튼 대신 이 글이 보입니다.",
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            paragraph(ui, line);
        });
    }
    let open_settings = ui.button("설정 탭 열기").clicked();
    ui.add_space(4.0);
    ui.strong("잠그기 전에 확인할 것");
    for line in [
        "보호는 한 방향입니다. 프로그램 안의 어떤 것도 보호를 끄지 못합니다.",
        "처음 설치하면 설정은 보호되지 않은 설정 단계로 시작합니다. 설치 프로그램은 설정을 보호하지 \
         않으므로, 모두 정한 뒤에 위의 방법으로 직접 보호합니다. 업데이트하거나, 프로그램을 지웠다가 다시 \
         설치해도 설정 폴더(C:\\ProgramData\\DigitalWellbeing)가 남으므로 설정, API 키, 보호가 그대로 \
         이어집니다.",
        "API 키가 동작하는지 설정 탭 → AI 연결의 \"키 확인\"으로 확인하세요. 보호된 뒤에 AI 연결이 끊기면 \
         모든 앱과 웹사이트가 잠깁니다(아래 \"모든 앱이 잠겼을 때\").",
        "생활 규칙, 항상 막을 프로그램과 웹사이트, 업데이트할 프로그램 목록, 지킬 시간대를 미리 \
         정하세요. 보호된 뒤에 이것들을 느슨하게 바꾸면 빨라도 6일 뒤에 적용됩니다.",
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            paragraph(ui, line);
        });
    }
    open_settings
}

fn lockdown_guide(ui: &mut egui::Ui) {
    paragraph(
        ui,
        "설정이 보호된 뒤에 AI 연결이 끊기면(키가 없거나, 취소되었거나, 크레딧이 없을 때), 이 \
         프로그램은 연결될 때까지 거의 모든 앱과 웹사이트를 막습니다. 키를 망가뜨려 AI 검사를 끄는 \
         길을 막기 위해서입니다. 인터넷이 끊긴 것만으로는 잠기지 않습니다.",
    );
    for line in [
        "열어 둘 수 있는 것: Claude Desktop, 브라우저의 Claude·Anthropic 페이지와 빈 새 탭과 Google \
         로그인 페이지, Windows 설정, 이 앱. 다른 탭은 닫히고 다른 앱은 꺼집니다.",
        "푸는 방법: 아래 링크로 Claude Console에 들어가 새 키를 만들거나 크레딧을 채웁니다. 링크는 \
         브라우저에서 열리고, 잠긴 동안에도 Claude Console 페이지는 닫히지 않습니다. 새 키는 설정 탭 → AI 연결에 붙여 넣고 \"적용\"을 누릅니다. 잠긴 동안에는 \
         키 변경이 예약되지 않고 바로 적용되고, 몇 초 안에 잠금이 풀립니다.",
        "크레딧을 채운 경우에는 30초쯤 뒤에 저절로 풀립니다.",
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            paragraph(ui, line);
        });
    }
    console_links(ui);
}

/// Links to the Claude Console, where API keys are made and credit is bought.
pub(crate) fn console_links(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.hyperlink_to(
            "Claude Console의 API 키 화면 열기",
            "https://platform.claude.com/settings/keys",
        );
        ui.label("·");
        ui.hyperlink_to(
            "크레딧(결제) 화면 열기",
            "https://platform.claude.com/settings/billing",
        );
        ui.label("·");
        ui.hyperlink_to(
            "예전 주소(console.anthropic.com)",
            "https://console.anthropic.com/settings/keys",
        );
    });
}

fn updates_guide(ui: &mut egui::Ui) {
    paragraph(
        ui,
        "작업 계정에는 관리자 권한이 없으므로, 업데이트에 관리자 권한이 필요한 프로그램은 이 \
         프로그램의 서비스가 대신 업데이트합니다. 사용자에게 관리자 권한을 주지는 않습니다.",
    );
    for line in [
        "1) 명령 프롬프트에서 winget search 카카오톡 처럼 찾아 \"ID\" 칸의 이름(예: Kakao.KakaoTalk)을 \
         확인합니다.",
        "2) 설정 탭 → 시스템 → 프로그램 업데이트에 그 이름을 넣고 \"적용\"을 누릅니다. 설정이 보호되어 \
         있으면 넣기는 예약되어 세 번 확인한 뒤에 적용됩니다.",
        "3) 적용된 뒤에는 그 아래의 \"지금 업데이트\"를 누르면 됩니다. 하루 한 번 저절로 업데이트하기가 \
         켜져 있으면, 새벽 4시 이후 PC가 켜져 있는 첫 때에 저절로 업데이트합니다.",
        "프로그램이 스스로 업데이트하려고 관리자 비밀번호 창을 띄우면, 그 창은 취소하고 위의 \"지금 \
         업데이트\"를 쓰세요.",
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            paragraph(ui, line);
        });
    }
}

fn assistant_guide(ui: &mut egui::Ui) {
    paragraph(
        ui,
        "AI 비서 탭에서는 하고 싶은 일을 평소에 쓰는 말로 적습니다. 그러면 AI 비서(이 PC의 agent \
         안에서 도는 Claude)가 이 프로그램의 기능을 대신 씁니다. AI 비서는 Claude Desktop과 같은 \
         도구를 쓰므로, 할 수 있는 일과 할 수 없는 일도 Claude Desktop과 같습니다(아래 \
         \"Claude Desktop으로 이 프로그램 쓰기\"의 2번과 3번).",
    );
    ui.add_space(4.0);
    ui.strong("이렇게 쓰면 됩니다");
    for line in [
        "글을 적고 \"보내기\"를 누릅니다. Ctrl+Enter를 눌러도 보내집니다.",
        "\"장보기, 빨래 개기, 은행 가기를 할 일에 올려 줘\"처럼 한 번에 여러 개를 말해도 됩니다. \
         할 일마다 이 프로그램의 AI가 따로 심사합니다.",
        "AI 비서가 쓴 기능과 이 프로그램의 AI의 답(승인, 거절과 이유)은 회색 줄로 나옵니다.",
        "답이 오는 동안에는 다음 말을 보낼 수 없습니다. 할 일을 여러 개 심사하면 1분 넘게 걸릴 수 \
         있습니다.",
        "\"새 대화\"를 누르면 대화가 지워집니다. 이미 한 일(추가된 할 일 등)은 그대로 남습니다. \
         주제가 바뀌면 새 대화를 시작하는 편이 AI 비용이 적게 듭니다.",
        "AI 비서도 다음에 할 일을 고르지 않습니다. \"다른 일 줘\"라고 하면 이유와 함께 '다른 일 \
         받기'를 요청하고, 무엇을 할지는 이 프로그램의 AI가 정합니다.",
    ] {
        ui.horizontal_wrapped(|ui| {
            ui.label("•");
            paragraph(ui, line);
        });
    }
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
            "\"앞으로 일주일 얼마나 바빠?\"",
            "Claude가 앞으로 7일 계획을 요청합니다. 이 프로그램의 AI가 날마다 일에 써야 할 시간과 남는 \
             여유를 계산하고, 그 계획은 통계 탭에도 보입니다.",
        ),
        (
            "\"공부 자료 보려고 유튜브 열어 줘\"",
            "Claude가 그 목적으로 접근을 요청합니다. 이 프로그램의 AI가 지금 할 일과 생활 규칙에 비추어 \
             심사하고, 허용할 시간도 정합니다. 항상 막을 프로그램과 웹사이트는 풀리지 않습니다.",
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
        "차단을 바로 풀기, 설정 보호 끄기: 그런 도구가 없습니다. 차단된 앱이나 사이트가 바로 필요하면 \
         Claude에게 목적을 말해 접근을 요청하게 하거나, Focus 앱의 차단 탭에서 직접 요청하세요. \
         요청은 이 프로그램의 AI가 심사합니다.",
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

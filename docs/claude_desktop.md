# Claude Desktop 연결 설명서

이 문서는 사용자가 PC의 Claude Desktop에서 할 일을 관리할 수 있게 연결하는 방법을 설명합니다.
코드는 `apps/mcp/src/main.rs` 한 파일입니다.

---

## 0. 이 문서에서 쓰는 말

| 말 | 뜻 |
|---|---|
| **Claude Desktop** | 사용자가 PC에 설치해서 쓰는 Claude 앱입니다. |
| **MCP** | Claude 같은 AI 앱이 바깥 프로그램의 기능(도구)을 부를 때 쓰는 약속입니다. Model Context Protocol의 줄임말입니다. |
| **MCP 서버** | AI 앱에 도구를 제공하는 프로그램입니다. 이 프로젝트의 MCP 서버는 `digital-wellbeing-mcp.exe`입니다. |
| **agent** | 로그인한 사용자마다 하나씩 SYSTEM 권한으로 도는, 차단과 할 일 플래너를 맡은 프로그램입니다([`multi_account.md`](multi_account.md)). |
| **Focus 앱** | 사용자가 작업 계정에서 보는 이 프로그램의 창(`digital-wellbeing-focus.exe`)입니다. 화면은 한국어이고, **할 일**, **차단**, **설정**, **통계**, **도움말** 탭이 있습니다. 이 문서는 이 탭들을 할 일 탭, 차단 탭, 설정 탭, 통계 탭, 도움말 탭이라고 부릅니다. |
| **설정 탭** | Focus 앱의 설정 화면입니다. 사용자는 이 탭에서 생활 규칙, 지시문, 차단 규칙 같은 설정을 고칩니다. 고친 내용은 자동으로 임시 저장되고, 사용자가 그 설정 아래의 **적용**을 눌러야 설정이 됩니다([`multi_account.md`](multi_account.md) 6.4절). |
| **할 일 탭** | Focus 앱을 열면 처음 보이는 탭입니다. 사용자는 이 탭에서 지금 할 일을 보고, 변경 요청을 보내고, 할 일 목록을 고칩니다([`task_planner.md`](task_planner.md) 7.1절). |
| **도움말 탭** | Focus 앱에서 이 프로그램을 쓰는 방법을 주제마다 설명하는 탭입니다. 이 탭에는 "Claude Desktop으로 이 프로그램 쓰기" 안내가 있습니다(2장). |
| **데일리 웰빙(daily wellbeing)** | AI가 사용자의 창을 지금 할 일에 비추어 검사하는 상태입니다. 이 앱은 하루 종일 돌고, 사용자는 할 일 탭의 변경 요청 "AI 검사 멈추기"로 AI 검사를 멈출 수 있습니다(**AI 검사 멈추기**). AI가 그 이유를 승인해야 멈춥니다. |
| **설정 단계** | 프로그램을 설치한 직후부터 사용자가 설정을 보호할 때까지의 기간입니다. 이 기간에 설정 탭에서 적용한 변경은 바로 효력이 생깁니다. |
| **설정 보호** | 사용자가 설정 탭에서 **설정 보호하기**를 눌러 설정 단계를 끝내는 것입니다. 보호는 한 방향이고, 프로그램 안의 어떤 화면이나 도구도 보호를 끄지 못합니다. |
| **엄격한 변경과 느슨한 변경** | 규칙을 더 엄격하게 만드는 설정 변경(더 많이 막고, 덜 허용하고, 더 자주 검사하는 변경)을 **엄격한 변경**이라고 합니다. 그 밖의 변경과, 어느 쪽인지 알 수 없는 변경(API 키, 모델, 생활 규칙, 지시문, 브라우저 목록 등)을 **느슨한 변경**이라고 합니다. 설정이 보호된 뒤에는 엄격한 변경은 바로 적용되고, 느슨한 변경은 예약된 업데이트가 됩니다([`multi_account.md`](multi_account.md) 6.4절의 표). |
| **예약된 업데이트** | 설정이 보호된 뒤에 저장하거나 예약한 느슨한 변경의 묶음입니다. agent는 지금의 설정을 그대로 두고 그 변경을 보관합니다. 예약된 업데이트는 확인 질문(바로 아래 줄)을 거쳐야 적용됩니다([`multi_account.md`](multi_account.md) 6.4절). |
| **확인 질문** | Focus 앱이 예약된 업데이트마다 사용자에게 묻는 "이 업데이트가 정말 맞나요?"라는 질문입니다. 첫 질문은 예약하고 이틀 뒤에 오고, 다음 질문은 앞 질문에 "예"라고 답하고 이틀 뒤에 옵니다. 세 번 연속 "예"이면 업데이트가 적용되고, 한 번이라도 "아니요"이면 거절됩니다. 사용자만 Focus 앱에서 답할 수 있습니다. |
| **생활 규칙** | 사용자가 설정 탭의 **AI 규칙** 영역에 적는, 늘 따라야 하는 규칙입니다. 할 일 고르기, 변경 요청 심사, 할 일 변경 심사가 모두 이 규칙을 따릅니다([`task_planner.md`](task_planner.md) 5.4절). |
| **할 일 변경** | 할 일 목록에 할 일을 추가하거나, 할 일을 고치거나 지우겠다고 agent의 AI에게 보내는 요청입니다. AI는 생활 규칙에 비추어 심사하고, 승인한 변경은 바로 적용됩니다. 추가할 때 비워 둔 칸(메모, 마감, 중요도, 예상 시간)은 AI가 채웁니다([`task_planner.md`](task_planner.md) 4.6절). |
| **지시문** | agent의 플래너가 AI에게 질문할 때 함께 보내는, "무엇을 어떤 기준으로 판단하라"는 글입니다. 할 일 고르기, 변경 요청 심사, 할 일 변경 심사에 하나씩 있습니다. 프로그램에 기본 지시문이 들어 있고, 사용자는 설정에서 자기 지시문으로 바꿀 수 있습니다([`task_planner.md`](task_planner.md) 5.6절). |
| **변경 요청** | 지금 할 일을 끝냈다고 하거나, 다른 일을 받거나, AI 검사를 멈추겠다고 agent의 AI에게 이유를 적어 보내는 요청입니다. AI가 이유를 승인해야 그대로 됩니다([`task_planner.md`](task_planner.md) 4.3절). |
| **다른 일 받기** | 지금 할 일을 그만두고 다른 일을 받겠다는 변경 요청입니다(`repick`). 사용자는 이유만 말하고, 다음에 무엇을 할지는 고르지 않습니다. AI가 이유를 승인하면, agent의 AI가 그 이유를 보고 다음 할 일을 고릅니다. 사용자도 Claude도 다음 할 일을 고르지 않습니다. 사용자가 고르면 게을러지기 때문입니다. |
| **항상 막을 프로그램과 항상 막을 웹사이트** | 설정의 두 목록입니다. 항상 막을 프로그램(`policy.json`의 `blacklist`, 블랙리스트)의 프로그램은 켜질 때마다 꺼집니다. 항상 막을 웹사이트(`ai_focus.json`의 `always_blocked_sites`)는 브라우저 탭의 제목에 들어 있는 말이고, 그 말이 든 탭은 AI에게 묻지 않고 언제나 닫힙니다([`ai_focus_rule.md`](ai_focus_rule.md) 5.12절). |

---

## 1. 무엇을 할 수 있고 무엇을 할 수 없는가

Claude Desktop은 `digital-wellbeing-mcp.exe`를 작업 계정의 권한으로 실행합니다. 이 MCP 서버는 Focus 앱과 **같은 agent API만** 부릅니다.
그래서 Claude는 사용자가 Focus 앱으로 할 수 있는 일만 할 수 있고, 확인 질문에 답하는 일이나 거절된 할 일 변경을 AI 없이 하기처럼 그중 일부는 할 수 없습니다. Claude를 연결해도 차단을 우회할 길이 새로 생기지 않습니다.

| Claude가 하려는 것 | 결과 |
|---|---|
| 할 일 목록, 지금 할 일, 차단 목록 보기 (`get_status`) | ✅ 할 수 있습니다. |
| 할 일 추가하기 (`propose_todo`) | agent의 AI가 설정의 지시문과 생활 규칙으로 심사하고, 승인한 것만 목록에 들어갑니다. 승인된 할 일은 바로 목록에 들어갑니다. Claude가 적지 않은 칸은 AI가 채우고, Claude는 AI가 채운 할 일을 돌려받습니다. |
| 할 일 고치기, 지우기 (`change_todo`) | 이유를 적어 요청하고, agent의 AI가 심사합니다. 승인된 변경은 바로 적용됩니다. |
| 거절된 할 일 변경을 AI 없이 하기 | ❌ 그런 도구가 없습니다. 사용자만 Focus 앱의 할 일 탭에서 할 수 있고, 그 변경도 두 시간 간격으로 세 번 확인해야 적용됩니다. |
| 할 일 받기 (`get_my_task`) | ✅ 할 수 있습니다. 어떤 할 일을 줄지는 agent의 AI가 고릅니다. |
| 지금 할 일을 끝냈다고 하기, 다른 일 받기, AI 검사 멈추기 (`request_change`) | Focus 앱처럼 이유를 적어 요청하고, agent의 AI가 심사합니다. 다른 일 받기가 승인되면, 다음 할 일은 agent의 AI가 고릅니다. |
| 다음에 할 일을 직접 고르기 | ❌ 그런 도구가 없습니다. 결정은 언제나 agent의 AI가 합니다. 예전 버전의 `request_change`에는 목록의 다른 할 일(`switch_todo`)이나 목록에 없는 일(`switch_free`)로 바꾸는 종류가 있었지만, 지금은 없습니다. Claude가 그 종류를 보내면 도구가 "unknown change"라는 오류를 돌려줍니다. MCP 서버는 Claude에게 "사용자는 다음에 할 일을 고르지 않는다. Claude를 통해서도 고르지 않는다. 사용자가 지금 할 일을 그만두고 싶어 하면, Claude나 사용자가 고른 일이 아니라 `repick`과 사용자의 이유를 보내라"고 안내합니다(`INSTRUCTIONS`). |
| 설정 보기 (`get_settings`) | ✅ 할 수 있습니다. Claude는 설정이 보호되어 있는지, 지금 적용된 설정 파일 세 개, 예약된 업데이트를 더한 설정 파일, 기본 지시문, 예약된 업데이트와 각 업데이트가 바꾸는 내용과 받은 "예" 답의 수, 최근 업데이트의 결과를 봅니다. **API 키는 보여 주지 않습니다.** |
| 생활 규칙, 지시문, 차단 규칙 같은 설정 바꾸기 (`schedule_settings_update`) | 설정이 보호된 뒤에만 쓸 수 있고, 설정 탭의 적용과 똑같이 다뤄집니다. 엄격한 변경은 바로 적용되고, 느슨한 변경은 예약됩니다. 예약된 업데이트는 사용자가 Focus 앱에서 확인 질문에 이틀 간격으로 세 번 "예"라고 답해야 적용됩니다(빨라도 6일 뒤). 사용자가 한 번이라도 "아니요"라고 답하면 거절됩니다. 설정 단계에서는 이 도구가 거부합니다. 설정 단계의 설정은 사용자가 설정 탭에서 직접 고칩니다. |
| 확인 질문에 답하기 | ❌ 그런 도구가 없습니다. 확인 질문에는 사용자만 Focus 앱에서 답할 수 있습니다. 그래서 Claude가 예약한 업데이트도 사용자가 세 번 승인하기 전에는 아무것도 바꾸지 못합니다. |
| 차단 바로 풀기, 설정 보호 끄기 | ❌ 그런 도구가 없습니다. 차단을 푸는 설정 변경은 느슨한 변경이므로 예약된 업데이트가 되어 확인 질문을 거칩니다. 차단된 앱이나 사이트를 바로 써야 하면, 사용자는 Focus 앱의 차단 탭에서 목적을 적어 접근을 요청합니다([`access_grants.md`](access_grants.md)). 보호를 끄는 기능은 프로그램 어디에도 없습니다. |

**할 일을 추천하는 지시문은 설정의 일부입니다.** 추천은 Claude Desktop이 하지 않고, agent가 설정의 지시문으로 합니다. 설정에 자기 지시문이 없으면, agent는 프로그램에 들어 있는 기본 지시문(`PICK_PROMPT`)을 씁니다.
지시문과 생활 규칙은 다른 설정과 똑같이 바뀝니다. 설정 단계에서는 사용자가 설정 탭에서 고치고 적용하면 바로 효력이 생깁니다. 프로그램은 지시문과 생활 규칙의 변경이 엄격한지 느슨한지 판단할 수 없으므로, 설정이 보호된 뒤에는 사용자가 설정 탭에서 고치든 Claude가 `schedule_settings_update`로 예약하든, 그 변경은 사용자가 확인 질문에 이틀 간격으로 세 번 "예"라고 답한 뒤에야 적용됩니다.
그래서 놀고 싶은 순간에 Claude에게 "생활 규칙에서 게임 금지를 빼 줘"라고 해도, 그 변경은 빨라도 6일 뒤에 적용되고 그동안 사용자는 차분한 날에 세 번 거절할 기회가 있습니다.
MCP 서버는 Claude에게 "예약된 업데이트는 사용자가 앱에서 이틀 간격으로 세 번 확인해야 적용된다고 알리고, 확인하라고 재촉하지 말라"고 안내합니다(`INSTRUCTIONS`).

예를 들어 놀고 싶은 순간에 사용자가 Claude에게 "할 일에 '놀기' 넣어 줘"라고 하면 다음과 같이 됩니다.
1. Claude는 `propose_todo`로 "놀기"를 추가해 달라고 요청합니다.
2. agent의 AI는 생활 규칙(예: "평일 18시 전에는 게임과 영상을 할당하지 않는다")에 비추어 그 추가를 심사하고 거절합니다.
3. Claude는 거절 이유를 사용자에게 전합니다. MCP 서버는 Claude에게 "거절되면 이유를 전하고, 말을 바꿔 다시 시도하지 말라"고 안내합니다(`INSTRUCTIONS`). 도구 설명도 "거절된 변경은 사용자만 앱에서 밀어붙일 수 있다"고 알려 줍니다.
   Claude가 그래도 말을 바꿔 다시 요청하면, agent의 AI는 이전 할 일 변경 기록을 함께 보고 심사합니다. 또 하루에 보낼 수 있는 할 일 변경의 수에는 한도(30개)가 있습니다.
4. 사용자가 그래도 그 할 일이 필요하다고 생각하면, 사용자는 Focus 앱의 할 일 탭에서 **AI 없이 하기(두 시간 간격으로 세 번 확인)**를 누를 수 있습니다. 그 변경은 두 시간 간격으로 세 번 확인한 뒤에 적용됩니다. Claude는 이 일을 대신할 수 없습니다.

---

## 2. 설정 방법

Focus 앱의 도움말 탭에도 같은 안내("Claude Desktop으로 이 프로그램 쓰기")가 있습니다. 그 안내에는 아래 3번의 설정 내용과 **이 내용 복사하기** 버튼, Claude에게 하는 말의 예(3장), Claude Desktop으로 할 수 없는 것(1장)이 있습니다. 사용자는 이 버튼으로 설정 내용을 복사해서 붙여 넣으면 됩니다.

1. Digital Wellbeing을 설치합니다. 설치기가 `C:\Program Files\DigitalWellbeing\digital-wellbeing-mcp.exe`를 함께 복사합니다.
2. 작업 계정에서 Claude Desktop을 엽니다. **설정 → 개발자(Developer) → 구성 편집(Edit Config)**을 누르면 `claude_desktop_config.json` 파일이 열립니다.
   (보통 `%APPDATA%\Claude\claude_desktop_config.json`에 있습니다. Microsoft Store판 Claude Desktop은 다른 곳에 둘 수 있으므로, 이 메뉴로 여는 편이 확실합니다.)
3. 파일에 다음 내용을 넣습니다. 이미 `mcpServers`가 있다면 그 안에 `"digital-wellbeing"` 항목만 추가합니다.

   ```json
   {
     "mcpServers": {
       "digital-wellbeing": {
         "command": "C:\\Program Files\\DigitalWellbeing\\digital-wellbeing-mcp.exe"
       }
     }
   }
   ```
4. Claude Desktop을 완전히 끄고(트레이 아이콘에서 종료) 다시 켭니다. 대화창의 도구 목록에 `digital-wellbeing`이 보이면 연결된 것입니다.

이 설정 파일은 작업 계정의 사용자 폴더에 있으므로 사용자가 마음대로 고칠 수 있습니다. 하지만 1장에서 설명한 대로 MCP 서버에는 Focus 앱보다 큰 권한이 없으므로, 사용자가 이 파일을 고쳐도 얻는 것이 없습니다.

Claude Desktop으로 설정을 바꾸는 업데이트를 예약하려면, 사용자는 먼저 Focus 앱의 설정 탭에서 설정을 마치고 설정을 보호해야 합니다([`multi_account.md`](multi_account.md) 4.3절). 설정 단계에서는 `schedule_settings_update`가 "the settings are not protected yet (setup)…"라는 오류를 돌려줍니다.
Claude가 느슨한 변경을 보내면, 사용자는 이틀 뒤부터 Focus 앱의 설정 탭에서 확인 질문을 받습니다. 그 업데이트는 설정 탭의 "예약된 업데이트" 목록에 "예약한 곳: Claude Desktop"으로 표시됩니다. 엄격한 변경은 바로 적용됩니다.

---

## 3. 사용 예

| 사용자가 Claude에게 하는 말 | Claude가 부르는 도구 |
|---|---|
| "지금 할 일이 뭐야?", "남은 할 일 보여 줘" | `get_status` |
| "'지도교수 메일 답장'을 할 일에 넣어 줘. 금요일까지야." | `propose_todo` (`title`, `deadline`). 메모, 중요도, 예상 시간은 agent의 AI가 채웁니다. |
| "'3장 초안' 마감이 다음 주 수요일로 미뤄졌어. 지도교수님이 그러셨어." | `get_status`로 할 일의 `id`를 본 뒤 `change_todo` (`todo_id`, `deadline`, `reason`) |
| "'영어 단어 외우기'는 학원에서 이미 끝냈으니 목록에서 빼 줘." | `get_status`로 할 일의 `id`를 본 뒤 `change_todo` (`todo_id`, `remove: true`, `reason`) |
| "할 일 하나 줘" | `get_my_task` |
| "3장 초안 10쪽 다 썼어. 끝났다고 해 줘." | `request_change` (`change: "done"`, `reason`) |
| "집주인한테 계약서 메일 보냈어. 14시 2분에 landlord@example.com으로, 제목은 '서명한 계약서'야. 끝났다고 해 줘." | `request_change` (`change: "done"`, `reason`, `evidence`). 결과가 분명한 일은 agent의 AI가 증거를 요구합니다. Claude Desktop으로는 화면 캡처를 붙일 수 없으므로, AI가 화면 캡처를 요구하면 사용자는 Focus 앱의 할 일 탭에서 캡처를 붙여 다시 요청합니다([`task_planner.md`](task_planner.md) 4.9절). |
| "이 증명 거의 다 됐어. 시간 끝나도 새 할 일 주지 말고 계속하게 해 줘." | `request_change` (`change: "notify_when_time_is_up"`, `reason`). agent의 AI가 심사합니다. |
| "자료가 아직 안 와서 막혔어. 다른 일 줘." | `request_change` (`change: "repick"`, `reason`). Claude는 다음 할 일을 고르거나 제안하지 않습니다. agent의 AI가 승인하면, agent의 AI가 그 이유를 보고 다음 할 일을 고릅니다. |
| "'영어 단어 외우기' 대신 '3장 초안'을 할래." | Claude는 다음 할 일을 정해 주는 도구가 없다고 알립니다. 사용자가 지금 할 일을 그만두고 싶다면, Claude는 그 이유로 `request_change` (`change: "repick"`)를 보냅니다. 무엇을 할지는 agent의 AI가 정합니다. |
| "지금 생활 규칙이랑 할 일 고르는 지시문 보여 줘." | `get_settings` |
| "생활 규칙에 '일요일 오후에는 운동이나 쉬기만 고른다'를 넣어 줘." | `get_settings`로 `files_with_scheduled_updates.planner`(예약된 업데이트를 더한 `planner.json`)를 본 뒤, 생활 규칙만 고친 파일 전체로 `schedule_settings_update` (`name: "planner"`, `content`). Claude는 사용자에게 이틀 뒤부터 Focus 앱에서 세 번 확인해야 적용된다고 알립니다. |
| "변경 요청 심사를 조금 덜 엄격하게 바꾸고 싶어." | `get_settings`로 기본 지시문(`built_in_instructions.change_review`)을 본 뒤, 고친 지시문을 `instructions.change_review`에 넣은 `planner.json` 전체로 `schedule_settings_update`. 이 업데이트도 세 번의 확인을 거칩니다. |
| "`steam.exe`를 항상 막아 줘." | `get_settings`로 `files_with_scheduled_updates.policy`를 본 뒤, `blacklist`에 `steam.exe`를 넣은 파일 전체로 `schedule_settings_update` (`name: "policy"`). 블랙리스트에 넣는 것은 엄격한 변경이므로 바로 적용됩니다. 이름은 정규식(글자의 모양을 나타내는 식)으로 적어도 됩니다. 예를 들어 `.*game.*\.exe`는 이름에 game이 들어간 모든 프로그램을 뜻합니다([`multi_account.md`](multi_account.md) 6.2절). |
| "유튜브는 항상 막아 줘." | `get_settings`로 `files_with_scheduled_updates.ai_focus`를 본 뒤, `always_blocked_sites`에 `youtube`를 넣은 파일 전체로 `schedule_settings_update` (`name: "ai_focus"`). 항상 막을 웹사이트에 넣는 것은 엄격한 변경이므로 바로 적용됩니다. 빼는 것은 느슨한 변경이므로 예약됩니다. |

**Claude Docs의 할 일 목록과의 관계:** 이 프로그램은 Claude Docs 문서를 직접 읽지 않습니다. 할 일의 원본은 이 프로그램의 목록입니다.
Claude Docs에 적어 둔 할 일을 옮기고 싶다면, Claude에게 "문서의 할 일을 하나씩 추가해 줘"라고 하면 됩니다. 각 할 일은 따로 심사를 받습니다. 하루에 보낼 수 있는 할 일 변경은 30개까지입니다.

---

## 4. 도구 설명 (`tools()`)

| 도구 | 인자 | 하는 일 |
|---|---|---|
| `get_status` | 없음 | agent의 `status`를 부르고, 데일리 웰빙이 켜져 있는지(`daily_wellbeing_on`), 지금 할 일(계획된 끝 `until`과 시간이 다 되었을 때의 동작 `when_time_is_up` 포함), 남은 할 일(id, 제목, 메모, 마감, 중요도, 사용자가 추가했는지), 최근 할 일 변경과 AI의 답(`recent_todo_changes`), 최근 변경 요청, 차단 목록만 추려서 돌려줍니다. 판정 기록 같은 긴 내용은 뺍니다. |
| `propose_todo` | `title`(필수), `notes`, `deadline`(`YYYY-MM-DD`), `priority`(1~5), `estimate_minutes`, `reason`(선택) | agent의 `todo_change`를 할 일 추가(`kind: "add"`)로, `via: "claude desktop"`과 함께 부릅니다. 승인 여부(`approved`), AI의 답(`reply`), AI가 답하지 못했는지(`ai_could_not_review`), AI가 빈 칸을 채운 할 일(`todo`)을 돌려줍니다. 도구 설명은 Claude에게 "사용자가 말하지 않은 칸은 비워 두라. 앱의 AI가 채운다"고 안내합니다. |
| `change_todo` | `todo_id`(필수), `reason`(필수), `remove`, `title`, `notes`, `deadline`, `priority`, `estimate_minutes` | `remove`가 `true`이면 그 할 일을 지우는 요청(`kind: "remove"`)을, 아니면 준 칸만 바꾸는 요청(`kind: "edit"`)을 agent의 `todo_change`로 보냅니다. `propose_todo`와 같은 모양의 답을 돌려줍니다. 지우기라면 `todo`는 지울 할 일입니다. |
| `get_my_task` | 없음 | agent의 `assign`을 부르고, 할당된 할 일을 돌려줍니다. |
| `request_change` | `change`(`done`, `repick`, `end`, `notify_when_time_is_up`, `reassign_when_time_is_up`), `reason`(필수), `evidence`(`done`의 증거 글) | agent의 `request_change`를 부르고, 승인 여부, AI의 답, 바뀐 할당을 돌려줍니다. `repick`은 "지금 할 일을 그만두고 다른 일을 받는다"(다른 일 받기)이고, 다음 할 일은 agent의 AI가 고릅니다. 도구 설명은 Claude에게 "사용자는 이유만 말하고 다음 할 일을 고르지 않는다"고 알려 줍니다. 예전의 `switch_todo`와 `switch_free`, 그리고 그 인자 `todo_id`와 `task`는 없앴습니다. 이 종류를 보내면 도구가 "unknown change '…'" 오류를 돌려줍니다. agent API 자체는 이 두 종류를 아직 받지만, MCP 서버는 보내지 않습니다([`task_planner.md`](task_planner.md) 4.3절). |
| `get_settings` | 없음 | agent의 `settings_get`을 부르고 다음을 돌려줍니다. 설정이 보호되어 있는지(`protected`), 지금 적용된 설정 파일(`files`: `policy`는 차단 목록, `ai_focus`는 AI 집중 설정, `planner`는 할 일 목록·생활 규칙·지시문), 기본 지시문 네 개(`built_in_instructions`: 할 일 고르기, 변경 요청 심사, 할 일 변경 심사, 앞으로 7일 계획), 예약된 업데이트를 더한 설정 파일(`files_with_scheduled_updates`: 설정 탭이 고치기 시작하는 내용과 같습니다. 할 일 변경의 예약된 업데이트는 더하지 않습니다), 예약된 업데이트(`scheduled_updates`: 번호, 파일 이름, 예약한 곳, 출처 `origin`(`settings` 또는 `todo_change`), 예약한 시각, 받은 "예" 답의 수 `confirmations`, 바뀌는 설정을 한 줄에 하나씩 적은 `changes`), 최근 업데이트의 결과(`recent_outcomes`). `ai_focus`의 API 키(`api_key`)는 어디에서도 빼고 돌려줍니다. |
| `schedule_settings_update` | `name`(`policy`, `ai_focus`, `planner` 중 하나, 필수), `content`(새 설정 파일 전체, 필수) | 먼저 `settings_get`으로 설정이 보호되어 있는지 확인하고, 설정 단계이면 아무것도 보내지 않고 오류를 돌려줍니다. 보호되어 있으면 agent의 `settings_put`을 `via: "claude desktop"`으로 부릅니다. agent는 설정 탭의 적용과 똑같이, 엄격한 변경은 바로 적용하고 느슨한 변경은 예약합니다. 도구는 agent의 답(`applied`: 바로 적용된 변경, `scheduled`: 예약된 업데이트의 번호와 첫 질문 시각과 변경, 없으면 `null`)과 "사용자가 앱에서 세 번 확인해야 적용된다"는 안내를 돌려줍니다. `ai_focus`의 `content`에 `api_key`가 없으면, 지금 적용된 키를 그대로 넣어 보냅니다. 그래서 Claude는 키를 보지 않고도 `ai_focus.json`을 고칠 수 있습니다. |

agent가 요청을 거부하거나 agent에 연결할 수 없으면, 도구는 오류 표시(`isError: true`)와 함께 이유를 돌려줍니다. Claude는 그 이유를 사용자에게 전합니다.

---

## 5. 동작 방식 (`main.rs`)

- **통신:** Claude Desktop은 이 프로그램을 실행하고, 표준 입력과 표준 출력으로 한 줄에 하나씩 JSON-RPC 2.0 메시지를 주고받습니다(MCP의 stdio 방식).
  이 프로그램은 표준 출력에 응답만 씁니다. 로그를 표준 출력에 쓰면 Claude Desktop이 응답을 해석하지 못하기 때문입니다.
- **처리하는 메시지:** `initialize`(서버 이름, 버전, 도구 기능, Claude에게 주는 안내문), `ping`, `tools/list`, `tools/call`입니다.
  `id`가 없는 알림(예: `notifications/initialized`)에는 답하지 않습니다. 모르는 메서드에는 "-32601 unknown method" 오류로 답합니다.
- **agent 찾기:** 이 프로그램은 자기 Windows 로그인 세션 번호를 알아내고, 그 세션의 agent 포트(`127.0.0.1:8770 + 세션 번호`)로 요청을 보냅니다. Focus 앱과 같은 방식입니다.
- **테스트:** `cargo test -p digital-wellbeing-mcp`는 가짜 agent로 다음을 확인합니다. 초기화와 도구 목록이 맞는지, 각 도구가 정해진 agent 요청만 보내는지(예: `propose_todo`는 `todo_change`의 추가를 보낸다),
  모르는 도구(예: `unblock`)는 agent에 아무것도 보내지 않고 오류를 돌려주는지, 모르는 메서드가 프로토콜 오류가 되는지.
  또 `request_change`의 `repick`이 agent에 `{ "kind": "repick" }`으로 가는지, 다음 할 일을 고르는 예전 종류(`switch_todo`)는 "unknown change" 오류로 거부되는지 확인합니다.
  또 `settings_updates_are_scheduled_only_once_protected_and_keep_the_key` 테스트는 설정 단계에서 `schedule_settings_update`가 아무것도 쓰지 않고 거부되는지, 보호된 뒤에는 `via: "claude desktop"`과 지금의 API 키를 담아 `settings_put`을 보내는지, `get_settings`의 답에 API 키가 없고 기본 지시문이 있는지 확인합니다.

---

## 6. 한계

1. **실제 Claude Desktop으로는 아직 시험하지 않았습니다.** 개발자는 표준 입출력으로 메시지를 직접 넣어서 응답 형식만 확인했습니다.
2. **할 일을 AI의 심사 없이 지우는 도구는 없습니다.** 할 일은 승인된 변경 요청(`done`)으로 끝나거나, `change_todo`의 지우기로 목록에서 빠집니다. 지우기는 agent의 AI가 이유를 심사하고, 승인한 것만 바로 적용됩니다.
   Claude가 그 할 일을 뺀 `planner.json`을 `schedule_settings_update`로 보낼 수도 있지만, 설정 파일을 거친 할 일 목록 변경은 느슨한 변경이므로 확인 질문에 세 번 "예"라는 답을 받은 뒤에 적용됩니다.
3. **같은 PC의 어떤 프로그램이든 agent API를 부를 수 있습니다.** 이것은 Focus 앱 때부터 같습니다. 그래서 agent API에는 사용자가 원래 할 수 있는 요청만 있습니다.
   MCP 서버에는 확인 질문에 답하는 도구가 없지만, agent API의 `settings_answer`는 같은 PC의 다른 프로그램도 부를 수 있습니다. 그래도 질문할 때가 되기 전의 답은 agent가 거부하므로, 이틀 간격은 건너뛸 수 없습니다.
4. **`schedule_settings_update`는 파일 전체를 받습니다.** agent는 그 파일을 지금의 파일과 비교해서 설정 하나하나의 변경으로 나눕니다. 한 파일에는 설정의 예약된 업데이트가 하나만 있으므로, 다음과 같이 됩니다.
   - 보낸 파일의 느슨한 변경이 이미 예약된 업데이트와 같으면, 그 업데이트와 "예" 답이 그대로 남습니다.
   - 보낸 파일에 이미 예약된 변경이 빠져 있으면(느슨한 변경이 하나도 없으면), 예약된 업데이트는 거둔 것으로 처리됩니다.
   - 느슨한 변경이 다르면, 새 업데이트가 예전 업데이트를 대신하고, 예전 업데이트가 받은 "예" 답은 사라집니다. 사용자가 설정 탭에서 예약해 둔 업데이트도 마찬가지입니다.
   그래서 `get_settings`는 예약된 업데이트를 더한 파일(`files_with_scheduled_updates`)과 업데이트마다 바뀌는 설정(`changes`, API 키는 빼고)을 보여 줍니다. 도구 설명은 Claude에게 "`files_with_scheduled_updates`에서 시작하라. 예약된 변경을 빼면 그 업데이트를 거두고, 바꾸면 대신하므로 확인을 처음부터 다시 받는다. 그것을 사용자에게 알리라"고 안내합니다. 다만 Claude가 이 안내를 따르지 않으면, 사용자가 예약해 둔 업데이트가 거둬지거나 대신될 수 있습니다. 설정 탭의 예약 목록에는 누가 예약했는지(`via`, 화면에서는 "예약한 곳")가 나오므로, 사용자는 이것을 알아볼 수 있습니다.
   그래서 사용자는 Claude에게 설정 변경을 맡기기 전에 설정 탭의 "예약된 업데이트" 목록을 확인하는 편이 안전합니다. 또 사용자는 확인 질문에 답하기 전에 **바뀌는 것**에서 무엇이 바뀌는지 읽습니다.

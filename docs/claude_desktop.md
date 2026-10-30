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
| **Settings 탭** | Focus 앱의 설정 화면입니다. 사용자는 설정이 잠겨 있지 않을 때 이 탭에서 할 일 목록, 생활 규칙, 차단 규칙 같은 모든 설정을 고칩니다([`multi_account.md`](multi_account.md) 6.4절). |
| **생활 규칙** | 사용자가 Settings 탭의 Task planner 화면에 적는 고정된 규칙입니다. 할 일 고르기, 변경 요청 심사, 할 일 제안 심사가 모두 이 규칙을 따릅니다([`task_planner.md`](task_planner.md) 5.4절). |

---

## 1. 무엇을 할 수 있고 무엇을 할 수 없는가

Claude Desktop은 `digital-wellbeing-mcp.exe`를 작업 계정의 권한으로 실행합니다. 이 MCP 서버는 Focus 앱과 **같은 agent API만** 부릅니다.
그래서 Claude는 사용자가 Focus 앱으로 할 수 있는 일만 할 수 있습니다. Claude를 연결해도 차단을 우회할 길이 새로 생기지 않습니다.

| Claude가 하려는 것 | 결과 |
|---|---|
| 할 일 목록, 지금 할 일, 차단 목록 보기 (`get_status`) | ✅ 할 수 있습니다. |
| 할 일 제안하기 (`propose_todo`) | 제안은 목록에 바로 들어가지 않습니다. agent의 AI가 고정된 지시문과 생활 규칙으로 심사하고, 승인한 것만 목록에 들어갑니다. |
| 할 일 받기 (`get_my_task`) | ✅ 할 수 있습니다. 어떤 할 일을 줄지는 agent의 AI가 고릅니다. |
| 지금 할 일을 끝내거나 바꾸기 (`request_change`) | Focus 앱처럼 이유를 적어 요청하고, agent의 AI가 심사합니다. |
| 차단 풀기, 규칙 바꾸기, 생활 규칙이나 Settings 탭에서 적은 할 일 목록 고치기 | ❌ MCP 서버에 그런 도구가 없습니다. agent API에는 Settings 탭이 쓰는 설정 요청(`settings_get`, `settings_put`)이 있지만, MCP 서버는 그 요청을 보내지 않습니다. 또 설정이 잠겨 있으면 agent가 그 요청을 거부합니다. |

**할 일을 추천하는 지시문은 고정되어 있습니다.** 추천은 Claude Desktop이 하지 않고, agent가 프로그램 코드 안의 지시문(`PICK_PROMPT`)으로 합니다.
사용자도 Claude도 이 지시문을 바꿀 수 없습니다. 사용자는 설정이 잠겨 있지 않을 때 Settings 탭에서 생활 규칙만 바꿀 수 있습니다.

예를 들어 놀고 싶은 순간에 사용자가 Claude에게 "할 일에 '놀기' 넣어 줘"라고 하면 다음과 같이 됩니다.
1. Claude는 `propose_todo`로 "놀기"를 제안합니다.
2. agent의 AI는 생활 규칙(예: "평일 18시 전에는 게임과 영상을 할당하지 않는다")에 비추어 제안을 심사하고 거절합니다.
3. Claude는 거절 이유를 사용자에게 전합니다. MCP 서버는 Claude에게 "거절되면 이유를 전하고, 말을 바꿔 다시 시도하지 말라"고 안내합니다(`INSTRUCTIONS`).
   Claude가 그래도 말을 바꿔 다시 제안하면, agent의 AI는 이전 제안 기록을 함께 보고 심사합니다. 또 하루에 제안할 수 있는 수에는 한도(30개)가 있습니다.

---

## 2. 설정 방법

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

---

## 3. 사용 예

| 사용자가 Claude에게 하는 말 | Claude가 부르는 도구 |
|---|---|
| "지금 할 일이 뭐야?", "남은 할 일 보여 줘" | `get_status` |
| "'지도교수 메일 답장'을 할 일에 넣어 줘. 금요일까지야." | `propose_todo` (`title`, `deadline`) |
| "할 일 하나 줘" | `get_my_task` |
| "3장 초안 10쪽 다 썼어. 끝났다고 해 줘." | `request_change` (`change: "done"`, `reason`) |
| "자료가 아직 안 와서 막혔어. 다른 할 일로 바꿔 줘." | `get_status`로 목록을 본 뒤 `request_change` (`change: "switch_todo"`, `todo_id`, `reason`) |

**Claude Docs의 할 일 목록과의 관계:** 이 프로그램은 Claude Docs 문서를 직접 읽지 않습니다. 할 일의 원본은 이 프로그램의 목록입니다.
Claude Docs에 적어 둔 할 일을 옮기고 싶다면, Claude에게 "문서의 할 일을 하나씩 제안해 줘"라고 하면 됩니다. 각 할 일은 따로 심사를 받습니다.

---

## 4. 도구 설명 (`tools()`)

| 도구 | 인자 | 하는 일 |
|---|---|---|
| `get_status` | 없음 | agent의 `status`를 부르고, 집중 세션, 지금 할 일, 남은 할 일(id, 제목, 메모, 마감, 중요도, 사용자가 제안했는지), 최근 제안과 변경 요청, 차단 목록만 추려서 돌려줍니다. 판정 기록 같은 긴 내용은 뺍니다. |
| `propose_todo` | `title`(필수), `notes`, `deadline`(`YYYY-MM-DD`), `priority`(1~5), `estimate_minutes` | agent의 `propose_todo`를 `via: "claude desktop"`으로 부르고, 승인 여부와 AI의 답을 돌려줍니다. |
| `get_my_task` | 없음 | agent의 `assign`을 부르고, 할당된 할 일을 돌려줍니다. |
| `request_change` | `change`(`done`, `switch_todo`, `switch_free`, `end`), `todo_id`, `task`, `reason`(필수) | agent의 `request_change`를 부르고, 승인 여부, AI의 답, 바뀐 할당을 돌려줍니다. |

agent가 요청을 거부하거나 agent에 연결할 수 없으면, 도구는 오류 표시(`isError: true`)와 함께 이유를 돌려줍니다. Claude는 그 이유를 사용자에게 전합니다.

---

## 5. 동작 방식 (`main.rs`)

- **통신:** Claude Desktop은 이 프로그램을 실행하고, 표준 입력과 표준 출력으로 한 줄에 하나씩 JSON-RPC 2.0 메시지를 주고받습니다(MCP의 stdio 방식).
  이 프로그램은 표준 출력에 응답만 씁니다. 로그를 표준 출력에 쓰면 Claude Desktop이 응답을 해석하지 못하기 때문입니다.
- **처리하는 메시지:** `initialize`(서버 이름, 버전, 도구 기능, Claude에게 주는 안내문), `ping`, `tools/list`, `tools/call`입니다.
  `id`가 없는 알림(예: `notifications/initialized`)에는 답하지 않습니다. 모르는 메서드에는 "-32601 unknown method" 오류로 답합니다.
- **agent 찾기:** 이 프로그램은 자기 Windows 로그인 세션 번호를 알아내고, 그 세션의 agent 포트(`127.0.0.1:8770 + 세션 번호`)로 요청을 보냅니다. Focus 앱과 같은 방식입니다.
- **테스트:** `cargo test -p digital-wellbeing-mcp`는 가짜 agent로 다음을 확인합니다. 초기화와 도구 목록이 맞는지, 각 도구가 정해진 agent 요청만 보내는지,
  모르는 도구(예: `unblock`)는 agent에 아무것도 보내지 않고 오류를 돌려주는지, 모르는 메서드가 프로토콜 오류가 되는지.

---

## 6. 한계

1. **실제 Claude Desktop으로는 아직 시험하지 않았습니다.** 개발자는 표준 입출력으로 메시지를 직접 넣어서 응답 형식만 확인했습니다.
2. **할 일을 지우는 도구는 없습니다.** 할 일은 승인된 변경 요청(`done`)으로 끝나거나, 사용자가 설정이 잠겨 있지 않을 때 Focus 앱의 Settings 탭(Task planner 화면)에서 지웁니다.
3. **같은 PC의 어떤 프로그램이든 agent API를 부를 수 있습니다.** 이것은 Focus 앱 때부터 같습니다. 그래서 agent API에는 사용자가 원래 할 수 있는 요청만 있습니다.

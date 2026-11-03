# Digital Wellbeing

Digital Wellbeing은 한 사람이 정한 할 일에 집중하도록 돕는 Windows 프로그램입니다. 이 문서는 이 프로그램이 무엇을 하는지, 어떻게 설치하는지, 저장소가 어떻게 이루어져 있는지 설명합니다. 자세한 설명은 `docs/` 폴더의 문서에 있습니다.

## 1. 낱말

- **할 일**: 사용자가 할 일 탭에 넣는 일입니다. 제목, 메모, 마감, 중요도, 예상 시간을 가집니다.
- **지금 할 일**: 할 일 목록에서 AI가 고른, 지금 해야 하는 일 하나입니다. 사용자가 "할 일 받기"를 누르면 AI가 고릅니다.
- **AI 검사**: AI(Claude)가 열린 창과 화면을 보고, 그 창이 지금 할 일에 필요한지 판정하는 것입니다.
- **차단**: AI가 지금 할 일과 상관없다고 판정한 앱을 끄거나 브라우저 탭을 닫는 것입니다. 차단은 시간이 지나도 풀리지 않습니다.
- **접근 요청**: 차단된 앱이나 웹사이트가 일에 필요하다고 AI에게 목적을 적어 보내는 것입니다. AI가 허용하면 정한 시간 동안 쓸 수 있습니다.
- **설정 보호**: 설정 단계를 끝내는 것입니다. 보호된 뒤에는 규칙을 더 엄격하게 만드는 변경만 바로 적용되고, 느슨하게 만드는 변경은 이틀 간격으로 세 번 확인해야 적용됩니다. 보호는 프로그램 안에서 끌 수 없습니다.
- **서비스와 agent**: 서비스는 관리자 권한(SYSTEM)으로 도는 Windows 서비스입니다. 서비스는 로그인한 계정마다 agent를 하나씩 띄우고, agent가 그 계정의 앱을 감시하고 차단합니다.
- **Focus 앱**: 사용자가 보는 창입니다. 할 일, AI 비서, 차단, 설정, 통계, 도움말 탭이 있습니다.

## 2. 이 프로그램이 하는 일

- 사용자가 할 일을 넣으면, AI가 생활 규칙과 마감을 보고 지금 할 일을 고르고 그 일에 쓸 시간을 정합니다.
- AI가 열린 창을 보고 지금 할 일과 상관없는 앱은 끄고, 상관없는 탭은 닫습니다. 막는 순간 "맞는 차단인가요?" 창이 떠서, 사용자는 그 자리에서 접근을 요청할 수 있습니다.
- 할 일을 끝냈거나, 막혔거나, 시간이 더 필요하면 사용자는 변경 요청을 보내고 AI가 생활 규칙에 비추어 정합니다.
- 항상 막을 프로그램과 웹사이트, 약속(시각이 정해진 일), 컴퓨터 없이 하는 일, 사용 중지 같은 기능이 있습니다.
- Focus 앱의 AI 비서 탭이나 Claude Desktop으로, 말로 이 프로그램을 쓸 수 있습니다.
- 설정이 보호된 뒤에는 API 키를 망가뜨리거나, 할 일을 받지 않거나, 시간대를 바꾸는 식으로 규칙을 피할 수 없게 막습니다.

## 3. 설치하기

1. 작업에 쓰는 Windows 계정을 표준 사용자로 바꿉니다. 작업 계정이 관리자이면 이 프로그램의 보호를 모두 풀 수 있습니다.
2. [릴리스 페이지](https://github.com/KomaSeo/Digital_Wellbeing_Extensible/releases)에서 `DigitalWellbeing-Setup-<버전>.exe`를 내려받아 실행하고, 관리자 비밀번호를 넣습니다. 이름이 `-TEST`로 끝나는 파일은 설정 보호를 풀 수 있는 시험용 테스트판이므로, 실제로 쓸 때는 고르지 않습니다.
3. 작업 계정으로 로그인하면 Focus 앱이 뜹니다. 설정 탭에서 Anthropic API 키를 넣고, 생활 규칙과 항상 막을 프로그램을 정하고, 할 일 탭에서 할 일을 넣습니다.
4. 며칠 써 보고 원하는 대로 동작하면, 설정 탭 맨 위에서 설정을 보호합니다.

설치와 우회 막기의 자세한 방법은 Focus 앱의 도움말 탭과 [`docs/multi_account.md`](docs/multi_account.md)에 있습니다. 버전마다 바뀐 것은 [`installer/RELEASE_NOTES.md`](installer/RELEASE_NOTES.md)에 있습니다.

## 4. 문서

| 문서 | 내용 |
|---|---|
| [`docs/multi_account.md`](docs/multi_account.md) | 서비스와 agent의 구조, 설치, 데이터 폴더, 설정 보호, 우회 막기 |
| [`docs/ai_focus_rule.md`](docs/ai_focus_rule.md) | AI 검사, 차단, 차단 창, 도우미 프로그램, 모든 앱 잠금 |
| [`docs/task_planner.md`](docs/task_planner.md) | 할 일, 지금 할 일 고르기, 변경 요청, 약속 |
| [`docs/access_grants.md`](docs/access_grants.md) | 접근 요청과 허용 |
| [`docs/assistant.md`](docs/assistant.md) | Focus 앱의 AI 비서 탭 |
| [`docs/claude_desktop.md`](docs/claude_desktop.md) | Claude Desktop으로 이 프로그램 쓰기 |
| [`docs/cost_reduction_options.md`](docs/cost_reduction_options.md) | AI 비용을 줄이는 방법과 소유자의 결정 |
| [`docs/linux_and_tests.md`](docs/linux_and_tests.md) | 리눅스에서 개발하고 시험하는 방법 |
| [`docs/design_decisions.md`](docs/design_decisions.md) | 기능마다 소유자가 요청한 것과 AI가 정한 세부 |

여러 기기 동기화는 아직 브랜치 `claude/multi-device-sync`에서 만들고 있고, 설명은 그 브랜치의 `docs/sync.md`에 있습니다.

## 5. 저장소 구성

이 저장소는 Rust 작업 공간(workspace)입니다.

| 경로 | 하는 일 |
|---|---|
| `apps/service` | `digital-wellbeing-service.exe`. 설치와 제거, Windows 서비스, 계정마다의 agent |
| `apps/focus` | `digital-wellbeing-focus.exe`. 사용자가 보는 Focus 앱(egui) |
| `apps/mcp` | `digital-wellbeing-mcp.exe`. Claude Desktop이 쓰는 MCP 서버 |
| `apps/desktop` | 예전의 독립 실행 창. 설치 파일에는 들어가지 않고, 개발할 때만 씁니다 |
| `crates/enforcer` | 프로세스를 살펴보고, 규칙이 막으라는 프로그램과 그 도우미를 끕니다 |
| `crates/list_based_rule` | 항상 막을 프로그램(블랙리스트와 화이트리스트) |
| `crates/ai_focus_rule` | AI 검사, 차단, 접근 허용, Claude API 호출, 사용량 기록 |
| `crates/task_planner` | 할 일 목록과 지금 할 일, 변경 요청과 그 AI 심사 |
| `crates/machine` | 데이터 폴더, 설정 보호, 모든 앱 잠금, agent의 사용자 API, AI 비서 |
| `crates/rule_ui`, `crates/tool-api` | 예전 독립 실행 창의 화면과 함수 호출 API |
| `installer/` | Inno Setup 설치 스크립트, 릴리스 버전과 릴리스 노트 |
| `.github/workflows/` | `ci.yml`(리눅스와 Windows의 테스트, 실제 설치 시험), `release.yml`(배포) |

## 6. 빌드와 테스트

```sh
cargo build --workspace
cargo test --workspace                 # 리눅스에서는 xvfb-run -a cargo test --workspace
cargo test -p machine --features test-edition
```

- 리눅스에서는 X11에서 agent가 돌기 때문에 개발과 끝에서 끝까지 테스트를 할 수 있습니다. 서비스 설치와 설치 파일은 Windows에서만 동작합니다([`docs/linux_and_tests.md`](docs/linux_and_tests.md)).
- 끝에서 끝까지 테스트는 Claude API 대신 테스트 안의 가짜 서버를 씁니다. 진짜 API로 하는 테스트는 `DW_E2E_ANTHROPIC_API_KEY` 환경 변수가 있을 때만 돕니다.

## 7. 배포

`installer/RELEASE_VERSION`의 버전을 올려 push하면 `release.yml`이 Windows에서 빌드하고 시험한 뒤, 본편과 테스트판의 설치 파일과 zip을 GitHub 릴리스에 올립니다. 릴리스 노트는 `installer/RELEASE_NOTES.md`의 내용이 그대로 쓰입니다.

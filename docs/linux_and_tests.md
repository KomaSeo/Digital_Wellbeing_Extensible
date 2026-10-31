# Linux 지원과 자동 테스트

이 문서는 두 가지를 설명합니다. 첫째, 이 프로그램의 어느 부분이 Windows 전용이고 어느 부분이 Linux에서도 도는지 설명합니다.
둘째, 개발자가 Windows PC 없이 프로그램을 끝에서 끝까지 시험하는 방법과, push할 때마다 도는 자동 검사(CI)를 설명합니다.

---

## 0. 이 문서에서 쓰는 말

| 말 | 뜻 |
|---|---|
| **X11** | Linux에서 창을 그리는 오래된 표준 방식입니다. 한 프로그램이 다른 프로그램의 창 제목을 읽고 키 입력을 보낼 수 있습니다. |
| **Wayland** | X11을 대신하는 새 방식입니다. 보안상 한 프로그램이 다른 프로그램의 창을 볼 수 없습니다. |
| **Xvfb** | 화면 없이 메모리 안에서만 도는 가상 X11 서버입니다. 테스트할 때 씁니다. |
| **CI** | GitHub Actions가 push할 때마다 자동으로 빌드하고 테스트하는 일입니다(`.github/workflows/ci.yml`). |
| **가짜 Claude 서버** | 테스트가 띄우는 작은 HTTP 서버입니다. Claude Messages API와 같은 모양의 답을 돌려주므로, API 키와 비용 없이 판정 과정을 시험할 수 있습니다. |

---

## 1. Windows 전용 기능과 공통 기능

| 기능 | Windows | Linux |
|---|---|---|
| 차단 규칙(블랙리스트), AI 판정, 할 일 플래너, 접근 요청, 설정 잠금, 토큰 사용량 | ✅ | ✅ (공통 코드) |
| agent와 그 사용자 API (`digital-wellbeing-service agent`) | ✅ 서비스가 사용자 세션마다 띄웁니다. | ✅ 사용자가 직접 실행합니다. |
| 맨 앞 창, 열린 창 목록, 탭 닫기(Ctrl+W), 창 최소화, 화면 캡처 | ✅ Win32 (`platform/windows.rs`) | ✅ X11 (`platform/x11.rs`). Wayland 세션에서는 동작하지 않습니다. |
| 서비스 등록, 설치기, 폴더 권한, 부팅 시 자동 시작 | ✅ (`install.rs`, `service.rs`, `spawn.rs`) | ❌ 없습니다. agent를 직접 실행하거나 systemd 사용자 서비스로 등록해야 합니다. |
| Focus 앱, 트레이 아이콘 | ✅ | Focus 앱 ✅, 트레이 아이콘 ❌ (창을 닫으면 앱이 끝납니다) |
| MCP 서버 (Claude Desktop 연결) | ✅ | ✅ |

**Linux에서 달라지는 점:**
- **데이터 폴더:** Windows는 `C:\ProgramData\DigitalWellbeing`이고, Linux는 `/var/lib/digital-wellbeing`입니다. 환경 변수 `DW_DATA_DIR`이 있으면 어느 운영체제에서든 그 폴더를 씁니다(`machine::data_dir`).
- **"세션"의 뜻:** Windows에서 세션은 Windows 로그인 세션입니다. Linux에서는 프로세스를 소유한 사용자 번호(uid)를 세션으로 봅니다. 그래서 agent는 자기 사용자의 프로세스만 끕니다(`machine::session`).
- **agent의 포트:** agent의 사용자 API 포트는 `8770 + (세션 번호 % 1000)`입니다. 환경 변수 `DW_PORT_BASE`가 있으면 8770 대신 그 값을 씁니다. 테스트가 여러 agent를 함께 띄울 때 씁니다.
- **꺼지지 않는 프로그램:** Linux의 핵심 프로그램(init, systemd, Xorg, 데스크톱 셸, 소리 데몬, sshd 등)도 끄지 않는 목록(`enforcer::never_terminate`)에 넣었습니다.
- **브라우저 목록:** 기본 브라우저 목록에 Linux 프로세스 이름(chromium, firefox-bin 등)을 넣었습니다. 목록의 이름은 어떤 철자로 적어도 맞춰 봅니다(`Chrome`, `chrome`, `chrome.exe`는 모두 같습니다).

**X11 구현이 하는 일** (`crates/ai_focus_rule/src/platform/x11.rs`):
- 맨 앞 창: 창 관리자가 있으면 `_NET_ACTIVE_WINDOW`를 읽고, 없으면 키보드 포커스를 가진 창에서 위로 올라가며 제목이 있는 창을 찾습니다.
- 열린 창 목록: `_NET_CLIENT_LIST`를 읽고, 창 관리자가 없으면 화면에 보이는 최상위 창을 씁니다.
- 프로그램 이름: 창의 `_NET_WM_PID`로 프로세스를 찾고 `/proc/<pid>/exe`에서 이름을 읽습니다. 없으면 창의 `WM_CLASS`를 씁니다.
- 탭 닫기: XTest로 Ctrl+W를 보냅니다. Windows와 같이, 판정한 창과 제목이 그대로인지, 사용자가 Shift·Ctrl·Alt·Super를 누르고 있지 않은지 먼저 확인합니다.
- 최소화: 창 관리자에게 `WM_CHANGE_STATE`(아이콘 상태) 요청을 보냅니다.
- 화면 캡처: 화면 전체를 `GetImage`로 읽습니다.

---

## 2. Windows PC 없이 끝에서 끝까지 시험하기

```bash
xvfb-run -a cargo test --workspace
```

`xvfb-run`은 가상 X 서버를 띄우고 그 안에서 테스트를 실행합니다. `DISPLAY`가 없으면 X11 테스트와 끝에서 끝까지 테스트는 "skipped"를 출력하고 건너뜁니다.

**X11 테스트** (`platform/x11.rs`의 `x11_desktop_sees_and_controls_a_window`): 테스트가 진짜 X11 창을 하나 만들고, X11 구현이 그 창의 제목과 프로세스를 읽는지, 화면을 캡처하는지, Ctrl+W와 최소화 요청이 실제로 도착하는지, 탭 제목이 바뀌면 Ctrl+W를 보내지 않는지 확인합니다.

**끝에서 끝까지 테스트** (`apps/service/tests/e2e_linux.rs`): 실제 agent 실행 파일을 띄우고, 사용자가 Focus 앱으로 하는 일을 같은 API로 합니다.
1. 테스트는 임시 데이터 폴더, 가짜 Claude 서버, 테스트 전용 포트를 정하고 agent를 실행합니다(`DW_DATA_DIR`, `DW_CLAUDE_API_URL`, `DW_PORT_BASE`).
2. 테스트는 진짜 X11 창 두 개를 만듭니다. 앞에는 "Funny cats - YouTube", 뒤에는 "Rust docs - Google Chrome"입니다. 테스트 프로세스 자신을 브라우저로 등록하므로, 이 창들은 브라우저 탭처럼 다뤄집니다.
3. 테스트는 사용자 API로 집중 세션을 시작합니다("write my thesis").
4. 가짜 Claude 서버는 제목에 "cats"가 있으면 `off_task`, 없으면 `on_task`라고 답합니다.
5. 테스트는 다음을 확인합니다.
   - 앞의 딴짓 탭에 Ctrl+W가 실제로 도착한다.
   - 뒤의 창도 제목으로 판정받는다.
   - 상태에 판정 기록, 차단, 오늘의 예상 비용이 나온다.
   - 설정 읽기와 쓰기, 할 일 제안이 된다.
   - 설정을 잠그면 agent가 설정 읽기를 거부한다.

**진짜 Claude API로 하는 끝에서 끝까지 테스트** (같은 파일의 `agent_end_to_end_on_x11_with_real_claude`): 위와 같은 일을 가짜 Claude 서버 대신 진짜 Claude API로 합니다.
- 환경 변수 `DW_E2E_ANTHROPIC_API_KEY`에 API 키가 있을 때만 실행되고, 없으면 건너뜁니다. 그래서 키가 없는 CI와 개발자 PC의 `cargo test`는 비용을 쓰지 않습니다.
- 실행 방법: `DW_E2E_ANTHROPIC_API_KEY=<키> xvfb-run -a cargo test -p digital-wellbeing-service --test e2e_linux -- --nocapture`
- 한 번 실행하면 판정 몇 번과 제안 심사 한 번을 부르므로 몇 센트가 듭니다.
- 진짜 AI의 답은 매번 조금씩 다를 수 있습니다. 그래서 이 테스트는 상식적인 답이면 모두 통과하는 것만 확인합니다. 집중 세션의 할 일은 "write a command-line tool in Rust"입니다. 고양이 영상 탭은 Ctrl+W를 받고 YouTube 차단 목록에 들어가야 합니다. Rust 문서 창은 차단되면 안 됩니다. 할 일 제안("Buy groceries")은 승인 여부와 상관없이 심사 결과가 나오기만 하면 됩니다.
- 테스트는 AI가 각 창에 내린 판정과 이유, 제안 심사 결과, 예상 비용을 출력합니다(`--nocapture`).

**운영체제의 인증서 저장소:** 프로그램은 Claude API에 HTTPS로 연결할 때 운영체제가 믿는 인증서(Windows는 Windows 인증서 저장소, Linux는 `/etc/ssl/certs`)를 믿습니다(`ureq`의 `native-certs` 기능).
회사 네트워크나 이 개발 서버처럼 HTTPS를 중간에서 검사하는 네트워크는 자기 인증서를 운영체제에 등록해 둡니다. 프로그램이 자기 안에 든 인증서 목록만 믿으면, 이런 네트워크에서는 연결이 "invalid peer certificate: UnknownIssuer"로 실패합니다.

**`DW_CLAUDE_API_URL`:** 이 환경 변수가 있으면 프로그램은 `https://api.anthropic.com` 대신 그 주소로 Claude를 부릅니다. 주소가 `127.0.0.1`이나 `localhost`이면 환경 변수의 프록시를 거치지 않습니다.
Windows에서 agent의 환경 변수는 SYSTEM 서비스의 환경 변수이고, 이것은 관리자만 바꿀 수 있습니다. 그래서 일반 사용자가 이 값으로 판정을 바꿀 수는 없습니다.

---

## 3. 자동 검사 (`.github/workflows/ci.yml`)

GitHub Actions는 모든 push마다 두 작업을 돌립니다.

| 작업 | 하는 일 |
|---|---|
| **linux** (ubuntu-latest) | Xvfb를 설치하고 `xvfb-run -a cargo test --workspace`를 실행합니다. 위의 X11 테스트와 끝에서 끝까지 테스트가 여기서 돕니다. |
| **windows** (windows-latest) | release 설정으로 테스트하고 빌드한 뒤, 실제로 서비스를 설치·업데이트하고, 서비스가 띄운 agent를 사용자 API로 시험하고, 서비스를 제거합니다. |

windows 작업의 설치 시험은 다음을 확인합니다.
1. `install --manage-all`이 성공하고, 서비스가 Running이고 자동 시작이며, 실행 파일 세 개가 Program Files에 복사된다.
2. 예전 버전이 남긴 것처럼 데이터 폴더에 파일을 만든 뒤 다시 설치(업데이트)해도, 데이터 폴더 안의 **모든 파일**에 SYSTEM과 Administrators 권한이 있고 읽을 수 있다. rc.5까지 있던 "권한 목록이 빈 파일" 버그는 이 검사에 걸립니다.
3. 데이터 폴더에 일반 사용자(Users, Everyone, Authenticated Users) 권한이 없다.
4. 업데이트 뒤 서비스가 로그에 "service running"과 "started agent"를 남긴다. 즉 서비스가 로그인한 사용자의 세션에 agent를 띄웠다.
5. 마지막 단계에서 `uninstall` 뒤 서비스가 돌지 않는다. 이 단계는 앞 단계가 실패해도 실행됩니다.

GitHub의 Windows 빌드 서버에는 작업을 실행하는 사용자(runneradmin)가 로그인한 세션이 있습니다. 그래서 서비스는 설치되자마자 그 세션에 agent를 띄웁니다.

**설치된 agent의 끝에서 끝까지 테스트** (`apps/service/tests/e2e_windows.rs`): 이 테스트는 설치된 서비스가 띄운 진짜 agent를, Focus 앱과 같은 사용자 API로 시험합니다.
1. CI는 서비스의 환경 변수(레지스트리의 서비스 키 아래 `Environment` 값)에 `DW_CLAUDE_API_URL=http://127.0.0.1:18765`를 넣고 서비스를 다시 시작합니다. 서비스가 띄우는 agent는 서비스의 환경 변수를 물려받으므로, agent는 Claude 대신 테스트의 가짜 Claude 서버를 부릅니다. 그래서 API 키와 비용이 들지 않습니다.
2. 테스트는 그 주소에 가짜 Claude 서버를 띄우고, 자기 세션의 agent가 사용자 API에 답할 때까지 기다립니다.
3. 테스트는 `settings_put`으로 AI 집중 설정을 바꿉니다. 테스트 프로세스 자신을 브라우저로 등록하므로, 테스트가 만든 창은 브라우저 탭처럼 다뤄집니다.
4. 테스트는 진짜 Windows 창 두 개를 만듭니다. 뒤에는 "Rust docs - Google Chrome", 앞에는 "Funny cats - YouTube"입니다. 그리고 집중 세션을 시작합니다.
5. 테스트는 다음을 확인합니다.
   - 딴짓 창이 앞에 있으면 Ctrl+W를 받고, 뒤에 있으면 최소화된다. Windows는 프로그램이 마음대로 창을 앞으로 가져오는 것을 막을 때가 있으므로, 테스트는 둘 중 어느 쪽이든 받아들이고 어느 쪽이었는지 출력합니다.
   - 할 일에 맞는 창은 최소화되지 않는다.
   - 두 창 모두 제목으로 판정받고, 상태에 딴짓 창의 차단과 오늘의 예상 비용이 나온다.
   - 할 일 제안이 되고, 설정을 잠그면 agent가 설정 읽기를 거부한다.

환경 변수 `DW_E2E_CLAUDE_ADDR`가 없으면 이 테스트는 건너뜁니다. 그래서 개발자 PC의 `cargo test`는 설치된 서비스를 건드리지 않습니다.
같은 일을 개발자 PC에서 하려면, 관리자 PowerShell에서 서비스를 설치하고 CI의 "End to end through the installed agent's API" 단계를 그대로 실행하면 됩니다. 이 테스트는 마지막에 설정을 1시간 잠그므로, 쓰고 있는 PC에서는 실행하지 마십시오.

실패하면 CI는 서비스와 agent의 로그를 출력합니다("Service and agent logs" 단계).

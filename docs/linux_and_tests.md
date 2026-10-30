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

**`DW_CLAUDE_API_URL`:** 이 환경 변수가 있으면 프로그램은 `https://api.anthropic.com` 대신 그 주소로 Claude를 부릅니다. 주소가 `127.0.0.1`이나 `localhost`이면 환경 변수의 프록시를 거치지 않습니다.
Windows에서 agent의 환경 변수는 SYSTEM 서비스의 환경 변수이고, 이것은 관리자만 바꿀 수 있습니다. 그래서 일반 사용자가 이 값으로 판정을 바꿀 수는 없습니다.

---

## 3. 자동 검사 (`.github/workflows/ci.yml`)

GitHub Actions는 모든 push마다 두 작업을 돌립니다.

| 작업 | 하는 일 |
|---|---|
| **linux** (ubuntu-latest) | Xvfb를 설치하고 `xvfb-run -a cargo test --workspace`를 실행합니다. 위의 X11 테스트와 끝에서 끝까지 테스트가 여기서 돕니다. |
| **windows** (windows-latest) | release 설정으로 테스트하고 빌드한 뒤, 실제로 서비스를 설치·업데이트·제거합니다. |

windows 작업의 설치 시험은 다음을 확인합니다.
1. `install --manage-all`이 성공하고, 서비스가 Running이고 자동 시작이며, 실행 파일 세 개가 Program Files에 복사된다.
2. 예전 버전이 남긴 것처럼 데이터 폴더에 파일을 만든 뒤 다시 설치(업데이트)해도, 데이터 폴더 안의 **모든 파일**에 SYSTEM과 Administrators 권한이 있고 읽을 수 있다. rc.5까지 있던 "권한 목록이 빈 파일" 버그는 이 검사에 걸립니다.
3. 데이터 폴더에 일반 사용자(Users, Everyone, Authenticated Users) 권한이 없다.
4. 업데이트 뒤 서비스가 로그에 "service running"을 남긴다.
5. `uninstall` 뒤 서비스가 돌지 않는다.

GitHub의 Windows 빌드 서버에는 로그인한 사용자가 없을 수 있습니다. 그래서 "서비스가 사용자 세션에 agent를 띄운다"는 부분은 이 검사가 보장하지 못합니다. 서버에 사용자 세션이 있으면, 검사는 agent 로그를 출력합니다.

# 관리자 계정 + 작업용 일반 계정 구성

목표:
- 작업은 **일반(표준) 계정**에서 한다.
- 차단 프로그램은 **관리자 권한**으로 돌고, 일반 계정에서는 **끄거나 설정을 바꿀 수 없다**.
- 컴퓨터를 켜면 **자동으로** 시작한다.

결론부터: 가능합니다. 다만 "관리자 계정에 로그인해서 프로그램을 띄워 두고, 다른 계정에서 일한다"는 방식으로는
안 되고, **Windows 서비스**로 만들어야 합니다. 이 문서는 왜 그런지(1장), 어떻게 만들었는지(2~3장),
설치·사용법(4~6장), 한계(7장) 순서로 정리했습니다.

> ⚠️ 이 기능은 Linux에서 Windows 대상으로 **컴파일 검사만** 했고, agent의 로컬 API와 설정 재로드만 Linux에서
> 실제로 돌려 봤습니다. 서비스 등록, 세션 안에 프로세스 띄우기, 다른 세션 화면 캡처는 **실제 Windows에서 아직
> 확인하지 못했습니다.** 처음 설치할 때 5장의 점검 목록을 꼭 한 번 따라 해 보세요.

---

## 1. 탐색: 왜 "관리자 계정에서 띄워 두기"로는 안 되나

### 1.1 Windows의 세션 분리

Windows는 로그인한 사용자마다 **세션**을 따로 만듭니다(세션 1, 2, …). 서비스는 세션 0에서 돕니다.
세션마다 화면(window station `winsta0`, 데스크톱 `default`), 창, 클립보드가 완전히 분리됩니다.

| 하려는 일 | 다른 세션에서 가능? |
|---|---|
| 프로세스 목록 보기 / 죽이기 | ✅ 관리자 권한이면 모든 세션의 프로세스를 죽일 수 있음 |
| 맨 앞 창 알아내기 (`GetForegroundWindow`) | ❌ 호출한 프로세스의 데스크톱에서만 동작 |
| 스크린샷 | ❌ 자기 데스크톱만 캡처 가능 |
| Ctrl+W 보내기 (`SendInput`) | ❌ 자기 데스크톱에만 입력 가능 |

그래서 관리자 계정 세션에서 돌리면 **목록 기반 차단은 되지만, AI 검사(창 제목, 스크린샷, 탭 닫기)는 작업 계정을
볼 수 없습니다.** 게다가:
- 관리자 계정이 **로그인된 상태로 남아 있어야** 합니다(빠른 사용자 전환). 재부팅하면 다시 로그인해야 합니다.
- "컴퓨터 시작할 때 자동 시작"도 관리자 계정 로그인에 묶입니다.

### 1.2 그래서 택한 구조

```
부팅
 └─ [서비스] DigitalWellbeing  (세션 0, LocalSystem, 자동 시작)
       │  2초마다: 로그인된 세션 목록 확인 → 관리 대상 계정이면 agent가 살아 있는지 확인, 없으면 띄움
       │
       ├─ 작업 계정 로그인 (세션 2)
       │    ├─ [agent]  digital-wellbeing-service.exe agent   ← SYSTEM 권한, 세션 2 안에서 실행
       │    │      · 목록 규칙 + AI 규칙 실행, 세션 2의 프로세스만 차단
       │    │      · 세션 2의 맨 앞 창 / 스크린샷 / Ctrl+W
       │    │      · 127.0.0.1:8772 로 "집중 세션" API만 열어 둠
       │    └─ [Focus 앱] digital-wellbeing-focus.exe        ← 사용자 권한, 로그인 시 자동 실행
       │           · 할 일 입력 / 세션 시작·종료 / AI 판정 보기  (agent API로만 통신)
       │
       └─ (관리자가 설정을 바꿀 때만) [관리 UI] digital-wellbeing.exe  ← 관리자 권한 (UAC)
              · C:\ProgramData\DigitalWellbeing\*.json 편집 → agent가 3초 안에 다시 읽음
```

핵심 아이디어는 **agent를 "SYSTEM 권한으로, 사용자 세션 안에서" 띄우는 것**입니다.
- SYSTEM 권한이므로 일반 사용자는 작업 관리자에서 끝낼 수 없습니다("액세스가 거부되었습니다").
- 사용자 세션 안에 있으므로 그 화면을 보고, 캡처하고, 키 입력을 보낼 수 있습니다.

어린이 보호 소프트웨어나 원격 지원 도구가 흔히 쓰는 방식입니다.
`apps/service/src/spawn.rs`의 `spawn_in_session` 주석에 구현 방법이 있습니다.

관리자 계정은 **설정을 바꿀 때 말고는 로그인할 필요가 없습니다.** 작업 계정에서 관리 UI를 실행하면 UAC 창이 뜨고,
거기에 관리자 비밀번호를 넣으면 됩니다.

---

## 2. 구성 요소

| 파일 | 실행 주체 | 역할 |
|---|---|---|
| `digital-wellbeing-service.exe install` | 관리자, 한 번 | 설치 (4장) |
| `digital-wellbeing-service.exe run` | Windows(서비스 관리자) | 감독자: agent 유지·재시작 |
| `digital-wellbeing-service.exe agent` | 서비스가 SYSTEM으로 | 세션별 차단 엔진 |
| `digital-wellbeing-focus.exe` | 작업 계정 사용자 | 할 일 입력, 상태 보기 |
| `digital-wellbeing.exe` | 관리자 (UAC) | 설정 편집. 머신 설치가 있으면 자동으로 "설정 편집 전용" 모드 |

설치 후 파일 위치:

```
C:\Program Files\DigitalWellbeing\          실행 파일 3개 (일반 사용자: 읽기/실행만)
C:\ProgramData\DigitalWellbeing\            SYSTEM + Administrators 만 접근 가능
    policy.json       블랙/화이트리스트 규칙
    ai_focus.json     AI 집중 설정 (API 키 포함)
    machine.json      관리 대상 계정 목록
    logs\service.log, logs\agent-session<N>.log
```

코드 위치:

| 경로 | 내용 |
|---|---|
| `apps/service/src/main.rs` | 명령 분기, 로그 설정 |
| `apps/service/src/service.rs` | 서비스 진입점, `Supervisor` (2초 주기 감시) |
| `apps/service/src/spawn.rs` | 세션 열거(WTS API), SYSTEM 토큰을 다른 세션에 띄우기 |
| `apps/service/src/agent.rs` | agent: 규칙 등록, 세션 필터, 설정 재로드 |
| `apps/service/src/install.rs` | 설치/제거 |
| `apps/focus/src/main.rs` | Focus 앱 |
| `crates/machine/` | 공용: 경로, `MachineConfig`, 세션 ID, agent↔Focus API |
| `crates/enforcer` | `set_process_filter` 추가 (세션 필터) |
| `crates/ai_focus_rule` | 읽기 전용 store, 설정 전용 UI 모드 추가 |

---

## 3. 일반 계정이 할 수 있는 것 / 없는 것

| 시도 | 결과 | 막는 장치 |
|---|---|---|
| 작업 관리자에서 agent 끝내기 | ❌ 액세스 거부 | agent가 SYSTEM 권한 |
| 서비스 중지 (`sc stop`, services.msc) | ❌ 액세스 거부 | 서비스 기본 ACL: 일반 사용자는 조회만 가능 |
| 설정 파일 수정, API 키 읽기 | ❌ 폴더 접근 불가 | `icacls`로 ProgramData 폴더를 SYSTEM+Administrators 전용으로 |
| exe를 다른 프로그램으로 바꿔치기 | ❌ | Program Files는 관리자만 쓰기 가능 |
| 시작 프로그램에서 끄기, 재부팅 | 효과 없음 | 서비스는 로그인 전에 자동 시작, agent는 로그인할 때마다 다시 뜸 |
| agent가 어떻게든 죽음 | 2초 안에 다시 뜸 | Supervisor |
| 서비스가 크래시 | 5초 뒤 재시작 (3회) | 서비스 복구 설정 |
| 로컬 API(8765 포트)로 규칙 변경 | ❌ | 머신 모드에서는 8765 API를 열지 않음 |
| agent API(877x)로 규칙 변경 | ❌ | `status / start_session / stop_session`만 존재 |
| Focus 앱 끄기 | ✅ 가능, 무해 | 차단은 agent가 계속함 |
| 할 일을 "유튜브 보기"로 적기 | ✅ 가능 | ⚠️ 할 일은 본인이 선언하는 것이라 막지 않음 (7장) |
| 집중 세션 끝내기 | ✅ 가능 | 목록 규칙(관리자 설정)은 세션과 무관하게 항상 적용 |

**전제: 작업 계정이 "표준 사용자"여야 합니다.** 작업 계정이 관리자면 위 장치가 전부 무의미합니다.
(설정 → 계정 → 가족 및 기타 사용자 → 계정 → 계정 유형 변경 → **표준 사용자**)

---

## 4. 설치

### 4.1 준비 (관리자 계정에서, 처음 한 번)

1. 관리자 계정을 하나 둡니다. 작업 계정은 **표준 사용자**로 바꿉니다(3장 마지막 줄).
2. Rust를 설치하고 저장소를 빌드합니다.
   ```powershell
   cargo build --release
   ```
   `target\release\`에 `digital-wellbeing-service.exe`, `digital-wellbeing-focus.exe`,
   `digital-wellbeing.exe`가 생깁니다.

### 4.2 설치 명령

**관리자 터미널**(PowerShell을 "관리자 권한으로 실행")에서:

```powershell
cd <저장소>\target\release
.\digital-wellbeing-service.exe install --manage <작업계정 사용자이름>
```

- `<작업계정 사용자이름>`은 로그인 이름입니다. 작업 계정에서 `whoami`를 치면 나오는 `PC이름\이름` 중 `이름` 부분입니다.
  Microsoft 계정이면 표시 이름이 아니라 `C:\Users\` 아래 폴더 이름과 같은 경우가 많습니다.
- `--manage`는 여러 번 쓸 수 있습니다. **생략하면 모든 계정(관리자 포함)이 관리됩니다.**

설치기가 하는 일(`install.rs` 맨 위 주석과 같음):
1. 이미 설치된 서비스가 있으면 멈춤
2. exe 3개를 `C:\Program Files\DigitalWellbeing\`로 복사
3. `C:\ProgramData\DigitalWellbeing\`을 만들고 SYSTEM+Administrators 전용으로 잠금
4. `machine.json`에 관리 대상 계정 기록
5. 서비스 `DigitalWellbeing` 등록 (자동 시작, LocalSystem, 실패 시 재시작)
6. 모든 사용자 로그인 시 Focus 앱 자동 실행 (`HKLM\...\Run`)
7. 서비스 시작

업데이트할 때도 새로 빌드한 뒤 같은 명령을 다시 실행하면 됩니다(기존 설정은 유지).

### 4.3 첫 설정 (관리 UI)

1. `C:\Program Files\DigitalWellbeing\digital-wellbeing.exe` 실행 → UAC → 관리자 비밀번호.
   상단에 `Machine install: editing C:\ProgramData\DigitalWellbeing …`이 보이면 머신 모드입니다.
2. **AI focus** 탭 → **API key**에 Anthropic API 키를 넣고 Save.
   - agent는 SYSTEM으로 돌기 때문에 **사용자 계정의 환경변수 `ANTHROPIC_API_KEY`를 보지 못합니다.** 여기에 저장하세요.
     파일은 평문이지만 폴더가 관리자 전용이라 작업 계정에서는 읽을 수 없습니다.
   - 화면에 "using ANTHROPIC_API_KEY"가 보인다면, 그건 **관리 UI를 띄운 관리자 프로세스**의 환경변수를 뜻합니다.
     agent와는 무관하니 그래도 키를 저장해 두세요.
3. 원하면 **List-based rule** 탭에서 항상 막을 프로그램(게임 등)을 블랙리스트에 넣습니다.
4. 창을 닫습니다. 변경은 3초 안에 agent에 반영됩니다. 관리 UI를 계속 켜 둘 필요는 없습니다.

---

## 5. 설치 확인 점검 목록 (처음 한 번)

작업 계정으로 로그인한 뒤:

1. 로그인하면 **Digital Wellbeing Focus** 창이 뜬다. 안 뜨면 시작 메뉴에서 실행해 본다.
2. Focus 창에 빨간 "agent not running…"이 **안** 보인다.
3. 작업 관리자 → 세부 정보 → `digital-wellbeing-service.exe`가 **사용자 이름 SYSTEM**으로 **두 개**
   (서비스 1, agent 1) 있다. agent를 "작업 끝내기" → 액세스 거부.
4. 할 일을 적고 **Start focus** → 1분 안에 "Recent AI checks"에 판정이 나온다.
   - 오류가 보이면 메시지를 확인한다(키 없음/잘못됨 → 4.3의 2번).
   - `on_task`인데 스크린샷이 의심스러우면: 관리자 계정에서 `logs\agent-session<N>.log`를 본다.
5. 블랙리스트에 `notepad.exe`를 넣고 메모장을 켜 본다 → 1초 안에 꺼진다.
6. 유튜브 같은 사이트를 열어 두고 2분 정도 기다린다 → 탭이 닫힌다.

agent가 안 뜨면 `C:\ProgramData\DigitalWellbeing\logs\service.log`를 관리자 계정으로 확인합니다
(`could not start agent for session N: …` 줄에 Windows 오류가 적혀 있습니다).

---

## 6. 매일 사용

**작업 계정에서**
1. 로그인하면 Focus 창이 뜹니다.
2. 할 일을 **구체적으로** 적고 **Start focus**. 예: `졸업논문 3장 작성 — Overleaf, 논문 PDF, Google Scholar 필요`
3. 하는 일이 바뀌면 내용을 고치고 **Change task**. 바꾸면 AI 차단은 초기화되고 바로 다시 검사합니다.
4. 끝나면 **End session**. 로그아웃하거나 재부팅해도 세션은 끝납니다. 할 일은 저장되지 않으므로 다음엔 다시 적습니다.
5. Focus 창은 닫아도 됩니다. 차단은 계속되고, 다시 열면 상태가 그대로 보입니다.

**설정을 바꿀 때 (관리자 비밀번호 필요)**
- 작업 계정에서 `digital-wellbeing.exe`를 실행 → UAC에 관리자 비밀번호 → 수정 → 닫기.
- 관리 대상 계정을 바꾸려면 `machine.json`의 `managed_users`를 고치거나, `install --manage …`를 다시 실행합니다.

**제거**
```powershell
# 관리자 터미널
& "C:\Program Files\DigitalWellbeing\digital-wellbeing-service.exe" uninstall
```
서비스와 자동 실행을 지웁니다. 프로그램 폴더와 데이터 폴더는 남기므로, 필요하면 직접 지웁니다.

---

## 7. 한계와 주의점

1. **실제 Windows에서 아직 검증하지 못했습니다** (맨 위 경고). 특히 SYSTEM agent의 스크린샷(xcap)과
   `SendInput`이 사용자 데스크톱에서 정상 동작하는지 확인이 필요합니다.
2. **할 일은 본인이 선언합니다.** "유튜브 보기"라고 적으면 AI도 유튜브를 허용합니다. 강제로 막고 싶은 것은
   AI가 아니라 **블랙리스트**(관리자 전용)에 넣으세요.
3. **물리적 접근은 막지 못합니다.** 안전 모드 부팅(서비스가 안 뜸), USB 부팅, 다른 OS 등. 안전 모드 진입도 막으려면
   BitLocker나 BIOS 암호 같은 별도 수단이 필요합니다.
4. **같은 이름의 프로세스는 그 세션 안에서 전부** 죽습니다(1인 1세션이면 문제없음).
5. **잠금 화면이나 다른 사용자로 전환된 동안**에는 맨 앞 창이 없거나 화면이 검게 찍힐 수 있습니다.
   그러면 판정이 이상할 수 있지만(대개 `unsure`), 그 시간엔 어차피 작업 계정을 쓰지 않습니다.
6. **로컬 포트**: agent API는 `127.0.0.1:(8770 + 세션번호)`입니다. 같은 PC의 다른 사용자도 접속할 수 있지만,
   할 수 있는 건 그 세션의 집중 세션 시작/종료뿐입니다.
7. **단독 실행 모드의 8765 API는 인증이 없습니다.** 머신 설치가 있으면 열지 않습니다. 하지만
   `digital-wellbeing.exe --standalone`으로 강제로 띄우면 다시 열리니, 공유 PC에서는 쓰지 마세요.
8. **로그 회전은 없습니다.** 로그 파일이 10MB를 넘으면 다음 시작 때 비웁니다.

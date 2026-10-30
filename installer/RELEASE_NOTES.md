## 설치 방법

1. 작업에 쓰는 Windows 계정을 **표준 사용자**로 바꿉니다(설정 → 계정 → 가족 및 기타 사용자 → 계정 유형 변경).
2. **`DigitalWellbeing-Setup-….exe`**를 내려받아 실행합니다. 관리자 비밀번호를 묻습니다.
3. 설치 중 "Work account" 화면에 작업 계정의 사용자 이름을 적습니다. 비워 두면 관리자 계정까지 모든 계정이 관리됩니다.
4. 설치가 끝나면 시작 메뉴의 **Digital Wellbeing settings (admin)**를 열고, AI focus 탭에 Anthropic API 키를 저장합니다.
5. 작업 계정으로 로그인하면 **Digital Wellbeing Focus** 창이 뜹니다.

설치 프로그램 없이 쓰려면 `…-windows-x64.zip`을 풀고 `install.cmd`를 실행합니다.
자세한 설명은 설치 폴더의 `docs\multi_account.md`에 있습니다.

## 주의

- **시험판입니다.** 이 버전은 GitHub Actions의 Windows에서 빌드와 자동 테스트만 거쳤고, 실제 PC에서 서비스·화면 캡처·탭 닫기를 확인하지 않았습니다. 처음 설치한 뒤 `docs\multi_account.md` 5장의 점검 목록을 따라 해 보세요.
- **서명되지 않은 실행 파일입니다.** Windows SmartScreen이 "알 수 없는 게시자" 경고를 띄웁니다. "추가 정보 → 실행"을 눌러야 합니다.
- 제거는 Windows 설정의 **앱 → 설치된 앱**에서 합니다. 설정 폴더 `C:\ProgramData\DigitalWellbeing`은 남으므로, 필요하면 직접 지웁니다.

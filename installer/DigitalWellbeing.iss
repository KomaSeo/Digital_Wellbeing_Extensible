; Responsibility: Build DigitalWellbeing-Setup-<version>.exe (Inno Setup 6): copy
; the three executables to Program Files and run
; `digital-wellbeing-service.exe install --manage-all` (every Windows account is managed,
; the administrators included: the owner's choice). When an earlier install left the owner's
; data, setup asks whether to keep it (the default) or delete it (`install --delete-data`).
; A setup page offers to shut the ways into safe mode (unchecked: `--block-safe-mode`).
; Uninstall asks in one window whether to delete the data (unchecked) and whether to open safe
; mode again (checked: `--restore-safe-mode`).
;
; Built by .github/workflows/release.yml:  ISCC.exe installer\DigitalWellbeing.iss
; The version comes from the APP_VERSION environment variable.

#define AppVersion GetEnv("APP_VERSION")
#if AppVersion == ""
  #define AppVersion "0.0.0-dev"
#endif
#define BuildDir "..\target\release"

[Setup]
AppId={{6F3C2A51-9B8E-4D7A-A0C2-2E5B9F1D7C44}
AppName=Digital Wellbeing
AppVersion={#AppVersion}
AppPublisher=Digital Wellbeing
DefaultDirName={autopf}\DigitalWellbeing
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=DigitalWellbeing-Setup-{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\digital-wellbeing-focus.exe
; Close running Focus apps of logged-on users so their exe can be replaced.
CloseApplications=force
RestartApplications=no

[Files]
Source: "{#BuildDir}\digital-wellbeing-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\digital-wellbeing-focus.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#BuildDir}\digital-wellbeing-mcp.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Digital Wellbeing\Digital Wellbeing Focus"; Filename: "{app}\digital-wellbeing-focus.exe"
Name: "{autoprograms}\Digital Wellbeing\Guide (Korean)"; Filename: "{app}\docs\multi_account.md"

[UninstallRun]
Filename: "{app}\digital-wellbeing-service.exe"; Parameters: "uninstall {code:UninstallOptions}"; Flags: runhidden waituntilterminated; RunOnceId: "UninstallService"

[Code]
var
  DeleteData: Boolean;
  RestoreSafeMode: Boolean;
  DeleteDataOnInstall: Boolean;
  SafeModePage: TInputOptionWizardPage;

{ One more setup page: shut the ways into safe mode (unchecked unless the owner checks it). }
procedure InitializeWizard();
begin
  SafeModePage := CreateInputOptionPage(wpWelcome,
    '안전 모드 막기 (선택)',
    '이 프로그램의 서비스는 안전 모드에서 시작되지 않습니다.',
    '아래를 체크하면 안전 모드로 들어가는 길을 막습니다. Windows 복구 환경(WinRE, Shift+다시 시작의 ' +
    '고급 시작 옵션), 시작에 실패했을 때 뜨는 복구 화면, 부팅 메뉴를 끕니다.' + #13#10 + #13#10 +
    '경고: 막으면 "이 PC 초기화", 시스템 복원, 자동 복구도 쓸 수 없습니다. Windows가 고장 나면 ' +
    'Windows 설치 USB로만 복구할 수 있으니, 설치 USB를 미리 만들어 두세요. BitLocker를 쓰면 복구 키도 ' +
    '따로 보관하세요. 프로그램을 제거할 때 다시 열 수 있습니다.',
    False, False);
  SafeModePage.Add('안전 모드로 들어가는 길 막기 (위의 경고를 읽었습니다)');
  SafeModePage.Values[0] := False;
end;

{ Ask, in one window, whether the owner's data stays on this PC and whether safe mode opens
  again. A silent uninstall keeps the data and opens safe mode again (the defaults). }
function InitializeUninstall(): Boolean;
var
  Form: TSetupForm;
  Note: TNewStaticText;
  DeleteBox, SafeModeBox: TNewCheckBox;
  UninstallButton, CancelButton: TNewButton;
begin
  Result := True;
  DeleteData := False;
  RestoreSafeMode := True;
  if UninstallSilent() then
    exit;
#if Ver >= EncodeVer(6, 3, 0)
  Form := CreateCustomForm(ScaleX(480), ScaleY(250), False, False);
#else
  Form := CreateCustomForm();
#endif
  try
    Form.ClientWidth := ScaleX(480);
    Form.ClientHeight := ScaleY(250);
    Form.Caption := 'Digital Wellbeing 제거';

    Note := TNewStaticText.Create(Form);
    Note.Parent := Form;
    Note.Left := ScaleX(16);
    Note.Top := ScaleY(16);
    Note.Width := Form.ClientWidth - ScaleX(32);
    Note.AutoSize := False;
    Note.Height := ScaleY(64);
    Note.WordWrap := True;
    Note.Caption := '사용자 정보는 할 일 목록, 생활 규칙, 지금 할 일, 설정, 차단과 접근 허용, 통계 기록, ' +
      'Anthropic API 키, 엄격 모드, 로그를 말합니다. 남겨 두면 다시 설치했을 때 그대로 이어서 씁니다.';

    DeleteBox := TNewCheckBox.Create(Form);
    DeleteBox.Parent := Form;
    DeleteBox.Left := ScaleX(16);
    DeleteBox.Top := ScaleY(92);
    DeleteBox.Width := Form.ClientWidth - ScaleX(32);
    DeleteBox.Caption := '이 PC에 남은 사용자 정보도 지우기 (되돌릴 수 없습니다)';
    DeleteBox.Checked := False;

    SafeModeBox := TNewCheckBox.Create(Form);
    SafeModeBox.Parent := Form;
    SafeModeBox.Left := ScaleX(16);
    SafeModeBox.Top := ScaleY(124);
    SafeModeBox.Width := Form.ClientWidth - ScaleX(32);
    SafeModeBox.Caption := '안전 모드와 Windows 복구 환경을 다시 열기';
    SafeModeBox.Checked := True;

    UninstallButton := TNewButton.Create(Form);
    UninstallButton.Parent := Form;
    UninstallButton.Caption := '제거';
    UninstallButton.Width := ScaleX(90);
    UninstallButton.Height := ScaleY(26);
    UninstallButton.Left := Form.ClientWidth - ScaleX(16 + 90 + 8 + 90);
    UninstallButton.Top := Form.ClientHeight - ScaleY(42);
    UninstallButton.ModalResult := mrOk;
    UninstallButton.Default := True;

    CancelButton := TNewButton.Create(Form);
    CancelButton.Parent := Form;
    CancelButton.Caption := '취소';
    CancelButton.Width := ScaleX(90);
    CancelButton.Height := ScaleY(26);
    CancelButton.Left := Form.ClientWidth - ScaleX(16 + 90);
    CancelButton.Top := Form.ClientHeight - ScaleY(42);
    CancelButton.ModalResult := mrCancel;
    CancelButton.Cancel := True;

    if Form.ShowModal() <> mrOk then
    begin
      Result := False;
      exit;
    end;
    DeleteData := DeleteBox.Checked;
    RestoreSafeMode := SafeModeBox.Checked;
  finally
    Form.Free();
  end;
end;

{ The options `uninstall` gets ([UninstallRun]). }
function UninstallOptions(Param: String): String;
begin
  Result := '';
  if DeleteData then
    Result := Result + ' --delete-data';
  if RestoreSafeMode then
    Result := Result + ' --restore-safe-mode';
end;

{ Stop a running service first, so its executable can be replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -Command "Stop-Service -Name DigitalWellbeing -ErrorAction SilentlyContinue"',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  { The owner's data an earlier install left: keep it (the default, and always when silent)
    or start from nothing. }
  DeleteDataOnInstall := False;
  if DirExists(ExpandConstant('{commonappdata}\DigitalWellbeing')) and not WizardSilent() then
    DeleteDataOnInstall := MsgBox(
      '이 PC에 전에 설치했을 때의 사용자 정보가 있습니다. 지우고 처음부터 설치할까요?' + #13#10 + #13#10 +
      '사용자 정보는 할 일 목록, 생활 규칙, 지금 할 일, 설정, 차단과 접근 허용, 통계 기록, ' +
      'Anthropic API 키, 엄격 모드, 로그를 말합니다.' + #13#10 + #13#10 +
      '예: 모두 지우고 처음부터 설치합니다. 되돌릴 수 없습니다.' + #13#10 +
      '아니요: 유지한 채로 설치합니다. 지금까지의 정보를 그대로 이어서 씁니다.',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
  Result := '';
end;

{ Register and start the service once the files are in place. }
procedure CurStepChanged(CurStep: TSetupStep);
var
  Params: String;
  ResultCode: Integer;
begin
  if CurStep <> ssPostInstall then
    exit;
  { Every account, also on an upgrade (plain "install" would keep an older list). }
  Params := 'install --manage-all';
  if DeleteDataOnInstall then
    Params := Params + ' --delete-data';
  if SafeModePage.Values[0] then
    Params := Params + ' --block-safe-mode';
  WizardForm.StatusLabel.Caption := 'Registering the Digital Wellbeing service...';
  if not Exec(ExpandConstant('{app}\digital-wellbeing-service.exe'), Params, ExpandConstant('{app}'),
      SW_HIDE, ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
    MsgBox('Registering the service failed (code ' + IntToStr(ResultCode) + ').' + #13#10 +
      'Open PowerShell as administrator and run:' + #13#10 +
      '"' + ExpandConstant('{app}') + '\digital-wellbeing-service.exe" ' + Params + #13#10 +
      'to see the error.', mbError, MB_OK);
end;

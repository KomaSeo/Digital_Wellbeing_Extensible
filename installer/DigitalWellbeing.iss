; Responsibility: Build DigitalWellbeing-Setup-<version>.exe (Inno Setup 6): copy
; the three executables to Program Files and run
; `digital-wellbeing-service.exe install --manage-all` (every Windows account is managed,
; the administrators included: the owner's choice). When an earlier install left the owner's
; data, setup asks whether to keep it (the default) or delete it (`install --delete-data`);
; uninstall asks the same and runs `uninstall` (or `uninstall --delete-data`).
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
Filename: "{app}\digital-wellbeing-service.exe"; Parameters: "uninstall {code:UninstallDataOption}"; Flags: runhidden waituntilterminated; RunOnceId: "UninstallService"

[Code]
var
  DeleteData: Boolean;
  DeleteDataOnInstall: Boolean;

{ Ask whether the owner's data stays on this PC (a silent uninstall keeps it). }
function InitializeUninstall(): Boolean;
begin
  DeleteData := False;
  if not UninstallSilent() then
    DeleteData := MsgBox(
      '이 PC에 남은 사용자 정보를 지울까요?' + #13#10 + #13#10 +
      '사용자 정보는 할 일 목록, 생활 규칙, 지금 할 일, 설정, 차단과 접근 허용, 통계 기록, ' +
      'Anthropic API 키, 설정 보호, 로그를 말합니다.' + #13#10 + #13#10 +
      '예: 모두 지웁니다. 되돌릴 수 없습니다.' + #13#10 +
      '아니요: 남겨 둡니다. 다시 설치하면 그대로 이어서 씁니다.',
      mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES;
  Result := True;
end;

{ The option `uninstall` gets ([UninstallRun]). }
function UninstallDataOption(Param: String): String;
begin
  if DeleteData then
    Result := '--delete-data'
  else
    Result := '';
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
      'Anthropic API 키, 설정 보호, 로그를 말합니다.' + #13#10 + #13#10 +
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
  WizardForm.StatusLabel.Caption := 'Registering the Digital Wellbeing service...';
  if not Exec(ExpandConstant('{app}\digital-wellbeing-service.exe'), Params, ExpandConstant('{app}'),
      SW_HIDE, ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
    MsgBox('Registering the service failed (code ' + IntToStr(ResultCode) + ').' + #13#10 +
      'Open PowerShell as administrator and run:' + #13#10 +
      '"' + ExpandConstant('{app}') + '\digital-wellbeing-service.exe" ' + Params + #13#10 +
      'to see the error.', mbError, MB_OK);
end;

; Responsibility: Build DigitalWellbeing-Setup-<version>.exe (Inno Setup 6): copy
; the three executables to Program Files, ask which Windows account to manage,
; and run `digital-wellbeing-service.exe install`; uninstall runs `uninstall`.
;
; Built by .github/workflows/release.yml:  ISCC.exe installer\DigitalWellbeing.iss
; The version comes from the APP_VERSION environment variable.

#define AppVersion GetEnv("APP_VERSION")
#if AppVersion == ""
  #define AppVersion "0.0.0-dev"
#endif

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
UninstallDisplayIcon={app}\digital-wellbeing.exe
; Close running Focus apps of logged-on users so their exe can be replaced.
CloseApplications=force
RestartApplications=no

[Files]
Source: "..\target\release\digital-wellbeing-service.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\digital-wellbeing-focus.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\digital-wellbeing.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\docs\*.md"; DestDir: "{app}\docs"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Digital Wellbeing\Digital Wellbeing settings (admin)"; Filename: "{app}\digital-wellbeing.exe"
Name: "{autoprograms}\Digital Wellbeing\Digital Wellbeing Focus"; Filename: "{app}\digital-wellbeing-focus.exe"
Name: "{autoprograms}\Digital Wellbeing\Guide (Korean)"; Filename: "{app}\docs\multi_account.md"

[UninstallRun]
Filename: "{app}\digital-wellbeing-service.exe"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "UninstallService"

[Code]
var
  UserPage: TInputQueryWizardPage;

procedure InitializeWizard;
begin
  UserPage := CreateInputQueryPage(wpWelcome,
    'Work account', 'Which Windows account should be managed?',
    'Enter the user name of the standard (non-administrator) account you work in, ' +
    'for example the part after the backslash in "whoami". ' +
    'Leave it empty to manage every account, including administrators.' + #13#10#13#10 +
    'The work account must be a standard user, or it could switch the blocking off.');
  UserPage.Add('User name:', False);
end;

{ Stop a running service first, so its executable can be replaced. }
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -Command "Stop-Service -Name DigitalWellbeing -ErrorAction SilentlyContinue"',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
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
  Params := 'install';
  if Trim(UserPage.Values[0]) <> '' then
    Params := Params + ' --manage "' + Trim(UserPage.Values[0]) + '"';
  WizardForm.StatusLabel.Caption := 'Registering the Digital Wellbeing service...';
  if not Exec(ExpandConstant('{app}\digital-wellbeing-service.exe'), Params, ExpandConstant('{app}'),
      SW_HIDE, ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
    MsgBox('Registering the service failed (code ' + IntToStr(ResultCode) + ').' + #13#10 +
      'Open PowerShell as administrator and run:' + #13#10 +
      '"' + ExpandConstant('{app}') + '\digital-wellbeing-service.exe" ' + Params + #13#10 +
      'to see the error.', mbError, MB_OK);
end;

; Responsibility: Build DigitalWellbeing-Setup-<version>.exe (Inno Setup 6): copy
; the three executables to Program Files and run
; `digital-wellbeing-service.exe install --manage-all` (every Windows account is managed,
; the administrators included: the owner's choice); uninstall runs `uninstall`.
;
; Built by .github/workflows/release.yml:  ISCC.exe installer\DigitalWellbeing.iss
; The version comes from the APP_VERSION environment variable. The test edition (which can
; turn the settings protection off) is built from the executables in BUILD_DIR with
; EDITION_SUFFIX=-TEST; it installs over the main edition and the main edition over it.

#define AppVersion GetEnv("APP_VERSION")
#if AppVersion == ""
  #define AppVersion "0.0.0-dev"
#endif
#define BuildDir GetEnv("BUILD_DIR")
#if BuildDir == ""
  #define BuildDir "..\target\release"
#endif
#define EditionSuffix GetEnv("EDITION_SUFFIX")
#if EditionSuffix == ""
  #define AppTitle "Digital Wellbeing"
#else
  #define AppTitle "Digital Wellbeing (TEST edition)"
#endif

[Setup]
AppId={{6F3C2A51-9B8E-4D7A-A0C2-2E5B9F1D7C44}
AppName={#AppTitle}
AppVersion={#AppVersion}
AppPublisher=Digital Wellbeing
DefaultDirName={autopf}\DigitalWellbeing
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=DigitalWellbeing-Setup-{#AppVersion}{#EditionSuffix}
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
Filename: "{app}\digital-wellbeing-service.exe"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "UninstallService"

[Code]
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
  { Every account, also on an upgrade (plain "install" would keep an older list). }
  Params := 'install --manage-all';
  WizardForm.StatusLabel.Caption := 'Registering the Digital Wellbeing service...';
  if not Exec(ExpandConstant('{app}\digital-wellbeing-service.exe'), Params, ExpandConstant('{app}'),
      SW_HIDE, ewWaitUntilTerminated, ResultCode) or (ResultCode <> 0) then
    MsgBox('Registering the service failed (code ' + IntToStr(ResultCode) + ').' + #13#10 +
      'Open PowerShell as administrator and run:' + #13#10 +
      '"' + ExpandConstant('{app}') + '\digital-wellbeing-service.exe" ' + Params + #13#10 +
      'to see the error.', mbError, MB_OK);
end;

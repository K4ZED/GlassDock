; ============================================================
;  GlassDock installer (Inno Setup 6)
;  Build:  .\build-installer.ps1   (or: ISCC.exe installer\GlassDock.iss)
;  Per-user install: no admin rights needed.
; ============================================================

#define AppName "GlassDock"
#define AppVersion "1.2.0"
#define AppPublisher "K4ZED"
#define AppURL "https://github.com/K4ZED/GlassDock"
#define AppExe "glassdock.exe"

[Setup]
AppId={{6E9C3B57-2D8A-4F1B-9C61-6A2F0C7E4D10}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
DefaultDirName={localappdata}\{#AppName}
DisableDirPage=auto
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=Output
OutputBaseFilename=GlassDock-Setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern dynamic
WizardSizePercent=110
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
LicenseFile=..\LICENSE
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
WelcomeLabel2=This will install [name/ver] on your computer.%n%nGlassDock replaces the Windows taskbar with a centered glass dock: pinned and open apps, a glass Start menu (press the Windows key), Control Center and Calendar.%n%nThe Windows taskbar is hidden while GlassDock runs and comes back when you quit it or uninstall.
FinishedLabel=[name] is installed.%n%nMove the cursor to the bottom middle of the screen to show the dock. Right-click the dock for settings (auto-hide, glass style, pins).%n%nTo get the Windows taskbar back, right-click the dock and choose Quit GlassDock.

[Tasks]
Name: "autostart"; Description: "Start {#AppName} automatically when I sign in"; GroupDescription: "Startup:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\restore-taskbar.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "Glass dock for Windows"
Name: "{userprograms}\{#AppName} - restore Windows taskbar"; Filename: "powershell.exe"; Parameters: "-ExecutionPolicy Bypass -File ""{app}\restore-taskbar.ps1"""; Comment: "Use only if the Windows taskbar stays hidden after GlassDock stopped"
Name: "{userdesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon
; backup auto-start: Startup folder shortcut (a second launch exits right away)
Name: "{userstartup}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: autostart

[Registry]
; auto-start at sign-in (same key install.ps1 uses); removed on uninstall
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"""; Tasks: autostart; Flags: uninsdeletevalue
; mark it Enabled in Settings > Apps > Startup (Windows can skip Run entries without this)
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"; ValueType: binary; ValueName: "{#AppName}"; ValueData: "02 00 00 00 00 00 00 00 00 00 00 00"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
; fastest auto-start: a per-user task that runs right at sign-in, without the Startup apps delay
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -Command ""$me = $env:USERDOMAIN + '\' + $env:USERNAME; $exe = '{app}\{#AppExe}'; Register-ScheduledTask -TaskName 'GlassDock' -Force -Action (New-ScheduledTaskAction -Execute $exe -WorkingDirectory '{app}') -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $me) -Principal (New-ScheduledTaskPrincipal -UserId $me -LogonType Interactive -RunLevel Limited) -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew -Priority 4) | Out-Null"""; Flags: runhidden waituntilterminated; Tasks: autostart; StatusMsg: "Setting up auto-start..."
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: nowait postinstall skipifsilent
; silent install = in-app update: start the dock again by itself
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WizardSilent

[UninstallRun]
Filename: "powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -Command ""Unregister-ScheduledTask -TaskName 'GlassDock' -Confirm:$false -ErrorAction SilentlyContinue"""; Flags: runhidden waituntilterminated; RunOnceId: "RemoveGlassDockTask"

[UninstallDelete]
; settings (pins, glass style, auto-hide) are kept unless the user asks below
Type: files; Name: "{app}\*.log"

[Code]
const
  WM_CLOSE = $0010;

{ Close a running GlassDock cleanly so it puts the Windows taskbar back. }
procedure CloseGlassDock();
var
  Wnd: HWND;
  Waited: Integer;
begin
  Wnd := FindWindowByClassName('GlassDock');
  if Wnd = 0 then Exit;
  PostMessage(Wnd, WM_CLOSE, 0, 0);
  Waited := 0;
  while (FindWindowByClassName('GlassDock') <> 0) and (Waited < 5000) do
  begin
    Sleep(100);
    Waited := Waited + 100;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseGlassDock();   { the exe cannot be replaced while it runs }
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  CloseGlassDock();
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    if MsgBox('Also delete your GlassDock settings (pinned apps, glass style, auto-hide)?', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(ExpandConstant('{userappdata}\GlassDock'), True, True, True);
  end;
end;

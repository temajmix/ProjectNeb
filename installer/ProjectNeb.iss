#define AppName "ProjectNeb"
#define AppVersion "0.3.0"
#define AppPublisher "ProjectNeb"
#define AppExe "ProjectNeb.exe"
; The release pipeline builds from A:\ProjectNeb; a worktree/QA build overrides the root with
; ISCC's /DRoot=... so it neither reads nor writes the release checkout.
#ifndef Root
#define Root "A:\ProjectNeb"
#endif

[Setup]
AppId={{7F1C2D64-9A3E-4B21-8E77-2C5F0A9B4D13}
AppName={#AppName} (Beta)
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion} Beta
AppPublisher={#AppPublisher}
AppPublisherURL=https://github.com/temajmix/ProjectNeb
DefaultDirName={localappdata}\ProjectNeb\app
DisableProgramGroupPage=yes
DefaultGroupName=ProjectNeb
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#Root}\dist
OutputBaseFilename=ProjectNeb-{#AppVersion}-beta-setup
SetupIconFile={#Root}\assets\neb.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
DisableWelcomePage=no

[Languages]
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Запускать ProjectNeb при входе в Windows"; GroupDescription: "Автозапуск:"
Name: "desktopicon"; Description: "Ярлык на рабочем столе"; GroupDescription: "Ярлыки:"; Flags: unchecked

[Files]
Source: "{#Root}\rust\target\release\ProjectNeb.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-shell.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-wallpaper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-updater.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-app.exe"; DestDir: "{app}"; Flags: ignoreversion
; neb-app.exe links the loader dynamically; without it next to the binary the settings window
; does not start at all.
Source: "{#Root}\rust\target\release\WebView2Loader.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-discord.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-wm.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-zapret.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-windhawk.exe"; DestDir: "{app}"; Flags: ignoreversion
; The dashboard the main window loads. Skip when `npm run build` never ran — neb-app then
; shows its embedded "UI не собран" page instead of a blank window.
Source: "{#Root}\src\Neb.UI\dist\*"; DestDir: "{app}\ui"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist
Source: "{#Root}\assets\neb.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\assets\logo-mark.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\assets\beta-config.json"; DestDir: "{localappdata}\ProjectNeb"; DestName: "config.json"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "{#Root}\docs\beta\README-BETA.md"; DestDir: "{app}"; Flags: ignoreversion isreadme

[InstallDelete]
; The 0.1.x entry point was a separate supervisor binary; 0.2.0 renamed it to ProjectNeb.exe.
Type: files; Name: "{app}\neb-supervisor.exe"

[Icons]
Name: "{group}\ProjectNeb"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\neb.ico"
Name: "{group}\README (Beta)"; Filename: "{app}\README-BETA.md"
Name: "{group}\Удалить ProjectNeb"; Filename: "{uninstallexe}"
Name: "{autodesktop}\ProjectNeb"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\neb.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ProjectNeb"; ValueData: """{app}\{#AppExe}"" --no-settings"; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#AppExe}"; Description: "Запустить ProjectNeb"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM ProjectNeb.exe /F"; Flags: runhidden; RunOnceId: "killMain"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-shell.exe /F"; Flags: runhidden; RunOnceId: "killShell"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-wallpaper.exe /F"; Flags: runhidden; RunOnceId: "killWlp"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-app.exe /F"; Flags: runhidden; RunOnceId: "killApp"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-discord.exe /F"; Flags: runhidden; RunOnceId: "killDiscord"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-wm.exe /F"; Flags: runhidden; RunOnceId: "killWm"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-zapret.exe /F"; Flags: runhidden; RunOnceId: "killZapret"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-windhawk.exe /F"; Flags: runhidden; RunOnceId: "killWindhawk"

[Code]
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM ProjectNeb.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM neb-shell.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM neb-wallpaper.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM neb-app.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

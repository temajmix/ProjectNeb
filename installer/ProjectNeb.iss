#define AppName "ProjectNeb"
#define AppVersion "0.1.0"
#define AppPublisher "ProjectNeb"
#define AppExe "neb-supervisor.exe"
#define Root "A:\ProjectNeb"

[Setup]
AppId={{7F1C2D64-9A3E-4B21-8E77-2C5F0A9B4D13}
AppName={#AppName} (Beta)
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion} Beta
AppPublisher={#AppPublisher}
AppPublisherURL=https://github.com/projectneb
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
Source: "{#Root}\rust\target\release\neb-supervisor.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-shell.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-wallpaper.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\rust\target\release\neb-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\assets\neb.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\assets\logo-mark.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\assets\beta-config.json"; DestDir: "{localappdata}\ProjectNeb"; DestName: "config.json"; Flags: onlyifdoesntexist uninsneveruninstall
Source: "{#Root}\docs\beta\README-BETA.md"; DestDir: "{app}"; Flags: ignoreversion isreadme

[Icons]
Name: "{group}\ProjectNeb"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\neb.ico"
Name: "{group}\README (Beta)"; Filename: "{app}\README-BETA.md"
Name: "{group}\Удалить ProjectNeb"; Filename: "{uninstallexe}"
Name: "{autodesktop}\ProjectNeb"; Filename: "{app}\{#AppExe}"; IconFilename: "{app}\neb.ico"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ProjectNeb"; ValueData: """{app}\{#AppExe}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#AppExe}"; Description: "Запустить ProjectNeb"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-supervisor.exe /F"; Flags: runhidden; RunOnceId: "killSup"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-shell.exe /F"; Flags: runhidden; RunOnceId: "killShell"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM neb-wallpaper.exe /F"; Flags: runhidden; RunOnceId: "killWlp"

[Code]
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM neb-supervisor.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM neb-shell.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM neb-wallpaper.exe /F', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Result := '';
end;

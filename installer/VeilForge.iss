; VeilForge installer (Inno Setup 6). Per-user: no administrator rights needed.

#ifndef AppVersion
  #define AppVersion "1.1.0"
#endif
; Where cargo put the build (scripts/package.ps1 passes CARGO_TARGET_DIR).
#ifndef TargetDir
  #define TargetDir "..\target"
#endif

[Setup]
AppId={{6E0C3B7D-52A4-4F19-8C6E-9B1D7A2F4E53}
AppName=VeilForge
AppVersion={#AppVersion}
AppVerName=VeilForge {#AppVersion}
AppPublisher=Zheer Khalid
AppPublisherURL=https://zheer.dev
AppSupportURL=https://github.com/zheerkhalid/veilforge
AppUpdatesURL=https://github.com/zheerkhalid/veilforge/releases
DefaultDirName={autopf}\VeilForge
DefaultGroupName=VeilForge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=VeilForge-Setup
SetupIconFile={#TargetDir}\veilforge.ico
UninstallDisplayIcon={app}\VeilForge.exe
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
LicenseFile=..\LICENSE
VersionInfoVersion={#AppVersion}
VersionInfoCompany=Zheer Khalid (zheer.dev)
VersionInfoDescription=VeilForge Setup

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "contextmenu"; Description: "Add ""Lock with VeilForge"" and ""Unlock with VeilForge"" to the Explorer right-click menu"; GroupDescription: "Explorer:"

[Files]
Source: "{#TargetDir}\release\VeilForge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\docs\HOW-IT-WORKS.md"; DestDir: "{app}"; DestName: "HOW-IT-WORKS.txt"; Flags: ignoreversion
Source: "..\CHANGELOG.md"; DestDir: "{app}"; DestName: "CHANGELOG.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\VeilForge"; Filename: "{app}\VeilForge.exe"
Name: "{group}\Uninstall VeilForge"; Filename: "{uninstallexe}"
Name: "{autodesktop}\VeilForge"; Filename: "{app}\VeilForge.exe"; Tasks: desktopicon

[Registry]
; Explorer right-click entries, per user. The same keys the Settings toggle
; writes (src/shellmenu.rs); all four are removed on uninstall whether they
; were created here or from Settings.
Root: HKCU; Subkey: "Software\Classes\*\shell\VeilForge.Lock"; ValueType: string; ValueName: ""; ValueData: "Lock with VeilForge"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\VeilForge.Lock"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\VeilForge.exe"",0"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\VeilForge.Lock\command"; ValueType: string; ValueName: ""; ValueData: """{app}\VeilForge.exe"" --lock ""%1"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\VeilForge.Unlock"; ValueType: string; ValueName: ""; ValueData: "Unlock with VeilForge"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\VeilForge.Unlock"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\VeilForge.exe"",0"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\VeilForge.Unlock\command"; ValueType: string; ValueName: ""; ValueData: """{app}\VeilForge.exe"" --unlock ""%1"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\VeilForge.Lock"; ValueType: string; ValueName: ""; ValueData: "Lock with VeilForge"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\VeilForge.Lock"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\VeilForge.exe"",0"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\VeilForge.Lock\command"; ValueType: string; ValueName: ""; ValueData: """{app}\VeilForge.exe"" --lock ""%1"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\VeilForge.Unlock"; ValueType: string; ValueName: ""; ValueData: "Unlock with VeilForge"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\VeilForge.Unlock"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\VeilForge.exe"",0"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\VeilForge.Unlock\command"; ValueType: string; ValueName: ""; ValueData: """{app}\VeilForge.exe"" --unlock ""%1"""; Tasks: contextmenu

[Run]
Filename: "{app}\VeilForge.exe"; Description: "{cm:LaunchProgram,VeilForge}"; Flags: nowait postinstall skipifsilent

; There is deliberately no [UninstallDelete] section for %APPDATA%\VeilForge:
; the encrypted map stays, so that anything still protected can be restored
; by installing VeilForge again and entering the master password.

[Code]
// Uninstalling while items are still protected would leave them locked or
// hidden with no program to bring them back. So the uninstaller first runs
// "VeilForge.exe --restore-all --uninstall", which asks for the master
// password and puts everything back exactly as it was.
//   exit code 0: nothing is protected (any more) -> uninstall continues
//   anything else: cancelled, or some items could not be restored
function InitializeUninstall(): Boolean;
var
  ResultCode: Integer;
  Exe: String;
begin
  Result := True;
  Exe := ExpandConstant('{app}\VeilForge.exe');
  if not FileExists(Exe) then
    exit;
  if UninstallSilent then
  begin
    // No questions can be asked: never remove the program while the map exists.
    if FileExists(ExpandConstant('{userappdata}\VeilForge\map.vfm')) then
      Result := False;
    exit;
  end;
  if MsgBox('Before VeilForge is removed, every file and folder it protects must be put back.' + #13#10#13#10 +
            'You will be asked for your master password. Everything is then restored exactly as it was: place, name, permissions and dates.' + #13#10#13#10 +
            'Continue?', mbConfirmation, MB_YESNO) <> IDYES then
  begin
    Result := False;
    exit;
  end;
  if not Exec(Exe, '--restore-all --uninstall', '', SW_SHOWNORMAL, ewWaitUntilTerminated, ResultCode) then
    ResultCode := -1;
  if ResultCode <> 0 then
  begin
    Result := MsgBox('Not everything was restored (the restore was cancelled, or some items could not be put back).' + #13#10#13#10 +
                     'If you uninstall now, items that are still protected STAY locked or hidden. They are not lost: install VeilForge again and enter your master password to restore them.' + #13#10#13#10 +
                     'Uninstall anyway?', mbError, MB_YESNO or MB_DEFBUTTON2) = IDYES;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    // Entries the Settings page may have created, whatever the install tasks were.
    RegDeleteKeyIncludingSubkeys(HKEY_CURRENT_USER, 'Software\Classes\*\shell\VeilForge.Lock');
    RegDeleteKeyIncludingSubkeys(HKEY_CURRENT_USER, 'Software\Classes\*\shell\VeilForge.Unlock');
    RegDeleteKeyIncludingSubkeys(HKEY_CURRENT_USER, 'Software\Classes\Directory\shell\VeilForge.Lock');
    RegDeleteKeyIncludingSubkeys(HKEY_CURRENT_USER, 'Software\Classes\Directory\shell\VeilForge.Unlock');
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'VeilForge');
  end;
end;

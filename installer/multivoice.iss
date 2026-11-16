; MultiVoice 安裝程式（Inno Setup 6.4 以上；GitHub Actions 建置，見 .github/workflows/release.yml）
;
;   iscc /DAppVersion=0.7.3 /DExeDir=target\release /DIconFile=app.ico installer\multivoice.iss
;
; - 安裝到使用者自己的資料夾（%LOCALAPPDATA%\Programs\MultiVoice），不需要系統管理員權限；
;   一鍵更新會直接換掉安裝資料夾裡的 multivoice.exe，所以不能裝在 Program Files
; - 開始功能表捷徑、可選的桌面捷徑、「應用程式」清單裡可以解除安裝
; - 解除安裝時詢問要不要一併刪除設定（%APPDATA%\MultiVoice）；安靜模式一律保留

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef ExeDir
  #define ExeDir "..\target\release"
#endif

[Setup]
; AppId 不能改：用來辨識同一個程式（升級、解除安裝）
AppId={{6C1C7E0B-3F2A-4E8B-9B57-5D1A2C9E4F31}
AppName=MultiVoice
AppVersion={#AppVersion}
AppVerName=MultiVoice {#AppVersion}
AppPublisher=XiaoYu0708
AppPublisherURL=https://github.com/XiaoYu0708/MultiVoice
AppSupportURL=https://github.com/XiaoYu0708/MultiVoice/issues
AppUpdatesURL=https://github.com/XiaoYu0708/MultiVoice/releases
VersionInfoVersion={#AppVersion}
VersionInfoDescription=MultiVoice 安裝程式
PrivilegesRequired=lowest
DefaultDirName={autopf}\MultiVoice
DisableProgramGroupPage=yes
DisableDirPage=auto
UninstallDisplayIcon={app}\multivoice.exe
UninstallDisplayName=MultiVoice
#ifdef IconFile
SetupIconFile={#IconFile}
#endif
OutputDir=..\dist
OutputBaseFilename=MultiVoice-Setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; 程式在執行時（同一個「default」設定檔的 mutex），安裝／解除安裝前請使用者關掉
AppMutex=MultiVoice-default
CloseApplications=yes

[Languages]
Name: "zhtw"; MessagesFile: "compiler:Languages\ChineseTraditional.isl"

[Tasks]
Name: "desktopicon"; Description: "建立桌面捷徑"; GroupDescription: "其他捷徑："

[Files]
Source: "{#ExeDir}\multivoice.exe"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\MultiVoice"; Filename: "{app}\multivoice.exe"; Comment: "遊戲多頻道語音"
Name: "{autodesktop}\MultiVoice"; Filename: "{app}\multivoice.exe"; Comment: "遊戲多頻道語音"; Tasks: desktopicon

[Run]
Filename: "{app}\multivoice.exe"; Description: "立即開啟 MultiVoice"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; 一鍵更新留下的檔案
Type: files; Name: "{app}\multivoice.exe.old"
Type: files; Name: "{app}\multivoice.exe.new"
Type: dirifempty; Name: "{app}"

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Data: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    Data := ExpandConstant('{userappdata}\MultiVoice');
    if DirExists(Data) and not UninstallSilent() then
      if MsgBox('要一併刪除 MultiVoice 的設定與身分資料嗎？' + #13#10 + #13#10 + Data + #13#10 + #13#10 +
                '（包含名稱、音訊設定、按鍵說話設定、頻道設定檔。選「否」會保留，之後重新安裝可以繼續使用）',
                mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(Data, True, True, True);
  end;
end;

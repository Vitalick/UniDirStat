#ifndef AppVersion
  #error AppVersion must be provided by package-windows.ps1
#endif

[Setup]
AppId={{10D03BB7-55AE-45E3-9C56-CA90891B147E}
AppName=UniDirStat
AppVersion={#AppVersion}
VersionInfoVersion={#NumericVersion}
DefaultDirName={localappdata}\Programs\UniDirStat
DefaultGroupName=UniDirStat
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputPath}
OutputBaseFilename=UniDirStat-{#AppVersion}-windows-x86_64-setup
SetupIconFile={#ProjectRoot}\assets\unidirstat.ico
UninstallDisplayIcon={app}\unidirstat.exe
LicenseFile={#ProjectRoot}\LICENSE
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"

[Files]
Source: "{#BinaryPath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#ProjectRoot}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\UniDirStat"; Filename: "{app}\unidirstat.exe"

[Run]
Filename: "{app}\unidirstat.exe"; Description: "{cm:LaunchProgram,UniDirStat}"; Flags: nowait postinstall skipifsilent

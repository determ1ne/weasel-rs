Unicode true
ManifestDPIAware true
RequestExecutionLevel admin
!ifdef DEV_INSTALLER
  SetCompress off
!else
  SetCompressor /SOLID lzma
!endif

!include MUI2.nsh
!include LogicLib.nsh
!include x64.nsh
!include FileFunc.nsh
!include TextFunc.nsh
!include Sections.nsh
!include WordFunc.nsh
!include WinVer.nsh

ManifestSupportedOS all

!ifndef PROJECT_ROOT
  !error "Use scripts\build-installer.ps1 to supply PROJECT_ROOT."
!endif
!ifndef PRODUCT_VERSION
  !error "Missing PRODUCT_VERSION."
!endif
!ifndef OUTPUT_FILE
  !error "Missing OUTPUT_FILE."
!endif

!define PRODUCT_NAME "小狼毫RS"

!define PRODUCT_KEY "Software\Weasel-RS"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Weasel-RS"
!define X64_RELEASE "${PROJECT_ROOT}\target\x86_64-pc-windows-msvc\release"
!define X86_RELEASE "${PROJECT_ROOT}\target\i686-pc-windows-msvc\release"

Name "${PRODUCT_NAME}"
OutFile "${OUTPUT_FILE}"
InstallDir "$PROGRAMFILES64\Weasel-RS"
VIProductVersion "${PRODUCT_VERSION}.0"
VIAddVersionKey /LANG=2052 "ProductName" "${PRODUCT_NAME}"
VIAddVersionKey /LANG=2052 "FileDescription" "小狼毫RS 安装程序"
VIAddVersionKey /LANG=2052 "FileVersion" "${PRODUCT_VERSION}"
VIAddVersionKey /LANG=2052 "ProductVersion" "${PRODUCT_VERSION}"
VIAddVersionKey /LANG=2052 "LegalCopyright" "Weasel-RS contributors"

!define MUI_ICON "${PROJECT_ROOT}\assets\weasel.ico"
!define MUI_UNICON "${PROJECT_ROOT}\assets\weasel.ico"
!define MUI_ABORTWARNING
!define MUI_WELCOMEPAGE_TEXT "欢迎安装小狼毫 RS，中州韵输入法的 Windows 文本服务框架前端。"
!insertmacro MUI_PAGE_WELCOME
!define MUI_LICENSEPAGE_CHECKBOX
!insertmacro MUI_PAGE_LICENSE "${PROJECT_ROOT}\LICENSE"
!ifndef DEV_INSTALLER
  !insertmacro MUI_PAGE_COMPONENTS
!endif
!insertmacro MUI_PAGE_INSTFILES
!define MUI_FINISHPAGE_REBOOTLATER_DEFAULT
!define MUI_FINISHPAGE_TEXT_REBOOT "小狼毫RS 已安装。部分正在使用的 DLL 或运行库需要重启后才能完成更新。请保存工作后重启计算机。"
!define MUI_FINISHPAGE_TEXT "小狼毫RS 已安装。"
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH
!insertmacro MUI_LANGUAGE "SimpChinese"

Var CopiedFiles
Var TriedRegistration
Var InstallerMutex
Var IsUpgrade
Var InstalledVersion
Var OldDll
Var BackupDll
Var BackupList
Var RetireResult
Var TransactionStarted
Var TransactionCommitted
Var PreviousTip32
Var PreviousTip64

!include "${PROJECT_ROOT}\installer\records.nsh"
!include "${PROJECT_ROOT}\installer\cleanup-backups.nsh"
!include "${PROJECT_ROOT}\installer\vcredist.nsh"
!include "${PROJECT_ROOT}\installer\processes.nsh"
!include "${PROJECT_ROOT}\installer\launch-broker.nsh"
!include "${PAYLOAD_INCLUDE}"

Function .onInstSuccess
  ${If} ${RebootFlag}
    SetErrorLevel 3010
  ${EndIf}
FunctionEnd

!macro LockInstaller
  ; Keep the handle until process exit; serialize install/uninstall across sessions.
  System::Call 'kernel32::CreateMutexW(p0, i0, w "Global\Weasel-RS-Installer") p.r0 ?e'
  Pop $1
  StrCpy $InstallerMutex $0
  ${If} $0 == 0
  ${OrIf} $1 == 183
    MessageBox MB_OK|MB_ICONSTOP "已有安装或卸载程序运行，或无法取得安装锁。" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
!macroend

!macro CheckPlatform
  ${IfNot} ${IsNativeAMD64}
    MessageBox MB_OK|MB_ICONSTOP "仅支持 x64 Windows，不支持 x86 或 ARM64 系统。" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  ${IfNot} ${AtLeastBuild} 17763
    MessageBox MB_OK|MB_ICONSTOP "需要 Windows 10 1809 或更新版本。" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  SetShellVarContext all
!macroend

!macro CheckBroker
  FindWindow $0 "weasel-rs-broker"
  ${If} $0 != 0
    MessageBox MB_OK|MB_ICONSTOP "请先从托盘退出小狼毫RS，再重新运行安装或卸载程序。" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
!macroend

Function .onInit
  !insertmacro CheckPlatform
  !insertmacro LockInstaller
!ifdef DEV_INSTALLER
  Call SelectAllDevComponents
!endif
  StrCpy $CopiedFiles 0
  StrCpy $TriedRegistration 0
  StrCpy $TransactionStarted 0
  StrCpy $TransactionCommitted 0
  StrCpy $PreviousTip32 ""
  StrCpy $PreviousTip64 ""
  StrCpy $IsUpgrade 0
  StrCpy $INSTDIR "$PROGRAMFILES64\Weasel-RS"
  ; makensis emits an x86 installer, while product metadata is intentionally
  ; machine-wide in the 64-bit registry view.
  SetRegView 64
  ReadRegStr $0 HKLM "${PRODUCT_KEY}" "InstallDir"
  ${If} $0 != ""
    ${If} $0 != $INSTDIR
      MessageBox MB_OK|MB_ICONSTOP "已有版本位于其他目录，请先卸载该版本。" /SD IDOK
      SetErrorLevel 1
      Quit
    ${EndIf}
    StrCpy $IsUpgrade 1
    ReadRegStr $InstalledVersion HKLM "${UNINSTALL_KEY}" "DisplayVersion"
    ${If} $InstalledVersion != ""
      ${VersionCompare} "$InstalledVersion" "${PRODUCT_VERSION}" $1
      ${If} $1 == 1
        MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "已安装 $InstalledVersion，即将安装较旧版本 ${PRODUCT_VERSION}。是否继续降级？" /SD IDNO IDYES version_confirmed
        SetErrorLevel 1
        Quit
        version_confirmed:
      ${EndIf}
      ${If} $1 == 0
        MessageBox MB_OKCANCEL|MB_ICONINFORMATION "已安装 ${PRODUCT_VERSION}。继续将覆盖安装，保留用户数据。" /SD IDOK IDOK version_ready
        Quit
      ${ElseIf} $1 == 2
        MessageBox MB_OKCANCEL|MB_ICONINFORMATION "将从 $InstalledVersion 升级到 ${PRODUCT_VERSION}，保留用户数据。" /SD IDOK IDOK version_ready
        Quit
      ${EndIf}
      version_ready:
    ${EndIf}
  ${EndIf}
  ; Remember both views before registration so a failed replacement can restore
  ; the previously active TIP. A non-owned registration requires explicit user
  ; approval but is no longer an unconditional installation blocker.
  SetRegView 32
  ReadRegStr $0 HKCR "CLSID\{16A7AEA9-9EE6-4540-8020-E384C0489BB1}\InprocServer32" ""
  StrCpy $PreviousTip32 "$0"
  SetRegView 64
  ReadRegStr $1 HKCR "CLSID\{16A7AEA9-9EE6-4540-8020-E384C0489BB1}\InprocServer32" ""
  StrCpy $PreviousTip64 "$1"
  ${If} $0 != ""
  ${AndIf} $0 != "$INSTDIR\x86\weasel_tip.dll"
    Goto foreign_registration
  ${EndIf}
  ${If} $1 != ""
  ${AndIf} $1 != "$INSTDIR\x64\weasel_tip.dll"
  ${AndIf} $1 != "$INSTDIR\weasel_tip.dll"
    Goto foreign_registration
  ${EndIf}
  ${If} $IsUpgrade == 0
    ${If} $0 != ""
    ${OrIf} $1 != ""
      Goto foreign_registration
    ${EndIf}
  ${EndIf}
  Goto registration_checked
  foreign_registration:
    MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "检测到不属于当前安装记录的 TIP 注册：$\r$\n$\r$\nx86：$PreviousTip32$\r$\nx64：$PreviousTip64$\r$\n$\r$\n继续安装将用当前版本替换这些注册。是否继续？" /SD IDNO IDYES registration_checked
    SetErrorLevel 1
    Quit
  registration_checked:
  ; A fixed, new directory prevents overwriting user files.
  StrCmp $IsUpgrade 1 fresh_directory
  IfFileExists "$INSTDIR\*.*" 0 fresh_directory
    MessageBox MB_OK|MB_ICONSTOP "安装目录已存在，请检查并手动处理旧文件后重试。" /SD IDOK
    SetErrorLevel 1
    Quit
  fresh_directory:
FunctionEnd

; Called only during the installation phase, never while browsing its pages.
Function StopApplicationProcesses
  StrCpy $ScanMode 0
  Call ScanApplicationProcesses
  StrCmp $ScanResult 0 processes_stopped
  StrCmp $ScanResult 20 settings_open
  StrCmp $ScanResult 10 0 process_error
  ; Use the bundled new broker, because an old installed broker may not know
  ; --shutdown. This command does not start a tray or managed children.
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File /oname=shutdown-broker.exe "${X64_RELEASE}\weasel-broker.exe"
  DetailPrint "正在请求算法服务正常退出……"
  nsExec::ExecToLog /TIMEOUT=40000 '"$PLUGINSDIR\shutdown-broker.exe" --shutdown "$INSTDIR"'
  Pop $0
  !insertmacro InstallLog "INFO: 退出命令返回：$0"
  SetOutPath "$INSTDIR"
  StrCpy $ScanMode 0
  Call ScanApplicationProcesses
  StrCmp $ScanResult 0 processes_stopped
  StrCmp $ScanResult 20 settings_open
  StrCmp $ScanResult 10 0 process_error
  MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "算法服务未能正常退出。是否强制结束仍在运行的小狼毫RS服务进程？" /SD IDNO IDYES force_stop
  SetErrorLevel 1
  Abort
  force_stop:
  StrCpy $ScanMode 1
  Call ScanApplicationProcesses
  StrCmp $ScanResult 20 settings_open
  StrCmp $ScanResult 0 0 process_error
  StrCpy $ScanMode 0
  Call ScanApplicationProcesses
  StrCmp $ScanResult 0 processes_stopped
  StrCmp $ScanResult 20 settings_open
  process_error:
    MessageBox MB_OK|MB_ICONSTOP "无法检查或结束小狼毫RS 进程，请手动退出相关程序后重试。" /SD IDOK
    SetErrorLevel 1
    Abort
  settings_open:
    MessageBox MB_OK|MB_ICONEXCLAMATION "设置应用仍在运行。请保存修改并关闭设置应用，然后重新运行安装程序。" /SD IDOK
    SetErrorLevel 1
    Abort
  processes_stopped:
FunctionEnd

; Rename before extraction: a loaded TIP must never be overwritten in place.
; GetTempFileName reserves a unique sibling, not a shared .old filename.
; The caller decides whether failure is fatal; rollback uses this as a
; best-effort primitive and must continue restoring unrelated files.
Function RetireDll
  StrCpy $RetireResult 1
  IfFileExists "$OldDll" 0 retire_done
  ${GetParent} "$OldDll" $0
  GetTempFileName $BackupDll "$0"
  Delete "$BackupDll"
  ${GetFileName} "$BackupDll" $1
  StrCpy $BackupDll "$OldDll.old.$1"
  ClearErrors
  Rename "$OldDll" "$BackupDll"
  ${If} ${Errors}
    StrCpy $RetireResult 0
    Return
  ${EndIf}
  ; Retain backups until all installation steps succeed.
  ClearErrors
  FileOpen $0 "$PLUGINSDIR\retired-dlls.txt" a
  ${If} ${Errors}
    Rename "$BackupDll" "$OldDll"
    StrCpy $RetireResult 0
    Return
  ${EndIf}
  FileWrite $0 "$BackupDll$\r$\n"
  ${If} ${Errors}
    FileClose $0
    Rename "$BackupDll" "$OldDll"
    StrCpy $RetireResult 0
    Return
  ${EndIf}
  FileClose $0
  retire_done:
FunctionEnd

Function DeleteRetiredDlls
  ClearErrors
  FileOpen $BackupList "$PLUGINSDIR\retired-dlls.txt" r
  IfErrors cleanup_done
  cleanup_next:
    ClearErrors
    FileRead $BackupList $BackupDll
    IfErrors cleanup_close
    ; Each record ends in CRLF; remove it before using the literal path.
    StrCpy $BackupDll $BackupDll -2
    ; Only the renamed OLD file is scheduled, never the new DLL's path.
    Delete /REBOOTOK "$BackupDll"
    Goto cleanup_next
  cleanup_close:
    FileClose $BackupList
  cleanup_done:
FunctionEnd

; Best-effort recovery for an installation that failed after replacing COM/TSF
; registration. The paths were captured before the transaction began.
Function RestorePreviousTipRegistration
  ${If} $PreviousTip32 != ""
    IfFileExists "$PreviousTip32" 0 restore_previous_x86_missing
    ClearErrors
    ExecWait '"$SYSDIR\regsvr32.exe" /s "$PreviousTip32"' $0
    ${If} ${Errors}
    ${OrIf} $0 != 0
      !insertmacro InstallLog "ERROR: 无法恢复先前的 x86 TIP 注册：$PreviousTip32"
    ${EndIf}
    Goto restore_previous_x64
    restore_previous_x86_missing:
      !insertmacro InstallLog "ERROR: 先前的 x86 TIP 文件不存在：$PreviousTip32"
  ${EndIf}
  restore_previous_x64:
  ${If} $PreviousTip64 != ""
    IfFileExists "$PreviousTip64" 0 restore_previous_x64_missing
    ${DisableX64FSRedirection}
    ClearErrors
    ExecWait '"$SYSDIR\regsvr32.exe" /s "$PreviousTip64"' $0
    ${EnableX64FSRedirection}
    ${If} ${Errors}
    ${OrIf} $0 != 0
      !insertmacro InstallLog "ERROR: 无法恢复先前的 x64 TIP 注册：$PreviousTip64"
    ${EndIf}
    Goto restore_previous_done
    restore_previous_x64_missing:
      !insertmacro InstallLog "ERROR: 先前的 x64 TIP 文件不存在：$PreviousTip64"
  ${EndIf}
  restore_previous_done:
FunctionEnd

; Explicit file list: never recursively remove the install root or user data.
!macro AssertSafeUninstallDirectory DIRECTORY
  ClearErrors
  ${un.GetFileAttributes} "${DIRECTORY}" "REPARSE_POINT" $0
  ${IfNot} ${Errors}
  ${AndIf} $0 == 1
    MessageBox MB_OK|MB_ICONSTOP "安装目录包含重定向目录，拒绝删除：${DIRECTORY}" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  ClearErrors
!macroend

!macro RemoveProgramFiles
  !insertmacro RemoveRimePayload
  !insertmacro RemoveWasmPayload
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_abc.settings.json"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_eleven.settings.json"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_ten.dll"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_ten.pdb"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_eleven.dll"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_eleven.pdb"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_abc.dll"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_abc.pdb"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_void.dll"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_void.pdb"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_wasm.dll"
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_wasm.pdb"
  RMDir /REBOOTOK "$INSTDIR\themes"
  Delete /REBOOTOK "$INSTDIR\weasel-broker.exe"
  Delete /REBOOTOK "$INSTDIR\weasel-server.exe"
  Delete /REBOOTOK "$INSTDIR\weasel-renderer.exe"
  Delete /REBOOTOK "$INSTDIR\weasel-settings.exe"
  Delete /REBOOTOK "$INSTDIR\x64\rime.dll"
  Delete /REBOOTOK "$INSTDIR\rime.dll"
  Delete /REBOOTOK "$INSTDIR\x64\weasel_tip.dll"
  Delete /REBOOTOK "$INSTDIR\weasel_tip.dll"
  Delete /REBOOTOK "$INSTDIR\x86\weasel_tip.dll"
  Delete /REBOOTOK "$INSTDIR\weasel_broker.pdb"
  Delete /REBOOTOK "$INSTDIR\weasel_server.pdb"
  Delete /REBOOTOK "$INSTDIR\weasel_renderer.pdb"
  Delete /REBOOTOK "$INSTDIR\weasel_settings.pdb"
  Delete /REBOOTOK "$INSTDIR\weasel_tip.pdb"
  Delete /REBOOTOK "$INSTDIR\x64\weasel_tip.pdb"
  Delete /REBOOTOK "$INSTDIR\rime.pdb"
  Delete /REBOOTOK "$INSTDIR\x86\weasel_tip.pdb"
  Delete /REBOOTOK "$INSTDIR\x64\rime.pdb"
  Delete /REBOOTOK "$INSTDIR\styles-LICENSE.txt"
  Delete /REBOOTOK "$INSTDIR\LICENSE"
  Delete /REBOOTOK "$INSTDIR\weasel.json"
  Delete /REBOOTOK "$INSTDIR\THIRD-PARTY-LICENSES.txt"
  Delete /REBOOTOK "$INSTDIR\THIRD-PARTY-GPL-3.0.txt"
  Delete /REBOOTOK "$INSTDIR\SETTINGS-LICENSE.txt"
  Delete /REBOOTOK "$INSTDIR\Uninstall.exe"
  Delete /REBOOTOK "$INSTDIR\installer-runtime.ps1"
  Delete "$INSTDIR\files.lst"
  Delete "$INSTDIR\files.pending.lst"
  Delete /REBOOTOK "$INSTDIR\weasel-installer-helper.exe"
  RMDir /REBOOTOK "$INSTDIR\x86"
  RMDir /REBOOTOK "$INSTDIR\x64"
  RMDir /REBOOTOK "$INSTDIR\rime-data"
  RMDir /REBOOTOK "$INSTDIR\theme-wasm"
  RMDir /REBOOTOK "$INSTDIR"
!macroend

Section "Weasel-RS" SEC_MAIN
  SectionIn RO
  Call BeginInstallLog
  InitPluginsDir
  Call BeginRecords
  SetOverwrite on
  SetOutPath "$INSTDIR"
  StrCpy $CopiedFiles 1
  ClearErrors
  !insertmacro ManagedFile "${X64_RELEASE}\weasel-broker.exe" "weasel-broker.exe"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel-server.exe" "weasel-server.exe"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel-renderer.exe" "weasel-renderer.exe"
  !insertmacro ManagedFile "${PROJECT_ROOT}\server\src\styles-LICENSE.txt" "styles-LICENSE.txt"
  !insertmacro ManagedFile "${PROJECT_ROOT}\LICENSE" "LICENSE"
  !insertmacro ManagedFile "${PROJECT_ROOT}\weasel.json" "weasel.json"
  !insertmacro ManagedFile "${PROJECT_ROOT}\THIRD-PARTY-LICENSES.txt" "THIRD-PARTY-LICENSES.txt"
  !insertmacro ManagedFile "${PROJECT_ROOT}\THIRD-PARTY-GPL-3.0.txt" "THIRD-PARTY-GPL-3.0.txt"
  SetOutPath "$INSTDIR\x64"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_tip.dll" "weasel_tip.dll"
  SetOutPath "$INSTDIR\x86"
  !insertmacro ManagedFile "${X86_RELEASE}\weasel_tip.dll" "weasel_tip.dll"
  CreateDirectory "$INSTDIR\rime-data"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 Weasel-RS 文件，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "librime" SEC_LIBRIME
  SectionIn RO
  ClearErrors
  SetOutPath "$INSTDIR"
  !insertmacro ManagedFile "${PROJECT_ROOT}\artifacts\librime\dist\lib\rime.dll" "rime.dll"
  SetOutPath "$INSTDIR\rime-data"
  !insertmacro RimePayload
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 librime，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "设置应用" SEC_SETTINGS
  ClearErrors
  SetOutPath "$INSTDIR"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel-settings.exe" "weasel-settings.exe"
  !insertmacro ManagedFile "${PROJECT_ROOT}\settings\LICENSE-NOTICE.txt" "SETTINGS-LICENSE.txt"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装设置应用，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

SectionGroup /e "候选主题" SEC_THEMES
Section "ten（必选）" SEC_THEME_TEN
  SectionIn RO
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_ten.dll" "weasel_theme_ten.dll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 ten 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "eleven" SEC_THEME_ELEVEN
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_eleven.dll" "weasel_theme_eleven.dll"
  !insertmacro ManagedFile "${X64_RELEASE}\themes\weasel_theme_eleven.settings.json" "weasel_theme_eleven.settings.json"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 eleven 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "abc" SEC_THEME_ABC
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_abc.dll" "weasel_theme_abc.dll"
  !insertmacro ManagedFile "${X64_RELEASE}\themes\weasel_theme_abc.settings.json" "weasel_theme_abc.settings.json"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 abc 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "void" SEC_THEME_VOID
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_void.dll" "weasel_theme_void.dll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 void 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

!ifdef HAVE_WASM_THEME
Section "wasm" SEC_THEME_WASM
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_wasm.dll" "weasel_theme_wasm.dll"
  SetOutPath "$INSTDIR\theme-wasm"
  ; Optional artifacts: preserve offline/skipped-build packaging.
  !insertmacro WasmPayload
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 WASM 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd
!endif

SectionGroupEnd

!ifndef MINI_INSTALLER
Section /o "调试符号" SEC_SYMBOLS
  ClearErrors
  SetOutPath "$INSTDIR"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_broker.pdb" "weasel_broker.pdb"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_server.pdb" "weasel_server.pdb"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_renderer.pdb" "weasel_renderer.pdb"
  ${If} ${SectionIsSelected} ${SEC_SETTINGS}
    !insertmacro ManagedFile "${X64_RELEASE}\weasel_settings.pdb" "weasel_settings.pdb"
  ${EndIf}
  !insertmacro ManagedFile "${PROJECT_ROOT}\artifacts\librime\dist\lib\rime.pdb" "rime.pdb"
  SetOutPath "$INSTDIR\x64"
  !insertmacro ManagedFile "${X64_RELEASE}\weasel_tip.pdb" "weasel_tip.pdb"
  SetOutPath "$INSTDIR\x86"
  !insertmacro ManagedFile "${X86_RELEASE}\weasel_tip.pdb" "weasel_tip.pdb"
  SetOutPath "$INSTDIR\themes"
  ${If} ${SectionIsSelected} ${SEC_THEME_TEN}
    !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_ten.pdb" "weasel_theme_ten.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_ELEVEN}
    !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_eleven.pdb" "weasel_theme_eleven.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_ABC}
    !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_abc.pdb" "weasel_theme_abc.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_VOID}
    !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_void.pdb" "weasel_theme_void.pdb"
  ${EndIf}
!ifdef HAVE_WASM_THEME
  ${If} ${SectionIsSelected} ${SEC_THEME_WASM}
!if /FileExists "${X64_RELEASE}\weasel_theme_wasm.pdb"
    !insertmacro ManagedFile "${X64_RELEASE}\weasel_theme_wasm.pdb" "weasel_theme_wasm.pdb"
!endif
  ${EndIf}
!endif
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装调试符号，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

!endif
; Commit only after every selected component has been staged successfully.
Section "-注册与安装信息" SEC_REGISTER
  SetOutPath "$StageRoot"
  ClearErrors
  WriteUninstaller "$StageRoot\Uninstall.exe"
  ${If} ${Errors}
    Goto install_failed
  ${EndIf}
  FileWriteUTF16LE $ManifestHandle "Uninstall.exe$\r$\n"
  IfErrors install_failed
  Call CloseRecords

  ; Prerequisite failure must leave the existing application untouched.
  Call InstallRuntime_x86
  Call InstallRuntime_x64
  Call StopApplicationProcesses
  ${If} $IsUpgrade == 1
    Call BackupOldPayload
  ${EndIf}
  StrCpy $TransactionStarted 1
  Call RemoveOldPayload
  Delete "$INSTDIR\files.lst"
  Delete "$INSTDIR\files.pending.lst"
  !insertmacro InstallLog "INFO: 开始提交新文件"
  Call CommitStagedPayload

  !insertmacro InstallLog "INFO: 开始注册 TIP"
  ; RegDLL is 32-bit in this installer; use native regsvr32 for the x64 DLL.
  SetOutPath "$INSTDIR"
  StrCpy $TriedRegistration 1
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\x86\weasel_tip.dll"' $0
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法启动 x86 TIP 注册程序"
    Goto install_failed
  ${EndIf}
  !insertmacro InstallLog "INFO: x86 TIP 注册退出码=$0"
  StrCmp $0 0 0 install_failed
  ${DisableX64FSRedirection}
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\x64\weasel_tip.dll"' $0
  ${EnableX64FSRedirection}
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法启动 x64 TIP 注册程序"
    Goto install_failed
  ${EndIf}
  !insertmacro InstallLog "INFO: x64 TIP 注册退出码=$0"
  StrCmp $0 0 0 install_failed

  ClearErrors
  WriteRegStr HKLM "${PRODUCT_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\weasel-broker.exe,0"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  IfErrors install_failed
  Call FinishRecords
  StrCpy $TransactionCommitted 1

  CreateDirectory "$SMPROGRAMS\小狼毫RS"
  Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS.lnk"
  CreateShortcut "$SMPROGRAMS\小狼毫RS\卸载.lnk" "$INSTDIR\Uninstall.exe"
  ${If} ${Errors}
    !insertmacro InstallLog "WARN: 无法创建卸载快捷方式"
  ${EndIf}
  nsExec::ExecToLog '"$INSTDIR\weasel-broker.exe" --install-shortcut "$SMPROGRAMS\小狼毫RS\小狼毫RS算法服务.lnk"'
  Pop $0
  ${If} $0 != 0
    !insertmacro InstallLog "WARN: 创建算法服务快捷方式失败：$0"
  ${EndIf}
  Call DeleteRetiredDlls
  Call CleanupOldDllBackups
  !insertmacro InstallLog "INFO: 正在初始化当前桌面用户……"
  Call ProvisionCurrentUser
  !insertmacro InstallLog "INFO: 正在启动算法服务……"
  Call LaunchBrokerUnelevated
  Call FinishInstallLog
  Goto install_done
  install_failed:
    MessageBox MB_OK|MB_ICONSTOP "安装或 TIP 注册失败。请检查文件权限和 x86/x64 VC++ 运行库。" /SD IDOK
    SetErrorLevel 1
    Abort
  install_done:
SectionEnd


!ifdef DEV_INSTALLER
; SEC_REGISTER is the final installation section. Select every component,
; including /o sections, while preserving read-only and other section flags.
Function SelectAllDevComponents
  Push $0
  Push $1
  ${For} $0 0 ${SEC_REGISTER}
    SectionGetFlags $0 $1
    IntOp $1 $1 | ${SF_SELECTED}
    SectionSetFlags $0 $1
  ${Next}
  Pop $1
  Pop $0
FunctionEnd
!else
!insertmacro MUI_FUNCTION_DESCRIPTION_BEGIN
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_MAIN} "小狼毫 RS 程序和所需运行库（必选）。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_LIBRIME} "Rime 输入引擎和共享方案数据（必选）。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_SETTINGS} "图形化设置应用。"
!ifndef MINI_INSTALLER
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_SYMBOLS} "用于崩溃分析的调试符号。"
!endif
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEMES} "选择安装的候选窗口主题。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_TEN} "Direct2D 候选栏，作为必选回退主题。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_ELEVEN} "XAML 候选栏，适用于 Windows 10 1903 及以上。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_ABC} "复古候选窗口，支持外部预编辑。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_VOID} "不显示窗口的主题及接口示例。"
!ifdef HAVE_WASM_THEME
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_WASM} "WebAssembly 候选主题后端及随附主题，支持加载自定义 .wasm 主题文件。"
!endif
!insertmacro MUI_FUNCTION_DESCRIPTION_END
!endif

Function .onInstFailed
  !insertmacro InstallLog "ERROR: 安装未完成；正在恢复安装前状态；日志：$InstallLog"
  Call CloseRecords
  ${If} $TransactionStarted == 1
  ${AndIf} $TransactionCommitted == 0
    ; Remove any newly registered classes before replacing the DLLs again.
    ${If} $TriedRegistration == 1
      ClearErrors
      ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x86\weasel_tip.dll"' $0
      ${DisableX64FSRedirection}
      ClearErrors
      ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x64\weasel_tip.dll"' $1
      ${EnableX64FSRedirection}
    ${EndIf}
    Call RemoveCommittedPayload
    ${If} $IsUpgrade == 1
      Call RestoreOldPayload
      WriteRegStr HKLM "${PRODUCT_KEY}" "InstallDir" "$INSTDIR"
      ${If} $InstalledVersion != ""
        WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "$InstalledVersion"
      ${EndIf}
    ${Else}
      DeleteRegKey HKLM "${UNINSTALL_KEY}"
      DeleteRegKey HKLM "${PRODUCT_KEY}"
      !insertmacro RemoveProgramFiles
    ${EndIf}
    ${If} $TriedRegistration == 1
      Call RestorePreviousTipRegistration
    ${EndIf}
    Call DeleteRetiredDlls
  ${ElseIf} $IsUpgrade == 0
  ${AndIf} $CopiedFiles == 1
    Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS.lnk"
    Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS算法服务.lnk"
    Delete "$SMPROGRAMS\小狼毫RS\卸载.lnk"
    RMDir "$SMPROGRAMS\小狼毫RS"
    DeleteRegKey HKLM "${UNINSTALL_KEY}"
    DeleteRegKey HKLM "${PRODUCT_KEY}"
    !insertmacro RemoveProgramFiles
  ${EndIf}
  ${If} $InstallLogHandle != ""
    FileClose $InstallLogHandle
    StrCpy $InstallLogHandle ""
  ${EndIf}
  MessageBox MB_OK|MB_ICONEXCLAMATION "安装未完成。已尝试恢复安装前状态。$\r$\n安装日志：$InstallLog" /SD IDOK
FunctionEnd

Function un.onInit
  !insertmacro CheckPlatform
  !insertmacro LockInstaller
  !insertmacro CheckBroker
  SetRegView 64
  ReadRegStr $0 HKLM "${PRODUCT_KEY}" "InstallDir"
  ${If} $0 != $INSTDIR
  ${OrIf} $INSTDIR != "$PROGRAMFILES64\Weasel-RS"
    MessageBox MB_OK|MB_ICONSTOP "安装目录校验失败，拒绝注销或删除文件。" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
  !insertmacro AssertSafeUninstallDirectory "$INSTDIR"
  !insertmacro AssertSafeUninstallDirectory "$INSTDIR\x86"
  !insertmacro AssertSafeUninstallDirectory "$INSTDIR\x64"
  !insertmacro AssertSafeUninstallDirectory "$INSTDIR\themes"
  !insertmacro ValidateRimePayloadRemoval
  !insertmacro ValidateWasmPayloadRemoval
FunctionEnd

Section "Uninstall"
  SetOutPath "$TEMP"
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x86\weasel_tip.dll"' $0
  IfErrors uninstall_failed
  StrCmp $0 0 0 uninstall_failed
  ; Run in the interactive shell's context; the elevated HKCU may belong to
  ; credentials supplied only for UAC.
  Call un.RemoveCurrentUserProvisioning
  ${DisableX64FSRedirection}
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x64\weasel_tip.dll"' $0
  ${EnableX64FSRedirection}
  IfErrors uninstall_failed
  StrCmp $0 0 0 uninstall_failed
  Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS.lnk"
  Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS算法服务.lnk"
  Delete "$SMPROGRAMS\小狼毫RS\卸载.lnk"
  RMDir "$SMPROGRAMS\小狼毫RS"
  !insertmacro RemoveProgramFiles
  DeleteRegKey HKLM "${UNINSTALL_KEY}"
  DeleteRegKey HKLM "${PRODUCT_KEY}"
  Goto uninstall_done
  uninstall_failed:
    MessageBox MB_OK|MB_ICONSTOP "TIP 注销失败，已停止删除文件。请关闭使用输入法的应用后重试。" /SD IDOK
    SetErrorLevel 1
    Abort
  uninstall_done:
SectionEnd

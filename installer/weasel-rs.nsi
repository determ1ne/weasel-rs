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
!include Sections.nsh

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
Var OldDll
Var BackupDll
Var BackupList

!include "${PROJECT_ROOT}\installer\vcredist.nsh"
!include "${PROJECT_ROOT}\installer\processes.nsh"
!include "${PROJECT_ROOT}\installer\launch-broker.nsh"

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
  SetRegView 64
  ReadRegStr $0 HKLM "SOFTWARE\Microsoft\Windows NT\CurrentVersion" "CurrentBuildNumber"
  ${If} $0 < 17763
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
  StrCpy $IsUpgrade 0
  StrCpy $INSTDIR "$PROGRAMFILES64\Weasel-RS"
  ReadRegStr $0 HKLM "${PRODUCT_KEY}" "InstallDir"
  ${If} $0 != ""
    ${If} $0 != $INSTDIR
      MessageBox MB_OK|MB_ICONSTOP "已有版本位于其他目录，请先卸载该版本。" /SD IDOK
      SetErrorLevel 1
      Quit
    ${EndIf}
    StrCpy $IsUpgrade 1
  ${EndIf}
  ; Also reject manual registrations, so rollback cannot remove someone else's TIP.
  SetRegView 32
  ReadRegStr $0 HKCR "CLSID\{16A7AEA9-9EE6-4540-8020-E384C0489BB1}\InprocServer32" ""
  SetRegView 64
  ReadRegStr $1 HKCR "CLSID\{16A7AEA9-9EE6-4540-8020-E384C0489BB1}\InprocServer32" ""
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
    MessageBox MB_OK|MB_ICONSTOP "检测到手动注册的 TIP，请先注销旧 TIP，再运行安装程序。" /SD IDOK
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
  StrCmp $ScanResult 10 0 process_error
  StrCpy $ScanMode 1
  Call ScanApplicationProcesses
  StrCmp $ScanResult 0 0 process_error
  StrCpy $ScanMode 0
  Call ScanApplicationProcesses
  StrCmp $ScanResult 0 processes_stopped
  process_error:
    MessageBox MB_OK|MB_ICONSTOP "无法检查或结束小狼毫RS 进程，请手动退出相关程序后重试。" /SD IDOK
    SetErrorLevel 1
    Abort
  processes_stopped:
FunctionEnd

; Rename before extraction: a loaded TIP must never be overwritten in place.
; GetTempFileName reserves a unique sibling, not a shared .old filename.
Function RetireDll
  IfFileExists "$OldDll" 0 retire_done
  ${GetParent} "$OldDll" $0
  GetTempFileName $BackupDll "$0"
  Delete "$BackupDll"
  ${GetFileName} "$BackupDll" $1
  StrCpy $BackupDll "$OldDll.old.$1"
  ClearErrors
  Rename "$OldDll" "$BackupDll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法重命名旧 DLL：$OldDll。请关闭相关应用后重试。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  ; Retain backups until all installation steps succeed.
  FileOpen $0 "$PLUGINSDIR\retired-dlls.txt" a
  FileWrite $0 "$BackupDll$\r$\n"
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

; Explicit file list: never recursively remove the install root or user data.
!macro RemoveProgramFiles
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
  RMDir "$INSTDIR\themes"
  Delete /REBOOTOK "$INSTDIR\weasel-broker.exe"
  Delete /REBOOTOK "$INSTDIR\weasel-server.exe"
  Delete /REBOOTOK "$INSTDIR\weasel-renderer.exe"
  Delete /REBOOTOK "$INSTDIR\x64\rime.dll"
  Delete /REBOOTOK "$INSTDIR\rime.dll"
  Delete /REBOOTOK "$INSTDIR\x64\weasel_tip.dll"
  Delete /REBOOTOK "$INSTDIR\weasel_tip.dll"
  Delete /REBOOTOK "$INSTDIR\x86\weasel_tip.dll"
  Delete /REBOOTOK "$INSTDIR\weasel_broker.pdb"
  Delete /REBOOTOK "$INSTDIR\weasel_server.pdb"
  Delete /REBOOTOK "$INSTDIR\weasel_renderer.pdb"
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
  Delete /REBOOTOK "$INSTDIR\Uninstall.exe"
  Delete /REBOOTOK "$INSTDIR\installer-runtime.ps1"
  Delete /REBOOTOK "$INSTDIR\weasel-installer-helper.exe"
  RMDir "$INSTDIR\x86"
  RMDir "$INSTDIR\x64"
  RMDir "$INSTDIR\rime-data"
  RMDir "$INSTDIR"
!macroend

Section "Weasel-RS" SEC_MAIN
  SectionIn RO
  ; Shared prerequisites remain installed even if our own install later fails.
  Call InstallRuntime_x86
  Call InstallRuntime_x64
  InitPluginsDir
  Call StopApplicationProcesses
  StrCpy $OldDll "$INSTDIR\x64\weasel_tip.dll"
  Call RetireDll
  ; Retire both the current layout and DLLs left by older installers.
  StrCpy $OldDll "$INSTDIR\weasel_tip.dll"
  Call RetireDll
  StrCpy $OldDll "$INSTDIR\x86\weasel_tip.dll"
  Call RetireDll
  StrCpy $OldDll "$INSTDIR\x64\rime.dll"
  Call RetireDll
  StrCpy $OldDll "$INSTDIR\rime.dll"
  Call RetireDll
  ; Retire every known theme, even deselected ones during an upgrade.
  StrCpy $OldDll "$INSTDIR\themes\weasel_theme_ten.dll"
  Call RetireDll
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_ten.pdb"
  StrCpy $OldDll "$INSTDIR\themes\weasel_theme_eleven.dll"
  Call RetireDll
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_eleven.pdb"
  StrCpy $OldDll "$INSTDIR\themes\weasel_theme_abc.dll"
  Call RetireDll
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_abc.pdb"
  StrCpy $OldDll "$INSTDIR\themes\weasel_theme_void.dll"
  Call RetireDll
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_void.pdb"
  StrCpy $OldDll "$INSTDIR\themes\weasel_theme_wasm.dll"
  Call RetireDll
  Delete /REBOOTOK "$INSTDIR\themes\weasel_theme_wasm.pdb"
  SetOverwrite on
  SetOutPath "$INSTDIR"
  StrCpy $CopiedFiles 1
  ClearErrors
  File "${X64_RELEASE}\weasel-broker.exe"
  File "${X64_RELEASE}\weasel-server.exe"
  File "${X64_RELEASE}\weasel-renderer.exe"
  File "${PROJECT_ROOT}\server\src\styles-LICENSE.txt"
  File "${PROJECT_ROOT}\LICENSE"
  File "${PROJECT_ROOT}\weasel.json"
  File "${PROJECT_ROOT}\THIRD-PARTY-LICENSES.txt"
  File "${PROJECT_ROOT}\THIRD-PARTY-GPL-3.0.txt"
  SetOutPath "$INSTDIR\x64"
  File "${X64_RELEASE}\weasel_tip.dll"
  SetOutPath "$INSTDIR\x86"
  File "${X86_RELEASE}\weasel_tip.dll"
  CreateDirectory "$INSTDIR\rime-data"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 Weasel-RS 文件，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "librime" SEC_LIBRIME
  SectionIn RO
  Call StopApplicationProcesses
  ClearErrors
  SetOutPath "$INSTDIR"
  File "${PROJECT_ROOT}\artifacts\librime\dist\lib\rime.dll"
  SetOutPath "$INSTDIR\rime-data"
  File /r "${PROJECT_ROOT}\assets\rime-data\*"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 librime，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

SectionGroup /e "候选主题" SEC_THEMES
Section "ten（必选）" SEC_THEME_TEN
  SectionIn RO
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  File "${X64_RELEASE}\weasel_theme_ten.dll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 ten 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "eleven" SEC_THEME_ELEVEN
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  File "${X64_RELEASE}\weasel_theme_eleven.dll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 eleven 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "abc" SEC_THEME_ABC
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  File "${X64_RELEASE}\weasel_theme_abc.dll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 abc 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "void" SEC_THEME_VOID
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  File "${X64_RELEASE}\weasel_theme_void.dll"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 void 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

Section "wasm" SEC_THEME_WASM
  ClearErrors
  SetOutPath "$INSTDIR\themes"
  File /nonfatal "${X64_RELEASE}\weasel_theme_wasm.dll"
  SetOutPath "$INSTDIR\theme-wasm"
  ; Optional artifacts: preserve offline/skipped-build packaging.
  File /nonfatal /r "${PROJECT_ROOT}\artifacts\theme-wasm\*"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装 WASM 主题，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

SectionGroupEnd

Section /o "调试符号" SEC_SYMBOLS
  ClearErrors
  SetOutPath "$INSTDIR"
  File "${X64_RELEASE}\weasel_broker.pdb"
  File "${X64_RELEASE}\weasel_server.pdb"
  File "${X64_RELEASE}\weasel_renderer.pdb"
  File "${PROJECT_ROOT}\artifacts\librime\dist\lib\rime.pdb"
  SetOutPath "$INSTDIR\x64"
  File "${X64_RELEASE}\weasel_tip.pdb"
  SetOutPath "$INSTDIR\x86"
  File "${X86_RELEASE}\weasel_tip.pdb"
  SetOutPath "$INSTDIR\themes"
  ${If} ${SectionIsSelected} ${SEC_THEME_TEN}
    File "${X64_RELEASE}\weasel_theme_ten.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_ELEVEN}
    File "${X64_RELEASE}\weasel_theme_eleven.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_ABC}
    File "${X64_RELEASE}\weasel_theme_abc.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_VOID}
    File "${X64_RELEASE}\weasel_theme_void.pdb"
  ${EndIf}
  ${If} ${SectionIsSelected} ${SEC_THEME_WASM}
    File /nonfatal "${X64_RELEASE}\weasel_theme_wasm.pdb"
  ${EndIf}
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法安装调试符号，请检查磁盘空间和文件权限。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
SectionEnd

; Register only after every selected component has been copied successfully.
Section "-注册与安装信息" SEC_REGISTER
  Call StopApplicationProcesses
  ; RegDLL is 32-bit in this installer; use native regsvr32 for the x64 DLL.
  SetOutPath "$INSTDIR"
  StrCpy $TriedRegistration 1
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\x86\weasel_tip.dll"' $0
  IfErrors install_failed
  StrCmp $0 0 0 install_failed
  ${DisableX64FSRedirection}
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s "$INSTDIR\x64\weasel_tip.dll"' $0
  ${EnableX64FSRedirection}
  IfErrors install_failed
  StrCmp $0 0 0 install_failed

  ClearErrors
  WriteUninstaller "$INSTDIR\Uninstall.exe"
  WriteRegStr HKLM "${PRODUCT_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\weasel-broker.exe,0"
  WriteRegStr HKLM "${UNINSTALL_KEY}" "UninstallString" '$\"$INSTDIR\Uninstall.exe$\"'
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKLM "${UNINSTALL_KEY}" "NoRepair" 1
  CreateDirectory "$SMPROGRAMS\小狼毫RS"
  Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS.lnk"
  CreateShortcut "$SMPROGRAMS\小狼毫RS\卸载.lnk" "$INSTDIR\Uninstall.exe"
  IfErrors install_failed
  nsExec::ExecToLog '"$INSTDIR\weasel-broker.exe" --install-shortcut "$SMPROGRAMS\小狼毫RS\小狼毫RS算法服务.lnk"'
  Pop $0
  ${If} $0 != 0
    DetailPrint "创建算法服务快捷方式失败：$0"
    Goto install_failed
  ${EndIf}
  IfErrors install_failed
  DetailPrint "正在为当前账户部署 Rime 数据……"
  nsExec::ExecToLog '"$INSTDIR\weasel-server.exe" --deploy --silent'
  Pop $0
  ${If} $0 != 0
    ; Registration and uninstall metadata already exist. Preserve this usable
    ; recovery point rather than unregistering/deleting an installed program.
    StrCpy $IsUpgrade 1
    MessageBox MB_OK|MB_ICONSTOP "Rime 部署失败（$0），程序文件已保留，可重新运行安装程序。请检查当前账户的 Rime 数据及日志文件。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  ClearErrors
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "小狼毫RS算法服务" '$\"$INSTDIR\weasel-broker.exe$\"'
  ${If} ${Errors}
    StrCpy $IsUpgrade 1
    MessageBox MB_OK|MB_ICONSTOP "无法设置当前账户的登录启动项。程序文件已保留，请检查注册表权限后重试。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  Call DeleteRetiredDlls
  DetailPrint "正在启动算法服务……"
  Call LaunchBrokerUnelevated
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
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_SYMBOLS} "用于崩溃分析的调试符号。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEMES} "选择安装的候选窗口主题。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_TEN} "Direct2D 候选栏，作为必选回退主题。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_ELEVEN} "XAML 候选栏，适用于 Windows 10 1903 及以上。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_ABC} "复古候选窗口，支持外部预编辑。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_VOID} "不显示窗口的主题及接口示例。"
  !insertmacro MUI_DESCRIPTION_TEXT ${SEC_THEME_WASM} "WebAssembly 候选主题后端及随附主题，支持加载自定义 .wasm 主题文件。"
!insertmacro MUI_FUNCTION_DESCRIPTION_END
!endif

Function .onInstFailed
  ${If} $IsUpgrade == 1
    ; Never remove the previous installation's registration or user data.
    MessageBox MB_OK|MB_ICONEXCLAMATION "安装未完成，现有文件及注册信息已保留。请重新运行安装程序完成更新。" /SD IDOK
    Return
  ${EndIf}
  ${If} $TriedRegistration == 1
    ; Best effort rollback, but keep files if either unregister operation fails.
    StrCpy $0 1
    StrCpy $1 1
    ClearErrors
    ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x86\weasel_tip.dll"' $0
    ${If} ${Errors}
      StrCpy $0 1
    ${EndIf}
    ${DisableX64FSRedirection}
    ClearErrors
    ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x64\weasel_tip.dll"' $1
    ${EnableX64FSRedirection}
    ${If} ${Errors}
      StrCpy $1 1
    ${EndIf}
    ${If} $0 != 0
    ${OrIf} $1 != 0
      MessageBox MB_OK|MB_ICONEXCLAMATION "无法完成输入法注销，安装文件已保留。请手动注销 x86 和 x64 TIP 后重试。" /SD IDOK
      Return
    ${EndIf}
  ${EndIf}
  ${If} $CopiedFiles == 1
    Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS.lnk"
    Delete "$SMPROGRAMS\小狼毫RS\小狼毫RS算法服务.lnk"
    Delete "$SMPROGRAMS\小狼毫RS\卸载.lnk"
    RMDir "$SMPROGRAMS\小狼毫RS"
    DeleteRegKey HKLM "${UNINSTALL_KEY}"
    DeleteRegKey HKLM "${PRODUCT_KEY}"
    !insertmacro RemoveProgramFiles
  ${EndIf}
FunctionEnd

Function un.onInit
  !insertmacro CheckPlatform
  !insertmacro LockInstaller
  !insertmacro CheckBroker
  ReadRegStr $0 HKLM "${PRODUCT_KEY}" "InstallDir"
  ${If} $0 != $INSTDIR
  ${OrIf} $INSTDIR != "$PROGRAMFILES64\Weasel-RS"
    MessageBox MB_OK|MB_ICONSTOP "安装目录校验失败，拒绝注销或删除文件。" /SD IDOK
    SetErrorLevel 1
    Quit
  ${EndIf}
FunctionEnd

Section "Uninstall"
  SetOutPath "$TEMP"
  ClearErrors
  ExecWait '"$SYSDIR\regsvr32.exe" /s /u "$INSTDIR\x86\weasel_tip.dll"' $0
  IfErrors uninstall_failed
  StrCmp $0 0 0 uninstall_failed
  ; Remove only our own startup command, leaving any user replacement intact.
  ReadRegStr $0 HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "小狼毫RS算法服务"
  ${If} $0 == '$\"$INSTDIR\weasel-broker.exe$\"'
    DeleteRegValue HKCU "Software\Microsoft\Windows\CurrentVersion\Run" "小狼毫RS算法服务"
  ${EndIf}
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

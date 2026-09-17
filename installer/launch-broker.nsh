!ifndef WEASEL_LAUNCH_BROKER_NSH
!define WEASEL_LAUNCH_BROKER_NSH
Function LaunchBrokerUnelevated
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${PROJECT_ROOT}\scripts\launch-broker.ps1"
  SetOutPath "$INSTDIR"
  ; A separate STA process avoids COM calls on NSIS's input-synchronous UI thread.
  ; ExecToStack captures the script's diagnostics; no elevated launch fallback.
  SetOutPath "$SYSDIR"
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -STA -ExecutionPolicy Bypass -File "$PLUGINSDIR\launch-broker.ps1" -InstallDirectory "$INSTDIR" -Mode Launch'
  Pop $0
  Pop $1
  SetOutPath "$INSTDIR"
  ${If} $0 != 0
    DetailPrint "Explorer 启动算法服务失败（$0）：$1"
    MessageBox MB_OK|MB_ICONEXCLAMATION "安装已完成，但无法通过 Explorer 启动算法服务（$0）。$\r$\n$\r$\n$1$\r$\n$\r$\n请从开始菜单打开“小狼毫RS算法服务”。" /SD IDOK
  ${EndIf}
FunctionEnd

Function ProvisionCurrentUser
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${PROJECT_ROOT}\scripts\launch-broker.ps1"
  SetOutPath "$SYSDIR"
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -STA -ExecutionPolicy Bypass -File "$PLUGINSDIR\launch-broker.ps1" -InstallDirectory "$INSTDIR" -Mode PostInstall'
  Pop $0
  Pop $1
  SetOutPath "$INSTDIR"
  ${If} $0 != 0
    !insertmacro InstallLog "WARN: 当前桌面用户初始化失败（$0）：$1"
    MessageBox MB_OK|MB_ICONEXCLAMATION "程序已经安装，但当前桌面用户初始化失败（$0）。$\r$\n$\r$\n$1$\r$\n$\r$\n请从开始菜单启动算法服务并重新部署 Rime。" /SD IDOK
  ${Else}
    !insertmacro InstallLog "INFO: 当前桌面用户初始化完成"
  ${EndIf}
FunctionEnd

Function un.RemoveCurrentUserProvisioning
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${PROJECT_ROOT}\scripts\launch-broker.ps1"
  SetOutPath "$SYSDIR"
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -STA -ExecutionPolicy Bypass -File "$PLUGINSDIR\launch-broker.ps1" -InstallDirectory "$INSTDIR" -Mode PostUninstall'
  Pop $0
  Pop $1
  SetOutPath "$TEMP"
  ${If} $0 != 0
    DetailPrint "无法清理当前桌面用户的启动项（$0）：$1"
  ${EndIf}
FunctionEnd
!endif

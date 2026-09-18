!ifndef WEASEL_UIACCESS_NSH
!define WEASEL_UIACCESS_NSH
Var UseUiAccess

; Run outside NSIS's plugin directory: System.dll is not a .NET assembly.
!macro CleanupUiAccessCertificate RECEIPT
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  File "${PROJECT_ROOT}\scripts\sign-renderer.ps1"
  SetOutPath "$SYSDIR"
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\sign-renderer.ps1" -Mode Cleanup -CertificatePath "${RECEIPT}"'
  Pop $0
  Pop $1
  SetOutPath "$INSTDIR"
  ${If} $0 != 0
    DetailPrint "UIAccess 证书清理失败（$0）：$1；证书记录：${RECEIPT}"
    MessageBox MB_OK|MB_ICONEXCLAMATION "无法移除本机 UIAccess 测试证书：$1$\r$\n证书记录：${RECEIPT}" /SD IDOK
  ${EndIf}
!macroend

Function PrepareUiAccessRenderer
  StrCpy $UseUiAccess 0
  MessageBox MB_YESNO|MB_ICONEXCLAMATION|MB_DEFBUTTON2 "是否为 renderer 创建并信任本机自签名证书？$\r$\n$\r$\n选择“是”可改善开始菜单等系统界面的候选框显示。安装器将向本机的受信任根证书和受信任发布者证书库添加专用测试证书，并启用 renderer 的 UIAccess权限（可跨权限级别访问界面）。$\r$\n$\r$\n选择“否”安装普通版本，不更改证书信任，但部分系统界面可能无法显示候选框。" /SD IDNO IDYES sign_renderer IDNO uiaccess_done
  sign_renderer:
  SetOutPath "$PLUGINSDIR"
  ClearErrors
  File "${PROJECT_ROOT}\scripts\sign-renderer.ps1"
  File /oname=renderer-uiaccess.exe "${X64_RELEASE}\uiaccess\weasel-renderer.exe"
  ${If} ${Errors}
    Abort
  ${EndIf}
  SetOutPath "$SYSDIR"
  nsExec::ExecToStack '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\sign-renderer.ps1" -Mode Sign -RendererPath "$PLUGINSDIR\renderer-uiaccess.exe" -CertificatePath "$PLUGINSDIR\new-uiaccess.cer"'
  Pop $0
  Pop $1
  !insertmacro InstallLog "INFO: UIAccess 签名（$0）：$1"
  SetOutPath "$INSTDIR"
  ${If} $0 == 2
    MessageBox MB_OK|MB_ICONSTOP "无法确认签名私钥已删除。请按日志中的证书指纹检查本机个人证书及私钥。$\r$\n$1" /SD IDOK
  ${EndIf}
  ${If} $0 != 0
    !insertmacro CleanupUiAccessCertificate "$PLUGINSDIR\new-uiaccess.cer"
    ${If} $0 != 0
      Abort
    ${EndIf}
    MessageBox MB_OK|MB_ICONEXCLAMATION "UIAccess 签名失败，将安装普通 renderer。$\r$\n请查看安装日志。" /SD IDOK
    Goto uiaccess_done
  ${EndIf}
  ClearErrors
  CopyFiles /SILENT "$PLUGINSDIR\renderer-uiaccess.exe" "$StageRoot\weasel-renderer.exe"
  ${If} ${Errors}
    Abort
  ${EndIf}
  StrCpy $UseUiAccess 1
  uiaccess_done:
  ClearErrors
FunctionEnd
!endif

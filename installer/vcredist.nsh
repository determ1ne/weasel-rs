!include WordFunc.nsh

!ifndef VC_X86_VERSION
  !error "Missing VC_X86_VERSION; use scripts\build-installer.ps1."
!endif
!ifndef VC_X64_VERSION
  !error "Missing VC_X64_VERSION; use scripts\build-installer.ps1."
!endif

; Inspect both registry views; do not assume that Installed alone means new enough.
!macro CheckRuntimeView ARCH VERSION VIEW
  SetRegView ${VIEW}
  ReadRegDWORD $0 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\${ARCH}" "Installed"
  ${If} $0 == 1
    ClearErrors
    ReadRegDWORD $1 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\${ARCH}" "Major"
    ReadRegDWORD $2 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\${ARCH}" "Minor"
    ReadRegDWORD $3 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\${ARCH}" "Bld"
    ReadRegDWORD $4 HKLM "SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\${ARCH}" "Rbld"
    ${IfNot} ${Errors}
      ${VersionCompare} "$1.$2.$3.$4" "${VERSION}" $5
      ${If} $5 != 2
        StrCpy $9 1
      ${EndIf}
    ${EndIf}
  ${EndIf}
!macroend

!macro CheckRuntime ARCH VERSION
  StrCpy $9 0
  !insertmacro CheckRuntimeView ${ARCH} ${VERSION} 32
  !insertmacro CheckRuntimeView ${ARCH} ${VERSION} 64
  ClearErrors
!macroend

!macro RuntimeFunction ARCH VERSION
Function InstallRuntime_${ARCH}
  !insertmacro CheckRuntime ${ARCH} ${VERSION}
  ${If} $9 == 1
    DetailPrint "Microsoft Visual C++ v14 Redistributable ${ARCH}: 已安装相同或更新版本，跳过。"
    Return
  ${EndIf}
  InitPluginsDir
  SetOutPath "$PLUGINSDIR"
  ClearErrors
  File "${PROJECT_ROOT}\artifacts\vcredist\vc_redist.${ARCH}.exe"
  ${If} ${Errors}
    MessageBox MB_OK|MB_ICONSTOP "无法解压 Microsoft Visual C++ v14 Redistributable ${ARCH} 安装程序。" /SD IDOK
    SetErrorLevel 1
    Abort
  ${EndIf}
  DetailPrint "正在安装 Microsoft Visual C++ v14 Redistributable ${ARCH} ${VERSION}…"
  StrCpy $6 -1
  ClearErrors
  ExecWait '"$PLUGINSDIR\vc_redist.${ARCH}.exe" /install /quiet /norestart /log "$TEMP\Weasel-RS-vcredist-${ARCH}.log"' $6
  ${If} ${Errors}
    StrCpy $6 -1
  ${EndIf}
  Delete "$PLUGINSDIR\vc_redist.${ARCH}.exe"
  ${If} $6 == 0
    Return
  ${ElseIf} $6 == 3010
    SetRebootFlag true
    DetailPrint "Microsoft Visual C++ v14 Redistributable ${ARCH} 安装成功，需要重启。"
    Return
  ${ElseIf} $6 == 1638
  ${OrIf} $6 == -2147023258 ; HRESULT_FROM_WIN32(ERROR_PRODUCT_VERSION)
    ; Another installer may have installed a newer runtime after our first check.
    !insertmacro CheckRuntime ${ARCH} ${VERSION}
    ${If} $9 == 1
      Return
    ${EndIf}
  ${EndIf}
  MessageBox MB_OK|MB_ICONSTOP "Microsoft Visual C++ v14 Redistributable ${ARCH} 安装失败，退出码：$6。$\r$\n日志：$TEMP\Weasel-RS-vcredist-${ARCH}.log" /SD IDOK
  SetErrorLevel 1
  Abort
FunctionEnd
!macroend

!insertmacro RuntimeFunction x86 ${VC_X86_VERSION}
!insertmacro RuntimeFunction x64 ${VC_X64_VERSION}

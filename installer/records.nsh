; Persist installer diagnostics independently of $PLUGINSDIR (which is temporary).
Var InstallLog
Var InstallLogHandle
Var ManifestHandle
Var ManifestReader
Var ManifestSource
Var ManifestEntry
Var ManifestRoot
Var ManifestRootLength
Var ManifestFullPath
Var ManifestPrefix
Var ManifestExtension
Var LogMessage

!macro InstallLog MESSAGE
  ; Logging must neither manufacture nor clear a caller's installation error.
  Push $R9
  ${If} ${Errors}
    StrCpy $R9 1
  ${Else}
    StrCpy $R9 0
  ${EndIf}
  DetailPrint "${MESSAGE}"
  ${If} $InstallLogHandle != ""
    StrCpy $LogMessage "${MESSAGE}"
    Push $0
    Push $1
    Push $2
    Push $3
    Push $4
    Push $5
    Push $6
    ${GetTime} "" "LS" $0 $1 $2 $3 $4 $5 $6
    FileWriteUTF16LE $InstallLogHandle "$2-$1-$0T$4:$5:$6Z $LogMessage$\r$\n"
    Pop $6
    Pop $5
    Pop $4
    Pop $3
    Pop $2
    Pop $1
    Pop $0
  ${EndIf}
  ${If} $R9 == 1
    SetErrors
  ${Else}
    ClearErrors
  ${EndIf}
  Pop $R9
!macroend

Function BeginInstallLog
  CreateDirectory "$LOCALAPPDATA\Weasel-RS\Installer"
  GetTempFileName $InstallLog "$LOCALAPPDATA\Weasel-RS\Installer"
  Rename "$InstallLog" "$InstallLog.log"
  StrCpy $InstallLog "$InstallLog.log"
  ClearErrors
  FileOpen $InstallLogHandle "$InstallLog" w
  ${If} ${Errors}
    StrCpy $InstallLogHandle ""
    MessageBox MB_OK|MB_ICONSTOP "无法创建安装日志，安装已停止。" /SD IDOK
    Abort
  ${EndIf}
  !insertmacro InstallLog "Weasel-RS ${PRODUCT_VERSION}; previous=$InstalledVersion; directory=$INSTDIR"
  !insertmacro InstallLog "安装日志：$InstallLog"
FunctionEnd

Function BeginRecords
  CreateDirectory "$INSTDIR"
  ClearErrors
  FileOpen $ManifestHandle "$INSTDIR\files.pending.lst" w
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法创建安装文件清单"
    Abort
  ${EndIf}
FunctionEnd

; Remove payload recorded by the previous successful or interrupted install.
; The manifest contains relative paths emitted by ManagedFile. Missing files
; are harmless. A malformed or redirected entry stops installation rather
; than allowing an elevated delete outside the installation directory.
Function RemoveManifestPayload
  IfFileExists "$ManifestSource" 0 remove_manifest_done
  ClearErrors
  FileOpen $ManifestReader "$ManifestSource" r
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法读取旧安装清单：$ManifestSource"
    Abort
  ${EndIf}

  System::Call 'kernel32::GetFullPathNameW(w "$INSTDIR", i ${NSIS_MAX_STRLEN}, w .rManifestRoot, p0) i.r0'
  ${If} $0 == 0
    FileClose $ManifestReader
    !insertmacro InstallLog "ERROR: 无法规范化安装目录"
    Abort
  ${EndIf}
  ${If} $0 >= ${NSIS_MAX_STRLEN}
    FileClose $ManifestReader
    !insertmacro InstallLog "ERROR: 安装目录路径过长"
    Abort
  ${EndIf}
  StrCpy $ManifestRoot "$ManifestRoot\"
  StrLen $ManifestRootLength "$ManifestRoot"

  remove_manifest_next:
    ClearErrors
    FileRead $ManifestReader $ManifestEntry
    IfErrors remove_manifest_close
    ${TrimNewLines} "$ManifestEntry" $ManifestEntry
    StrCmp $ManifestEntry "" remove_manifest_next
    StrCpy $ManifestFullPath ""
    System::Call 'kernel32::GetFullPathNameW(w "$INSTDIR\$ManifestEntry", i ${NSIS_MAX_STRLEN}, w .rManifestFullPath, p0) i.r0'
    ${If} $0 == 0
      Goto remove_manifest_invalid
    ${EndIf}
    ${If} $0 >= ${NSIS_MAX_STRLEN}
      Goto remove_manifest_invalid
    ${EndIf}
    StrCpy $ManifestPrefix "$ManifestFullPath" $ManifestRootLength
    StrCmp $ManifestPrefix "$ManifestRoot" 0 remove_manifest_invalid

    System::Call 'kernel32::GetFileAttributesW(w "$ManifestFullPath") i.r0'
    StrCmp $0 -1 remove_manifest_next
    ; Never follow a directory or a reparse-point entry.
    IntOp $0 $0 & 0x410
    StrCmp $0 0 0 remove_manifest_invalid

    ${GetFileExt} "$ManifestFullPath" $ManifestExtension
    StrCmp $ManifestExtension "dll" 0 remove_manifest_regular
      StrCpy $OldDll "$ManifestFullPath"
      Call RetireDll
      Goto remove_manifest_next

    remove_manifest_regular:
      ClearErrors
      Delete "$ManifestFullPath"
      ${If} ${Errors}
        FileClose $ManifestReader
        !insertmacro InstallLog "ERROR: 无法删除旧安装文件：$ManifestEntry"
        Abort
      ${EndIf}
      !insertmacro InstallLog "INFO: 已删除旧安装文件：$ManifestEntry"
      Goto remove_manifest_next

    remove_manifest_invalid:
      FileClose $ManifestReader
      !insertmacro InstallLog "ERROR: 旧安装清单包含不安全路径：$ManifestEntry"
      Abort

  remove_manifest_close:
    FileClose $ManifestReader
  remove_manifest_done:
    ClearErrors
FunctionEnd

Function RemoveOldPayload
  StrCpy $ManifestSource "$INSTDIR\files.lst"
  Call RemoveManifestPayload
  ; Also remove payload left by an interrupted installation before truncating
  ; its pending list for this run.
  StrCpy $ManifestSource "$INSTDIR\files.pending.lst"
  Call RemoveManifestPayload
FunctionEnd

; Only successfully extracted payload files are recorded. Never enumerate the
; destination directory: it may contain files added by the user.
!macro ManagedFile SOURCE NAME
  File "/oname=${NAME}" "${SOURCE}"
  ${IfNot} ${Errors}
    ; Store paths relative to $INSTDIR so a later installer can validate and
    ; remove only its own payload without embedding a machine-specific path.
    Push $R7
    Push $R8
    StrLen $R7 "$INSTDIR"
    StrCpy $R8 "$OUTDIR" $R7
    ${If} $R8 != "$INSTDIR"
      SetErrors
      !insertmacro InstallLog "ERROR: 文件不在安装目录内：$OUTDIR\${NAME}"
    ${Else}
      StrCpy $R8 "$OUTDIR" "" $R7
      ${If} $R8 == ""
        FileWriteUTF16LE $ManifestHandle "${NAME}$\r$\n"
      ${Else}
        StrCpy $R8 "$R8" "" 1
        FileWriteUTF16LE $ManifestHandle "$R8\${NAME}$\r$\n"
      ${EndIf}
      !insertmacro InstallLog "文件：$OUTDIR\${NAME}"
    ${EndIf}
    Pop $R8
    Pop $R7
  ${EndIf}
!macroend

Function FinishRecords
  FileClose $ManifestHandle
  StrCpy $ManifestHandle ""
  ; No rollback manifest is kept. The old files.lst remains authoritative
  ; until the new payload has been installed successfully.
  ClearErrors
  Delete "$INSTDIR\files.lst"
  ${If} ${Errors}
    !insertmacro InstallLog "WARN: 无法替换旧安装清单；保留 files.pending.lst"
    Goto records_done
  ${EndIf}
  ClearErrors
  Rename "$INSTDIR\files.pending.lst" "$INSTDIR\files.lst"
  ${If} ${Errors}
    !insertmacro InstallLog "WARN: 无法归档 files.lst；保留 files.pending.lst"
  ${Else}
    !insertmacro InstallLog "INFO: files.lst 已归档"
  ${EndIf}
  records_done:
  !insertmacro InstallLog "安装完成；日志：$InstallLog"
  FileClose $InstallLogHandle
  StrCpy $InstallLogHandle ""
FunctionEnd

; Persist machine-install diagnostics independently of both $PLUGINSDIR and
; the administrator account used for UAC.
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
Var PathCheckCandidate
Var PathCheckRoot
Var PathCheckRootWithSlash
Var PathCheckRootLength
Var PathCheckFull
Var PathCheckPrefix
Var PathCheckCursor
Var PathCheckParent
Var PathCheckResult
Var StageRoot
Var RollbackRoot
Var RollbackManifest
Var RollbackHandle
Var ManagedFinalOutDir
Var ManagedRelativeDir
Var PayloadTargetDirectory

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
  ; CheckPlatform selects the all-users shell context, so APPDATA resolves to
  ; the machine-wide ProgramData directory here.
  CreateDirectory "$APPDATA\Weasel-RS\Installer"
  GetTempFileName $InstallLog "$APPDATA\Weasel-RS\Installer"
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
  InitPluginsDir
  StrCpy $StageRoot "$PLUGINSDIR\payload-stage"
  StrCpy $RollbackRoot "$PLUGINSDIR\payload-rollback"
  CreateDirectory "$StageRoot"
  ClearErrors
  FileOpen $ManifestHandle "$StageRoot\files.pending.lst" w
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法创建安装文件清单"
    Abort
  ${EndIf}
FunctionEnd

Function CloseRecords
  ${If} $ManifestHandle != ""
    FileClose $ManifestHandle
    StrCpy $ManifestHandle ""
  ${EndIf}
FunctionEnd

; Validate an existing file or directory lexically and reject every reparse
; point between it and the installation root. Callers set PathCheckCandidate
; and inspect PathCheckResult. This never follows a junction during deletion.
Function CheckManagedPath
  StrCpy $PathCheckResult 0
  StrLen $0 "$INSTDIR"
  ${If} $0 >= ${NSIS_MAX_STRLEN}
    Return
  ${EndIf}
  ClearErrors
  GetFullPathName $PathCheckRoot "$INSTDIR"
  ${If} ${Errors}
    Return
  ${EndIf}
  StrLen $0 "$PathCheckCandidate"
  ${If} $0 >= ${NSIS_MAX_STRLEN}
    Return
  ${EndIf}
  ClearErrors
  GetFullPathName $PathCheckFull "$PathCheckCandidate"
  ${If} ${Errors}
    Return
  ${EndIf}

  StrCmp $PathCheckFull $PathCheckRoot path_check_walk
  StrCpy $PathCheckRootWithSlash "$PathCheckRoot\"
  StrLen $PathCheckRootLength "$PathCheckRootWithSlash"
  StrCpy $PathCheckPrefix "$PathCheckFull" $PathCheckRootLength
  StrCmp $PathCheckPrefix $PathCheckRootWithSlash path_check_walk path_check_done

  path_check_walk:
    StrCpy $PathCheckCursor "$PathCheckFull"
  path_check_next:
    ClearErrors
    ${GetFileAttributes} "$PathCheckCursor" "REPARSE_POINT" $0
    ${If} ${Errors}
    ${OrIf} $0 == 1
      Goto path_check_done
    ${EndIf}
    StrCmp $PathCheckCursor $PathCheckRoot path_check_valid
    ${GetParent} "$PathCheckCursor" $PathCheckParent
    StrCmp $PathCheckParent "" path_check_done
    StrCmp $PathCheckParent $PathCheckCursor path_check_done
    StrCpy $PathCheckCursor "$PathCheckParent"
    Goto path_check_next

  path_check_valid:
    StrCpy $PathCheckResult 1
  path_check_done:
    ClearErrors
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

  StrLen $0 "$INSTDIR"
  ${If} $0 >= ${NSIS_MAX_STRLEN}
    FileClose $ManifestReader
    !insertmacro InstallLog "ERROR: 安装目录路径过长"
    Abort
  ${EndIf}
  ClearErrors
  GetFullPathName $ManifestRoot "$INSTDIR"
  ${If} ${Errors}
    FileClose $ManifestReader
    !insertmacro InstallLog "ERROR: 无法规范化安装目录"
    Abort
  ${EndIf}
  StrCpy $ManifestRoot "$ManifestRoot\"
  StrLen $ManifestRootLength "$ManifestRoot"

  remove_manifest_next:
    ClearErrors
    FileReadUTF16LE $ManifestReader $ManifestEntry
    IfErrors remove_manifest_close
    ${TrimNewLines} "$ManifestEntry" $ManifestEntry
    StrCmp $ManifestEntry "" remove_manifest_next
    ; Missing files are harmless. GetFullPathName requires the path portion to
    ; exist, so skip them before canonicalizing the manifest entry.
    IfFileExists "$INSTDIR\$ManifestEntry" 0 remove_manifest_next
    StrLen $0 "$INSTDIR\$ManifestEntry"
    ${If} $0 >= ${NSIS_MAX_STRLEN}
      Goto remove_manifest_invalid
    ${EndIf}
    StrCpy $ManifestFullPath ""
    ClearErrors
    GetFullPathName $ManifestFullPath "$INSTDIR\$ManifestEntry"
    ${If} ${Errors}
      Goto remove_manifest_invalid
    ${EndIf}
    StrCpy $ManifestPrefix "$ManifestFullPath" $ManifestRootLength
    StrCmp $ManifestPrefix "$ManifestRoot" 0 remove_manifest_invalid

    StrCpy $PathCheckCandidate "$ManifestFullPath"
    Call CheckManagedPath
    StrCmp $PathCheckResult 1 0 remove_manifest_invalid
    ClearErrors
    ${GetFileAttributes} "$ManifestFullPath" "DIRECTORY" $0
    ${If} ${Errors}
      Goto remove_manifest_next
    ${EndIf}
    StrCmp $0 0 0 remove_manifest_invalid

    ${GetFileExt} "$ManifestFullPath" $ManifestExtension
    StrCmp $ManifestExtension "dll" 0 remove_manifest_regular
      StrCpy $OldDll "$ManifestFullPath"
      Call RetireDll
      ${If} $RetireResult != 1
        FileClose $ManifestReader
        !insertmacro InstallLog "ERROR: 无法退役旧 DLL：$ManifestEntry"
        MessageBox MB_OK|MB_ICONSTOP "无法重命名旧 DLL：$ManifestFullPath。请关闭相关应用后重试。" /SD IDOK
        SetErrorLevel 1
        Abort
      ${EndIf}
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

; Copy the previous successful payload before any destructive upgrade step.
; The backup manifest is written only after each copy succeeds.
Function BackupOldPayload
  CreateDirectory "$RollbackRoot"
  IfFileExists "$INSTDIR\files.lst" backup_payload_copy_manifest 0
    !insertmacro InstallLog "ERROR: 已安装版本缺少 files.lst，无法安全升级"
    MessageBox MB_OK|MB_ICONSTOP "现有安装缺少文件清单，无法安全升级。请先卸载旧版本；如果卸载程序也不可用，请备份用户数据后手动清理安装目录。" /SD IDOK
    Abort

  backup_payload_copy_manifest:
  ClearErrors
  CopyFiles /SILENT "$INSTDIR\files.lst" "$RollbackRoot"
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法备份旧安装清单"
    Abort
  ${EndIf}
  ClearErrors
  Rename "$RollbackRoot\files.lst" "$RollbackRoot\old-files.lst"
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法保存旧安装清单副本"
    Abort
  ${EndIf}

  StrCpy $RollbackManifest "$RollbackRoot\rollback.lst"
  ClearErrors
  FileOpen $RollbackHandle "$RollbackManifest" w
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法创建升级回滚清单"
    Abort
  ${EndIf}
  ClearErrors
  FileOpen $ManifestReader "$INSTDIR\files.lst" r
  ${If} ${Errors}
    FileClose $RollbackHandle
    StrCpy $RollbackHandle ""
    !insertmacro InstallLog "ERROR: 无法读取待备份的旧安装清单"
    Abort
  ${EndIf}

  backup_payload_next:
    ClearErrors
    FileReadUTF16LE $ManifestReader $ManifestEntry
    IfErrors backup_payload_reader_close
    ${TrimNewLines} "$ManifestEntry" $ManifestEntry
    StrCmp $ManifestEntry "" backup_payload_next
    IfFileExists "$INSTDIR\$ManifestEntry" 0 backup_payload_next
    StrCpy $PathCheckCandidate "$INSTDIR\$ManifestEntry"
    Call CheckManagedPath
    StrCmp $PathCheckResult 1 0 backup_payload_invalid
    ClearErrors
    ${GetFileAttributes} "$PathCheckCandidate" "DIRECTORY" $0
    ${If} ${Errors}
    ${OrIf} $0 == 1
      Goto backup_payload_invalid
    ${EndIf}
    ${GetParent} "$RollbackRoot\$ManifestEntry" $0
    CreateDirectory "$0"
    ClearErrors
    CopyFiles /SILENT "$INSTDIR\$ManifestEntry" "$0"
    ${If} ${Errors}
      FileClose $ManifestReader
      FileClose $RollbackHandle
      StrCpy $RollbackHandle ""
      !insertmacro InstallLog "ERROR: 无法备份旧安装文件：$ManifestEntry"
      Abort
    ${EndIf}
    FileWriteUTF16LE $RollbackHandle "$ManifestEntry$\r$\n"
    ${If} ${Errors}
      FileClose $ManifestReader
      FileClose $RollbackHandle
      StrCpy $RollbackHandle ""
      !insertmacro InstallLog "ERROR: 无法记录升级回滚文件：$ManifestEntry"
      Abort
    ${EndIf}
    Goto backup_payload_next

  backup_payload_invalid:
    FileClose $ManifestReader
    FileClose $RollbackHandle
    StrCpy $RollbackHandle ""
    !insertmacro InstallLog "ERROR: 旧安装清单包含不安全路径：$ManifestEntry"
    Abort
  backup_payload_reader_close:
    FileClose $ManifestReader
    FileClose $RollbackHandle
    StrCpy $RollbackHandle ""
    ClearErrors
FunctionEnd

; Copy every staged file to its final location. If this stops part-way,
; onInstFailed removes entries from the same pending list before restoration.
Function CommitStagedPayload
  ClearErrors
  FileOpen $ManifestReader "$StageRoot\files.pending.lst" r
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法读取新安装清单"
    Abort
  ${EndIf}
  commit_payload_next:
    ClearErrors
    FileReadUTF16LE $ManifestReader $ManifestEntry
    IfErrors commit_payload_close
    ${TrimNewLines} "$ManifestEntry" $ManifestEntry
    StrCmp $ManifestEntry "" commit_payload_next
    IfFileExists "$StageRoot\$ManifestEntry" 0 commit_payload_missing
    ; CheckManagedPath uses the general-purpose registers internally. Keep the
    ; copy destination in a dedicated variable so validation cannot turn it
    ; into an attribute value such as "0".
    ${GetParent} "$INSTDIR\$ManifestEntry" $PayloadTargetDirectory
    CreateDirectory "$PayloadTargetDirectory"
    StrCpy $PathCheckCandidate "$PayloadTargetDirectory"
    Call CheckManagedPath
    StrCmp $PathCheckResult 1 0 commit_payload_invalid
    ClearErrors
    CopyFiles /SILENT "$StageRoot\$ManifestEntry" "$PayloadTargetDirectory"
    ${If} ${Errors}
      FileClose $ManifestReader
      !insertmacro InstallLog "ERROR: 无法提交新安装文件：$ManifestEntry"
      Abort
    ${EndIf}
    !insertmacro InstallLog "文件：$INSTDIR\$ManifestEntry"
    Goto commit_payload_next
  commit_payload_missing:
    FileClose $ManifestReader
    !insertmacro InstallLog "ERROR: staging 中缺少文件：$ManifestEntry"
    Abort
  commit_payload_invalid:
    FileClose $ManifestReader
    !insertmacro InstallLog "ERROR: 不安全的安装目标：$ManifestEntry"
    Abort
  commit_payload_close:
    FileClose $ManifestReader
    ClearErrors
FunctionEnd

; Restore files copied by BackupOldPayload after a failed upgrade commit.
Function RestoreOldPayload
  IfFileExists "$RollbackManifest" 0 restore_old_manifest
  ClearErrors
  FileOpen $ManifestReader "$RollbackManifest" r
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法读取升级回滚清单"
    Goto restore_old_manifest
  ${EndIf}
  restore_old_next:
    ClearErrors
    FileReadUTF16LE $ManifestReader $ManifestEntry
    IfErrors restore_old_close
    ${TrimNewLines} "$ManifestEntry" $ManifestEntry
    StrCmp $ManifestEntry "" restore_old_next
    IfFileExists "$RollbackRoot\$ManifestEntry" 0 restore_old_next
    ${GetParent} "$INSTDIR\$ManifestEntry" $PayloadTargetDirectory
    CreateDirectory "$PayloadTargetDirectory"
    StrCpy $PathCheckCandidate "$PayloadTargetDirectory"
    Call CheckManagedPath
    ${If} $PathCheckResult != 1
      !insertmacro InstallLog "ERROR: 回滚目标不安全：$ManifestEntry"
      Goto restore_old_next
    ${EndIf}
    ClearErrors
    CopyFiles /SILENT "$RollbackRoot\$ManifestEntry" "$PayloadTargetDirectory"
    ${If} ${Errors}
      !insertmacro InstallLog "ERROR: 无法恢复旧安装文件：$ManifestEntry"
    ${Else}
      !insertmacro InstallLog "INFO: 已恢复旧安装文件：$ManifestEntry"
    ${EndIf}
    Goto restore_old_next
  restore_old_close:
    FileClose $ManifestReader
  restore_old_manifest:
    IfFileExists "$RollbackRoot\old-files.lst" 0 restore_old_done
    Delete "$INSTDIR\files.lst"
    ClearErrors
    CopyFiles /SILENT "$RollbackRoot\old-files.lst" "$INSTDIR"
    ${IfNot} ${Errors}
      ClearErrors
      Rename "$INSTDIR\old-files.lst" "$INSTDIR\files.lst"
      ${If} ${Errors}
        !insertmacro InstallLog "ERROR: 无法恢复旧安装清单"
      ${EndIf}
    ${EndIf}
  restore_old_done:
    ClearErrors
FunctionEnd

; Move files that may already have been copied from staging out of the way.
; Rollback must keep going when one file cannot be retired, otherwise a single
; lock would prevent every unrelated old file from being restored.
Function RemoveCommittedPayload
  ClearErrors
  FileOpen $ManifestReader "$StageRoot\files.pending.lst" r
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法读取待回滚的新安装清单"
    Return
  ${EndIf}
  rollback_remove_next:
    ClearErrors
    FileReadUTF16LE $ManifestReader $ManifestEntry
    IfErrors rollback_remove_close
    ${TrimNewLines} "$ManifestEntry" $ManifestEntry
    StrCmp $ManifestEntry "" rollback_remove_next
    IfFileExists "$INSTDIR\$ManifestEntry" 0 rollback_remove_next
    StrCpy $PathCheckCandidate "$INSTDIR\$ManifestEntry"
    Call CheckManagedPath
    ${If} $PathCheckResult != 1
      !insertmacro InstallLog "ERROR: 跳过不安全的回滚清理路径：$ManifestEntry"
      Goto rollback_remove_next
    ${EndIf}
    ClearErrors
    ${GetFileAttributes} "$PathCheckFull" "DIRECTORY" $0
    ${If} ${Errors}
    ${OrIf} $0 == 1
      !insertmacro InstallLog "ERROR: 跳过无效的回滚清理项：$ManifestEntry"
      Goto rollback_remove_next
    ${EndIf}
    StrCpy $OldDll "$PathCheckFull"
    Call RetireDll
    ${If} $RetireResult != 1
      !insertmacro InstallLog "ERROR: 无法移开新安装文件，旧版对应文件可能无法恢复：$ManifestEntry"
    ${EndIf}
    Goto rollback_remove_next
  rollback_remove_close:
    FileClose $ManifestReader
    ClearErrors
FunctionEnd

; Only successfully extracted payload files are recorded. Never enumerate the
; destination directory: it may contain files added by the user.
!macro ManagedFile SOURCE NAME
  ; SetOutPath creates the destination directory but no payload bytes are
  ; written until the complete path has passed the root and reparse checks.
  StrCpy $PathCheckCandidate "$OUTDIR"
  Call CheckManagedPath
  ${If} $PathCheckResult != 1
    SetErrors
    !insertmacro InstallLog "ERROR: 不安全的安装目标：$OUTDIR\${NAME}"
    SetErrorLevel 1
    Abort
  ${EndIf}
  Push $R9
  ; Continue from the canonical path checked above. This also prevents a
  ; syntactically unusual but equivalent OUTDIR from escaping the staging tree
  ; while its relative path is calculated.
  StrCpy $ManagedFinalOutDir "$PathCheckFull"
  Push $R7
  StrLen $R7 "$INSTDIR"
  StrCpy $ManagedRelativeDir "$ManagedFinalOutDir" "" $R7
  Pop $R7
  ${If} $ManagedRelativeDir == ""
    SetOutPath "$StageRoot"
  ${Else}
    StrCpy $ManagedRelativeDir "$ManagedRelativeDir" "" 1
    SetOutPath "$StageRoot\$ManagedRelativeDir"
  ${EndIf}
  ClearErrors
  File "/oname=${NAME}" "${SOURCE}"
  ${If} ${Errors}
    StrCpy $R9 1
  ${Else}
    StrCpy $R9 0
  ${EndIf}
  SetOutPath "$ManagedFinalOutDir"
  ${If} $R9 == 1
    SetErrors
    Pop $R9
    !insertmacro InstallLog "ERROR: 无法释放安装文件：${NAME}"
    SetErrorLevel 1
    Abort
  ${Else}
    ClearErrors
  ${EndIf}
  ; Store paths relative to $INSTDIR so a later installer can validate and
  ; remove only its own payload without embedding a machine-specific path.
  Push $R7
  Push $R8
  StrLen $R7 "$INSTDIR"
  StrCpy $R8 "$OUTDIR" "" $R7
  ${If} $R8 == ""
    FileWriteUTF16LE $ManifestHandle "${NAME}$\r$\n"
  ${Else}
    StrCpy $R8 "$R8" "" 1
    FileWriteUTF16LE $ManifestHandle "$R8\${NAME}$\r$\n"
  ${EndIf}
  ${If} ${Errors}
    Pop $R8
    Pop $R7
    Pop $R9
    !insertmacro InstallLog "ERROR: 无法记录安装文件：${NAME}"
    SetErrorLevel 1
    Abort
  ${EndIf}
  Pop $R8
  Pop $R7
  Pop $R9
!macroend

Function FinishRecords
  Call CloseRecords
  ; Keep the old list authoritative through staging, copy and registration.
  ClearErrors
  CopyFiles /SILENT "$StageRoot\files.pending.lst" "$INSTDIR"
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法复制新安装清单"
    Abort
  ${EndIf}
  Delete "$INSTDIR\files.lst"
  ClearErrors
  Rename "$INSTDIR\files.pending.lst" "$INSTDIR\files.lst"
  ${If} ${Errors}
    !insertmacro InstallLog "ERROR: 无法归档 files.lst"
    Abort
  ${EndIf}
  !insertmacro InstallLog "INFO: files.lst 已归档"
FunctionEnd

Function FinishInstallLog
  !insertmacro InstallLog "安装完成；日志：$InstallLog"
  FileClose $InstallLogHandle
  StrCpy $InstallLogHandle ""
FunctionEnd

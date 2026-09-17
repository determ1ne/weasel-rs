; Only the exact backup family created by RetireDll is eligible. Do not scan
; recursively or delete generic *.tmp files, manifests, or user data.
Var CleanupDirectory
Var CleanupPattern
Var CleanupFind
Var CleanupName

!macro CleanupDllBackups DIRECTORY DLL
  StrCpy $CleanupDirectory "${DIRECTORY}"
  StrCpy $CleanupPattern "${DLL}.old.ns*.tmp"
  Call CleanupDllBackupFamily
!macroend

Function CleanupDllBackupFamily
  ; Reject redirected directories and entries, including directory junctions.
  IfFileExists "$CleanupDirectory\*.*" 0 cleanup_family_done
  StrCpy $PathCheckCandidate "$CleanupDirectory"
  Call CheckManagedPath
  StrCmp $PathCheckResult 1 0 cleanup_family_done
  FindFirst $CleanupFind $CleanupName "$CleanupDirectory\$CleanupPattern"
  StrCmp $CleanupName "" cleanup_family_close
  cleanup_family_next:
    StrCpy $PathCheckCandidate "$CleanupDirectory\$CleanupName"
    Call CheckManagedPath
    ${If} $PathCheckResult == 1
      ClearErrors
      ${GetFileAttributes} "$CleanupDirectory\$CleanupName" "DIRECTORY" $0
    ${EndIf}
    ${If} $PathCheckResult == 1
    ${AndIf} $0 == 0
      ClearErrors
      Delete /REBOOTOK "$CleanupDirectory\$CleanupName"
      ${If} ${Errors}
        !insertmacro InstallLog "WARN: 无法清理旧 DLL 备份：$CleanupDirectory\$CleanupName"
      ${Else}
        !insertmacro InstallLog "INFO: 已删除或安排重启删除旧 DLL 备份：$CleanupDirectory\$CleanupName"
      ${EndIf}
    ${EndIf}
    FindNext $CleanupFind $CleanupName
    StrCmp $CleanupName "" 0 cleanup_family_next
  cleanup_family_close:
    FindClose $CleanupFind
  cleanup_family_done:
    ClearErrors
FunctionEnd

Function CleanupOldDllBackups
  !insertmacro CleanupDllBackups "$INSTDIR" "weasel_tip.dll"
  !insertmacro CleanupDllBackups "$INSTDIR" "rime.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\x64" "weasel_tip.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\x64" "rime.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\x86" "weasel_tip.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\themes" "weasel_theme_ten.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\themes" "weasel_theme_eleven.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\themes" "weasel_theme_abc.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\themes" "weasel_theme_void.dll"
  !insertmacro CleanupDllBackups "$INSTDIR\themes" "weasel_theme_wasm.dll"
FunctionEnd

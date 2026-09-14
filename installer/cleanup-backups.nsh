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
  System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR") i.r0'
  IntOp $0 $0 & 0x400
  StrCmp $0 0 0 cleanup_family_done
  System::Call 'kernel32::GetFileAttributesW(w "$CleanupDirectory") i.r0'
  StrCmp $0 -1 cleanup_family_done
  IntOp $0 $0 & 0x400
  StrCmp $0 0 0 cleanup_family_done
  FindFirst $CleanupFind $CleanupName "$CleanupDirectory\$CleanupPattern"
  StrCmp $CleanupName "" cleanup_family_close
  cleanup_family_next:
    System::Call 'kernel32::GetFileAttributesW(w "$CleanupDirectory\$CleanupName") i.r0'
    ; FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT
    IntOp $0 $0 & 0x410
    ${If} $0 == 0
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

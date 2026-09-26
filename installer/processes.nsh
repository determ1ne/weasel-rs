!ifndef WEASEL_PROCESSES_NSH
!define WEASEL_PROCESSES_NSH

Var ScanMode
Var ScanResult
Var ProcessSnapshot
Var ProcessEntry
Var ProcessHandle
Var ProcessName
Var ProcessPass
Var ProcessId
Var ProcessTerminate
Var ScanPassLimit

; ScanResult: 0 = clear, 10 = service running, 1 = error.
; This installer is x86-unicode, so PROCESSENTRY32W is 556 bytes even on x64.
; Never terminate by filename alone: query and terminate the same open handle.
!macro DefineScanApplicationProcesses PREFIX
Function ${PREFIX}ScanApplicationProcesses
  StrCpy $ScanResult 0
  StrCpy $ProcessPass 0
  scan_pass:
    StrCpy $ProcessName "weasel-broker.exe"
    ${If} $ProcessPass == 1
      StrCpy $ProcessName "weasel-server.exe"
    ${ElseIf} $ProcessPass == 2
      StrCpy $ProcessName "weasel-renderer.exe"
    ${ElseIf} $ProcessPass == 3
      StrCpy $ProcessName "weasel-settings.exe"
    ${EndIf}
    ; The settings executable cannot participate in the broker's graceful
    ; shutdown. Always stop the path-verified installed instance immediately;
    ; service processes still require ScanMode=1 before they are terminated.
    StrCpy $ProcessTerminate 0
    ${If} $ScanMode == 1
    ${OrIf} $ProcessPass == 3
      StrCpy $ProcessTerminate 1
    ${EndIf}
    StrCpy $ProcessHandle 0
    System::Call 'kernel32::CreateToolhelp32Snapshot(i 2, i 0) p.s'
    Pop $ProcessSnapshot
    StrCmp $ProcessSnapshot -1 scan_error
    System::Alloc 556
    Pop $ProcessEntry
    StrCmp $ProcessEntry 0 scan_snapshot_error
    System::Call '*$ProcessEntry(i 556)'
    System::Call 'kernel32::Process32FirstW(p $ProcessSnapshot, p $ProcessEntry) i.r0 ?e'
    Pop $5
    Goto scan_check
  scan_next:
    System::Call 'kernel32::Process32NextW(p $ProcessSnapshot, p $ProcessEntry) i.r0 ?e'
    Pop $5
  scan_check:
    ${If} $0 == 0
      StrCmp $5 18 scan_end scan_entry_error
    ${EndIf}
    System::Call '*$ProcessEntry(i, i, i.r1, i, i, i, i, i, i, &w260.r2)'
    StrCpy $ProcessId $1
    StrCmp $2 $ProcessName 0 scan_next
    ; QUERY_LIMITED_INFORMATION | SYNCHRONIZE (+ TERMINATE when requested).
    StrCpy $0 0x101000
    ${If} $ProcessTerminate == 1
      IntOp $0 $0 | 1
    ${EndIf}
    System::Call 'kernel32::OpenProcess(i r0, i 0, i r1) p.s ?e'
    Pop $5
    Pop $ProcessHandle
    ${If} $ProcessHandle == 0
      StrCmp $5 87 scan_next scan_entry_error
    ${EndIf}
    StrCpy $4 ${NSIS_MAX_STRLEN}
    System::Call 'kernel32::QueryFullProcessImageNameW(p $ProcessHandle, i 0, w .r3, *i r4) i.r0'
    StrCmp $0 0 scan_entry_error
    StrCmp $3 "$INSTDIR\$ProcessName" 0 scan_close_process
    ${If} $ProcessTerminate == 0
      StrCpy $ScanResult 10
    ${Else}
      System::Call 'kernel32::WaitForSingleObject(p $ProcessHandle, i 0) i.r0'
      ${If} $0 != 0
        DetailPrint "结束未退出的进程：$ProcessName（PID $ProcessId）"
        System::Call 'kernel32::TerminateProcess(p $ProcessHandle, i 1) i.r0'
        ${If} $0 == 0
          ; The process may have exited between the wait and termination.
          System::Call 'kernel32::WaitForSingleObject(p $ProcessHandle, i 0) i.r0'
          StrCmp $0 0 0 scan_entry_error
        ${EndIf}
      ${EndIf}
      System::Call 'kernel32::WaitForSingleObject(p $ProcessHandle, i 10000) i.r0'
      StrCmp $0 0 0 scan_entry_error
    ${EndIf}
  scan_close_process:
    System::Call 'kernel32::CloseHandle(p $ProcessHandle)'
    StrCpy $ProcessHandle 0
    Goto scan_next
  scan_end:
    System::Free $ProcessEntry
    System::Call 'kernel32::CloseHandle(p $ProcessSnapshot)'
    IntOp $ProcessPass $ProcessPass + 1
    IntCmp $ProcessPass $ScanPassLimit scan_done scan_pass scan_done
  scan_entry_error:
    ${If} $ProcessHandle != 0
      System::Call 'kernel32::CloseHandle(p $ProcessHandle)'
    ${EndIf}
    System::Free $ProcessEntry
  scan_snapshot_error:
    System::Call 'kernel32::CloseHandle(p $ProcessSnapshot)'
  scan_error:
    StrCpy $ScanResult 1
  scan_done:
FunctionEnd
!macroend

!insertmacro DefineScanApplicationProcesses ""
!insertmacro DefineScanApplicationProcesses "un."
!endif

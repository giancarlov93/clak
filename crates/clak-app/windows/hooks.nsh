; The NSIS installer hooks: the app sets up by itself
; PATH, start with Windows, clak:// links, skill and MCP clients ("--setup"), and before being
; removed it removes only what is its own ("--uninstall").

!macro NSIS_HOOK_PREINSTALL
  ; A running copy must be closed before overwriting it; "--update" makes it reopen afterwards.
  IfFileExists "$INSTDIR\ClakApp.exe" 0 clak_closed
    ExecWait '"$INSTDIR\ClakApp.exe" --quit --update'
    ; Copies before 0.3.3 return at once: wait until the exe can be written (the running copy
    ; has gone), or the installer forces it closed and the reopened app can stay blank.
    StrCpy $R9 0
    clak_wait:
      ClearErrors
      FileOpen $R8 "$INSTDIR\ClakApp.exe" a
      IfErrors 0 clak_free
      IntOp $R9 $R9 + 1
      IntCmp $R9 120 clak_closed
      Sleep 250
      Goto clak_wait
    clak_free:
      FileClose $R8
  clak_closed:
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$INSTDIR\ClakApp.exe" --setup'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\ClakApp.exe" --quit'
  ExecWait '"$INSTDIR\ClakApp.exe" --uninstall'
!macroend

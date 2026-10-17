; The NSIS installer hooks: the app sets up by itself
; PATH, start with Windows, clak:// links, skill and MCP clients ("--setup"), and before being
; removed it removes only what is its own ("--uninstall").

!macro NSIS_HOOK_PREINSTALL
  ; A running copy must be closed before overwriting it; "--update" makes it reopen afterwards.
  IfFileExists "$INSTDIR\ClakApp.exe" 0 +2
    ExecWait '"$INSTDIR\ClakApp.exe" --quit --update'
!macroend

!macro NSIS_HOOK_POSTINSTALL
  ExecWait '"$INSTDIR\ClakApp.exe" --setup'
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  ExecWait '"$INSTDIR\ClakApp.exe" --quit'
  ExecWait '"$INSTDIR\ClakApp.exe" --uninstall'
!macroend

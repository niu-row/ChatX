!ifndef CHATX_INSTALLER_HOOKS
!define CHATX_INSTALLER_HOOKS
!define CHATX_HOOK_DIR "${__FILEDIR__}"

!macro CHATX_STOP_INSTALLED_RUNTIME
  ; Preserve the standard confirmation/cancel behavior before stopping children.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
  Push $0
  InitPluginsDir
  File /oname=$PLUGINSDIR\chatx-stop-runtime.ps1 "${CHATX_HOOK_DIR}\stop-runtime.ps1"
  nsExec::ExecToLog /TIMEOUT=30000 '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\chatx-stop-runtime.ps1" -InstallDir "$INSTDIR"'
  Pop $0
  ${If} $0 != 0
    Pop $0
    Abort "ChatX runtime files are still in use. Exit ChatX from the system tray and retry. No runtime files have been replaced."
  ${EndIf}
  Pop $0
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro CHATX_STOP_INSTALLED_RUNTIME
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro CHATX_STOP_INSTALLED_RUNTIME
!macroend
!endif

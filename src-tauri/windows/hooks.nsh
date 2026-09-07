!ifndef CHATX_INSTALLER_HOOKS
!define CHATX_INSTALLER_HOOKS
!define CHATX_HOOK_DIR "${__FILEDIR__}"

!macro CHATX_STOP_INSTALLED_RUNTIME
  ; Stop the exact installed ChatX desktop/runtime processes before NSIS checks
  ; the main executable. Closing the window normally only hides ChatX to tray.
  Push $0
  InitPluginsDir
  File /oname=$PLUGINSDIR\chatx-stop-runtime.ps1 "${CHATX_HOOK_DIR}\stop-runtime.ps1"
  nsExec::ExecToLog /TIMEOUT=35000 '"$SYSDIR\WindowsPowerShell\v1.0\powershell.exe" -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "$PLUGINSDIR\chatx-stop-runtime.ps1" -InstallDir "$INSTDIR" -MainBinaryName "${MAINBINARYNAME}.exe"'
  Pop $0
  ${If} $0 != 0
    Pop $0
    Abort "ChatX could not release the installed files. Close any remaining ChatX process and retry. The installer log contains the exact locked file; no runtime files have been replaced."
  ${EndIf}
  Pop $0

  ; Defense in depth: catch a user relaunching ChatX after cleanup.
  !insertmacro CheckIfAppIsRunning "${MAINBINARYNAME}.exe" "${PRODUCTNAME}"
!macroend

!macro NSIS_HOOK_PREINSTALL
  !insertmacro CHATX_STOP_INSTALLED_RUNTIME
!macroend

!macro NSIS_HOOK_PREUNINSTALL
  !insertmacro CHATX_STOP_INSTALLED_RUNTIME
!macroend
!endif

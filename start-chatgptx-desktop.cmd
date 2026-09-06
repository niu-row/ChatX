@echo off
setlocal
cd /d "%~dp0"
title ChatGPTX Desktop

where node >nul 2>nul || goto :no_node

if exist "src-tauri\target\release\chatgptx-desktop.exe" (
  start "" "src-tauri\target\release\chatgptx-desktop.exe"
  exit /b 0
)

where npm >nul 2>nul || goto :no_node
if exist ".tools\cargo\bin\cargo.exe" (
  set "CARGO_HOME=%CD%\.tools\cargo"
  set "RUSTUP_HOME=%CD%\.tools\rustup"
  set "PATH=%CD%\.tools\cargo\bin;%PATH%"
)
where rustc >nul 2>nul || goto :no_rust
where cargo >nul 2>nul || goto :no_rust

echo [ChatGPTX] Building Node backend and starting Tauri desktop console...
call npm run desktop:dev || goto :failed
exit /b 0

:no_node
echo [ERROR] Node.js 20 or newer was not found.
goto :failed_pause

:no_rust
echo [ERROR] Rust toolchain was not found and no prebuilt desktop executable exists.
echo Install Rust with rustup, then reopen this terminal.
echo Tauri dialog support requires Rust 1.77.2 or newer.
goto :failed_pause

:failed
echo.
echo [ERROR] ChatGPTX Desktop failed to start. Review the error above.

:failed_pause
pause
exit /b 1

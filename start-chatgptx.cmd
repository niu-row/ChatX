@echo off
setlocal
cd /d "%~dp0"
title ChatGPTX Local Console

where node >nul 2>nul || goto :no_node
set "CHATGPTX_CHECK_PORT=3210"
if defined CHATGPTX_PORT set "CHATGPTX_CHECK_PORT=%CHATGPTX_PORT%"
node scripts\check-running.mjs "%CHATGPTX_CHECK_PORT%" >nul 2>nul && goto :already_running

if not exist "node_modules\@modelcontextprotocol\server" (
  echo [ChatGPTX] Installing dependencies...
  call npm install || goto :failed
)

echo [ChatGPTX] Building and opening the local console...
call npm run console || goto :failed
exit /b 0

:already_running
echo [ChatGPTX] An instance is already running. Opening the local console...
start "" "http://127.0.0.1:%CHATGPTX_CHECK_PORT%/"
exit /b 0

:no_node
echo [ERROR] Node.js was not found. Install Node.js 20 or newer.
goto :failed_pause

:failed
echo.
echo [ERROR] ChatGPTX failed to start. Keep this window open and review the error above.

:failed_pause
pause
exit /b 1

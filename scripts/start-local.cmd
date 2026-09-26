@echo off
setlocal
cd /d "%~dp0"
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0start-local.ps1" %*
set "exitCode=%ERRORLEVEL%"
endlocal & exit /b %exitCode%

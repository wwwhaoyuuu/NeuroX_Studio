@echo off
setlocal

powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install-local-app.ps1"
if errorlevel 1 (
  echo.
  echo NeuroX Studio installation failed.
  pause
  exit /b 1
)

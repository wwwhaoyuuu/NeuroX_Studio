@echo off
setlocal

if exist "%~dp0local-env.bat" call "%~dp0local-env.bat"

if not defined RHS_VCVARS64 set "RHS_VCVARS64=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if exist "%RHS_VCVARS64%" goto vcvars_ready
echo Visual C++ environment script was not found: %RHS_VCVARS64%
exit /b 1

:vcvars_ready
if defined FRONTPANEL_SDK goto sdk_ready
echo FRONTPANEL_SDK is not set. Configure scripts\local-env.bat first.
exit /b 1

:sdk_ready
call "%RHS_VCVARS64%"
if errorlevel 1 exit /b %ERRORLEVEL%
set "PATH=%USERPROFILE%\.cargo\bin;%FRONTPANEL_SDK%\API\lib\x64;%PATH%"

cd /d "%~dp0.."
cargo %*

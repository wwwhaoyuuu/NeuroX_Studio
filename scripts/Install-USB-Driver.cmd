@echo off
setlocal
if not "%~1"=="" if /i not "%~1"=="--check" (
    echo Usage: Install-USB-Driver.cmd [--check]
    exit /b 2
)
for %%F in (okusb.inf okusb.cat okuart.inf okuart.cat WdfCoInstaller01009.dll WinUSBCoInstaller2.dll) do (
    if not exist "%~dp0drivers\amd64\%%F" (
        echo Missing driver file: %%F. Extract the complete hardware ZIP first.
        exit /b 2
    )
)
if /i "%~1"=="--check" (
    echo Driver files present. Check only: no driver installed and no device opened.
    exit /b 0
)
set "ARCH=%PROCESSOR_ARCHITECTURE%"
if defined PROCESSOR_ARCHITEW6432 set "ARCH=%PROCESSOR_ARCHITEW6432%"
if /i not "%ARCH%"=="AMD64" (
    echo This package supports Windows x64 only, not ARM64 or 32-bit Windows.
    pause
    exit /b 2
)
"%SystemRoot%\System32\fltmc.exe" >nul 2>&1
if errorlevel 1 (
    echo Right-click Install-USB-Driver.cmd and select Run as administrator.
    echo This installs the bundled Opal Kelly USB driver, not the NeuroX application.
    pause
    exit /b 1
)
echo Install the original FrontPanel 5.2.10 USB driver for the lab controller?
echo Existing newer or higher-ranked drivers will not be forcibly replaced.
"%SystemRoot%\System32\choice.exe" /c YN /n /m "Continue [Y/N]? "
if errorlevel 2 exit /b 0
if errorlevel 1 goto install
exit /b 1
:install
"%SystemRoot%\System32\pnputil.exe" /add-driver "%~dp0drivers\amd64\okusb.inf" /install
set "RESULT=%ERRORLEVEL%"
if "%RESULT%"=="3010" (
    echo Windows requests a restart. Restart manually when convenient.
) else if "%RESULT%"=="0" (
    echo Driver package processed. Connect the powered data USB port and scan in NeuroX.
    echo This does not verify board detection or physical stimulation output.
) else (
    echo Driver installation failed with code %RESULT%. Keep the Windows error above.
    echo Do not disable driver signing or Windows security protections.
)
pause
exit /b %RESULT%

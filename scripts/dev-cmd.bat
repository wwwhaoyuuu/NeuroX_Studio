@echo off
setlocal

call "%~dp0dev-cargo.bat" run -p rhs-cli -- %*
exit /b %ERRORLEVEL%

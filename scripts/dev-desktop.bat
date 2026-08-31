@echo off
setlocal

pushd "%~dp0..\apps\rhs-desktop"
if not exist "node_modules\lucide\dist\umd\lucide.js" (
  call npm install
  if errorlevel 1 goto :frontend_failed
)
call npm run build
if errorlevel 1 goto :frontend_failed
popd

pushd "%~dp0.."
call "%~dp0dev-cargo.bat" --config .\work\cargo-direct.toml run -p rhs-desktop
set "EXIT_CODE=%ERRORLEVEL%"
popd
exit /b %EXIT_CODE%

:frontend_failed
set "EXIT_CODE=%ERRORLEVEL%"
popd
exit /b %EXIT_CODE%

@echo off
setlocal
if exist "%~dp0local-env.bat" call "%~dp0local-env.bat"
if not defined RHS_VCVARS64 set "RHS_VCVARS64=C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if not exist "%RHS_VCVARS64%" exit /b 1
call "%RHS_VCVARS64%"
if errorlevel 1 exit /b %ERRORLEVEL%
cd /d "%~dp0.."
if not exist "target\native-tests" mkdir "target\native-tests"
cl.exe /nologo /EHsc /std:c++14 /MD /DWIN32 /D_WINDOWS /DNOMINMAX /I"crates\rhs-device\native" /I"vendor\intan-rhythmstim-api-230822" "crates\rhs-device\native\tests\stim_sequence_test.cpp" /Fo"target\native-tests\stim_sequence_test.obj" /Fe"target\native-tests\stim_sequence_test.exe"
if errorlevel 1 exit /b %ERRORLEVEL%
"target\native-tests\stim_sequence_test.exe"
exit /b %ERRORLEVEL%

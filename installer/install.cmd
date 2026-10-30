@echo off
rem Responsibility: Install from the zip without the setup program: ask for the
rem work account and run "digital-wellbeing-service.exe install" as administrator.
net session >nul 2>&1
if errorlevel 1 (
    powershell -NoProfile -Command "Start-Process -Verb RunAs -FilePath '%~f0'"
    exit /b
)
cd /d "%~dp0"
echo Digital Wellbeing - machine install
echo.
set "DW_USER="
set /p DW_USER=Work account user name (empty = manage every account): 
if "%DW_USER%"=="" (
    digital-wellbeing-service.exe install --manage-all
) else (
    digital-wellbeing-service.exe install --manage "%DW_USER%"
)
echo.
pause

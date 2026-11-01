@echo off
rem Responsibility: Install from the zip without the setup program: run
rem "digital-wellbeing-service.exe install --manage-all" as administrator (every
rem Windows account is managed, the administrators included).
net session >nul 2>&1
if errorlevel 1 (
    powershell -NoProfile -Command "Start-Process -Verb RunAs -FilePath '%~f0'"
    exit /b
)
cd /d "%~dp0"
echo Digital Wellbeing - machine install (every account is managed)
echo.
digital-wellbeing-service.exe install --manage-all
echo.
pause

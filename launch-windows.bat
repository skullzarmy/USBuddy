@echo off
:: USBuddy launcher for Windows.
:: Double-click this file to start USBuddy.
:: If SmartScreen prompts, click "More info" then "Run anyway".

setlocal EnableDelayedExpansion

set "DRIVE_ROOT=%~dp0"
:: Strip trailing backslash.
if "%DRIVE_ROOT:~-1%"=="\" set "DRIVE_ROOT=%DRIVE_ROOT:~0,-1%"

set "CURRENT=%DRIVE_ROOT%\current.json"

if not exist "%CURRENT%" (
    echo USBuddy: current.json not found.
    echo Re-run the installer to set up this drive.
    pause
    exit /b 1
)

:: Read the "active" field using PowerShell.
for /f "delims=" %%A in ('powershell -NoProfile -Command ^
    "(Get-Content -Raw '%CURRENT%' | ConvertFrom-Json).active"') do (
    set "ACTIVE=%%A"
)

if "!ACTIVE!"=="" (
    echo USBuddy: could not read active version from current.json.
    pause
    exit /b 1
)

set "RUNTIME=%DRIVE_ROOT%\versions\!ACTIVE!\bin\windows-x64\usbuddy-runtime.exe"

if not exist "!RUNTIME!" (
    echo USBuddy: runtime binary not found at !RUNTIME!
    pause
    exit /b 1
)

start "" "!RUNTIME!" serve --drive "%DRIVE_ROOT%" --open-browser

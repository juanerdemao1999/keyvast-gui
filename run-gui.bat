@echo off
REM ============================================================
REM  Keyvast GUI - one-click launcher (runs the prebuilt binary)
REM  Double-click this file to open the GUI for testing.
REM  - Working dir is forced to the repo root so the GUI finds
REM    third_party\opalkelly\windows-x64\okFrontPanel.dll,
REM    config, and recordings.
REM  - Uses the already-built release exe = instant start, no cargo.
REM  - If the exe is missing, it builds once, then launches.
REM  Tip: to always run the LATEST source, use gui.bat instead
REM  (it does an incremental rebuild before launching).
REM ============================================================
title Keyvast GUI
cd /d "%~dp0"
set RUST_LOG=info
set "EXE=target\release\kv-gui.exe"

if not exist "%EXE%" (
    echo [Keyvast] Release binary not found - building once ^(first run^)...
    cargo build --release -p kv-gui
    if errorlevel 1 (
        echo.
        echo [Keyvast] Build failed. Press any key to exit.
        pause >nul
        exit /b 1
    )
)

echo [Keyvast] Launching GUI: %EXE%
echo [Keyvast] Pick Source = Simulator to test without hardware, or RHD for the headstage.
echo [Keyvast] (Close the GUI window to return here.)
"%EXE%"

if errorlevel 1 (
    echo.
    echo [Keyvast] GUI exited with a non-zero code. Press any key to close.
    pause >nul
)

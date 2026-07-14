@echo off
REM ============================================================
REM  Keyvast GUI - one-click launcher (incrementally builds, then runs)
REM  Double-click this file to open the GUI for testing.
REM  - Working dir is forced to the repo root so the GUI finds
REM    third_party\opalkelly\windows-x64\okFrontPanel.dll,
REM    config, and recordings.
REM  - Runs an incremental release build before launch so hardware fixes in
REM    the source tree can never be hidden by a stale prebuilt executable.
REM ============================================================
title Keyvast GUI
cd /d "%~dp0"
REM  Logging: default is a concise info-level console log. Run
REM     run-gui.bat debug
REM  for verbose RHD bring-up logs -- the full per-port x per-delay MISO scan
REM  plus raw AuxCmd3/INTAN and amplifier hex dumps. Scoped to kv_rhd so the
REM  egui/wgpu internals stay quiet.
set "RUST_LOG=info"
if /I "%~1"=="debug" (
    set "RUST_LOG=info,kv_rhd=debug"
    echo [Keyvast] DEBUG logging enabled: RUST_LOG=info,kv_rhd=debug
)
set "EXE=target\release\kv-gui.exe"

echo [Keyvast] Checking for source updates...
cargo build --release -p kv-gui
if errorlevel 1 (
    echo.
    echo [Keyvast] Build failed. Press any key to exit.
    pause >nul
    exit /b 1
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

@echo off
title Keyvast GUI
echo [Keyvast] Building and launching GUI...
cargo run --bin kv-gui --release
if errorlevel 1 (
    echo.
    echo [Keyvast] Build failed. Press any key to exit.
    pause >nul
)

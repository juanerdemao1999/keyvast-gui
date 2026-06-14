@echo off
title Keyvast GUI
echo [Keyvast] Building and launching GUI...
set CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
set CARGO_NET_RETRY=10
set CARGO_HTTP_TIMEOUT=120
set CARGO_HTTP_LOW_SPEED_LIMIT=1
cargo run --bin kv-gui --release
if errorlevel 1 (
    echo.
    echo [Keyvast] Build failed. Press any key to exit.
    pause >nul
)

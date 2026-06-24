@echo off
title Keyvast GUI (log capture)
echo [Keyvast] Building and launching GUI; RHD diagnostics will be written to kvlog.txt
echo [Keyvast] Connect the headstage, pick Source=RHD, press Start, watch a few seconds,
echo [Keyvast] then CLOSE the GUI window. kvlog.txt will contain the scan + first-block log.
set CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse
set CARGO_NET_RETRY=10
set CARGO_HTTP_TIMEOUT=120
set CARGO_HTTP_LOW_SPEED_LIMIT=1
set RUST_LOG=info
cargo run --bin kv-gui --release 2> kvlog.txt
echo.
echo [Keyvast] Done. Diagnostics saved to kvlog.txt
pause >nul

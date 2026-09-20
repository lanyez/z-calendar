@echo off
set PATH=E:\tools\mingw64\bin;%PATH%
cd /d "%~dp0"
cargo build --release
cargo test --release

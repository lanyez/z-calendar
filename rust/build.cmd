@echo off
setlocal
rem MinGW-w64 on PATH: gcc is needed by ring, windres by build.rs for the exe icon + version info
set PATH=E:\tools\mingw64\bin;%PATH%
cd /d "%~dp0"

rem Local gate. The repo remote is Gitee, so .github/workflows/ci.yml never runs here;
rem this script is what actually enforces the checks before a release:
rem   1. release build      fails when windres is missing instead of silently linking the
rem                         prebuilt resource object (stale exe version info)
rem   2. clippy             errors are fatal, existing warnings are only reported
rem   3. tests              offline unit tests (network tests are #[ignore]: run them with
rem                         "cargo test --release -- --ignored")
rem   4. tag/version check  when HEAD carries a tag, it must equal the Cargo.toml version

cargo build --release
if errorlevel 1 exit /b 1

cargo clippy --all-targets
if errorlevel 1 exit /b 1

cargo test --release
if errorlevel 1 exit /b 1

set "VER="
for /f "tokens=3 delims= " %%a in ('findstr /b /c:"version = " Cargo.toml') do if not defined VER set "VER=%%~a"
set "TAG="
for /f "delims=" %%t in ('git describe --exact-match --tags HEAD 2^>nul') do set "TAG=%%t"
if not defined TAG goto :untagged
if "%TAG%"=="%VER%" goto :version_ok
echo [ERROR] git tag %TAG% does not match Cargo.toml version %VER%
exit /b 1
:version_ok
echo [OK] tag %TAG% matches Cargo.toml version
goto :done
:untagged
echo [OK] HEAD is not tagged, Cargo.toml version is %VER%
:done
endlocal

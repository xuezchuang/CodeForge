@echo off
setlocal

cd /d "%~dp0"

set "REPO_ROOT=%~dp0"
set "MANIFEST=%REPO_ROOT%src-tui\Cargo.toml"
set "CLI_BIN=%REPO_ROOT%src-tauri\target\release\codeforge.exe"
set "SRC_TUI_CLI_BIN=%REPO_ROOT%src-tui\target\release\codeforge.exe"
set "LAST_EXIT=0"

if not exist "%MANIFEST%" (
  echo [ERROR] Cannot find src-tui\Cargo.toml at: %MANIFEST%
  set "LAST_EXIT=1"
  goto :__done
)

echo [1/1] Build CodeForge CLI from src-tui in release mode...
call cargo build --manifest-path "%MANIFEST%" --release --bin codeforge
if errorlevel 1 (
  echo [ERROR] codeforge CLI release build failed.
  set "LAST_EXIT=1"
  goto :__done
)

if not exist "%SRC_TUI_CLI_BIN%" (
  echo [ERROR] Release binary not found: %SRC_TUI_CLI_BIN%
  set "LAST_EXIT=1"
  goto :__done
)

if not exist "%REPO_ROOT%src-tauri\target\release" mkdir "%REPO_ROOT%src-tauri\target\release"
copy /Y "%SRC_TUI_CLI_BIN%" "%CLI_BIN%" >nul
if errorlevel 1 (
  echo [ERROR] Failed to copy CodeForge CLI to: %CLI_BIN%
  set "LAST_EXIT=1"
  goto :__done
)

echo.
echo Finished.
echo CodeForge CLI release binary: %CLI_BIN%

:__done
echo.
if "%LAST_EXIT%" NEQ "0" (
  echo Build finished with failures. Press any key to close.
) else (
  echo Build finished successfully. Press any key to close.
)
pause
exit /b %LAST_EXIT%

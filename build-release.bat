@echo off
setlocal
cd /d "%~dp0"
set "LAST_EXIT=0"
set "APP_EXE=%~dp0src-tauri\target\release\codeforge-desktop.exe"

echo Building CodeForge release binary without installer bundles...
call :__check_fresh
if "%LAST_EXIT%"=="0" (
  echo.
  echo Release binary is already up to date.
  echo App:
  echo   %APP_EXE%
  goto :__done
)

set "LAST_EXIT=0"
call npm run tauri build -- --no-bundle
if errorlevel 1 (
  echo.
  echo Release binary build failed with exit code 1.
  set "LAST_EXIT=1"
  goto :__done
)

if "%LAST_EXIT%"=="0" (
  echo.
  echo Release binary build finished.
  echo App:
  echo   %~dp0src-tauri\target\release\codeforge-desktop.exe
)

goto :__done

:__done
echo.
if "%LAST_EXIT%" NEQ "0" (
  echo Build finished with failures. Press any key to close.
) else (
  echo Build finished successfully. Press any key to close.
)
pause
exit /b %LAST_EXIT%

:__check_fresh
if not exist "%APP_EXE%" (
  set "LAST_EXIT=1"
  exit /b 0
)

powershell -NoProfile -ExecutionPolicy Bypass -Command "$ErrorActionPreference = 'Stop'; $root = (Resolve-Path -LiteralPath '%~dp0').Path; $exe = Get-Item -LiteralPath $env:APP_EXE; $inputs = @('package.json', 'package-lock.json', 'index.html', 'tsconfig.json', 'tsconfig.app.json', 'tsconfig.node.json', 'vite.config.ts', 'src', 'public', 'crates', 'src-tauri\Cargo.toml', 'src-tauri\Cargo.lock', 'src-tauri\build.rs', 'src-tauri\tauri.conf.json', 'src-tauri\capabilities', 'src-tauri\icons', 'src-tauri\src'); foreach ($inputPath in $inputs) { $fullPath = Join-Path $root $inputPath; if (!(Test-Path -LiteralPath $fullPath)) { continue }; $newerInput = Get-ChildItem -LiteralPath $fullPath -Recurse -File -Force | Where-Object { $_.FullName -notmatch '[\\/](target|node_modules|dist)[\\/]' -and $_.LastWriteTimeUtc -gt $exe.LastWriteTimeUtc } | Select-Object -First 1; if ($newerInput) { exit 1 } }; exit 0"
if errorlevel 1 (
  set "LAST_EXIT=1"
) else (
  set "LAST_EXIT=0"
)
exit /b 0

@echo off
setlocal

cd /d "%~dp0"

echo Building CodeForge release installer bundles...
call npm run tauri build
if errorlevel 1 (
  echo.
  echo Release installer build failed with exit code %errorlevel%.
  exit /b %errorlevel%
)

echo.
echo Release installer build finished.
echo Bundles:
echo   %~dp0src-tauri\target\release\bundle\nsis\CodeForge_0.1.0_x64-setup.exe
echo   %~dp0src-tauri\target\release\bundle\msi\CodeForge_0.1.0_x64_en-US.msi

endlocal

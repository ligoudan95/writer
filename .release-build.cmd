@echo off
rem Release build: NSIS installer + updater signature
rem Private key location: APPDATA Writer update-private.key (do not commit)
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
set TAURI_SIGNING_PRIVATE_KEY=%APPDATA%\Writer\update-private.key
set TAURI_SIGNING_PRIVATE_KEY_PASSWORD=
cd /d F:\writer
call npx tauri build 2>&1
echo BUILD_EXIT=%ERRORLEVEL%

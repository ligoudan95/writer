@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
cd /d D:\writer
call npx tauri build --no-bundle
echo BUILD_EXIT=%ERRORLEVEL%

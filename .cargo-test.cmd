@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
cd /d D:\writer\src-tauri
%USERPROFILE%\.cargo\bin\cargo test
echo TEST_EXIT=%ERRORLEVEL%

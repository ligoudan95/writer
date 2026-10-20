@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
cd /d D:\writer\src-tauri
%USERPROFILE%\.cargo\bin\cargo build
echo CARGO_EXIT=%ERRORLEVEL%

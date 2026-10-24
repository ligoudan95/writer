@echo off
rem 正式发布构建：NSIS 安装器 + 更新签名（私钥： %APPDATA%\Writer\update-private.key ，勿外传勿入库）
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul
set PATH=%PATH%;%USERPROFILE%\.cargo\bin
set TAURI_SIGNING_PRIVATE_KEY=%APPDATA%\Writer\update-private.key
set TAURI_SIGNING_PRIVATE_KEY_PASSWORD=
cd /d F:\writer
call npx tauri build 2>&1
echo BUILD_EXIT=%ERRORLEVEL%
echo 产物与 latest.json 在 src-tauri\target\release\bundle\nsis\ —— 随 Release 一并上传

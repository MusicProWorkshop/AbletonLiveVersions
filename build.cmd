@echo off
rem Builds LiveVersions.exe (see build.ps1).   build.cmd --release
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*

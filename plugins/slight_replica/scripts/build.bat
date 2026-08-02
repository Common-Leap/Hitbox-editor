@echo off
setlocal
where py >nul 2>&1
if %errorlevel% equ 0 (
    py -3 "%~dp0..\tools\build_plugin.py" %*
) else (
    python "%~dp0..\tools\build_plugin.py" %*
)
